use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use fuser::{
    Errno, FileAttr, FileType as FuseFileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyStatfs, ReplyWrite, Request, TimeOrNow,
};
use parking_lot::RwLock;

use crate::inode::InodeTable;
use crate::object_store::{FileStat, ObjectStore};
use crate::FlatConfig;

unsafe extern "C" {
    fn getuid() -> u32;
    fn getgid() -> u32;
}

unsafe fn libc_getuid() -> u32 {
    unsafe { getuid() }
}

unsafe fn libc_getgid() -> u32 {
    unsafe { getgid() }
}

const TTL: Duration = Duration::from_secs(1);

struct OpenFile {
    path: String,
    buffer: Vec<u8>,
    dirty: bool,
}

pub struct FlatFuse {
    store: ObjectStore,
    rt: tokio::runtime::Handle,
    inodes: InodeTable,
    open_files: RwLock<HashMap<u64, OpenFile>>,
    next_fh: AtomicU64,
    uid: u32,
    gid: u32,
    writeback: bool,
}

impl FlatFuse {
    pub fn new(
        op: opendal::Operator,
        rt: tokio::runtime::Handle,
        config: FlatConfig,
    ) -> Self {
        let uid = unsafe { libc_getuid() };
        let gid = unsafe { libc_getgid() };

        Self {
            store: ObjectStore::new(op, config.dir_ttl, config.writeback),
            rt,
            inodes: InodeTable::new(),
            open_files: RwLock::new(HashMap::new()),
            next_fh: AtomicU64::new(1),
            uid,
            gid,
            writeback: config.writeback,
        }
    }

    fn alloc_fh(&self) -> u64 {
        self.next_fh.fetch_add(1, Ordering::Relaxed)
    }

    fn make_file_attr(&self, ino: u64, stat: &FileStat) -> FileAttr {
        let (kind, perm) = if stat.is_dir {
            (FuseFileType::Directory, 0o755)
        } else {
            (FuseFileType::RegularFile, 0o644)
        };
        FileAttr {
            ino: fuser::INodeNo(ino),
            size: stat.size,
            blocks: stat.size.div_ceil(512),
            atime: stat.last_modified,
            mtime: stat.last_modified,
            ctime: stat.last_modified,
            crtime: stat.last_modified,
            kind,
            perm,
            nlink: 1,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    fn join_path(parent_path: &str, name: &str) -> String {
        if parent_path.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", parent_path, name)
        }
    }

    async fn flush_file(&self, fh: u64) -> Result<(), Errno> {
        let (path, data) = {
            let files = self.open_files.read();
            match files.get(&fh) {
                Some(f) if f.dirty => (f.path.clone(), Bytes::from(f.buffer.clone())),
                Some(_) => return Ok(()),
                None => return Err(Errno::EBADF),
            }
        };

        self.store.write(&path, data).await.map_err(|_| Errno::EIO)?;

        {
            let mut files = self.open_files.write();
            if let Some(f) = files.get_mut(&fh) {
                f.dirty = false;
            }
        }

        Ok(())
    }
}

fn to_errno(e: &opendal::Error) -> Errno {
    match e.kind() {
        opendal::ErrorKind::NotFound => Errno::ENOENT,
        opendal::ErrorKind::PermissionDenied => Errno::EACCES,
        _ => Errno::EIO,
    }
}

impl Filesystem for FlatFuse {
    fn init(
        &mut self,
        _req: &Request,
        _config: &mut fuser::KernelConfig,
    ) -> std::io::Result<()> {
        Ok(())
    }

    fn lookup(
        &self,
        _req: &Request,
        parent: fuser::INodeNo,
        name: &OsStr,
        reply: ReplyEntry,
    ) {
        let name = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(Errno::EINVAL);
                return;
            }
        };

        let parent_path = match self.inodes.resolve(parent.into()) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        let path = Self::join_path(&parent_path, name);

