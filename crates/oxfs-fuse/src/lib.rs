use std::ffi::OsStr;
use std::sync::Arc;
use std::time::Duration;

use fuser::{
    Errno, FileAttr, FileType as FuseFileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyWrite, Request,
};
use oxfs_cache::CacheLayer;
use oxfs_meta::{FileType, MetaEngine};
use oxfs_vfs::Vfs;

const TTL: Duration = Duration::from_secs(1);

pub struct OxfsFuse<M: MetaEngine, C: CacheLayer> {
    vfs: Arc<Vfs<M, C>>,
    rt: tokio::runtime::Handle,
}

impl<M: MetaEngine, C: CacheLayer> OxfsFuse<M, C> {
    pub fn new(vfs: Arc<Vfs<M, C>>, rt: tokio::runtime::Handle) -> Self {
        Self { vfs, rt }
    }
}

fn to_fuse_file_type(ft: FileType) -> FuseFileType {
    match ft {
        FileType::Regular => FuseFileType::RegularFile,
        FileType::Directory => FuseFileType::Directory,
        FileType::Symlink => FuseFileType::Symlink,
    }
}

fn to_file_attr(attr: &oxfs_meta::InodeAttr) -> FileAttr {
    FileAttr {
        ino: fuser::INodeNo(attr.inode),
        size: attr.size,
        blocks: attr.blocks,
        atime: attr.atime,
        mtime: attr.mtime,
        ctime: attr.ctime,
        crtime: attr.ctime,
        kind: to_fuse_file_type(attr.kind),
        perm: attr.mode as u16,
        nlink: attr.nlink,
        uid: attr.uid,
        gid: attr.gid,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

fn vfs_err_to_errno(e: &oxfs_vfs::VfsError) -> Errno {
    match e {
        oxfs_vfs::VfsError::Meta(oxfs_meta::MetaError::NotFound) => Errno::ENOENT,
        oxfs_vfs::VfsError::Meta(oxfs_meta::MetaError::AlreadyExists) => Errno::EEXIST,
        oxfs_vfs::VfsError::Meta(oxfs_meta::MetaError::NotDirectory) => Errno::ENOTDIR,
        oxfs_vfs::VfsError::Meta(oxfs_meta::MetaError::NotEmpty) => Errno::ENOTEMPTY,
        oxfs_vfs::VfsError::Meta(oxfs_meta::MetaError::IsDirectory) => Errno::EISDIR,
        oxfs_vfs::VfsError::Invalid(_) => Errno::EINVAL,
        _ => Errno::EIO,
    }
}

impl<M: MetaEngine + 'static, C: CacheLayer + 'static> Filesystem for OxfsFuse<M, C> {
    fn init(
        &mut self,
        _req: &Request,
        _config: &mut fuser::KernelConfig,
    ) -> std::io::Result<()> {
        self.rt
            .block_on(self.vfs.init())
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(())
    }

    fn getattr(
        &self,
        _req: &Request,
        ino: fuser::INodeNo,
        _fh: Option<fuser::FileHandle>,
        reply: ReplyAttr,
    ) {
        match self.rt.block_on(self.vfs.get_attr(ino.into())) {
            Ok(attr) => reply.attr(&TTL, &to_file_attr(&attr)),
            Err(e) => reply.error(vfs_err_to_errno(&e)),
        }
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
        match self.rt.block_on(self.vfs.lookup(parent.into(), name)) {
            Ok(attr) => reply.entry(&TTL, &to_file_attr(&attr), fuser::Generation(0)),
            Err(e) => reply.error(vfs_err_to_errno(&e)),
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
        let ino_u64: u64 = ino.into();
        let attr = match self.rt.block_on(self.vfs.get_attr(ino_u64)) {
            Ok(a) => a,
            Err(e) => {
                reply.error(vfs_err_to_errno(&e));
                return;
            }
        };

        let entries = match self.rt.block_on(self.vfs.readdir(ino_u64)) {
            Ok(e) => e,
            Err(e) => {
                reply.error(vfs_err_to_errno(&e));
                return;
            }
        };

        let mut full = vec![
            (attr.inode, FuseFileType::Directory, ".".to_string()),
            (attr.inode, FuseFileType::Directory, "..".to_string()),
        ];

        for entry in entries {
            full.push((entry.inode, to_fuse_file_type(entry.kind), entry.name));
        }

        for (i, (ino, kind, name)) in full.into_iter().enumerate().skip(offset as usize) {
            if reply.add(fuser::INodeNo(ino), (i + 1) as u64, kind, &name) {
                break;
            }
        }
        reply.ok();
    }

    fn read(
        &self,
        _req: &Request,
        ino: fuser::INodeNo,
        _fh: fuser::FileHandle,
        offset: u64,
        size: u32,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyData,
    ) {
        match self.rt.block_on(self.vfs.read(ino.into(), offset, size)) {
            Ok(data) => reply.data(&data),
            Err(e) => reply.error(vfs_err_to_errno(&e)),
        }
    }

    fn mkdir(
        &self,
        req: &Request,
        parent: fuser::INodeNo,
        name: &OsStr,
        mode: u32,
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
        match self.rt.block_on(self.vfs.create(
            parent.into(),
            name,
            FileType::Directory,
            mode,
            req.uid(),
            req.gid(),
        )) {
            Ok(attr) => reply.entry(&TTL, &to_file_attr(&attr), fuser::Generation(0)),
            Err(e) => reply.error(vfs_err_to_errno(&e)),
        }
    }

    fn create(
        &self,
        req: &Request,
        parent: fuser::INodeNo,
        name: &OsStr,
        mode: u32,
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
        match self.rt.block_on(self.vfs.create(
            parent.into(),
            name,
            FileType::Regular,
            mode,
            req.uid(),
            req.gid(),
        )) {
            Ok(attr) => {
                let fa = to_file_attr(&attr);
                reply.created(&TTL, &fa, fuser::Generation(0), fuser::FileHandle(attr.inode), fuser::FopenFlags::empty());
            }
            Err(e) => reply.error(vfs_err_to_errno(&e)),
        }
    }

    fn write(
        &self,
        _req: &Request,
        ino: fuser::INodeNo,
        _fh: fuser::FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: fuser::WriteFlags,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyWrite,
    ) {
        match self.rt.block_on(self.vfs.write(ino.into(), offset, data)) {
            Ok(written) => reply.written(written),
            Err(e) => reply.error(vfs_err_to_errno(&e)),
        }
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
        match self.rt.block_on(self.vfs.unlink(parent.into(), name)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(vfs_err_to_errno(&e)),
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
        match self.rt.block_on(self.vfs.unlink(parent.into(), name)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(vfs_err_to_errno(&e)),
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
        let src = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(Errno::EINVAL);
                return;
            }
        };
        let dst = match newname.to_str() {
            Some(n) => n,
            None => {
                reply.error(Errno::EINVAL);
                return;
            }
        };
        match self
            .rt
            .block_on(self.vfs.rename(parent.into(), src, newparent.into(), dst))
        {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(vfs_err_to_errno(&e)),
        }
    }
}
