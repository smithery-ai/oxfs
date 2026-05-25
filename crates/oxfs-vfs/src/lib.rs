use std::sync::Arc;

use bytes::Bytes;
use oxfs_cache::CacheLayer;
use oxfs_meta::{
    DirEntry, FileType, InodeAttr, MetaEngine, Slice, CHUNK_SIZE,
};

#[derive(Debug, thiserror::Error)]
pub enum VfsError {
    #[error("meta: {0}")]
    Meta(#[from] oxfs_meta::MetaError),
    #[error("data: {0}")]
    Data(#[from] oxfs_data::DataError),
    #[error("invalid argument: {0}")]
    Invalid(String),
}

pub type VfsResult<T> = std::result::Result<T, VfsError>;

pub struct Vfs<M: MetaEngine, C: CacheLayer> {
    meta: Arc<M>,
    cache: Arc<C>,
}

impl<M: MetaEngine, C: CacheLayer> Vfs<M, C> {
    pub fn new(meta: Arc<M>, cache: Arc<C>) -> Self {
        Self { meta, cache }
    }

    pub async fn init(&self) -> VfsResult<()> {
        self.meta.init().await?;
        Ok(())
    }

    pub async fn get_attr(&self, inode: u64) -> VfsResult<InodeAttr> {
        Ok(self.meta.get_attr(inode).await?)
    }

    pub async fn lookup(&self, parent: u64, name: &str) -> VfsResult<InodeAttr> {
        Ok(self.meta.lookup(parent, name).await?)
    }

    pub async fn readdir(&self, inode: u64) -> VfsResult<Vec<DirEntry>> {
        Ok(self.meta.readdir(inode).await?)
    }

    pub async fn create(
        &self,
        parent: u64,
        name: &str,
        kind: FileType,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> VfsResult<InodeAttr> {
        Ok(self.meta.create(parent, name, kind, mode, uid, gid).await?)
    }

    pub async fn read(&self, inode: u64, offset: u64, size: u32) -> VfsResult<Bytes> {
        let attr = self.meta.get_attr(inode).await?;
        if attr.kind != FileType::Regular {
            return Err(VfsError::Invalid("not a regular file".into()));
        }

        if offset >= attr.size {
            return Ok(Bytes::new());
        }

        let end = std::cmp::min(offset + size as u64, attr.size);
        let mut result = Vec::with_capacity((end - offset) as usize);
        let mut pos = offset;

        while pos < end {
            let chunk_idx = (pos / CHUNK_SIZE) as u32;
            let chunk_offset = pos % CHUNK_SIZE;
            let bytes_in_chunk = std::cmp::min(CHUNK_SIZE - chunk_offset, end - pos);

            let slices = self.meta.read_slices(inode, chunk_idx).await?;

            let mut chunk_buf = vec![0u8; CHUNK_SIZE as usize];
            let mut chunk_filled = 0u64;

            for slice in &slices {
                let data = self.cache.read_slice(slice.id).await?;
                let start = slice.offset as usize;
                let len = std::cmp::min(data.len(), slice.length as usize);
                chunk_buf[start..start + len].copy_from_slice(&data[..len]);
                chunk_filled = std::cmp::max(chunk_filled, slice.offset + len as u64);
            }

            let start = chunk_offset as usize;
            let end_in_chunk = start + bytes_in_chunk as usize;
            result.extend_from_slice(&chunk_buf[start..end_in_chunk]);
            pos += bytes_in_chunk;
        }

        Ok(Bytes::from(result))
    }

    pub async fn write(&self, inode: u64, offset: u64, data: &[u8]) -> VfsResult<u32> {
        let attr = self.meta.get_attr(inode).await?;
        if attr.kind != FileType::Regular {
            return Err(VfsError::Invalid("not a regular file".into()));
        }

        let mut written = 0usize;
        let mut pos = offset;

        while written < data.len() {
            let chunk_idx = (pos / CHUNK_SIZE) as u32;
            let chunk_offset = pos % CHUNK_SIZE;
            let bytes_in_chunk =
                std::cmp::min(CHUNK_SIZE - chunk_offset, (data.len() - written) as u64) as usize;

            let slice_data = Bytes::copy_from_slice(&data[written..written + bytes_in_chunk]);
            let slice_id = self.meta.next_slice_id().await?;

            self.cache.write_slice(slice_id, slice_data).await?;

            let slice = Slice {
                id: slice_id,
                offset: chunk_offset,
                length: bytes_in_chunk as u64,
            };
            self.meta.write_slice(inode, chunk_idx, slice).await?;

            written += bytes_in_chunk;
            pos += bytes_in_chunk as u64;
        }

        let new_size = std::cmp::max(attr.size, offset + data.len() as u64);
        if new_size != attr.size {
            self.meta.set_attr(inode, Some(new_size)).await?;
        }

        Ok(written as u32)
    }

    pub async fn unlink(&self, parent: u64, name: &str) -> VfsResult<()> {
        Ok(self.meta.unlink(parent, name).await?)
    }

    pub async fn rename(
        &self,
        src_parent: u64,
        src_name: &str,
        dst_parent: u64,
        dst_name: &str,
    ) -> VfsResult<()> {
        Ok(self
            .meta
            .rename(src_parent, src_name, dst_parent, dst_name)
            .await?)
    }
}