        match self.rt.block_on(self.store.stat(&path)) {
            Ok(stat) => {
                let ino = self.inodes.allocate(&path);
                let attr = self.make_file_attr(ino, &stat);
                reply.entry(&TTL, &attr, fuser::Generation(0));
            }
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    fn getattr(
        &self,
        _req: &Request,
        ino: fuser::INodeNo,
        _fh: Option<fuser::FileHandle>,
        reply: ReplyAttr,
    ) {
        let path = match self.inodes.resolve(ino.into()) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        {
            let files = self.open_files.read();
            for (_, f) in files.iter() {
                if f.path == path && f.dirty {
                    let stat = FileStat {
                        size: f.buffer.len() as u64,
                        is_dir: false,
                        last_modified: SystemTime::now(),
                    };
                    let ino_val: u64 = ino.into();
                    let attr = self.make_file_attr(ino_val, &stat);
                    reply.attr(&TTL, &attr);
                    return;
                }
            }
        }

        match self.rt.block_on(self.store.stat(&path)) {
            Ok(stat) => {
                let ino_val: u64 = ino.into();
                let attr = self.make_file_attr(ino_val, &stat);
                reply.attr(&TTL, &attr);
            }
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: fuser::INodeNo,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<fuser::FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let path = match self.inodes.resolve(ino.into()) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        if let Some(new_size) = size {
            let result = self.rt.block_on(async {
                if new_size == 0 {
                    self.store.write(&path, Vec::<u8>::new()).await.map_err(|_| Errno::EIO)?;
                } else {
                    let data = self.store.read(&path).await.map_err(|e| to_errno(&e))?;
                    let mut bytes = data.to_vec();
                    bytes.resize(new_size as usize, 0);
                    self.store.write(&path, bytes).await.map_err(|_| Errno::EIO)?;
                }
                Ok::<(), Errno>(())
            });

            if let Err(e) = result {
                reply.error(e);
                return;
            }
        }

        match self.rt.block_on(self.store.stat(&path)) {
            Ok(stat) => {
                let ino_val: u64 = ino.into();
                let attr = self.make_file_attr(ino_val, &stat);
                reply.attr(&TTL, &attr);
            }
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: fuser::INodeNo,
        _fh: fuser::FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let path = match self.inodes.resolve(ino.into()) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        let ino_val: u64 = ino.into();

        let entries = match self.rt.block_on(self.store.list_dir(&path)) {
            Ok(e) => e,
            Err(_) => {
                reply.error(Errno::EIO);
                return;
            }
        };

        let mut full: Vec<(u64, FuseFileType, String)> = vec![
            (ino_val, FuseFileType::Directory, ".".to_string()),
            (ino_val, FuseFileType::Directory, "..".to_string()),
        ];

        for (name, is_dir) in &entries {
            let child_path = Self::join_path(&path, name);
            let child_ino = self.inodes.allocate(&child_path);
            let ftype = if *is_dir {
                FuseFileType::Directory
            } else {
                FuseFileType::RegularFile
            };
            full.push((child_ino, ftype, name.clone()));
        }

        for (i, (ino, kind, name)) in full.into_iter().enumerate().skip(offset as usize) {
            if reply.add(fuser::INodeNo(ino), (i + 1) as u64, kind, &name) {
                break;
            }
        }
        reply.ok();
    }

    fn open(
        &self,
        _req: &Request,
        ino: fuser::INodeNo,
        _flags: fuser::OpenFlags,
        reply: fuser::ReplyOpen,
    ) {
        let path = match self.inodes.resolve(ino.into()) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        let fh = self.alloc_fh();
        self.open_files.write().insert(fh, OpenFile {
            path,
            buffer: Vec::new(),
            dirty: false,
        });

        reply.opened(fuser::FileHandle(fh), fuser::FopenFlags::empty());
    }

    fn read(
        &self,
        _req: &Request,
        ino: fuser::INodeNo,
        fh: fuser::FileHandle,
        offset: u64,
        size: u32,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyData,
    ) {
        let fh_val: u64 = fh.into();

        {
            let files = self.open_files.read();
            if let Some(f) = files.get(&fh_val).filter(|f| f.dirty) {
                let start = offset as usize;
                let end = std::cmp::min(start + size as usize, f.buffer.len());
                if start >= f.buffer.len() {
                    reply.data(&[]);
                } else {
                    reply.data(&f.buffer[start..end]);
                }
                return;
            }
        }

        let path = match self.inodes.resolve(ino.into()) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        match self.rt.block_on(self.store.read(&path)) {
            Ok(bytes) => {
                let start = offset as usize;
                if start >= bytes.len() {
                    reply.data(&[]);
                } else {
                    let end = std::cmp::min(start + size as usize, bytes.len());
                    reply.data(&bytes[start..end]);
                }
            }
            Err(e) => {
                if e.kind() != opendal::ErrorKind::NotFound {
                    tracing::warn!("read {} failed: {:?}", path, e);
                }
                reply.error(to_errno(&e));
            }
        }
    }

    fn write(
        &self,
        _req: &Request,
        _ino: fuser::INodeNo,
        fh: fuser::FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: fuser::WriteFlags,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyWrite,
    ) {
        let fh_val: u64 = fh.into();
        let mut files = self.open_files.write();
        let file = match files.get_mut(&fh_val) {
            Some(f) => f,
            None => {
                reply.error(Errno::EBADF);
                return;
            }
        };

        if file.buffer.is_empty() && !file.dirty {
            let path = file.path.clone();
            drop(files);
            let existing = self
                .rt
                .block_on(async { self.store.read(&path).await.ok().map(|d| d.to_vec()) });
            let mut files = self.open_files.write();
            let file = files.get_mut(&fh_val).unwrap();
            if let Some(existing_data) = existing {
                file.buffer = existing_data;
            }
            let end = offset as usize + data.len();
            if end > file.buffer.len() {
                file.buffer.resize(end, 0);
            }
            file.buffer[offset as usize..end].copy_from_slice(data);
            file.dirty = true;
            reply.written(data.len() as u32);
            return;
        }

        let end = offset as usize + data.len();
        if end > file.buffer.len() {
            file.buffer.resize(end, 0);
        }
        file.buffer[offset as usize..end].copy_from_slice(data);
        file.dirty = true;
        reply.written(data.len() as u32);
    }

    fn release(
        &self,
        _req: &Request,
        _ino: fuser::INodeNo,
        fh: fuser::FileHandle,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let fh_val: u64 = fh.into();
        if let Err(e) = self.rt.block_on(self.flush_file(fh_val)) {
            tracing::warn!("flush on release failed: {:?}", e);
        }
        self.open_files.write().remove(&fh_val);
        reply.ok();
    }

    fn fsync(
        &self,
        _req: &Request,
        _ino: fuser::INodeNo,
        fh: fuser::FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        let fh_val: u64 = fh.into();
        match self.rt.block_on(self.flush_file(fh_val)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn flush(
        &self,
        _req: &Request,
        _ino: fuser::INodeNo,
        fh: fuser::FileHandle,
        _lock_owner: fuser::LockOwner,
        reply: ReplyEmpty,
    ) {
        if self.writeback {
            reply.ok();
            return;
        }
        let fh_val: u64 = fh.into();
        match self.rt.block_on(self.flush_file(fh_val)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn create(
        &self,
        _req: &Request,
        parent: fuser::INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let name = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(Errno::EINVAL);
                return;
            }
        };

        let parent_path = match self.inodes.resolve(parent.into()) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        let path = Self::join_path(&parent_path, name);
        let ino = self.inodes.allocate(&path);
        let fh = self.alloc_fh();

        self.open_files.write().insert(fh, OpenFile {
            path: path.clone(),
            buffer: Vec::new(),
            dirty: false,
        });

        self.rt.block_on(self.store.invalidate_dir(&parent_path));

        let stat = FileStat {
            size: 0,
            is_dir: false,
            last_modified: SystemTime::now(),
        };
        let attr = self.make_file_attr(ino, &stat);
        reply.created(
            &TTL,
            &attr,
            fuser::Generation(0),
            fuser::FileHandle(fh),
            fuser::FopenFlags::empty(),
        );
    }

    fn unlink(
        &self,
        _req: &Request,
        parent: fuser::INodeNo,
        name: &OsStr,
        reply: ReplyEmpty,
    ) {
        let name = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(Errno::EINVAL);
                return;
            }
        };

        let parent_path = match self.inodes.resolve(parent.into()) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        let path = Self::join_path(&parent_path, name);

        match self.rt.block_on(async {
            self.store.delete(&path).await.map_err(|_| Errno::EIO)?;
            self.store.invalidate_dir(&parent_path).await;
            Ok::<(), Errno>(())
        }) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: fuser::INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let name = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(Errno::EINVAL);
                return;
            }
        };

        let parent_path = match self.inodes.resolve(parent.into()) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        let path = Self::join_path(&parent_path, name);
        let dir_path = format!("{}/", path);

        match self.rt.block_on(async {
            self.store
                .create_dir(&dir_path)
                .await
                .map_err(|_| Errno::EIO)?;
            self.store.invalidate_dir(&parent_path).await;
            Ok::<(), Errno>(())
        }) {
            Ok(()) => {
                let ino = self.inodes.allocate(&path);
                let stat = FileStat {
                    size: 0,
                    is_dir: true,
                    last_modified: SystemTime::now(),
                };
                let attr = self.make_file_attr(ino, &stat);
                reply.entry(&TTL, &attr, fuser::Generation(0));
            }
            Err(e) => reply.error(e),
        }
    }

    fn rmdir(
        &self,
        _req: &Request,
        parent: fuser::INodeNo,
        name: &OsStr,
        reply: ReplyEmpty,
    ) {
        let name = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(Errno::EINVAL);
                return;
            }
        };

