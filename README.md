# oxfs

FUSE filesystem backed by any object store. Pure Rust.

Mount S3, R2, GCS, Tigris, or local disk as a POSIX filesystem with a
pluggable metadata engine (redb or SQLite) and tiered caching.

## Quick start

```bash
# prerequisites
brew install macfuse   # macOS (reboot after install)
cargo build --release

# mount with local FS backend
mkdir -p /tmp/oxfs-mount /tmp/oxfs-data
./target/release/oxfs mount /tmp/oxfs-mount \
  --backend fs --root /tmp/oxfs-data

# mount with S3
./target/release/oxfs mount /tmp/oxfs-mount \
  --backend s3 --bucket my-bucket --region us-east-1

# unmount
umount /tmp/oxfs-mount
```

## Options

```
oxfs mount <mountpoint>
  --backend <fs|s3>           Storage backend (default: fs)
  --root <path>               Root directory for fs backend
  --bucket <name>             S3 bucket name
  --region <region>           S3 region
  --endpoint <url>            S3-compatible endpoint (R2, Tigris, MinIO)
  --meta-db <path>            Metadata database path (default: oxfs.db)
  --meta-backend <redb|sqlite> Metadata engine (default: redb)
  --cache-mem-mb <N>          L1 memory cache size (default: 256)
  --cache-disk-path <path>    L2 disk cache directory
  --cache-disk-mb <N>         L2 disk cache max size (default: 1024)
  --default-permissions       Kernel-enforced permission checks
```

## Architecture

```
FUSE (fuser) ── VFS ── Cache (moka L1 + disk L2) ── Data (OpenDAL)
                 │
            Metadata (redb or SQLite)
```

Five crates:

| Crate | Role |
|-------|------|
| `oxfs-meta` | MetaEngine trait + redb/SQLite impls |
| `oxfs-data` | OpenDAL wrapper for slice I/O |
| `oxfs-vfs` | Inode mgmt, read/write, cache, prefetch |
| `oxfs-fuse` | fuser Filesystem impl |
| `oxfs` | CLI binary |

## POSIX compliance

99.6% on pjdfstest (8755/8789 tests passing on Linux). Remaining
failures are POSIX edge cases (ENAMETOOLONG, sticky bit enforcement).

Supported: regular files, directories, symlinks, hard links, FIFOs,
sockets, block/char device nodes, chmod, chown, rename, truncate,
utimensat with nanosecond precision.

## Running tests

```bash
cargo test

# POSIX compliance (Linux, requires Docker)
docker build -f Dockerfile.test -t oxfs-test .
docker run --rm --privileged oxfs-test '
  mkdir -p /tmp/m /tmp/d
  /app/target/release/oxfs mount /tmp/m --backend fs --root /tmp/d \
    --meta-db /tmp/t.db --default-permissions 2>/dev/null &
  sleep 2 && cd /tmp/m
  prove -r /opt/pjdfstest/tests/
'
```

## License

MIT OR Apache-2.0
