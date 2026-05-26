# POSIX metadata in flat mode via S3 user metadata

## Problem

Flat mode can't represent symlinks, permission bits, or ownership. Git clone fails on repos with symlinks, and git status shows phantom diffs because the executable bit doesn't persist.

## Approach

Store POSIX metadata as S3 user-metadata headers on each object, matching the GeeseFS/TigrisFS convention for interoperability:

- `x-amz-meta-mode` (file mode bits, including S_IFLNK for symlinks)
- `x-amz-meta-uid`, `x-amz-meta-gid`
- `x-amz-meta-mtime`
- `x-amz-meta---symlink-target` (symlink target path)

Symlinks: zero-byte object with the symlink target metadata key set.

Opt-in via `--enable-perms` and `--enable-mtime` flags (off by default). Objects uploaded outside oxfs get defaults: 644 files, 755 dirs, mounting user's uid/gid.

## Architecture

Lives in CachedOperator. stat cache already stores opendal::Metadata which includes user metadata. Parse metadata keys into a richer struct. write accepts optional POSIX attrs and sets x-amz-meta-* headers. No new layer, no sidecar files, no database.

## Performance constraint

ListObjectsV2 on AWS S3 and R2 does NOT return user metadata. Every stat of a non-cached file requires a HEAD request. Yandex S3 and Tigris return metadata in listings (proprietary extensions).

Mitigation: stat cache absorbs repeat access. Git's pattern (checkout stats everything once, then git status re-stats from cache) works fine. Burst cost is one HEAD per file, amortized by TTL.

## Industry survey

| Project | Metadata location | Symlinks | Tradeoff |
|---------|------------------|----------|----------|
| Mountpoint-S3 (AWS) | Nowhere | Refused | Honest but limiting |
| s3fs-fuse | S3 user-metadata | Yes | HEAD per stat, 15min cache |
| GeeseFS (Yandex) | S3 user-metadata | Yes (opt-in) | Built for Yandex S3 listing ext |
| TigrisFS | S3 user-metadata | Yes (opt-in) | Fork of GeeseFS, Tigris listing ext |
| JuiceFS | External DB | Yes | Full POSIX but requires Redis/PG |

## Also needed for git

- Deferred create: skip the immediate PUT on create, coalesce with first flush
- Recommend --writeback mode for git workloads (single PUT per file)

## Won't solve (need Posix mode)

Atomic rename, hardlinks, nanosecond timestamps, device nodes.
