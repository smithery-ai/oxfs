use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use rusqlite::Connection;

use crate::meta::*;
use crate::types::*;

pub struct SqliteMetaEngine {
    conn: Mutex<Connection>,
}

impl SqliteMetaEngine {
    pub fn new(path: &Path) -> MetaResult<Self> {
        let conn = Connection::open(path)
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn in_memory() -> MetaResult<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

fn system_time_to_nanos(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_nanos() as i64
}

fn nanos_to_system_time(nanos: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(nanos as u64)
}

fn file_type_to_int(ft: FileType) -> i32 {
    match ft {
        FileType::Regular => 1,
        FileType::Directory => 2,
        FileType::Symlink => 3,
        FileType::Fifo => 4,
        FileType::Socket => 5,
        FileType::BlockDevice => 6,
        FileType::CharDevice => 7,
    }
}

fn int_to_file_type(i: i32) -> FileType {
    match i {
        2 => FileType::Directory,
        3 => FileType::Symlink,
        4 => FileType::Fifo,
        5 => FileType::Socket,
        6 => FileType::BlockDevice,
        7 => FileType::CharDevice,
        _ => FileType::Regular,
    }
}

fn touch_parent(conn: &Connection, parent: u64) {
    let now = system_time_to_nanos(SystemTime::now());
    let _ = conn.execute(
        "UPDATE node SET mtime = ?1, ctime = ?1 WHERE inode = ?2",
        rusqlite::params![now, parent as i64],
    );
}

fn get_attr_locked(conn: &Connection, inode: u64) -> MetaResult<InodeAttr> {
    conn.query_row(
        "SELECT inode, kind, mode, uid, gid, size, nlink, atime, mtime, ctime FROM node WHERE inode = ?1",
        [inode as i64],
        |row| {
            Ok(InodeAttr {
                inode: row.get::<_, i64>(0)? as u64,
                kind: int_to_file_type(row.get(1)?),
                mode: row.get::<_, i64>(2)? as u32,
                uid: row.get::<_, i64>(3)? as u32,
                gid: row.get::<_, i64>(4)? as u32,
                size: row.get::<_, i64>(5)? as u64,
                blocks: (row.get::<_, i64>(5)? as u64).div_ceil(512),
                nlink: row.get::<_, i64>(6)? as u32,
                atime: nanos_to_system_time(row.get(7)?),
                mtime: nanos_to_system_time(row.get(8)?),
                ctime: nanos_to_system_time(row.get(9)?),
                rdev: 0,
            })
        },
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => MetaError::NotFound,
        _ => MetaError::Internal(e.to_string()),
    })
}

fn lookup_locked(conn: &Connection, parent: u64, name: &str) -> MetaResult<InodeAttr> {
    let child_inode: i64 = conn
        .query_row(
            "SELECT child FROM edge WHERE parent = ?1 AND name = ?2",
            rusqlite::params![parent as i64, name],
            |row| row.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => MetaError::NotFound,
            _ => MetaError::Internal(e.to_string()),
        })?;
    get_attr_locked(conn, child_inode as u64)
}

fn read_slices_locked(conn: &Connection, inode: u64, chunk_idx: u32) -> MetaResult<Vec<Slice>> {
    let json: String = conn
        .query_row(
            "SELECT slices FROM chunk WHERE inode = ?1 AND chunk_idx = ?2",
            rusqlite::params![inode as i64, chunk_idx as i64],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "[]".to_string());
    serde_json::from_str(&json).map_err(|e| MetaError::Internal(e.to_string()))
}

fn lock(conn: &Mutex<Connection>) -> std::sync::MutexGuard<'_, Connection> {
    conn.lock().unwrap_or_else(|e| e.into_inner())
}

