use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use parking_lot::RwLock;

use crate::cache::CachedOperator;

struct OpenFile {
    path: String,
    buffer: Vec<u8>,
    dirty: bool,
    flush_generation: u64,
}

#[derive(Clone, Debug)]
pub struct FileStat {
    pub size: u64,
    pub is_dir: bool,
    pub last_modified: SystemTime,
}

#[derive(Debug)]
pub enum VfsError {
    NotFound,
    PermissionDenied,
    Io,
    BadFd,
}

pub struct FlatVfs {
    op: CachedOperator,
    open_files: RwLock<HashMap<u64, OpenFile>>,
    next_fh: AtomicU64,
    writeback: bool,
}

fn opendal_to_vfs(e: opendal::Error) -> VfsError {
    match e.kind() {
        opendal::ErrorKind::NotFound => VfsError::NotFound,
        opendal::ErrorKind::PermissionDenied => VfsError::PermissionDenied,
        _ => VfsError::Io,
    }
}

impl FlatVfs {
    pub fn new(op: CachedOperator, writeback: bool) -> Self {
        Self {
            op,
            open_files: RwLock::new(HashMap::new()),
            next_fh: AtomicU64::new(1),
            writeback,
        }
    }

    pub fn writeback(&self) -> bool {
        self.writeback
    }

