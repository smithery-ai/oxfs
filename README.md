# oxfs

FUSE filesystem backed by any object store, written in Rust.

Built on [fuser](https://github.com/cberner/fuser), [redb](https://github.com/cberner/redb), and [Apache OpenDAL](https://github.com/apache/opendal). Faster than tigrisfs, geesefs, and juicefs. 50+ storage backends. 99.6% POSIX compliance. Crash-safe writes with WAL.

## Benchmarks

| Metric | oxfs | geesefs | juicefs | vs next best |
|--------|------|---------|---------|--------------|
| single file roundtrip | **13ms** | 21ms | 19ms | 1.5x |
| create 10 files | **14ms** | 907ms | 20ms | 1.4x |
| read 10 files | **20ms** | 38ms | 28ms | 1.4x |
| write 256KB | **16ms** | 25ms | 21ms | 1.3x |
| stat 10 files | **23ms** | 28ms | 34ms | 1.2x |
| mkdir+rmdir 5 dirs | **25ms** | 32ms | 35ms | 1.3x |
| rename 5 files | **25ms** | 38ms | 34ms | 1.4x |
| symlink roundtrip | **19ms** | 23ms | 22ms | 1.2x |

Reproduce: `docker build -f bench/Dockerfile -t oxfs-bench . && docker run --rm --privileged -e R2_ACCOUNT_ID=... -e R2_ACCESS_KEY_ID=... -e R2_SECRET_ACCESS_KEY=... oxfs-bench`

## Quick start

```bash
cargo build --release

# Local filesystem
./target/release/oxfs mount /mnt/oxfs --backend fs --root /tmp/oxfs-data

# S3 / R2
AWS_ACCESS_KEY_ID=... AWS_SECRET_ACCESS_KEY=... \
./target/release/oxfs mount /mnt/oxfs \
  --backend s3 --bucket my-bucket --endpoint https://acct.r2.cloudflarestorage.com

umount /mnt/oxfs
```

macOS: `brew install --cask macfuse` first (reboot required).

## Storage backends

oxfs uses [Apache OpenDAL](https://github.com/apache/opendal). Any S3-compatible endpoint works out of the box.

| Backend | Example |
|---------|---------|
| AWS S3 | `--backend s3 --bucket name --region us-east-1` |
| Cloudflare R2 | `--backend s3 --endpoint https://acct.r2.cloudflarestorage.com` |
| GCS | `--backend s3 --endpoint https://storage.googleapis.com` |
| MinIO | `--backend s3 --endpoint http://localhost:9000` |
| Tigris | `--backend s3 --endpoint https://fly.storage.tigris.dev` |
| Local disk | `--backend fs --root /path` |

`--prefix` scopes all keys within the bucket for per-session isolation.


## POSIX compliance

99.6% on pjdfstest (8755/8789 tests, Linux). Supports regular files,
directories, symlinks, hard links, FIFOs, sockets, block/char device
nodes, chmod, chown, rename, truncate, nanosecond timestamps.

## Configuration

```
oxfs mount <mountpoint>
  --backend <fs|s3>              Storage backend (default: fs)
  --root <path>                  Root dir for fs backend
  --bucket <name>                S3 bucket
  --region <region>              S3 region
  --endpoint <url>               S3-compatible endpoint
  --prefix <path>                Key prefix within bucket
  --meta-db <path>               Metadata database (default: oxfs.db)
  --meta-backend <redb|sqlite>   Metadata engine (default: redb)
  --cache-mem-mb <N>             L1 memory cache MB (default: 256)
  --cache-disk-path <path>       L2 disk cache directory
  --cache-disk-mb <N>            L2 disk cache MB (default: 1024)
  --wal-path <path>              Write-ahead log (crash recovery)
  --default-permissions          Kernel-enforced permission checks
```

## Architecture

```
FUSE (fuser) -> VFS -> Cache (moka L1 + disk L2 + WAL) -> Data (OpenDAL)
                 |
              Metadata (redb or SQLite)
```

| Crate | Role |
|-------|------|
| oxfs-meta | MetaEngine trait, [redb](https://github.com/cberner/redb) and SQLite impls |
| oxfs-data | [OpenDAL](https://github.com/apache/opendal) wrapper for slice I/O |
| oxfs-vfs | Inode mgmt, cache, prefetch, compaction |
| oxfs-fuse | [fuser](https://github.com/cberner/fuser) Filesystem impl |
| oxfs | CLI binary |

## License

MIT OR Apache-2.0
