use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use moka::future::Cache;
use oxfs_data::{DataEngine, DataResult};
use parking_lot::Mutex;
use tokio::sync::Notify;

#[async_trait]
pub trait CacheLayer: Send + Sync {
    async fn read_slice(&self, slice_id: u64) -> DataResult<Bytes>;
    async fn write_slice(&self, slice_id: u64, data: Bytes) -> DataResult<()>;
    async fn delete_slice(&self, slice_id: u64) -> DataResult<()>;
    async fn flush_dirty(&self) -> DataResult<()>;
    async fn replay_wal(&self);
    fn dirty_count(&self) -> usize;
    fn flush_signal(&self) -> Arc<Notify>;
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

    async fn replay_wal(&self) {}

    fn dirty_count(&self) -> usize { 0 }

    fn flush_signal(&self) -> Arc<Notify> { Arc::new(Notify::new()) }
}

pub struct CacheConfig {
    pub mem_max_bytes: u64,
    pub disk_path: Option<PathBuf>,
    pub disk_max_bytes: u64,
    pub wal_path: Option<PathBuf>,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            mem_max_bytes: 256 * 1024 * 1024,
            disk_path: None,
            disk_max_bytes: 1024 * 1024 * 1024,
            wal_path: None,
        }
    }
}

pub struct TieredCache<D: DataEngine> {
    inner: D,
    l1: Cache<u64, Bytes>,
    dirty_data: Mutex<HashMap<u64, Bytes>>,
    wal: Option<crate::wal::Wal>,
    flush_notify: Arc<Notify>,
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

        let wal = config.wal_path.as_ref().and_then(|p| {
            crate::wal::Wal::open(p)
                .map_err(|e| tracing::warn!("failed to open WAL at {}: {}", p.display(), e))
                .ok()
        });

        Self {
            inner,
            l1,
            dirty_data: Mutex::new(HashMap::new()),
            wal,
            flush_notify: Arc::new(Notify::new()),
            disk_path: config.disk_path,
            disk_max_bytes: config.disk_max_bytes,
            disk_used: AtomicU64::new(0),
            l1_hits: AtomicU64::new(0),
            l2_hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }


    async fn flush_dirty_inner(&self) -> DataResult<()> {
        let items: Vec<(u64, Bytes)> = {
            let mut dd = self.dirty_data.lock();
            dd.drain().collect()
        };

        if items.is_empty() {
            return Ok(());
        }

        tracing::debug!(count = items.len(), "flushing dirty slices (parallel)");

        let futs: Vec<_> = items.iter().map(|(sid, data)| {
            let sid = *sid;
            let data = data.clone();
            async move {
                (sid, data.clone(), self.inner.write_slice(sid, data).await)
            }
        }).collect();

        let results = futures::future::join_all(futs).await;

        let mut first_error = None;
        for (sid, data, result) in results {
            match result {
                Ok(()) => {
                    self.disk_write(sid, &data).await;
                }
                Err(e) => {
                    // Re-insert failed slices back into dirty_data
                    self.dirty_data.lock().insert(sid, data);
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
            }
        }

        if let Some(e) = first_error {
            return Err(e);
        }

        Ok(())
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
        // Check dirty buffer first (unflushed writes)
        if let Some(data) = self.dirty_data.lock().get(&slice_id).cloned() {
            self.l1_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(data);
        }

        if let Some(data) = self.l1.get(&slice_id).await {
            self.l1_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(data);
        }

        if let Some(data) = self.disk_read(slice_id).await {
            self.l2_hits.fetch_add(1, Ordering::Relaxed);
            self.l1.insert(slice_id, data.clone()).await;
            return Ok(data);
        }

        self.misses.fetch_add(1, Ordering::Relaxed);
        let data = self.inner.read_slice(slice_id).await?;
        self.l1.insert(slice_id, data.clone()).await;
        self.disk_write(slice_id, &data).await;
        Ok(data)
    }

    async fn write_slice(&self, slice_id: u64, data: Bytes) -> DataResult<()> {
        if let Some(ref wal) = self.wal {
            let _ = wal.append(slice_id, &data);
        }
        self.l1.insert(slice_id, data.clone()).await;
        self.dirty_data.lock().insert(slice_id, data);
        self.flush_notify.notify_one();
        Ok(())
    }

    async fn delete_slice(&self, slice_id: u64) -> DataResult<()> {
        self.dirty_data.lock().remove(&slice_id);
        self.l1.invalidate(&slice_id).await;
        self.disk_delete(slice_id).await;
        let _ = self.inner.delete_slice(slice_id).await;
        Ok(())
    }

    async fn flush_dirty(&self) -> DataResult<()> {
        let result = self.flush_dirty_inner().await;
        if result.is_ok() && let Some(ref wal) = self.wal {
            let _ = wal.clear();
        }
        result
    }

    async fn replay_wal(&self) {
        let Some(ref wal) = self.wal else { return };
        let entries = match wal.read_all() {
            Ok(e) if e.is_empty() => return,
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("WAL replay read failed: {}", e);
                return;
            }
        };
        tracing::info!(count = entries.len(), "replaying WAL entries");
        for (slice_id, data) in entries {
            self.l1.insert(slice_id, data.clone()).await;
            self.dirty_data.lock().insert(slice_id, data);
        }
        if let Err(e) = self.flush_dirty_inner().await {
            tracing::warn!("WAL replay flush failed: {}", e);
        }
    }

    fn dirty_count(&self) -> usize {
        self.dirty_data.lock().len()
    }

    fn flush_signal(&self) -> Arc<Notify> {
        Arc::clone(&self.flush_notify)
    }
}
