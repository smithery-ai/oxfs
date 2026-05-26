use std::time::Duration;

use bytes::Bytes;
use opendal::Operator;

#[derive(Clone)]
struct CachedContent {
    data: Bytes,
    etag: String,
}

/// Caching wrapper around an OpenDAL operator.
///
/// Owns all cache invariants: every mutation automatically invalidates
/// the affected stat/content/dir caches, including the parent directory
/// listing. Callers never need to do manual cache maintenance.
pub struct CachedOperator {
    op: Operator,
    dir_cache: moka::future::Cache<String, Vec<(String, bool)>>,
    stat_cache: moka::future::Cache<String, opendal::Metadata>,
    content_cache: moka::future::Cache<String, CachedContent>,
    writeback: bool,
}

impl CachedOperator {
    pub fn new(op: Operator, dir_ttl: Duration, writeback: bool) -> Self {
        let dir_cache = moka::future::Cache::builder()
            .time_to_live(dir_ttl)
            .max_capacity(10_000)
            .build();
        let stat_cache = moka::future::Cache::builder()
            .time_to_live(dir_ttl)
            .max_capacity(100_000)
            .build();
        let content_cache = moka::future::Cache::builder()
            .max_capacity(256 * 1024 * 1024)
            .weigher(|_key: &String, val: &CachedContent| -> u32 {
                u32::try_from(val.data.len()).unwrap_or(u32::MAX)
            })
            .build();

        Self {
            op,
            dir_cache,
            stat_cache,
            content_cache,
            writeback,
        }
    }

    pub async fn stat(&self, path: &str) -> Result<opendal::Metadata, opendal::Error> {
        if let Some(cached) = self.stat_cache.get(path).await {
            return Ok(cached);
        }

        let meta = self.op.stat(path).await?;
        self.stat_cache
            .insert(path.to_string(), meta.clone())
            .await;
        Ok(meta)
    }

    pub async fn read(&self, path: &str) -> Result<Bytes, opendal::Error> {
        if self.writeback {
            return self.op.read(path).await.map(|d| d.to_bytes());
        }

        if let Some(cached) = self.content_cache.get(path).await {
            match self.op.stat_with(path).if_none_match(&cached.etag).await {
                Err(e) if e.kind() == opendal::ErrorKind::ConditionNotMatch => {
                    return Ok(cached.data);
                }
                Err(e)
                    if e.kind() != opendal::ErrorKind::NotFound
                        && e.kind() != opendal::ErrorKind::PermissionDenied =>
                {
                    return Ok(cached.data);
                }
                Ok(_) | Err(_) => {}
            }
        }

        let etag = self
            .op
            .stat(path)
            .await
            .ok()
            .and_then(|m| m.etag().map(|e| e.to_string()));

        if let Some(etag) = etag {
            match self.op.read_with(path).if_match(&etag).await {
                Ok(data) => {
                    let bytes = data.to_bytes();
                    self.content_cache
                        .insert(
                            path.to_string(),
                            CachedContent {
                                data: bytes.clone(),
                                etag,
                            },
                        )
                        .await;
                    return Ok(bytes);
                }
                Err(e) if e.kind() == opendal::ErrorKind::ConditionNotMatch => {}
                Err(e) => return Err(e),
            }
        }

        self.op.read(path).await.map(|d| d.to_bytes())
    }

    pub async fn write(
        &self,
        path: &str,
        data: impl Into<opendal::Buffer>,
    ) -> Result<(), opendal::Error> {
        self.op.write(path, data).await?;
        self.invalidate_path(path).await;
        Ok(())
    }

    pub async fn delete(&self, path: &str) -> Result<(), opendal::Error> {
        self.op.delete(path).await?;
        self.invalidate_path(path).await;
        Ok(())
    }

    pub async fn copy(&self, from: &str, to: &str) -> Result<(), opendal::Error> {
        self.op.copy(from, to).await?;
        self.invalidate_path(to).await;
        Ok(())
    }

    pub async fn create_dir(&self, path: &str) -> Result<(), opendal::Error> {
        self.op.create_dir(path).await?;
        self.invalidate_path(path).await;
        Ok(())
    }

    pub async fn list(&self, path: &str) -> Result<Vec<(String, bool)>, opendal::Error> {
        if let Some(cached) = self.dir_cache.get(path).await {
            return Ok(cached);
        }

        let dir_key = if path.is_empty() {
            "/".to_string()
        } else {
            format!("{}/", path)
        };
        let raw_entries = self.op.list_with(&dir_key).recursive(false).await?;

        let entries: Vec<(String, bool)> = raw_entries
            .into_iter()
            .filter_map(|e| {
                let name = e.name().to_string();
                if name.is_empty() || name == "/" {
                    return None;
                }
                let is_dir = name.ends_with('/');
                let clean_name = name.trim_end_matches('/').to_string();
                if clean_name.is_empty() {
                    return None;
                }
                Some((clean_name, is_dir))
            })
            .collect();

        self.dir_cache
            .insert(path.to_string(), entries.clone())
            .await;
        Ok(entries)
    }

    /// Invalidate all caches affected by a mutation at `path`:
    /// stat and content for the path itself (both with and without
    /// trailing slash), and the parent directory listing.
    async fn invalidate_path(&self, path: &str) {
        let normalized = path.trim_end_matches('/');
        let with_slash = format!("{}/", normalized);

        self.stat_cache.remove(normalized).await;
        self.stat_cache.remove(&with_slash).await;
        self.content_cache.remove(normalized).await;
        self.content_cache.remove(&with_slash).await;

        if let Some(parent) = Self::parent(normalized) {
            self.dir_cache.remove(parent).await;
        }
    }

    fn parent(path: &str) -> Option<&str> {
        if path.is_empty() {
            return None;
        }
        match path.rfind('/') {
            Some(pos) => Some(&path[..pos]),
            None => Some(""),
        }
    }
}
