use std::time::SystemTime;

use async_trait::async_trait;

use crate::types::*;

#[derive(Debug, thiserror::Error)]
pub enum MetaError {
    #[error("not found")]
    NotFound,
    #[error("already exists")]
    AlreadyExists,
    #[error("not a directory")]
    NotDirectory,
    #[error("directory not empty")]
    NotEmpty,
    #[error("is a directory")]
    IsDirectory,
    #[error("internal: {0}")]
    Internal(String),
}

pub type MetaResult<T> = std::result::Result<T, MetaError>;

#[derive(Debug, Default)]
pub struct SetAttrRequest {
    pub size: Option<u64>,
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub atime: Option<SystemTime>,
    pub mtime: Option<SystemTime>,
}

#[derive(Debug, Default)]
pub struct StatFs {
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub bsize: u32,
    pub namelen: u32,
}

#[async_trait]
pub trait MetaEngine: Send + Sync {
    async fn init(&self) -> MetaResult<()>;
    async fn get_attr(&self, inode: u64) -> MetaResult<InodeAttr>;
    async fn lookup(&self, parent: u64, name: &str) -> MetaResult<InodeAttr>;
    async fn readdir(&self, inode: u64) -> MetaResult<Vec<DirEntry>>;
    async fn create(&self, parent: u64, name: &str, kind: FileType, mode: u32, uid: u32, gid: u32) -> MetaResult<InodeAttr>;
    async fn read_slices(&self, inode: u64, chunk_idx: u32) -> MetaResult<Vec<Slice>>;
    async fn write_slice(&self, inode: u64, chunk_idx: u32, slice: Slice) -> MetaResult<()>;
    async fn set_attr(&self, inode: u64, req: SetAttrRequest) -> MetaResult<InodeAttr>;
    async fn unlink(&self, parent: u64, name: &str) -> MetaResult<()>;
    async fn rename(&self, src_parent: u64, src_name: &str, dst_parent: u64, dst_name: &str) -> MetaResult<()>;
    async fn next_slice_id(&self) -> MetaResult<u64>;
    async fn symlink(&self, parent: u64, name: &str, target: &str, uid: u32, gid: u32) -> MetaResult<InodeAttr>;
    async fn readlink(&self, inode: u64) -> MetaResult<String>;
    async fn statfs(&self) -> MetaResult<StatFs>;
    async fn get_chunks_for_inode(&self, inode: u64) -> MetaResult<Vec<(u32, Vec<Slice>)>>;
    async fn replace_slices(&self, inode: u64, chunk_idx: u32, slices: Vec<Slice>) -> MetaResult<()>;
}
