---
title: R2 Integration Test Plan
target: replace tigrisfs in flamecast containers
---

# R2 Integration Test Plan

## Environment

```
Endpoint: https://{R2_ACCOUNT_ID}.r2.cloudflarestorage.com
Bucket:   connector-factory-workspaces
Prefix:   test-oxfs-{timestamp}/
Auth:     AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY (from Infisical)
```

## Test script: tests/r2_smoke.sh

Run against a real R2 bucket. Each test is an assertion that prints
PASS/FAIL. Exit 1 on first failure.

### Phase 1: Mount + basic I/O

1. Mount oxfs with S3 backend pointing at R2
2. Assert: mount point is accessible (ls returns 0)
3. Write a file, read it back, assert content matches
4. Write a 10MB file, md5sum it, read back, assert md5 matches
5. Assert: file shows correct size via stat

### Phase 2: Directory operations

6. mkdir nested dirs (3 levels), assert they exist
7. Create files in nested dirs, assert readdir returns them
8. Rename a file across directories, assert old path gone, new path exists
9. rmdir (must be empty), assert gone
10. rm -rf a tree with files, assert all gone

### Phase 3: Metadata integrity

11. chmod 755 a file, stat it, assert mode is 755
12. Create a symlink, readlink it, assert target matches
13. Write a file, sleep 1s, write again, assert mtime changed
14. Hard link a file, assert nlink=2, unlink one, assert nlink=1

### Phase 4: Concurrent access

15. 4 threads write 50 files each (200 total), assert all 200 exist
16. 4 threads read the same large file simultaneously, assert all get same md5

### Phase 5: Session prefix isolation

17. Mount with prefix A, write file X
18. Mount with prefix B, assert file X does not exist
19. Unmount B, re-mount A, assert file X still exists

### Phase 6: Crash recovery

20. Write 100 files, kill -9 the oxfs process
21. Re-mount same prefix, assert all 100 files readable with correct content

### Phase 7: Flamecast workload simulation

22. Mount, set HOME=/workspace, run: git clone a small repo
23. Assert .git/ exists, git log works
24. Write a file to simulate SDK jsonl output, read it back
25. Create .claude/projects/ directory tree, write session state, verify
