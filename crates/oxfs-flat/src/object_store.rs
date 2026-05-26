use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use opendal::Operator;

#[derive(Clone, Debug)]
pub struct FileStat {
    pub size: u64,
    pub is_dir: bool,
    pub last_modified: SystemTime,
}

#[derive(Clone)]
struct CachedContent {
    data: Bytes,
    etag: String,
}

pub struct ObjectStore {
    op: Operator,
    dir_cache: moka::future::Cache<String, Vec<(String, bool)>>,
    stat_cache: moka::future::Cache<String, FileStat>,
    content_cache: moka::future::Cache<String, CachedContent>,
    writeback: bool,
}

impl ObjectStore {
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

    pub async fn stat(&self, path: &str) -> Result<FileStat, opendal::Error> {
        if path.is_empty() {
            return Ok(FileStat {
                size: 0,
                is_dir: true,
                last_modified: UNIX_EPOCH,
            });
        }

        if let Some(cached) = self.stat_cache.get(path).await {
            return Ok(cached);
        }

        match self.op.stat(path).await {
            Ok(meta) => {
                let stat = Self::meta_to_stat(&meta);
                self.stat_cache.insert(path.to_string(), stat.clone()).await;
                Ok(stat)
            }
            Err(e) if e.kind() == opendal::ErrorKind::NotFound => {
                let dir_path = Self::dir_key(path);
                match self.op.stat(&dir_path).await {
                    Ok(meta) => {
                        let stat = FileStat {
                            size: 0,
                            is_dir: true,
                            last_modified: meta
                                .last_modified()
                                .map(|t| UNIX_EPOCH + Duration::from_secs(t.timestamp() as u64))
                                .unwrap_or(UNIX_EPOCH),
                        };
                        self.stat_cache.insert(path.to_string(), stat.clone()).await;
                        Ok(stat)
                    }
                    Err(e2) if e2.kind() == opendal::ErrorKind::NotFound => {
                        let entries = self.op.list_with(&dir_path).recursive(false).await;
                        match entries {
                            Ok(entries) if !entries.is_empty() => {
                                let stat = FileStat {
                                    size: 0,
                                    is_dir: true,
                                    last_modified: UNIX_EPOCH,
                                };
                                self.stat_cache.insert(path.to_string(), stat.clone()).await;
                                Ok(stat)
                            }
                            _ => Err(opendal::Error::new(
                                opendal::ErrorKind::NotFound,
                                "not found",
                            )),
                        }
                    }
                    Err(e2) => Err(e2),
                }
            }
            Err(e) => Err(e),
        }
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

    pub async fn write(&self, path: &str, data: impl Into<opendal::Buffer>) -> Result<(), opendal::Error> {
        self.op.write(path, data).await?;
        self.invalidate(path).await;
        Ok(())
    }

    pub async fn delete(&self, path: &str) -> Result<(), opendal::Error> {
        self.op.delete(path).await?;
        self.invalidate(path).await;
        Ok(())
    }

    pub async fn copy(&self, from: &str, to: &str) -> Result<(), opendal::Error> {
        self.op.copy(from, to).await?;
        self.invalidate(from).await;
        self.invalidate(to).await;
        Ok(())
    }

    pub async fn create_dir(&self, path: &str) -> Result<(), opendal::Error> {
        self.op.create_dir(path).await?;
        Ok(())
    }

    pub async fn list_dir(&self, path: &str) -> Result<Vec<(String, bool)>, opendal::Error> {
        if let Some(cached) = self.dir_cache.get(path).await {
            return Ok(cached);
        }

        let dir_key = Self::dir_key(path);
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

        self.dir_cache.insert(path.to_string(), entries.clone()).await;
        Ok(entries)
    }

    pub async fn invalidate(&self, path: &str) {
        self.stat_cache.remove(path).await;
        self.content_cache.remove(path).await;
    }

    pub async fn invalidate_dir(&self, path: &str) {
        self.dir_cache.remove(path).await;
    }

    fn dir_key(path: &str) -> String {
        if path.is_empty() {
            "/".to_string()
        } else {
            format!("{}/", path)
        }
    }

    fn meta_to_stat(meta: &opendal::Metadata) -> FileStat {
        FileStat {
            size: meta.content_length(),
            is_dir: meta.is_dir(),
            last_modified: meta
                .last_modified()
                .map(|t| UNIX_EPOCH + Duration::from_secs(t.timestamp() as u64))
                .unwrap_or(UNIX_EPOCH),
        }
    }
}
