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

#[async_trait]
pub trait MetaEngine: Send + Sync {
    async fn init(&self) -> MetaResult<()>;
    async fn get_attr(&self, inode: u64) -> MetaResult<InodeAttr>;
    async fn lookup(&self, parent: u64, name: &str) -> MetaResult<InodeAttr>;
    async fn readdir(&self, inode: u64) -> MetaResult<Vec<DirEntry>>;
    async fn create(&self, parent: u64, name: &str, kind: FileType, mode: u32, uid: u32, gid: u32) -> MetaResult<InodeAttr>;
    async fn read_slices(&self, inode: u64, chunk_idx: u32) -> MetaResult<Vec<Slice>>;
    async fn write_slice(&self, inode: u64, chunk_idx: u32, slice: Slice) -> MetaResult<()>;
    async fn set_attr(&self, inode: u64, size: Option<u64>) -> MetaResult<InodeAttr>;
    async fn unlink(&self, parent: u64, name: &str) -> MetaResult<()>;
    async fn rename(&self, src_parent: u64, src_name: &str, dst_parent: u64, dst_name: &str) -> MetaResult<()>;
    async fn next_slice_id(&self) -> MetaResult<u64>;
}
