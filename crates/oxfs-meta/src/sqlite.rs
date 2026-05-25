use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use rusqlite::Connection;
use tokio::sync::Mutex;

use crate::engine::*;
use crate::types::*;

pub struct SqliteMetaEngine {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteMetaEngine {
    pub fn new(path: &Path) -> MetaResult<Self> {
        let conn = Connection::open(path)
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn in_memory() -> MetaResult<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }
}

fn system_time_to_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs() as i64
}

fn secs_to_system_time(secs: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs as u64)
}

fn file_type_to_int(ft: FileType) -> i32 {
    match ft {
        FileType::Regular => 1,
        FileType::Directory => 2,
        FileType::Symlink => 3,
    }
}

fn int_to_file_type(i: i32) -> FileType {
    match i {
        2 => FileType::Directory,
        3 => FileType::Symlink,
        _ => FileType::Regular,
    }
}

#[async_trait]
impl MetaEngine for SqliteMetaEngine {
    async fn init(&self) -> MetaResult<()> {
        let conn = self.conn.lock().await;
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

        // Seed root inode if missing
        let root_exists: bool = conn
            .query_row("SELECT COUNT(*) FROM node WHERE inode = 1", [], |row| row.get::<_, i64>(0))
            .map(|c| c > 0)
            .map_err(|e| MetaError::Internal(e.to_string()))?;

        if !root_exists {
            let now = system_time_to_secs(SystemTime::now());
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
        let conn = self.conn.lock().await;
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
                    blocks: (row.get::<_, i64>(5)? as u64 + 511) / 512,
                    nlink: row.get::<_, i64>(6)? as u32,
                    atime: secs_to_system_time(row.get(7)?),
                    mtime: secs_to_system_time(row.get(8)?),
                    ctime: secs_to_system_time(row.get(9)?),
                })
            },
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => MetaError::NotFound,
            _ => MetaError::Internal(e.to_string()),
        })
    }

    async fn lookup(&self, parent: u64, name: &str) -> MetaResult<InodeAttr> {
        let conn = self.conn.lock().await;
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
        drop(conn);
        self.get_attr(child_inode as u64).await
    }

    async fn readdir(&self, inode: u64) -> MetaResult<Vec<DirEntry>> {
        let attr = self.get_attr(inode).await?;
        if attr.kind != FileType::Directory {
            return Err(MetaError::NotDirectory);
        }

        let conn = self.conn.lock().await;
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
        let parent_attr = self.get_attr(parent).await?;
        if parent_attr.kind != FileType::Directory {
            return Err(MetaError::NotDirectory);
        }

        let conn = self.conn.lock().await;

        // Check for duplicates
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

        // Allocate inode
        let inode: i64 = conn
            .query_row(
                "UPDATE counter SET value = value + 1 WHERE name = 'next_inode' RETURNING value",
                [],
                |row| row.get(0),
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;

        let now = system_time_to_secs(SystemTime::now());
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

        drop(conn);
        self.get_attr(inode as u64).await
    }

    async fn read_slices(&self, inode: u64, chunk_idx: u32) -> MetaResult<Vec<Slice>> {
        let conn = self.conn.lock().await;
        let json: String = conn
            .query_row(
                "SELECT slices FROM chunk WHERE inode = ?1 AND chunk_idx = ?2",
                rusqlite::params![inode as i64, chunk_idx as i64],
                |row| row.get(0),
            )
            .unwrap_or_else(|_| "[]".to_string());

        serde_json::from_str(&json).map_err(|e| MetaError::Internal(e.to_string()))
    }

    async fn write_slice(&self, inode: u64, chunk_idx: u32, slice: Slice) -> MetaResult<()> {
        let mut slices = self.read_slices(inode, chunk_idx).await?;
        slices.push(slice);
        let json = serde_json::to_string(&slices).map_err(|e| MetaError::Internal(e.to_string()))?;

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO chunk (inode, chunk_idx, slices) VALUES (?1, ?2, ?3)
             ON CONFLICT (inode, chunk_idx) DO UPDATE SET slices = ?3",
            rusqlite::params![inode as i64, chunk_idx as i64, json],
        )
        .map_err(|e| MetaError::Internal(e.to_string()))?;

        Ok(())
    }

    async fn set_attr(&self, inode: u64, size: Option<u64>) -> MetaResult<InodeAttr> {
        if let Some(new_size) = size {
            let conn = self.conn.lock().await;
            let now = system_time_to_secs(SystemTime::now());
            conn.execute(
                "UPDATE node SET size = ?1, mtime = ?2, ctime = ?2 WHERE inode = ?3",
                rusqlite::params![new_size as i64, now, inode as i64],
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        }
        self.get_attr(inode).await
    }

    async fn unlink(&self, parent: u64, name: &str) -> MetaResult<()> {
        let attr = self.lookup(parent, name).await?;
        if attr.kind == FileType::Directory {
            let children = self.readdir(attr.inode).await?;
            if !children.is_empty() {
                return Err(MetaError::NotEmpty);
            }
        }

        let conn = self.conn.lock().await;
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

        // Remove node if nlink reaches 0
        conn.execute(
            "DELETE FROM node WHERE inode = ?1 AND nlink <= 0",
            [attr.inode as i64],
        )
        .map_err(|e| MetaError::Internal(e.to_string()))?;

        if attr.kind == FileType::Directory {
            conn.execute(
                "UPDATE node SET nlink = nlink - 1 WHERE inode = ?1",
                [parent as i64],
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        }

        Ok(())
    }

    async fn rename(
        &self,
        src_parent: u64,
        src_name: &str,
        dst_parent: u64,
        dst_name: &str,
    ) -> MetaResult<()> {
        let attr = self.lookup(src_parent, src_name).await?;

        // Remove destination if it exists
        if self.lookup(dst_parent, dst_name).await.is_ok() {
            self.unlink(dst_parent, dst_name).await?;
        }

        let conn = self.conn.lock().await;
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

        Ok(())
    }

    async fn next_slice_id(&self) -> MetaResult<u64> {
        let conn = self.conn.lock().await;
        let id: i64 = conn
            .query_row(
                "UPDATE counter SET value = value + 1 WHERE name = 'next_slice' RETURNING value",
                [],
                |row| row.get(0),
            )
            .map_err(|e| MetaError::Internal(e.to_string()))?;
        Ok(id as u64)
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
