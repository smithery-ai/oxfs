mod cache;
mod prefetch;

pub use cache::{CacheConfig, CacheLayer, PassthroughCache, TieredCache};
pub use prefetch::Prefetcher;

use std::sync::Arc;

use bytes::Bytes;
use oxfs_meta::{
    DirEntry, FileType, InodeAttr, MetaEngine, SetAttrRequest, Slice, StatFs, CHUNK_SIZE,
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

pub struct Vfs<M: MetaEngine + 'static, C: CacheLayer + 'static> {
    meta: Arc<M>,
    cache: Arc<C>,
    prefetcher: Prefetcher,
}

impl<M: MetaEngine + 'static, C: CacheLayer + 'static> Vfs<M, C> {
    pub fn new(meta: Arc<M>, cache: Arc<C>) -> Self {
        Self {
            meta,
            cache,
            prefetcher: Prefetcher::new(4),
        }
    }

    pub fn with_prefetch(meta: Arc<M>, cache: Arc<C>, prefetch_chunks: u32) -> Self {
        Self {
            meta,
            cache,
            prefetcher: Prefetcher::new(prefetch_chunks),
        }
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

        tracing::debug!(inode, offset, size, file_size = attr.size, "read");

        let end = std::cmp::min(offset + size as u64, attr.size);
        let mut result = Vec::with_capacity((end - offset) as usize);
        let mut pos = offset;

        while pos < end {
            let chunk_idx = (pos / CHUNK_SIZE) as u32;
            let chunk_offset = pos % CHUNK_SIZE;
            let bytes_in_chunk = std::cmp::min(CHUNK_SIZE - chunk_offset, end - pos);

            // Trigger prefetch on sequential access
            if let Some(chunks) = self.prefetcher.record_and_should_prefetch(inode, chunk_idx) {
                prefetch::spawn_prefetch(&self.meta, &self.cache, inode, chunks);
            }

            let slices = self.meta.read_slices(inode, chunk_idx).await?;

            let buf_end = (chunk_offset + bytes_in_chunk) as usize;
            let mut chunk_buf = vec![0u8; buf_end];

            for slice in &slices {
                let data = self.cache.read_slice(slice.id).await?;
                let s_start = slice.offset as usize;
                let s_end = s_start + std::cmp::min(data.len(), slice.length as usize);
                let copy_start = std::cmp::max(s_start, chunk_offset as usize);
                let copy_end = std::cmp::min(s_end, buf_end);
                if copy_start < copy_end {
                    let data_offset = copy_start - s_start;
                    chunk_buf[copy_start..copy_end]
                        .copy_from_slice(&data[data_offset..data_offset + (copy_end - copy_start)]);
                }
            }

            result.extend_from_slice(&chunk_buf[chunk_offset as usize..buf_end]);
            pos += bytes_in_chunk;
        }

        Ok(Bytes::from(result))
    }

    pub async fn write(&self, inode: u64, offset: u64, data: &[u8]) -> VfsResult<u32> {
        let attr = self.meta.get_attr(inode).await?;
        if attr.kind != FileType::Regular {
            return Err(VfsError::Invalid("not a regular file".into()));
        }

        tracing::debug!(inode, offset, len = data.len(), "write");

        let mut written = 0usize;
        let mut pos = offset;

        while written < data.len() {
            let chunk_idx = (pos / CHUNK_SIZE) as u32;
            let chunk_offset = pos % CHUNK_SIZE;
            let bytes_in_chunk =
                std::cmp::min(CHUNK_SIZE - chunk_offset, (data.len() - written) as u64) as usize;

            let slice_data = Bytes::copy_from_slice(&data[written..written + bytes_in_chunk]);
            let slice_id = self.meta.next_slice_id().await?;

            tracing::trace!(slice_id, bytes = bytes_in_chunk, "PUT slice");
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
            self.meta
                .set_attr(inode, SetAttrRequest { size: Some(new_size), ..Default::default() })
                .await?;
        }

        Ok(written as u32)
    }

    pub async fn set_attr(&self, inode: u64, req: SetAttrRequest) -> VfsResult<InodeAttr> {
        Ok(self.meta.set_attr(inode, req).await?)
    }

    pub async fn unlink(&self, parent: u64, name: &str) -> VfsResult<()> {
        Ok(self.meta.unlink(parent, name).await?)
    }

    pub async fn forget(&self, inode: u64) {
        self.prefetcher.remove(inode);
        self.meta.forget(inode).await;
    }

    pub async fn link(&self, parent: u64, name: &str, inode: u64) -> VfsResult<InodeAttr> {
        Ok(self.meta.link(parent, name, inode).await?)
    }

    pub async fn mknod(&self, parent: u64, name: &str, mode: u32, uid: u32, gid: u32) -> VfsResult<InodeAttr> {
        Ok(self.meta.mknod(parent, name, mode, uid, gid).await?)
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

    pub async fn symlink(
        &self,
        parent: u64,
        name: &str,
        target: &str,
        uid: u32,
        gid: u32,
    ) -> VfsResult<InodeAttr> {
        Ok(self.meta.symlink(parent, name, target, uid, gid).await?)
    }

    pub async fn readlink(&self, inode: u64) -> VfsResult<String> {
        Ok(self.meta.readlink(inode).await?)
    }

    pub async fn statfs(&self) -> VfsResult<StatFs> {
        Ok(self.meta.statfs().await?)
    }

    pub async fn flush(&self) -> VfsResult<()> {
        self.cache.flush_dirty().await.map_err(|e| VfsError::Data(e))?;
        Ok(())
    }

    pub async fn compact_slices(&self, inode: u64) -> VfsResult<()> {
        self.cache.flush_dirty().await.map_err(|e| VfsError::Data(e))?;
        let chunks = self.meta.get_chunks_for_inode(inode).await?;

        for (chunk_idx, slices) in chunks {
            if slices.len() <= 1 {
                continue;
            }

            let mut max_end = 0u64;
            for s in &slices {
                max_end = std::cmp::max(max_end, s.offset + s.length);
            }

            let mut buf = vec![0u8; max_end as usize];
            for s in &slices {
                let data = self.cache.read_slice(s.id).await?;
                let start = s.offset as usize;
                let len = std::cmp::min(data.len(), s.length as usize);
                buf[start..start + len].copy_from_slice(&data[..len]);
            }

            let merged_id = self.meta.next_slice_id().await?;
            self.cache
                .write_slice(merged_id, Bytes::from(buf))
                .await?;

            let merged = Slice {
                id: merged_id,
                offset: 0,
                length: max_end,
            };
            self.meta
                .replace_slices(inode, chunk_idx, vec![merged])
                .await?;

            for s in &slices {
                let _ = self.cache.delete_slice(s.id).await;
            }
        }

        Ok(())
    }
}
