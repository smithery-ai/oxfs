use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use redb::{Database, ReadableTable, TableDefinition};

use crate::engine::*;
use crate::types::*;

const NODES: TableDefinition<u64, &[u8]> = TableDefinition::new("nodes");
const EDGES: TableDefinition<(&str, u64), &[u8]> = TableDefinition::new("edges");
const CHUNKS: TableDefinition<(u64, u32), &[u8]> = TableDefinition::new("chunks");
const SYMLINKS: TableDefinition<u64, &str> = TableDefinition::new("symlinks");
const COUNTERS: TableDefinition<&str, u64> = TableDefinition::new("counters");

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct Node {
    kind: u8,
    mode: u32,
    uid: u32,
    gid: u32,
    size: u64,
    nlink: u32,
    atime_ns: i64,
    mtime_ns: i64,
    ctime_ns: i64,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct Edge {
    child: u64,
    kind: u8,
}

fn ft2u(ft: FileType) -> u8 {
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

fn u2ft(v: u8) -> FileType {
    match v {
        2 => FileType::Directory,
        3 => FileType::Symlink,
        4 => FileType::Fifo,
        5 => FileType::Socket,
        6 => FileType::BlockDevice,
        7 => FileType::CharDevice,
        _ => FileType::Regular,
    }
}

fn t2ns(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_nanos() as i64
}

fn ns2t(ns: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(ns as u64)
}

fn now() -> i64 { t2ns(SystemTime::now()) }

fn to_attr(ino: u64, n: &Node) -> InodeAttr {
    InodeAttr {
        inode: ino,
        kind: u2ft(n.kind),
        mode: n.mode,
        uid: n.uid,
        gid: n.gid,
        size: n.size,
        blocks: (n.size + 511) / 512,
        nlink: n.nlink,
        atime: ns2t(n.atime_ns),
        mtime: ns2t(n.mtime_ns),
        ctime: ns2t(n.ctime_ns),
        rdev: 0,
    }
}

fn enc<T: serde::Serialize>(v: &T) -> Vec<u8> {
    serde_json::to_vec(v).unwrap()
}

fn dec<T: serde::de::DeserializeOwned>(b: &[u8]) -> MetaResult<T> {
    serde_json::from_slice(b).map_err(|e| MetaError::Internal(e.to_string()))
}

fn err(e: impl std::fmt::Display) -> MetaError {
    MetaError::Internal(e.to_string())
}

pub struct RedbMetaEngine {
    db: Mutex<Database>,
}

impl RedbMetaEngine {
    pub fn new(path: &Path) -> MetaResult<Self> {
        let db = Database::create(path).map_err(err)?;
        Ok(Self { db: Mutex::new(db) })
    }
}

fn lock(m: &Mutex<Database>) -> std::sync::MutexGuard<'_, Database> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[async_trait]
impl MetaEngine for RedbMetaEngine {
    async fn init(&self) -> MetaResult<()> {
        let db = lock(&self.db);
        let txn = db.begin_write().map_err(err)?;
        {
            // Create all tables
            let _ = txn.open_table(EDGES).map_err(err)?;
            let _ = txn.open_table(CHUNKS).map_err(err)?;
            let _ = txn.open_table(SYMLINKS).map_err(err)?;
        }
        {
            let mut nodes = txn.open_table(NODES).map_err(err)?;
            if nodes.get(ROOT_INODE).map_err(err)?.is_none() {
                let n = now();
                let root = Node {
                    kind: ft2u(FileType::Directory), mode: 0o40755,
                    uid: 0, gid: 0, size: 0, nlink: 2,
                    atime_ns: n, mtime_ns: n, ctime_ns: n,
                };
                nodes.insert(ROOT_INODE, enc(&root).as_slice()).map_err(err)?;
            }
        }
        {
            let mut ctr = txn.open_table(COUNTERS).map_err(err)?;
            if ctr.get("next_inode").map_err(err)?.is_none() {
                ctr.insert("next_inode", 2u64).map_err(err)?;
            }
            if ctr.get("next_slice").map_err(err)?.is_none() {
                ctr.insert("next_slice", 1u64).map_err(err)?;
            }
        }
        txn.commit().map_err(err)?;
        Ok(())
    }

    async fn get_attr(&self, inode: u64) -> MetaResult<InodeAttr> {
        let db = lock(&self.db);
        let txn = db.begin_read().map_err(err)?;
        let nodes = txn.open_table(NODES).map_err(err)?;
        let g = nodes.get(inode).map_err(err)?.ok_or(MetaError::NotFound)?;
        let n: Node = dec(g.value())?;
        Ok(to_attr(inode, &n))
    }

    async fn lookup(&self, parent: u64, name: &str) -> MetaResult<InodeAttr> {
        if name.len() > NAME_MAX { return Err(MetaError::NameTooLong); }
        let db = lock(&self.db);
        let txn = db.begin_read().map_err(err)?;
        let child_ino = {
            let edges = txn.open_table(EDGES).map_err(err)?;
            let g = edges.get((name, parent)).map_err(err)?.ok_or(MetaError::NotFound)?;
            let e: Edge = dec(g.value())?;
            e.child
        };
        let nodes = txn.open_table(NODES).map_err(err)?;
        let g = nodes.get(child_ino).map_err(err)?.ok_or(MetaError::NotFound)?;
        let n: Node = dec(g.value())?;
        Ok(to_attr(child_ino, &n))
    }

    async fn readdir(&self, inode: u64) -> MetaResult<Vec<DirEntry>> {
        let db = lock(&self.db);
        let txn = db.begin_read().map_err(err)?;
        {
            let nodes = txn.open_table(NODES).map_err(err)?;
            let g = nodes.get(inode).map_err(err)?.ok_or(MetaError::NotFound)?;
            let n: Node = dec(g.value())?;
            if u2ft(n.kind) != FileType::Directory {
                return Err(MetaError::NotDirectory);
            }
        }
        let edges = txn.open_table(EDGES).map_err(err)?;
        let mut entries = Vec::new();
        for item in edges.iter().map_err(err)? {
            let item = item.map_err(err)?;
            let (name, par) = item.0.value();
            if par == inode {
                let e: Edge = dec(item.1.value())?;
                entries.push(DirEntry {
                    inode: e.child,
                    name: name.to_string(),
                    kind: u2ft(e.kind),
                });
            }
        }
        Ok(entries)
    }

    async fn create(
        &self, parent: u64, name: &str, kind: FileType,
        mode: u32, uid: u32, gid: u32,
    ) -> MetaResult<InodeAttr> {
        if name.len() > NAME_MAX { return Err(MetaError::NameTooLong); }
        let db = lock(&self.db);
        let txn = db.begin_write().map_err(err)?;

        // Check parent is dir
        let mut parent_node: Node = {
            let nodes = txn.open_table(NODES).map_err(err)?;
            let g = nodes.get(parent).map_err(err)?.ok_or(MetaError::NotFound)?;
            dec(g.value())?
        };
        if u2ft(parent_node.kind) != FileType::Directory {
            return Err(MetaError::NotDirectory);
        }

        // Check no duplicate
        {
            let edges = txn.open_table(EDGES).map_err(err)?;
            if edges.get((name, parent)).map_err(err)?.is_some() {
                return Err(MetaError::AlreadyExists);
            }
        }

        // Alloc inode
        let inode: u64 = {
            let mut ctr = txn.open_table(COUNTERS).map_err(err)?;
            let g = ctr.get("next_inode").map_err(err)?.ok_or(MetaError::Internal("no counter".into()))?;
            let v = g.value();
            drop(g);
            ctr.insert("next_inode", v + 1).map_err(err)?;
            v
        };

        let n = now();
        let nlink = if kind == FileType::Directory { 2 } else { 1 };
        let node = Node {
            kind: ft2u(kind), mode, uid, gid, size: 0, nlink,
            atime_ns: n, mtime_ns: n, ctime_ns: n,
        };

        // Write node + update parent + write edge
        {
            let mut nodes = txn.open_table(NODES).map_err(err)?;
            nodes.insert(inode, enc(&node).as_slice()).map_err(err)?;
            parent_node.mtime_ns = n;
            parent_node.ctime_ns = n;
            if kind == FileType::Directory { parent_node.nlink += 1; }
            nodes.insert(parent, enc(&parent_node).as_slice()).map_err(err)?;
        }
        {
            let mut edges = txn.open_table(EDGES).map_err(err)?;
            let ev = Edge { child: inode, kind: ft2u(kind) };
            edges.insert((name, parent), enc(&ev).as_slice()).map_err(err)?;
        }

        txn.commit().map_err(err)?;
        Ok(to_attr(inode, &node))
    }

    async fn read_slices(&self, inode: u64, chunk_idx: u32) -> MetaResult<Vec<Slice>> {
        let db = lock(&self.db);
        let txn = db.begin_read().map_err(err)?;
        let chunks = txn.open_table(CHUNKS).map_err(err)?;
        match chunks.get((inode, chunk_idx)).map_err(err)? {
            Some(g) => dec(g.value()),
            None => Ok(vec![]),
        }
    }

    async fn write_slice(&self, inode: u64, chunk_idx: u32, slice: Slice) -> MetaResult<()> {
        let db = lock(&self.db);
        let txn = db.begin_write().map_err(err)?;
        {
            let mut chunks = txn.open_table(CHUNKS).map_err(err)?;
            let mut slices: Vec<Slice> = match chunks.get((inode, chunk_idx)).map_err(err)? {
                Some(g) => dec(g.value())?,
                None => vec![],
            };
            slices.push(slice);
            chunks.insert((inode, chunk_idx), enc(&slices).as_slice()).map_err(err)?;
        }
        txn.commit().map_err(err)?;
        Ok(())
    }

    async fn set_attr(&self, inode: u64, req: SetAttrRequest) -> MetaResult<InodeAttr> {
        let db = lock(&self.db);
        let txn = db.begin_write().map_err(err)?;
        let node = {
            let mut nodes = txn.open_table(NODES).map_err(err)?;
            let g = nodes.get(inode).map_err(err)?.ok_or(MetaError::NotFound)?;
            let mut n: Node = dec(g.value())?;
            drop(g);
            let ts = now();
            if let Some(s) = req.size { n.size = s; n.mtime_ns = ts; n.ctime_ns = ts; }
            if let Some(m) = req.mode { n.mode = m; n.ctime_ns = ts; }
            if req.uid.is_some() || req.gid.is_some() {
                if let Some(u) = req.uid { n.uid = u; }
                if let Some(g) = req.gid { n.gid = g; }
                n.mode &= !0o6000;
                n.ctime_ns = ts;
            }
            if let Some(a) = req.atime { n.atime_ns = t2ns(a); }
            if let Some(m) = req.mtime { n.mtime_ns = t2ns(m); n.ctime_ns = ts; }
            nodes.insert(inode, enc(&n).as_slice()).map_err(err)?;
            n
        };
        txn.commit().map_err(err)?;
        Ok(to_attr(inode, &node))
    }

    async fn unlink(&self, parent: u64, name: &str) -> MetaResult<()> {
        if name.len() > NAME_MAX { return Err(MetaError::NameTooLong); }
        let db = lock(&self.db);
        let txn = db.begin_write().map_err(err)?;

        // Get edge info
        let (child_ino, child_kind) = {
            let edges = txn.open_table(EDGES).map_err(err)?;
            let g = edges.get((name, parent)).map_err(err)?.ok_or(MetaError::NotFound)?;
            let e: Edge = dec(g.value())?;
            (e.child, u2ft(e.kind))
        };

        // Check dir empty
        if child_kind == FileType::Directory {
            let edges = txn.open_table(EDGES).map_err(err)?;
            for item in edges.iter().map_err(err)? {
                let item = item.map_err(err)?;
                let (_, p) = item.0.value();
                if p == child_ino { return Err(MetaError::NotEmpty); }
            }
        }

        // Remove edge
        {
            let mut edges = txn.open_table(EDGES).map_err(err)?;
            edges.remove((name, parent)).map_err(err)?;
        }

        // Update nodes
        {
            let mut nodes = txn.open_table(NODES).map_err(err)?;
            let g = nodes.get(child_ino).map_err(err)?.ok_or(MetaError::NotFound)?;
            let mut cn: Node = dec(g.value())?;
            drop(g);
            cn.nlink = cn.nlink.saturating_sub(1);
            nodes.insert(child_ino, enc(&cn).as_slice()).map_err(err)?;

            let g = nodes.get(parent).map_err(err)?.ok_or(MetaError::NotFound)?;
            let mut pn: Node = dec(g.value())?;
            drop(g);
            let ts = now();
            pn.mtime_ns = ts;
            pn.ctime_ns = ts;
            if child_kind == FileType::Directory { pn.nlink = pn.nlink.saturating_sub(1); }
            nodes.insert(parent, enc(&pn).as_slice()).map_err(err)?;
        }

        txn.commit().map_err(err)?;
        Ok(())
    }

    async fn rename(
        &self, src_parent: u64, src_name: &str,
        dst_parent: u64, dst_name: &str,
    ) -> MetaResult<()> {
        if src_name.len() > NAME_MAX || dst_name.len() > NAME_MAX { return Err(MetaError::NameTooLong); }
        if src_parent == dst_parent && src_name == dst_name { return Ok(()); }

        let db = lock(&self.db);
        let txn = db.begin_write().map_err(err)?;

        // Get source edge
        let src_edge: Edge = {
            let edges = txn.open_table(EDGES).map_err(err)?;
            let g = edges.get((src_name, src_parent)).map_err(err)?.ok_or(MetaError::NotFound)?;
            dec(g.value())?
        };

        // Remove dst if exists
        let dst_existed = {
            let edges = txn.open_table(EDGES).map_err(err)?;
            match edges.get((dst_name, dst_parent)).map_err(err)? {
                Some(g) => { let e: Edge = dec(g.value())?; Some(e) }
                None => None,
            }
        };

        if let Some(dst_edge) = dst_existed {
            {
                let mut edges = txn.open_table(EDGES).map_err(err)?;
                edges.remove((dst_name, dst_parent)).map_err(err)?;
            }
            // Read dst child node, modify, write back
            let dn: Option<Node> = {
                let nodes = txn.open_table(NODES).map_err(err)?;
                nodes.get(dst_edge.child).map_err(err)?.map(|g| dec(g.value())).transpose()?
            };
            if let Some(mut dn) = dn {
                dn.nlink = dn.nlink.saturating_sub(1);
                let mut nodes = txn.open_table(NODES).map_err(err)?;
                nodes.insert(dst_edge.child, enc(&dn).as_slice()).map_err(err)?;
            }
            if u2ft(dst_edge.kind) == FileType::Directory {
                let pn: Option<Node> = {
                    let nodes = txn.open_table(NODES).map_err(err)?;
                    nodes.get(dst_parent).map_err(err)?.map(|g| dec(g.value())).transpose()?
                };
                if let Some(mut pn) = pn {
                    pn.nlink = pn.nlink.saturating_sub(1);
                    let mut nodes = txn.open_table(NODES).map_err(err)?;
                    nodes.insert(dst_parent, enc(&pn).as_slice()).map_err(err)?;
                }
            }
        }

        // Move edge
        {
            let mut edges = txn.open_table(EDGES).map_err(err)?;
            edges.remove((src_name, src_parent)).map_err(err)?;
            edges.insert((dst_name, dst_parent), enc(&src_edge).as_slice()).map_err(err)?;
        }

        // Update timestamps + nlinks: read all affected nodes first, then write
        let ts = now();
        let src_child_node: Option<Node> = {
            let nodes = txn.open_table(NODES).map_err(err)?;
            nodes.get(src_edge.child).map_err(err)?.map(|g| dec(g.value())).transpose()?
        };
        let src_parent_node: Option<Node> = {
            let nodes = txn.open_table(NODES).map_err(err)?;
            nodes.get(src_parent).map_err(err)?.map(|g| dec(g.value())).transpose()?
        };
        let dst_parent_node: Option<Node> = if src_parent != dst_parent {
            let nodes = txn.open_table(NODES).map_err(err)?;
            nodes.get(dst_parent).map_err(err)?.map(|g| dec(g.value())).transpose()?
        } else { None };

        {
            let mut nodes = txn.open_table(NODES).map_err(err)?;
            if let Some(mut sn) = src_child_node {
                sn.ctime_ns = ts;
                nodes.insert(src_edge.child, enc(&sn).as_slice()).map_err(err)?;
            }
            if let Some(mut sp) = src_parent_node {
                sp.mtime_ns = ts;
                sp.ctime_ns = ts;
                if u2ft(src_edge.kind) == FileType::Directory && src_parent != dst_parent {
                    sp.nlink = sp.nlink.saturating_sub(1);
                }
                nodes.insert(src_parent, enc(&sp).as_slice()).map_err(err)?;
            }
            if let Some(mut dp) = dst_parent_node {
                dp.mtime_ns = ts;
                dp.ctime_ns = ts;
                if u2ft(src_edge.kind) == FileType::Directory { dp.nlink += 1; }
                nodes.insert(dst_parent, enc(&dp).as_slice()).map_err(err)?;
            }
        }

        txn.commit().map_err(err)?;
        Ok(())
    }

    async fn next_slice_id(&self) -> MetaResult<u64> {
        let db = lock(&self.db);
        let txn = db.begin_write().map_err(err)?;
        let id = {
            let mut ctr = txn.open_table(COUNTERS).map_err(err)?;
            let g = ctr.get("next_slice").map_err(err)?.ok_or(MetaError::Internal("no counter".into()))?;
            let v = g.value();
            drop(g);
            ctr.insert("next_slice", v + 1).map_err(err)?;
            v
        };
        txn.commit().map_err(err)?;
        Ok(id)
    }

    async fn symlink(
        &self, parent: u64, name: &str, target: &str,
        uid: u32, gid: u32,
    ) -> MetaResult<InodeAttr> {
        if name.len() > NAME_MAX { return Err(MetaError::NameTooLong); }
        let db = lock(&self.db);
        let txn = db.begin_write().map_err(err)?;

        // Check parent
        {
            let nodes = txn.open_table(NODES).map_err(err)?;
            let g = nodes.get(parent).map_err(err)?.ok_or(MetaError::NotFound)?;
            let pn: Node = dec(g.value())?;
            if u2ft(pn.kind) != FileType::Directory { return Err(MetaError::NotDirectory); }
        }
        {
            let edges = txn.open_table(EDGES).map_err(err)?;
            if edges.get((name, parent)).map_err(err)?.is_some() {
                return Err(MetaError::AlreadyExists);
            }
        }

        let inode: u64 = {
            let mut ctr = txn.open_table(COUNTERS).map_err(err)?;
            let g = ctr.get("next_inode").map_err(err)?.ok_or(MetaError::Internal("no counter".into()))?;
            let v = g.value(); drop(g);
            ctr.insert("next_inode", v + 1).map_err(err)?;
            v
        };

        let ts = now();
        let node = Node {
            kind: ft2u(FileType::Symlink), mode: 0o120777,
            uid, gid, size: target.len() as u64, nlink: 1,
            atime_ns: ts, mtime_ns: ts, ctime_ns: ts,
        };

        {
            let mut nodes = txn.open_table(NODES).map_err(err)?;
            nodes.insert(inode, enc(&node).as_slice()).map_err(err)?;
            // Touch parent
            let g = nodes.get(parent).map_err(err)?.ok_or(MetaError::NotFound)?;
            let mut pn: Node = dec(g.value())?; drop(g);
            pn.mtime_ns = ts; pn.ctime_ns = ts;
            nodes.insert(parent, enc(&pn).as_slice()).map_err(err)?;
        }
        {
            let mut sym = txn.open_table(SYMLINKS).map_err(err)?;
            sym.insert(inode, target).map_err(err)?;
        }
        {
            let mut edges = txn.open_table(EDGES).map_err(err)?;
            let ev = Edge { child: inode, kind: ft2u(FileType::Symlink) };
            edges.insert((name, parent), enc(&ev).as_slice()).map_err(err)?;
        }

        txn.commit().map_err(err)?;
        Ok(to_attr(inode, &node))
    }

    async fn readlink(&self, inode: u64) -> MetaResult<String> {
        let db = lock(&self.db);
        let txn = db.begin_read().map_err(err)?;
        let sym = txn.open_table(SYMLINKS).map_err(err)?;
        let g = sym.get(inode).map_err(err)?.ok_or(MetaError::NotFound)?;
        Ok(g.value().to_string())
    }

    async fn statfs(&self) -> MetaResult<StatFs> {
        let db = lock(&self.db);
        let txn = db.begin_read().map_err(err)?;
        let nodes = txn.open_table(NODES).map_err(err)?;
        let mut files = 0u64;
        for _ in nodes.iter().map_err(err)? { files += 1; }
        Ok(StatFs {
            blocks: 1 << 20, bfree: 1 << 19, bavail: 1 << 19,
            files, ffree: u64::MAX - files, bsize: 4096, namelen: 255,
        })
    }

    async fn get_chunks_for_inode(&self, inode: u64) -> MetaResult<Vec<(u32, Vec<Slice>)>> {
        let db = lock(&self.db);
        let txn = db.begin_read().map_err(err)?;
        let chunks = txn.open_table(CHUNKS).map_err(err)?;
        let mut result = Vec::new();
        for item in chunks.range((inode, 0u32)..=(inode, u32::MAX)).map_err(err)? {
            let item = item.map_err(err)?;
            let (_, idx) = item.0.value();
            let slices: Vec<Slice> = dec(item.1.value())?;
            result.push((idx, slices));
        }
        Ok(result)
    }

    async fn replace_slices(&self, inode: u64, chunk_idx: u32, slices: Vec<Slice>) -> MetaResult<()> {
        let db = lock(&self.db);
        let txn = db.begin_write().map_err(err)?;
        {
            let mut chunks = txn.open_table(CHUNKS).map_err(err)?;
            chunks.insert((inode, chunk_idx), enc(&slices).as_slice()).map_err(err)?;
        }
        txn.commit().map_err(err)?;
        Ok(())
    }

    async fn link(&self, parent: u64, name: &str, inode: u64) -> MetaResult<InodeAttr> {
        if name.len() > NAME_MAX { return Err(MetaError::NameTooLong); }
        let db = lock(&self.db);
        let txn = db.begin_write().map_err(err)?;

        let node_kind: u8 = {
            let nodes = txn.open_table(NODES).map_err(err)?;
            let g = nodes.get(inode).map_err(err)?.ok_or(MetaError::NotFound)?;
            let n: Node = dec(g.value())?;
            if u2ft(n.kind) == FileType::Directory { return Err(MetaError::PermissionDenied); }
            let g2 = nodes.get(parent).map_err(err)?.ok_or(MetaError::NotFound)?;
            let pn: Node = dec(g2.value())?;
            if u2ft(pn.kind) != FileType::Directory { return Err(MetaError::NotDirectory); }
            n.kind
        };

        {
            let edges = txn.open_table(EDGES).map_err(err)?;
            if edges.get((name, parent)).map_err(err)?.is_some() {
                return Err(MetaError::AlreadyExists);
            }
        }

        let updated_node: Node = {
            let mut nodes = txn.open_table(NODES).map_err(err)?;
            let ts = now();

            let g = nodes.get(inode).map_err(err)?.ok_or(MetaError::NotFound)?;
            let mut n: Node = dec(g.value())?; drop(g);
            n.nlink += 1;
            n.ctime_ns = ts;
            nodes.insert(inode, enc(&n).as_slice()).map_err(err)?;

            let g = nodes.get(parent).map_err(err)?.ok_or(MetaError::NotFound)?;
            let mut pn: Node = dec(g.value())?; drop(g);
            pn.mtime_ns = ts;
            pn.ctime_ns = ts;
            nodes.insert(parent, enc(&pn).as_slice()).map_err(err)?;

            n
        };

        {
            let mut edges = txn.open_table(EDGES).map_err(err)?;
            let ev = Edge { child: inode, kind: node_kind };
            edges.insert((name, parent), enc(&ev).as_slice()).map_err(err)?;
        }

        txn.commit().map_err(err)?;
        Ok(to_attr(inode, &updated_node))
    }

    async fn mknod(
        &self, parent: u64, name: &str, mode: u32,
        uid: u32, gid: u32,
    ) -> MetaResult<InodeAttr> {
        let file_type = mode & 0o170000;
        let kind = match file_type {
            0o010000 => FileType::Fifo,
            0o020000 => FileType::CharDevice,
            0o060000 => FileType::BlockDevice,
            0o100000 => FileType::Regular,
            0o140000 => FileType::Socket,
            _ => return Err(MetaError::NotSupported),
        };
        self.create(parent, name, kind, mode & 0o7777, uid, gid).await
    }

    async fn forget(&self, inode: u64) {
        let db = lock(&self.db);
        let txn = match db.begin_write() { Ok(t) => t, Err(_) => return };

        let should_delete = {
            let nodes = match txn.open_table(NODES) { Ok(t) => t, Err(_) => return };
            match nodes.get(inode) {
                Ok(Some(g)) => {
                    let n: Node = match dec(g.value()) { Ok(n) => n, Err(_) => return };
                    n.nlink == 0
                }
                _ => false,
            }
        };

        if should_delete {
            {
                let mut nodes = match txn.open_table(NODES) { Ok(t) => t, Err(_) => return };
                let _ = nodes.remove(inode);
            }
            {
                let mut chunks = match txn.open_table(CHUNKS) { Ok(t) => t, Err(_) => return };
                let keys: Vec<(u64, u32)> = match chunks.range((inode, 0u32)..=(inode, u32::MAX)) {
                    Ok(r) => r.filter_map(|item| item.ok().map(|i| i.0.value())).collect(),
                    Err(_) => vec![],
                };
                for k in keys { let _ = chunks.remove(k); }
            }
            {
                let mut sym = match txn.open_table(SYMLINKS) { Ok(t) => t, Err(_) => return };
                let _ = sym.remove(inode);
            }
            let _ = txn.commit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (RedbMetaEngine, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let engine = RedbMetaEngine::new(&dir.path().join("test.redb")).unwrap();
        (engine, dir)
    }

    #[tokio::test]
    async fn root_exists() {
        let (e, _d) = setup();
        e.init().await.unwrap();
        let a = e.get_attr(ROOT_INODE).await.unwrap();
        assert_eq!(a.kind, FileType::Directory);
    }

    #[tokio::test]
    async fn create_lookup() {
        let (e, _d) = setup();
        e.init().await.unwrap();
        let a = e.create(ROOT_INODE, "f.txt", FileType::Regular, 0o644, 1000, 1000).await.unwrap();
        let b = e.lookup(ROOT_INODE, "f.txt").await.unwrap();
        assert_eq!(a.inode, b.inode);
    }

    #[tokio::test]
    async fn readdir_works() {
        let (e, _d) = setup();
        e.init().await.unwrap();
        e.create(ROOT_INODE, "a", FileType::Regular, 0o644, 0, 0).await.unwrap();
        e.create(ROOT_INODE, "b", FileType::Directory, 0o755, 0, 0).await.unwrap();
        assert_eq!(e.readdir(ROOT_INODE).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn unlink_forget() {
        let (e, _d) = setup();
        e.init().await.unwrap();
        let a = e.create(ROOT_INODE, "g", FileType::Regular, 0o644, 0, 0).await.unwrap();
        e.unlink(ROOT_INODE, "g").await.unwrap();
        assert_eq!(e.get_attr(a.inode).await.unwrap().nlink, 0);
        e.forget(a.inode).await;
        assert!(matches!(e.get_attr(a.inode).await, Err(MetaError::NotFound)));
    }

    #[tokio::test]
    async fn slice_roundtrip() {
        let (e, _d) = setup();
        e.init().await.unwrap();
        let a = e.create(ROOT_INODE, "d", FileType::Regular, 0o644, 0, 0).await.unwrap();
        e.write_slice(a.inode, 0, Slice { id: 1, offset: 0, length: 4096 }).await.unwrap();
        let s = e.read_slices(a.inode, 0).await.unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].length, 4096);
    }

    #[tokio::test]
    async fn rename_noop() {
        let (e, _d) = setup();
        e.init().await.unwrap();
        let a = e.create(ROOT_INODE, "k", FileType::Regular, 0o644, 0, 0).await.unwrap();
        e.rename(ROOT_INODE, "k", ROOT_INODE, "k").await.unwrap();
        assert_eq!(e.lookup(ROOT_INODE, "k").await.unwrap().inode, a.inode);
    }
}
