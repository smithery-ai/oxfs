---
title: Fork fuser, add io_uring + passthrough FUSE transport
status: planned
priority: high
depends_on: none
estimated_effort: multi-session
kernel_requirement: "Linux >= 6.14"
---

# Fork fuser for FUSE ABI 7.40+

## Why

fuser negotiates ABI 7.31. Kernel is at 7.45. The gap locks out:
- **io_uring transport** (7.42): zero-copy SQE/CQE ring replaces read()/write() on /dev/fuse
- **Passthrough mode** (7.40): kernel bypasses userspace for I/O on a backing file
- STATX, request timeouts, id-mapped mounts, parallel direct writes

Nobody else has this in Rust. Mountpoint-S3 (AWS) is stuck at the same ceiling.

## io_uring protocol details

Uses `IORING_OP_URING_CMD` with 128-byte SQEs. Two opcodes:
- `FUSE_IO_URING_CMD_REGISTER` (1): register header+payload buffer pair
- `FUSE_IO_URING_CMD_COMMIT_AND_FETCH` (2): atomically commit response + fetch next request

Init handshake: set `FUSE_OVER_IO_URING` (bit 41, `1ULL << 41`) in FUSE_INIT.
Per-CPU queues created automatically. Ring entry states cycle:
`FRRS_AVAILABLE -> FRRS_FUSE_REQ -> FRRS_USERSPACE -> FRRS_COMMIT -> FRRS_AVAILABLE`

Kernel source: `fs/fuse/dev_uring.c`, `fs/fuse/dev_uring_i.h`

## Ecosystem state (as of May 2026)

- **libfuse**: io_uring merged (PR #1177, April 2025). Production-ready.
- **fuser**: no support. PR #510 added constants, no transport impl.
- **io-uring crate** (tokio-rs/io-uring): PR #388 adds `Entry128::with_cmd` for FUSE.
- **virtiofsd**: uses virtio transport, not io_uring FUSE.

## Architecture

Fork fuser into oxfs workspace as `oxfs-fuse-sys` or similar.
Keep existing `Filesystem` trait API. Add second session mode:

```
Session::new(...)         // traditional /dev/fuse (macOS + older Linux)
Session::new_uring(...)   // io_uring transport (Linux 6.14+)
```

Runtime detection:
```
Container boots -> oxfs checks kernel version
  >= 6.14 -> io_uring transport
  < 6.14  -> traditional /dev/fuse fallback
```

## Implementation steps

1. Vendor fuser 0.17 source into `crates/oxfs-fuse-sys/`
2. Bump ABI negotiation from 7.31 to 7.42+
3. Add `FUSE_OVER_IO_URING` init flag
4. Implement `UringSession` using `io-uring` crate (needs Entry128 support)
5. Wire `REGISTER` + `COMMIT_AND_FETCH` loop
6. Add passthrough mode (7.40): `FOPEN_PASSTHROUGH` + `FUSE_DEV_IOC_BACKING_OPEN`
7. Benchmark against traditional path

## Testing

- colima VM is kernel 6.8 (too old). Need 6.14+.
- Options: cloud VM (Ubuntu 25.04+, Fedora 42+), or build now / test later.
- Traditional path keeps working locally and on CF containers today.
- CF will eventually run 6.14+; oxfs auto-upgrades when they do.

## Key files to read

- `torvalds/linux: fs/fuse/dev_uring.c`
- `torvalds/linux: include/uapi/linux/fuse.h` (FUSE_KERNEL_MINOR_VERSION)
- `cberner/fuser: src/session.rs` (current session loop)
- `cberner/fuser: src/mnt/fuse_pure.rs` (pure-Rust mount)
- `libfuse/libfuse: lib/fuse_loop_mt.c` (reference io_uring impl)
