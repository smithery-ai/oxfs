use async_trait::async_trait;
use bytes::Bytes;
use oxfs_data::{DataEngine, DataResult};

#[async_trait]
pub trait CacheLayer: Send + Sync {
    async fn read_slice(&self, slice_id: u64) -> DataResult<Bytes>;
    async fn write_slice(&self, slice_id: u64, data: Bytes) -> DataResult<()>;
    async fn delete_slice(&self, slice_id: u64) -> DataResult<()>;
}

pub struct PassthroughCache<D: DataEngine> {
    inner: D,
}

impl<D: DataEngine> PassthroughCache<D> {
    pub fn new(inner: D) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl<T: DataEngine> CacheLayer for PassthroughCache<T> {
    async fn read_slice(&self, slice_id: u64) -> DataResult<Bytes> {
        self.inner.read_slice(slice_id).await
    }

    async fn write_slice(&self, slice_id: u64, data: Bytes) -> DataResult<()> {
        self.inner.write_slice(slice_id, data).await
    }

    async fn delete_slice(&self, slice_id: u64) -> DataResult<()> {
        self.inner.delete_slice(slice_id).await
    }
}
