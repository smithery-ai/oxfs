use std::ffi::OsStr;
use std::time::{Duration, SystemTime};

use fuser::{
    Errno, FileAttr, FileType as FuseFileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyStatfs, ReplyWrite, Request, TimeOrNow,
};

use crate::inode::InodeTable;
use crate::vfs::{FileStat, FlatVfs, VfsError};
use crate::FlatConfig;

unsafe extern "C" {
    fn getuid() -> u32;
    fn getgid() -> u32;
}

const TTL: Duration = Duration::from_secs(1);

pub struct FlatFuse {
    vfs: FlatVfs,
    rt: tokio::runtime::Handle,
    inodes: InodeTable,
    uid: u32,
    gid: u32,
}

impl FlatFuse {
    pub fn new(
        operator: opendal::Operator,
        rt: tokio::runtime::Handle,
        config: FlatConfig,
    ) -> Self {
        let uid = unsafe { getuid() };
        let gid = unsafe { getgid() };
        let op = crate::cache::CachedOperator::new(operator, config.dir_ttl, config.writeback);

        Self {
            vfs: FlatVfs::new(op, config.writeback),
            rt,
            inodes: InodeTable::new(),
            uid,
            gid,
        }
    }

    fn make_attr(&self, ino: u64, stat: &FileStat) -> FileAttr {
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

    fn join_path(parent: &str, name: &str) -> String {
        if parent.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", parent, name)
        }
    }
}

