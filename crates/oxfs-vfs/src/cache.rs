use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use moka::future::Cache;
use oxfs_data::{DataEngine, DataResult};
use parking_lot::Mutex;

#[async_trait]
pub trait CacheLayer: Send + Sync {
    async fn read_slice(&self, slice_id: u64) -> DataResult<Bytes>;
    async fn write_slice(&self, slice_id: u64, data: Bytes) -> DataResult<()>;
    async fn delete_slice(&self, slice_id: u64) -> DataResult<()>;
    async fn flush_dirty(&self) -> DataResult<()>;
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

    async fn flush_dirty(&self) -> DataResult<()> {
        Ok(())
    }
}

pub struct CacheConfig {
    pub mem_max_bytes: u64,
    pub disk_path: Option<PathBuf>,
    pub disk_max_bytes: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            mem_max_bytes: 256 * 1024 * 1024,
            disk_path: None,
            disk_max_bytes: 1024 * 1024 * 1024,
        }
    }
}

pub struct TieredCache<D: DataEngine> {
    inner: D,
    l1: Cache<u64, Bytes>,
    dirty: Mutex<HashSet<u64>>,
    disk_path: Option<PathBuf>,
    disk_max_bytes: u64,
    disk_used: AtomicU64,
    l1_hits: AtomicU64,
    l2_hits: AtomicU64,
    misses: AtomicU64,
}

impl<D: DataEngine> TieredCache<D> {
    pub fn new(inner: D, config: CacheConfig) -> Self {
        let l1 = Cache::builder()
            .weigher(|_key: &u64, value: &Bytes| -> u32 {
                value.len().try_into().unwrap_or(u32::MAX)
            })
            .max_capacity(config.mem_max_bytes)
            .build();

        if let Some(ref path) = config.disk_path {
            let _ = std::fs::create_dir_all(path);
        }

        Self {
            inner,
            l1,
            dirty: Mutex::new(HashSet::new()),
            disk_path: config.disk_path,
            disk_max_bytes: config.disk_max_bytes,
            disk_used: AtomicU64::new(0),
            l1_hits: AtomicU64::new(0),
            l2_hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub fn stats(&self) -> (u64, u64, u64) {
        (
            self.l1_hits.load(Ordering::Relaxed),
            self.l2_hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }

    fn disk_key(&self, slice_id: u64) -> Option<PathBuf> {
        self.disk_path.as_ref().map(|base| {
            let prefix = slice_id / 1000;
            base.join(format!("{prefix:04}")).join(format!("{slice_id:012}"))
        })
    }

    async fn disk_read(&self, slice_id: u64) -> Option<Bytes> {
        let path = self.disk_key(slice_id)?;
        tokio::fs::read(&path).await.ok().map(Bytes::from)
    }

    async fn disk_write(&self, slice_id: u64, data: &Bytes) {
        let Some(path) = self.disk_key(slice_id) else { return };
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let len = data.len() as u64;
        if self.disk_used.load(Ordering::Relaxed) + len > self.disk_max_bytes {
            return;
        }
        if tokio::fs::write(&path, data).await.is_ok() {
            self.disk_used.fetch_add(len, Ordering::Relaxed);
        }
    }

    async fn disk_delete(&self, slice_id: u64) {
        let Some(path) = self.disk_key(slice_id) else { return };
        if let Ok(meta) = tokio::fs::metadata(&path).await {
            let len = meta.len();
            if tokio::fs::remove_file(&path).await.is_ok() {
                self.disk_used.fetch_sub(len.min(self.disk_used.load(Ordering::Relaxed)), Ordering::Relaxed);
            }
        }
    }
}

#[async_trait]
impl<D: DataEngine> CacheLayer for TieredCache<D> {
    async fn read_slice(&self, slice_id: u64) -> DataResult<Bytes> {
        // L1 check (includes dirty slices not yet flushed)
        if let Some(data) = self.l1.get(&slice_id).await {
            self.l1_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(data);
        }

        // L2 check
        if let Some(data) = self.disk_read(slice_id).await {
            self.l2_hits.fetch_add(1, Ordering::Relaxed);
            self.l1.insert(slice_id, data.clone()).await;
            return Ok(data);
        }

        // Miss: fetch from backend
        self.misses.fetch_add(1, Ordering::Relaxed);
        let data = self.inner.read_slice(slice_id).await?;
        self.l1.insert(slice_id, data.clone()).await;
        self.disk_write(slice_id, &data).await;
        Ok(data)
    }

    async fn write_slice(&self, slice_id: u64, data: Bytes) -> DataResult<()> {
        // Optimistic: write to L1 only, mark dirty
        self.l1.insert(slice_id, data.clone()).await;
        self.dirty.lock().insert(slice_id);
        tracing::trace!(slice_id, bytes = data.len(), "write_slice (buffered)");
        Ok(())
    }

    async fn delete_slice(&self, slice_id: u64) -> DataResult<()> {
        self.dirty.lock().remove(&slice_id);
        self.l1.invalidate(&slice_id).await;
        self.disk_delete(slice_id).await;
        // Best-effort delete from backend (may not exist if never flushed)
        let _ = self.inner.delete_slice(slice_id).await;
        Ok(())
    }

    async fn flush_dirty(&self) -> DataResult<()> {
        let to_flush: Vec<u64> = {
            let mut dirty = self.dirty.lock();
            let ids: Vec<u64> = dirty.drain().collect();
            ids
        };

        if to_flush.is_empty() {
            return Ok(());
        }

        tracing::debug!(count = to_flush.len(), "flushing dirty slices to backend");

        let mut errors = Vec::new();
        for slice_id in &to_flush {
            if let Some(data) = self.l1.get(slice_id).await {
                if let Err(e) = self.inner.write_slice(*slice_id, data.clone()).await {
                    // Re-mark as dirty on failure
                    self.dirty.lock().insert(*slice_id);
                    errors.push(e);
                } else {
                    // Write-through to disk cache on flush
                    self.disk_write(*slice_id, &data).await;
                }
            }
        }

        if let Some(e) = errors.into_iter().next() {
            return Err(e);
        }

        Ok(())
    }
}