    fn alloc_fh(&self) -> u64 {
        self.next_fh.fetch_add(1, Ordering::Relaxed)
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

    pub async fn stat(&self, path: &str) -> Result<FileStat, VfsError> {
        if path.is_empty() {
            return Ok(FileStat {
                size: 0,
                is_dir: true,
                last_modified: UNIX_EPOCH,
            });
        }

        match self.op.stat(path).await {
            Ok(meta) => Ok(Self::meta_to_stat(&meta)),
            Err(e) if e.kind() == opendal::ErrorKind::NotFound => {
                let dir_path = Self::dir_key(path);
                match self.op.stat(&dir_path).await {
                    Ok(meta) => Ok(FileStat {
                        size: 0,
                        is_dir: true,
                        last_modified: meta
                            .last_modified()
                            .map(|t| UNIX_EPOCH + Duration::from_secs(t.timestamp() as u64))
                            .unwrap_or(UNIX_EPOCH),
                    }),
                    Err(e2) if e2.kind() == opendal::ErrorKind::NotFound => {
                        let entries = self.op.list(path).await;
                        match entries {
                            Ok(entries) if !entries.is_empty() => Ok(FileStat {
                                size: 0,
                                is_dir: true,
                                last_modified: UNIX_EPOCH,
                            }),
                            _ => Err(VfsError::NotFound),
                        }
                    }
                    Err(_) => Err(VfsError::Io),
                }
            }
            Err(e) if e.kind() == opendal::ErrorKind::PermissionDenied => {
                Err(VfsError::PermissionDenied)
            }
            Err(_) => Err(VfsError::Io),
        }
    }

    pub fn dirty_stat(&self, path: &str) -> Option<FileStat> {
        let files = self.open_files.read();
        files
            .values()
            .find(|f| f.path == path && f.dirty)
            .map(|f| FileStat {
                size: f.buffer.len() as u64,
                is_dir: false,
                last_modified: SystemTime::now(),
            })
    }

    pub async fn read(&self, path: &str) -> Result<Bytes, VfsError> {
        self.op.read(path).await.map_err(opendal_to_vfs)
    }

    pub fn read_dirty(&self, fh: u64, offset: u64, size: u32) -> Option<Bytes> {
        let files = self.open_files.read();
        let f = files.get(&fh).filter(|f| f.dirty)?;
        let start = offset as usize;
        if start >= f.buffer.len() {
            Some(Bytes::new())
        } else {
            let end = std::cmp::min(start + size as usize, f.buffer.len());
            Some(Bytes::copy_from_slice(&f.buffer[start..end]))
        }
    }

    pub async fn write_at(&self, fh: u64, offset: u64, data: &[u8]) -> Result<u32, VfsError> {
        let needs_read = {
            let files = self.open_files.read();
            match files.get(&fh) {
                Some(f) => f.buffer.is_empty() && !f.dirty,
                None => return Err(VfsError::BadFd),
            }
        };

        if needs_read {
            let path = {
                let files = self.open_files.read();
                match files.get(&fh) {
                    Some(f) => f.path.clone(),
                    None => return Err(VfsError::BadFd),
                }
            };
            let existing = self.op.read(&path).await.ok().map(|d| d.to_vec());
            let mut files = self.open_files.write();
            let file = match files.get_mut(&fh) {
                Some(f) => f,
                None => return Err(VfsError::BadFd),
            };
            if file.buffer.is_empty() && !file.dirty
                && let Some(existing_data) = existing {
                    file.buffer = existing_data;
                }
        }

        let mut files = self.open_files.write();
        let file = match files.get_mut(&fh) {
            Some(f) => f,
            None => return Err(VfsError::BadFd),
        };
        let end = offset as usize + data.len();
        if end > file.buffer.len() {
            file.buffer.resize(end, 0);
        }
        file.buffer[offset as usize..end].copy_from_slice(data);
        file.dirty = true;
        file.flush_generation += 1;
        Ok(data.len() as u32)
    }

    pub async fn truncate(&self, path: &str, new_size: u64) -> Result<(), VfsError> {
        if new_size == 0 {
            self.op
                .write(path, Vec::<u8>::new())
                .await
                .map_err(opendal_to_vfs)?;
        } else {
            let data = self.read(path).await?;
            let mut bytes = data.to_vec();
            bytes.resize(new_size as usize, 0);
            self.op.write(path, bytes).await.map_err(opendal_to_vfs)?;
        }

        let mut files = self.open_files.write();
        for f in files.values_mut() {
            if f.path == path && f.dirty {
                f.buffer.resize(new_size as usize, 0);
            }
        }

        Ok(())
    }

    pub fn open(&self, path: String) -> u64 {
        let fh = self.alloc_fh();
        self.open_files.write().insert(
            fh,
            OpenFile {
                path,
                buffer: Vec::new(),
                dirty: false,
                flush_generation: 0,
            },
        );
        fh
    }

    pub async fn create_empty(&self, path: &str) -> Result<(), VfsError> {
        self.op
            .write(path, Vec::<u8>::new())
            .await
            .map_err(opendal_to_vfs)
    }

    pub async fn flush(&self, fh: u64) -> Result<(), VfsError> {
        let (path, data, flush_gen) = {
            let files = self.open_files.read();
            match files.get(&fh) {
                Some(f) if f.dirty => {
                    (f.path.clone(), Bytes::from(f.buffer.clone()), f.flush_generation)
                }
                Some(_) => return Ok(()),
                None => return Err(VfsError::BadFd),
            }
        };

        self.op.write(&path, data).await.map_err(opendal_to_vfs)?;

        {
            let mut files = self.open_files.write();
            if let Some(f) = files.get_mut(&fh)
                && f.flush_generation == flush_gen {
                    f.dirty = false;
                }
        }

        Ok(())
    }

    pub async fn release(&self, fh: u64) {
        if let Err(e) = self.flush(fh).await {
            tracing::warn!("flush on release failed: {:?}", e);
        }
        self.open_files.write().remove(&fh);
    }

    pub async fn delete(&self, path: &str) -> Result<(), VfsError> {
        self.op.delete(path).await.map_err(opendal_to_vfs)
    }

    pub async fn create_dir(&self, path: &str) -> Result<(), VfsError> {
        self.op.create_dir(path).await.map_err(opendal_to_vfs)
    }

    pub async fn list(&self, path: &str) -> Result<Vec<(String, bool)>, VfsError> {
        self.op.list(path).await.map_err(opendal_to_vfs)
    }

    pub async fn rename(&self, src: &str, dst: &str) -> Result<(), VfsError> {
        self.op.copy(src, dst).await.map_err(opendal_to_vfs)?;
        self.op.delete(src).await.map_err(opendal_to_vfs)
    }
}
