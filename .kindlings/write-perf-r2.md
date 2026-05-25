---
title: R2 write performance bottleneck analysis
status: active
priority: high
---

# R2 Write Performance

## The problem

Each file close() triggers a synchronous flush to R2. R2 PUT latency
is ~1.5s per request. Creating 100 small files = 100 sequential
close() calls = 150 seconds.

## What we've done

1. write-back buffering: write() stores to L1 only, instant
2. writeback_cache mount option: kernel coalesces writes
3. coalesce-then-flush: merge N slices per chunk into 1 PUT
4. parallel flush: multiple PUTs within one close() run concurrently

## What's left

The bottleneck is now: each file close() does 1 R2 PUT sequentially.
Shell creates files one at a time, so closes are sequential.

## Strategies to explore

### Async close (background flush)
Return from close() immediately, flush in a background task.
Trade-off: lose the POSIX guarantee that data is durable after close.
JuiceFS does this: 5-second background flush timer.

### Batch close coalescing
Accumulate close() calls for a short window (10-50ms), flush all
pending inodes in one parallel batch. Like TCP Nagle for flushes.

### S3 multipart upload
Start upload on first write, stream parts as data arrives. Complete
on close. Eliminates the "upload entire file on close" step.
Only helps for files written sequentially (not random writes).

### Connection pooling / keep-alive
Verify OpenDAL reuses HTTP connections to R2. Each new TCP+TLS
handshake adds ~200ms. Connection pool should amortize this.

## Baseline numbers (macOS -> R2)

- Single PUT (small file): ~1.5s
- Single PUT (1MB): ~1.5s (bottleneck is latency, not bandwidth)
- 10 sequential PUTs: ~15s
- This is comparable to tigrisfs (same R2 endpoint, same latency)