fn to_errno(e: VfsError) -> Errno {
    match e {
        VfsError::NotFound => Errno::ENOENT,
        VfsError::PermissionDenied => Errno::EACCES,
        VfsError::BadFd => Errno::EBADF,
        VfsError::Io => Errno::EIO,
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

    fn lookup(&self, _req: &Request, parent: fuser::INodeNo, name: &OsStr, reply: ReplyEntry) {
        let name = match name.to_str() {
            Some(n) => n,
            None => { reply.error(Errno::EINVAL); return; }
        };
        let parent_path = match self.inodes.resolve(parent.into()) {
            Some(p) => p,
            None => { reply.error(Errno::ENOENT); return; }
        };
        let path = Self::join_path(&parent_path, name);

        match self.rt.block_on(self.vfs.stat(&path)) {
            Ok(stat) => {
                let ino = self.inodes.allocate(&path);
                reply.entry(&TTL, &self.make_attr(ino, &stat), fuser::Generation(0));
            }
            Err(e) => reply.error(to_errno(e)),
        }
    }

    fn getattr(&self, _req: &Request, ino: fuser::INodeNo, _fh: Option<fuser::FileHandle>, reply: ReplyAttr) {
        let path = match self.inodes.resolve(ino.into()) {
            Some(p) => p,
            None => { reply.error(Errno::ENOENT); return; }
        };

        if let Some(stat) = self.vfs.dirty_stat(&path) {
            reply.attr(&TTL, &self.make_attr(ino.into(), &stat));
            return;
        }

        match self.rt.block_on(self.vfs.stat(&path)) {
            Ok(stat) => reply.attr(&TTL, &self.make_attr(ino.into(), &stat)),
            Err(e) => reply.error(to_errno(e)),
        }
    }

    fn setattr(
        &self, _req: &Request, ino: fuser::INodeNo,
        _mode: Option<u32>, _uid: Option<u32>, _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>, _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>, _fh: Option<fuser::FileHandle>,
        _crtime: Option<SystemTime>, _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>, _flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let path = match self.inodes.resolve(ino.into()) {
            Some(p) => p,
            None => { reply.error(Errno::ENOENT); return; }
        };

        if let Some(new_size) = size
            && let Err(e) = self.rt.block_on(self.vfs.truncate(&path, new_size))
        {
            reply.error(to_errno(e));
            return;
        }

        match self.rt.block_on(self.vfs.stat(&path)) {
            Ok(stat) => reply.attr(&TTL, &self.make_attr(ino.into(), &stat)),
            Err(e) => reply.error(to_errno(e)),
        }
    }

    fn readdir(
        &self, _req: &Request, ino: fuser::INodeNo, _fh: fuser::FileHandle,
        offset: u64, mut reply: ReplyDirectory,
    ) {
        let path = match self.inodes.resolve(ino.into()) {
            Some(p) => p,
            None => { reply.error(Errno::ENOENT); return; }
        };

        let ino_val: u64 = ino.into();
        let entries = match self.rt.block_on(self.vfs.list(&path)) {
            Ok(e) => e,
            Err(e) => { reply.error(to_errno(e)); return; }
        };

        let mut full: Vec<(u64, FuseFileType, String)> = vec![
            (ino_val, FuseFileType::Directory, ".".into()),
            (ino_val, FuseFileType::Directory, "..".into()),
        ];
        for (name, is_dir) in &entries {
            let child_path = Self::join_path(&path, name);
            let child_ino = self.inodes.allocate(&child_path);
            let kind = if *is_dir { FuseFileType::Directory } else { FuseFileType::RegularFile };
            full.push((child_ino, kind, name.clone()));
        }

        for (i, (ino, kind, name)) in full.into_iter().enumerate().skip(offset as usize) {
            if reply.add(fuser::INodeNo(ino), (i + 1) as u64, kind, &name) {
                break;
            }
        }
        reply.ok();
    }

    fn open(&self, _req: &Request, ino: fuser::INodeNo, _flags: fuser::OpenFlags, reply: fuser::ReplyOpen) {
        let path = match self.inodes.resolve(ino.into()) {
            Some(p) => p,
            None => { reply.error(Errno::ENOENT); return; }
        };
        let fh = self.vfs.open(path);
        reply.opened(fuser::FileHandle(fh), fuser::FopenFlags::empty());
    }

    fn read(
        &self, _req: &Request, ino: fuser::INodeNo, fh: fuser::FileHandle,
        offset: u64, size: u32, _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>, reply: ReplyData,
    ) {
        let fh_val: u64 = fh.into();

        if let Some(data) = self.vfs.read_dirty(fh_val, offset, size) {
            reply.data(&data);
            return;
        }

        let path = match self.inodes.resolve(ino.into()) {
            Some(p) => p,
            None => { reply.error(Errno::ENOENT); return; }
        };

        match self.rt.block_on(self.vfs.read(&path)) {
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
                if !matches!(e, VfsError::NotFound) {
                    tracing::warn!("read {} failed: {:?}", path, e);
                }
                reply.error(to_errno(e));
            }
        }
    }

    fn write(
        &self, _req: &Request, _ino: fuser::INodeNo, fh: fuser::FileHandle,
        offset: u64, data: &[u8], _write_flags: fuser::WriteFlags,
        _flags: fuser::OpenFlags, _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyWrite,
    ) {
        match self.rt.block_on(self.vfs.write_at(fh.into(), offset, data)) {
            Ok(written) => reply.written(written),
            Err(e) => reply.error(to_errno(e)),
        }
    }

    fn release(
        &self, _req: &Request, _ino: fuser::INodeNo, fh: fuser::FileHandle,
        _flags: fuser::OpenFlags, _lock_owner: Option<fuser::LockOwner>,
        _flush: bool, reply: ReplyEmpty,
    ) {
        self.rt.block_on(self.vfs.release(fh.into()));
        reply.ok();
    }

    fn fsync(
        &self, _req: &Request, _ino: fuser::INodeNo, fh: fuser::FileHandle,
        _datasync: bool, reply: ReplyEmpty,
    ) {
        match self.rt.block_on(self.vfs.flush(fh.into())) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(to_errno(e)),
        }
    }

    fn flush(
        &self, _req: &Request, _ino: fuser::INodeNo, fh: fuser::FileHandle,
        _lock_owner: fuser::LockOwner, reply: ReplyEmpty,
    ) {
        if self.vfs.writeback() {
            reply.ok();
            return;
        }
        match self.rt.block_on(self.vfs.flush(fh.into())) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(to_errno(e)),
        }
    }

    fn create(
        &self, _req: &Request, parent: fuser::INodeNo, name: &OsStr,
        _mode: u32, _umask: u32, _flags: i32, reply: ReplyCreate,
    ) {
        let name = match name.to_str() {
            Some(n) => n,
            None => { reply.error(Errno::EINVAL); return; }
        };
        let parent_path = match self.inodes.resolve(parent.into()) {
            Some(p) => p,
            None => { reply.error(Errno::ENOENT); return; }
        };

        let path = Self::join_path(&parent_path, name);
        let ino = self.inodes.allocate(&path);
        let fh = self.vfs.open(path);

        let stat = FileStat { size: 0, is_dir: false, last_modified: SystemTime::now() };
        reply.created(&TTL, &self.make_attr(ino, &stat), fuser::Generation(0), fuser::FileHandle(fh), fuser::FopenFlags::empty());
    }

    fn unlink(&self, _req: &Request, parent: fuser::INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let name = match name.to_str() {
            Some(n) => n,
            None => { reply.error(Errno::EINVAL); return; }
        };
        let parent_path = match self.inodes.resolve(parent.into()) {
            Some(p) => p,
            None => { reply.error(Errno::ENOENT); return; }
        };
        let path = Self::join_path(&parent_path, name);

        match self.rt.block_on(self.vfs.delete(&path)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(to_errno(e)),
        }
    }

    fn mkdir(
        &self, _req: &Request, parent: fuser::INodeNo, name: &OsStr,
        _mode: u32, _umask: u32, reply: ReplyEntry,
    ) {
        let name = match name.to_str() {
            Some(n) => n,
            None => { reply.error(Errno::EINVAL); return; }
        };
        let parent_path = match self.inodes.resolve(parent.into()) {
            Some(p) => p,
            None => { reply.error(Errno::ENOENT); return; }
        };
        let path = Self::join_path(&parent_path, name);
        let dir_path = format!("{}/", path);

        match self.rt.block_on(self.vfs.create_dir(&dir_path)) {
            Ok(()) => {
                let ino = self.inodes.allocate(&path);
                let stat = FileStat { size: 0, is_dir: true, last_modified: SystemTime::now() };
                reply.entry(&TTL, &self.make_attr(ino, &stat), fuser::Generation(0));
            }
            Err(e) => reply.error(to_errno(e)),
        }
    }

    fn rmdir(&self, _req: &Request, parent: fuser::INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let name = match name.to_str() {
            Some(n) => n,
            None => { reply.error(Errno::EINVAL); return; }
        };
        let parent_path = match self.inodes.resolve(parent.into()) {
            Some(p) => p,
            None => { reply.error(Errno::ENOENT); return; }
        };
        let path = Self::join_path(&parent_path, name);
        let dir_path = format!("{}/", path);

        match self.rt.block_on(self.vfs.delete(&dir_path)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(to_errno(e)),
        }
    }

    fn rename(
        &self, _req: &Request, parent: fuser::INodeNo, name: &OsStr,
        newparent: fuser::INodeNo, newname: &OsStr, _flags: fuser::RenameFlags,
        reply: ReplyEmpty,
    ) {
        let src_name = match name.to_str() {
            Some(n) => n,
            None => { reply.error(Errno::EINVAL); return; }
        };
        let dst_name = match newname.to_str() {
            Some(n) => n,
            None => { reply.error(Errno::EINVAL); return; }
        };
        let src_parent = match self.inodes.resolve(parent.into()) {
            Some(p) => p,
            None => { reply.error(Errno::ENOENT); return; }
        };
        let dst_parent = match self.inodes.resolve(newparent.into()) {
            Some(p) => p,
            None => { reply.error(Errno::ENOENT); return; }
        };
        let src = Self::join_path(&src_parent, src_name);
        let dst = Self::join_path(&dst_parent, dst_name);

        match self.rt.block_on(self.vfs.rename(&src, &dst)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(to_errno(e)),
        }
    }

    fn statfs(&self, _req: &Request, _ino: fuser::INodeNo, reply: ReplyStatfs) {
        reply.statfs(1_000_000_000, 500_000_000, 500_000_000, 1_000_000, 500_000, 4096, 255, 0);
    }

    fn access(&self, _req: &Request, _ino: fuser::INodeNo, _mask: fuser::AccessFlags, reply: ReplyEmpty) {
        reply.ok();
    }
}
