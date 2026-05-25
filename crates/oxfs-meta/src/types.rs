use serde::{Deserialize, Serialize};
use std::time::SystemTime;

pub const ROOT_INODE: u64 = 1;
pub const CHUNK_SIZE: u64 = 64 * 1024 * 1024; // 64 MiB

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileType {
    Regular,
    Directory,
    Symlink,
}

#[derive(Debug, Clone)]
pub struct InodeAttr {
    pub inode: u64,
    pub size: u64,
    pub blocks: u64,
    pub atime: SystemTime,
    pub mtime: SystemTime,
    pub ctime: SystemTime,
    pub kind: FileType,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
}

impl InodeAttr {
    pub fn new_dir(inode: u64, mode: u32, uid: u32, gid: u32) -> Self {
        let now = SystemTime::now();
        Self {
            inode,
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            kind: FileType::Directory,
            mode,
            nlink: 2,
            uid,
            gid,
        }
    }

    pub fn new_file(inode: u64, mode: u32, uid: u32, gid: u32) -> Self {
        let now = SystemTime::now();
        Self {
            inode,
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            kind: FileType::Regular,
            mode,
            nlink: 1,
            uid,
            gid,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub inode: u64,
    pub name: String,
    pub kind: FileType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Slice {
    pub id: u64,
    pub offset: u64,
    pub length: u64,
}