#[async_trait]
impl MetaEngine for SqliteMetaEngine {
    async fn init(&self) -> MetaResult<()> {
        let conn = lock(&self.conn);
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS node (
                inode   INTEGER PRIMARY KEY,
                kind    INTEGER NOT NULL,
                mode    INTEGER NOT NULL,
                uid     INTEGER NOT NULL,
                gid     INTEGER NOT NULL,
                size    INTEGER NOT NULL DEFAULT 0,
                nlink   INTEGER NOT NULL DEFAULT 1,
                atime   INTEGER NOT NULL,
                mtime   INTEGER NOT NULL,
                ctime   INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS edge (
                parent  INTEGER NOT NULL,
                name    TEXT NOT NULL,
                child   INTEGER NOT NULL,
                kind    INTEGER NOT NULL,
                PRIMARY KEY (parent, name)
            );
            CREATE TABLE IF NOT EXISTS chunk (
                inode       INTEGER NOT NULL,
                chunk_idx   INTEGER NOT NULL,
                slices      TEXT NOT NULL DEFAULT '[]',
                PRIMARY KEY (inode, chunk_idx)
            );
            CREATE TABLE IF NOT EXISTS symlink (
                inode   INTEGER PRIMARY KEY,
                target  TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS xattr (
                inode   INTEGER NOT NULL,
                key     TEXT NOT NULL,
                value   BLOB NOT NULL,
                PRIMARY KEY (inode, key)
            );
            CREATE TABLE IF NOT EXISTS slice_ref (
                slice_id    INTEGER PRIMARY KEY,
                refcount    INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE IF NOT EXISTS counter (
                name    TEXT PRIMARY KEY,
                value   INTEGER NOT NULL
            );",
        )
        .map_err(|e| MetaError::Internal(e.to_string()))?;

        let root_exists: bool = conn
            .query_row("SELECT COUNT(*) FROM node WHERE inode = 1", [], |row| row.get::<_, i64>(0))
            .map(|c| c > 0)
            .map_err(|e| MetaError::Internal(e.to_string()))?;

        if !root_exists {
            let now = system_time_to_nanos(SystemTime::now());
            conn.execute(
                "INSERT INTO node (inode, kind, mode, uid, gid, size, nlink, atime, mtime, ctime) VALUES (1, 2, 16877, 0, 0, 0, 2, ?1, ?1, ?1)",
                [now],
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;

            conn.execute(
                "INSERT OR IGNORE INTO counter (name, value) VALUES ('next_inode', 2)",
                [],
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;

            conn.execute(
                "INSERT OR IGNORE INTO counter (name, value) VALUES ('next_slice', 1)",
                [],
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        }

        Ok(())
    }

    async fn get_attr(&self, inode: u64) -> MetaResult<InodeAttr> {
        let conn = lock(&self.conn);
        get_attr_locked(&conn, inode)
    }

    async fn lookup(&self, parent: u64, name: &str) -> MetaResult<InodeAttr> {
        if name.len() > NAME_MAX { return Err(MetaError::NameTooLong); }
        let conn = lock(&self.conn);
        lookup_locked(&conn, parent, name)
    }

    async fn readdir(&self, inode: u64) -> MetaResult<Vec<DirEntry>> {
        let conn = lock(&self.conn);
        let attr = get_attr_locked(&conn, inode)?;
        if attr.kind != FileType::Directory {
            return Err(MetaError::NotDirectory);
        }

        let mut stmt = conn
            .prepare("SELECT child, name, kind FROM edge WHERE parent = ?1")
            .map_err(|e| MetaError::Internal(e.to_string()))?;

        let entries = stmt
            .query_map([inode as i64], |row| {
                Ok(DirEntry {
                    inode: row.get::<_, i64>(0)? as u64,
                    name: row.get(1)?,
                    kind: int_to_file_type(row.get(2)?),
                })
            })
            .map_err(|e| MetaError::Internal(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| MetaError::Internal(e.to_string()))?;

        Ok(entries)
    }

    async fn create(
        &self,
        parent: u64,
        name: &str,
        kind: FileType,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> MetaResult<InodeAttr> {
        if name.len() > NAME_MAX {
            return Err(MetaError::NameTooLong);
        }
        let conn = lock(&self.conn);
        let parent_attr = get_attr_locked(&conn, parent)?;
        if parent_attr.kind != FileType::Directory {
            return Err(MetaError::NotDirectory);
        }

        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM edge WHERE parent = ?1 AND name = ?2",
                rusqlite::params![parent as i64, name],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .map_err(|e| MetaError::Internal(e.to_string()))?;

        if exists {
            return Err(MetaError::AlreadyExists);
        }

        let inode: i64 = conn
            .query_row(
                "UPDATE counter SET value = value + 1 WHERE name = 'next_inode' RETURNING value",
                [],
                |row| row.get(0),
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;

        let now = system_time_to_nanos(SystemTime::now());
        let nlink: i64 = if kind == FileType::Directory { 2 } else { 1 };

        conn.execute(
            "INSERT INTO node (inode, kind, mode, uid, gid, size, nlink, atime, mtime, ctime) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?7, ?7)",
            rusqlite::params![inode, file_type_to_int(kind), mode as i64, uid as i64, gid as i64, nlink, now],
        )
        .map_err(|e| MetaError::Internal(e.to_string()))?;

        conn.execute(
            "INSERT INTO edge (parent, name, child, kind) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![parent as i64, name, inode, file_type_to_int(kind)],
        )
        .map_err(|e| MetaError::Internal(e.to_string()))?;

        if kind == FileType::Directory {
            conn.execute(
                "UPDATE node SET nlink = nlink + 1 WHERE inode = ?1",
                [parent as i64],
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        }

        touch_parent(&conn, parent);
        get_attr_locked(&conn, inode as u64)
    }

    async fn read_slices(&self, inode: u64, chunk_idx: u32) -> MetaResult<Vec<Slice>> {
        let conn = lock(&self.conn);
        read_slices_locked(&conn, inode, chunk_idx)
    }

    async fn write_slice(&self, inode: u64, chunk_idx: u32, slice: Slice) -> MetaResult<()> {
        let conn = lock(&self.conn);
        let mut slices = read_slices_locked(&conn, inode, chunk_idx)?;
        slices.push(slice);
        let json = serde_json::to_string(&slices).map_err(|e| MetaError::Internal(e.to_string()))?;

        conn.execute(
            "INSERT INTO chunk (inode, chunk_idx, slices) VALUES (?1, ?2, ?3)
             ON CONFLICT (inode, chunk_idx) DO UPDATE SET slices = ?3",
            rusqlite::params![inode as i64, chunk_idx as i64, json],
        )
        .map_err(|e| MetaError::Internal(e.to_string()))?;

        Ok(())
    }

    async fn set_attr(&self, inode: u64, req: SetAttrRequest) -> MetaResult<InodeAttr> {
        let conn = lock(&self.conn);
        let now = system_time_to_nanos(SystemTime::now());

        if let Some(new_size) = req.size {
            conn.execute(
                "UPDATE node SET size = ?1, mtime = ?2, ctime = ?2 WHERE inode = ?3",
                rusqlite::params![new_size as i64, now, inode as i64],
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        }
        if let Some(mode) = req.mode {
            conn.execute(
                "UPDATE node SET mode = ?1, ctime = ?2 WHERE inode = ?3",
                rusqlite::params![mode as i64, now, inode as i64],
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        }
        if req.uid.is_some() || req.gid.is_some() {
            let cur = get_attr_locked(&conn, inode)?;
            let new_uid = req.uid.unwrap_or(cur.uid);
            let new_gid = req.gid.unwrap_or(cur.gid);
            // POSIX: clear setuid/setgid on chown
            let cleared_mode = cur.mode & !0o6000;
            conn.execute(
                "UPDATE node SET uid = ?1, gid = ?2, mode = ?3, ctime = ?4 WHERE inode = ?5",
                rusqlite::params![new_uid as i64, new_gid as i64, cleared_mode as i64, now, inode as i64],
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        }
        if let Some(atime) = req.atime {
            conn.execute(
                "UPDATE node SET atime = ?1 WHERE inode = ?2",
                rusqlite::params![system_time_to_nanos(atime), inode as i64],
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        }
        if let Some(mtime) = req.mtime {
            conn.execute(
                "UPDATE node SET mtime = ?1, ctime = ?2 WHERE inode = ?3",
                rusqlite::params![system_time_to_nanos(mtime), now, inode as i64],
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        }

        get_attr_locked(&conn, inode)
    }

    async fn unlink(&self, parent: u64, name: &str) -> MetaResult<()> {
        if name.len() > NAME_MAX { return Err(MetaError::NameTooLong); }
        let conn = lock(&self.conn);
        let attr = lookup_locked(&conn, parent, name)?;

        if attr.kind == FileType::Directory {
            let mut stmt = conn
                .prepare("SELECT COUNT(*) FROM edge WHERE parent = ?1")
                .map_err(|e| MetaError::Internal(e.to_string()))?;
            let count: i64 = stmt
                .query_row([attr.inode as i64], |row| row.get(0))
                .map_err(|e| MetaError::Internal(e.to_string()))?;
            if count > 0 {
                return Err(MetaError::NotEmpty);
            }
        }

        conn.execute(
            "DELETE FROM edge WHERE parent = ?1 AND name = ?2",
            rusqlite::params![parent as i64, name],
        )
        .map_err(|e| MetaError::Internal(e.to_string()))?;

        conn.execute(
            "UPDATE node SET nlink = nlink - 1 WHERE inode = ?1",
            [attr.inode as i64],
        )
        .map_err(|e| MetaError::Internal(e.to_string()))?;

        // Node with nlink=0 stays until forget() is called by the kernel
        // (the kernel keeps the inode alive while any fd is open)

        if attr.kind == FileType::Directory {
            conn.execute(
                "UPDATE node SET nlink = nlink - 1 WHERE inode = ?1",
                [parent as i64],
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        }

        touch_parent(&conn, parent);
        Ok(())
    }

    async fn rename(
        &self,
        src_parent: u64,
        src_name: &str,
        dst_parent: u64,
        dst_name: &str,
    ) -> MetaResult<()> {
        if src_name.len() > NAME_MAX || dst_name.len() > NAME_MAX { return Err(MetaError::NameTooLong); }
        if src_parent == dst_parent && src_name == dst_name {
            return Ok(());
        }

        let conn = lock(&self.conn);
        let attr = lookup_locked(&conn, src_parent, src_name)?;

        // Remove destination if it exists (within same lock)
        if let Ok(dst_attr) = lookup_locked(&conn, dst_parent, dst_name) {
            if dst_attr.kind == FileType::Directory {
                let count: i64 = conn
                    .query_row("SELECT COUNT(*) FROM edge WHERE parent = ?1", [dst_attr.inode as i64], |row| row.get(0))
                    .map_err(|e| MetaError::Internal(e.to_string()))?;
                if count > 0 {
                    return Err(MetaError::NotEmpty);
                }
            }

            conn.execute(
                "DELETE FROM edge WHERE parent = ?1 AND name = ?2",
                rusqlite::params![dst_parent as i64, dst_name],
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;

            conn.execute(
                "UPDATE node SET nlink = nlink - 1 WHERE inode = ?1",
                [dst_attr.inode as i64],
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;

            conn.execute(
                "DELETE FROM node WHERE inode = ?1 AND nlink <= 0",
                [dst_attr.inode as i64],
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;

            if dst_attr.kind == FileType::Directory {
                conn.execute(
                    "UPDATE node SET nlink = nlink - 1 WHERE inode = ?1",
                    [dst_parent as i64],
                )
                .map_err(|e| MetaError::Internal(e.to_string()))?;
            }
        }

        conn.execute(
            "UPDATE edge SET parent = ?1, name = ?2 WHERE parent = ?3 AND name = ?4",
            rusqlite::params![dst_parent as i64, dst_name, src_parent as i64, src_name],
        )
        .map_err(|e| MetaError::Internal(e.to_string()))?;

        if attr.kind == FileType::Directory && src_parent != dst_parent {
            conn.execute(
                "UPDATE node SET nlink = nlink - 1 WHERE inode = ?1",
                [src_parent as i64],
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;
            conn.execute(
                "UPDATE node SET nlink = nlink + 1 WHERE inode = ?1",
                [dst_parent as i64],
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        }

        let now = system_time_to_nanos(SystemTime::now());
        let _ = conn.execute(
            "UPDATE node SET ctime = ?1 WHERE inode = ?2",
            rusqlite::params![now, attr.inode as i64],
        );
        touch_parent(&conn, src_parent);
        if src_parent != dst_parent {
            touch_parent(&conn, dst_parent);
        }

        Ok(())
    }

    async fn next_slice_id(&self) -> MetaResult<u64> {
        let conn = lock(&self.conn);
        let id: i64 = conn
            .query_row(
                "UPDATE counter SET value = value + 1 WHERE name = 'next_slice' RETURNING value",
                [],
                |row| row.get(0),
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        Ok(id as u64)
    }

    async fn symlink(&self, parent: u64, name: &str, target: &str, uid: u32, gid: u32) -> MetaResult<InodeAttr> {
        if name.len() > NAME_MAX { return Err(MetaError::NameTooLong); }
        let conn = lock(&self.conn);
        let parent_attr = get_attr_locked(&conn, parent)?;
        if parent_attr.kind != FileType::Directory {
            return Err(MetaError::NotDirectory);
        }

        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM edge WHERE parent = ?1 AND name = ?2",
                rusqlite::params![parent as i64, name],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        if exists {
            return Err(MetaError::AlreadyExists);
        }

        let inode: i64 = conn
            .query_row(
                "UPDATE counter SET value = value + 1 WHERE name = 'next_inode' RETURNING value",
                [],
                |row| row.get(0),
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;

        let now = system_time_to_nanos(SystemTime::now());
        conn.execute(
            "INSERT INTO node (inode, kind, mode, uid, gid, size, nlink, atime, mtime, ctime) VALUES (?1, 3, 41471, ?2, ?3, ?4, 1, ?5, ?5, ?5)",
            rusqlite::params![inode, uid as i64, gid as i64, target.len() as i64, now],
        )
        .map_err(|e| MetaError::Internal(e.to_string()))?;

        conn.execute(
            "INSERT INTO symlink (inode, target) VALUES (?1, ?2)",
            rusqlite::params![inode, target],
        )
        .map_err(|e| MetaError::Internal(e.to_string()))?;

        conn.execute(
            "INSERT INTO edge (parent, name, child, kind) VALUES (?1, ?2, ?3, 3)",
            rusqlite::params![parent as i64, name, inode],
        )
        .map_err(|e| MetaError::Internal(e.to_string()))?;

        get_attr_locked(&conn, inode as u64)
    }

    async fn readlink(&self, inode: u64) -> MetaResult<String> {
        let conn = lock(&self.conn);
        conn.query_row(
            "SELECT target FROM symlink WHERE inode = ?1",
            [inode as i64],
            |row| row.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => MetaError::NotFound,
            _ => MetaError::Internal(e.to_string()),
        })
    }

    async fn statfs(&self) -> MetaResult<StatFs> {
        let conn = lock(&self.conn);
        let files: u64 = conn
            .query_row("SELECT COUNT(*) FROM node", [], |row| row.get::<_, i64>(0))
            .map(|c| c as u64)
            .map_err(|e| MetaError::Internal(e.to_string()))?;

        Ok(StatFs {
            blocks: 1 << 20,
            bfree: 1 << 19,
            bavail: 1 << 19,
            files,
            ffree: u64::MAX - files,
            bsize: 4096,
            namelen: 255,
        })
    }

    async fn get_chunks_for_inode(&self, inode: u64) -> MetaResult<Vec<(u32, Vec<Slice>)>> {
        let conn = lock(&self.conn);
        let mut stmt = conn
            .prepare("SELECT chunk_idx, slices FROM chunk WHERE inode = ?1")
            .map_err(|e| MetaError::Internal(e.to_string()))?;

        let rows = stmt
            .query_map([inode as i64], |row| {
                let idx: i64 = row.get(0)?;
                let json: String = row.get(1)?;
                Ok((idx as u32, json))
            })
            .map_err(|e| MetaError::Internal(e.to_string()))?;

        let mut result = Vec::new();
        for row in rows {
            let (idx, json) = row.map_err(|e| MetaError::Internal(e.to_string()))?;
            let slices: Vec<Slice> = serde_json::from_str(&json)
                .map_err(|e| MetaError::Internal(e.to_string()))?;
            result.push((idx, slices));
        }
        Ok(result)
    }

    async fn replace_slices(&self, inode: u64, chunk_idx: u32, slices: Vec<Slice>) -> MetaResult<()> {
        let conn = lock(&self.conn);
        let json = serde_json::to_string(&slices).map_err(|e| MetaError::Internal(e.to_string()))?;
        conn.execute(
            "INSERT INTO chunk (inode, chunk_idx, slices) VALUES (?1, ?2, ?3)
             ON CONFLICT (inode, chunk_idx) DO UPDATE SET slices = ?3",
            rusqlite::params![inode as i64, chunk_idx as i64, json],
        )
        .map_err(|e| MetaError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn link(&self, parent: u64, name: &str, inode: u64) -> MetaResult<InodeAttr> {
        if name.len() > NAME_MAX { return Err(MetaError::NameTooLong); }
        let conn = lock(&self.conn);
        let attr = get_attr_locked(&conn, inode)?;
        if attr.kind == FileType::Directory {
            return Err(MetaError::PermissionDenied);
        }

        let parent_attr = get_attr_locked(&conn, parent)?;
        if parent_attr.kind != FileType::Directory {
            return Err(MetaError::NotDirectory);
        }

        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM edge WHERE parent = ?1 AND name = ?2",
                rusqlite::params![parent as i64, name],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        if exists {
            return Err(MetaError::AlreadyExists);
        }

        conn.execute(
            "INSERT INTO edge (parent, name, child, kind) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![parent as i64, name, inode as i64, file_type_to_int(attr.kind)],
        )
        .map_err(|e| MetaError::Internal(e.to_string()))?;

        let now = system_time_to_nanos(SystemTime::now());
        conn.execute(
            "UPDATE node SET nlink = nlink + 1, ctime = ?1 WHERE inode = ?2",
            rusqlite::params![now, inode as i64],
        )
        .map_err(|e| MetaError::Internal(e.to_string()))?;

        touch_parent(&conn, parent);
        get_attr_locked(&conn, inode)
    }

    async fn forget(&self, inode: u64) {
        let conn = lock(&self.conn);
        let nlink: i64 = conn
            .query_row("SELECT nlink FROM node WHERE inode = ?1", [inode as i64], |row| row.get(0))
            .unwrap_or(1);
        if nlink <= 0 {
            let _ = conn.execute("DELETE FROM chunk WHERE inode = ?1", [inode as i64]);
            let _ = conn.execute("DELETE FROM xattr WHERE inode = ?1", [inode as i64]);
            let _ = conn.execute("DELETE FROM symlink WHERE inode = ?1", [inode as i64]);
            let _ = conn.execute("DELETE FROM node WHERE inode = ?1", [inode as i64]);
        }
    }

    async fn mknod(&self, parent: u64, name: &str, mode: u32, uid: u32, gid: u32) -> MetaResult<InodeAttr> {
        let file_type = mode & 0o170000;
        let kind = match file_type {
            0o010000 => FileType::Fifo,        // S_IFIFO
            0o020000 => FileType::CharDevice,  // S_IFCHR
            0o060000 => FileType::BlockDevice, // S_IFBLK
            0o100000 => FileType::Regular,     // S_IFREG
            0o140000 => FileType::Socket,      // S_IFSOCK
            _ => return Err(MetaError::NotSupported),
        };
        let perm = mode & 0o7777;
        self.create(parent, name, kind, perm, uid, gid).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup() -> SqliteMetaEngine {
        let engine = SqliteMetaEngine::in_memory().unwrap();
        engine.init().await.unwrap();
        engine
    }

    #[tokio::test]
    async fn root_exists_after_init() {
        let engine = setup().await;
        let attr = engine.get_attr(ROOT_INODE).await.unwrap();
        assert_eq!(attr.kind, FileType::Directory);
        assert_eq!(attr.inode, ROOT_INODE);
    }

    #[tokio::test]
    async fn create_and_lookup_file() {
        let engine = setup().await;
        let attr = engine
            .create(ROOT_INODE, "hello.txt", FileType::Regular, 0o644, 1000, 1000)
            .await
            .unwrap();
        assert_eq!(attr.kind, FileType::Regular);
        assert_eq!(attr.mode, 0o644);

        let looked = engine.lookup(ROOT_INODE, "hello.txt").await.unwrap();
        assert_eq!(looked.inode, attr.inode);
    }

    #[tokio::test]
    async fn create_and_readdir() {
        let engine = setup().await;
        engine
            .create(ROOT_INODE, "a", FileType::Regular, 0o644, 0, 0)
            .await
            .unwrap();
        engine
            .create(ROOT_INODE, "b", FileType::Directory, 0o755, 0, 0)
            .await
            .unwrap();

        let entries = engine.readdir(ROOT_INODE).await.unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[tokio::test]
    async fn duplicate_create_fails() {
        let engine = setup().await;
        engine
            .create(ROOT_INODE, "dup", FileType::Regular, 0o644, 0, 0)
            .await
            .unwrap();
        let err = engine
            .create(ROOT_INODE, "dup", FileType::Regular, 0o644, 0, 0)
            .await;
        assert!(matches!(err, Err(MetaError::AlreadyExists)));
    }

    #[tokio::test]
    async fn unlink_file() {
        let engine = setup().await;
        engine
            .create(ROOT_INODE, "gone", FileType::Regular, 0o644, 0, 0)
            .await
            .unwrap();
        engine.unlink(ROOT_INODE, "gone").await.unwrap();
        assert!(matches!(
            engine.lookup(ROOT_INODE, "gone").await,
            Err(MetaError::NotFound)
        ));
    }

    #[tokio::test]
    async fn unlink_defers_cleanup_until_forget() {
        let engine = setup().await;
        let attr = engine
            .create(ROOT_INODE, "data", FileType::Regular, 0o644, 0, 0)
            .await
            .unwrap();
        let slice = Slice { id: 1, offset: 0, length: 100 };
        engine.write_slice(attr.inode, 0, slice).await.unwrap();

        engine.unlink(ROOT_INODE, "data").await.unwrap();

        // Node still exists with nlink=0 (kernel keeps it alive while fds are open)
        let after = engine.get_attr(attr.inode).await.unwrap();
        assert_eq!(after.nlink, 0);

        // Lookup by name should fail
        assert!(matches!(engine.lookup(ROOT_INODE, "data").await, Err(MetaError::NotFound)));

        // Forget triggers actual cleanup
        engine.forget(attr.inode).await;
        assert!(matches!(engine.get_attr(attr.inode).await, Err(MetaError::NotFound)));
    }

    #[tokio::test]
    async fn rename_file() {
        let engine = setup().await;
        engine
            .create(ROOT_INODE, "old", FileType::Regular, 0o644, 0, 0)
            .await
            .unwrap();
        engine
            .rename(ROOT_INODE, "old", ROOT_INODE, "new")
            .await
            .unwrap();
        assert!(matches!(
            engine.lookup(ROOT_INODE, "old").await,
            Err(MetaError::NotFound)
        ));
        engine.lookup(ROOT_INODE, "new").await.unwrap();
    }

    #[tokio::test]
    async fn rename_same_path_is_noop() {
        let engine = setup().await;
        let attr = engine
            .create(ROOT_INODE, "keep", FileType::Regular, 0o644, 0, 0)
            .await
            .unwrap();
        engine
            .rename(ROOT_INODE, "keep", ROOT_INODE, "keep")
            .await
            .unwrap();
        let after = engine.lookup(ROOT_INODE, "keep").await.unwrap();
        assert_eq!(after.inode, attr.inode);
    }

    #[tokio::test]
    async fn slice_roundtrip() {
        let engine = setup().await;
        engine
            .create(ROOT_INODE, "data", FileType::Regular, 0o644, 0, 0)
            .await
            .unwrap();
        let attr = engine.lookup(ROOT_INODE, "data").await.unwrap();

        let slice = Slice { id: 1, offset: 0, length: 4096 };
        engine.write_slice(attr.inode, 0, slice.clone()).await.unwrap();

        let slices = engine.read_slices(attr.inode, 0).await.unwrap();
        assert_eq!(slices.len(), 1);
        assert_eq!(slices[0].length, 4096);
    }
}