        let parent_path = match self.inodes.resolve(parent.into()) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        let path = Self::join_path(&parent_path, name);
        let dir_path = format!("{}/", path);

        match self.rt.block_on(async {
            self.store
                .delete(&dir_path)
                .await
                .map_err(|_| Errno::EIO)?;
            self.store.invalidate_dir(&parent_path).await;
            Ok::<(), Errno>(())
        }) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn rename(
        &self,
        _req: &Request,
        parent: fuser::INodeNo,
        name: &OsStr,
        newparent: fuser::INodeNo,
        newname: &OsStr,
        _flags: fuser::RenameFlags,
        reply: ReplyEmpty,
    ) {
        let src_name = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(Errno::EINVAL);
                return;
            }
        };
        let dst_name = match newname.to_str() {
            Some(n) => n,
            None => {
                reply.error(Errno::EINVAL);
                return;
            }
        };

        let src_parent_path = match self.inodes.resolve(parent.into()) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let dst_parent_path = match self.inodes.resolve(newparent.into()) {
            Some(p) => p,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        let src_path = Self::join_path(&src_parent_path, src_name);
        let dst_path = Self::join_path(&dst_parent_path, dst_name);

        match self.rt.block_on(async {
            self.store
                .copy(&src_path, &dst_path)
                .await
                .map_err(|_| Errno::EIO)?;
            self.store
                .delete(&src_path)
                .await
                .map_err(|_| Errno::EIO)?;
            self.store.invalidate_dir(&src_parent_path).await;
            self.store.invalidate_dir(&dst_parent_path).await;
            Ok::<(), Errno>(())
        }) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn statfs(&self, _req: &Request, _ino: fuser::INodeNo, reply: ReplyStatfs) {
        reply.statfs(
            1_000_000_000,
            500_000_000,
            500_000_000,
            1_000_000,
            500_000,
            4096,
            255,
            0,
        );
    }

    fn access(
        &self,
        _req: &Request,
        _ino: fuser::INodeNo,
        _mask: fuser::AccessFlags,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }
}
