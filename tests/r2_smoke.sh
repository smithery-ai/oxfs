#!/bin/bash
set -euo pipefail

# R2 Integration Smoke Test
# Requires: R2_ACCOUNT_ID, R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY in env
# Usage: ./tests/r2_smoke.sh [oxfs-binary-path]

OXFS="${1:-./target/release/oxfs}"
BUCKET="connector-factory-workspaces"
PREFIX="test-oxfs-$(date +%s)"
MOUNT="/tmp/oxfs-r2-test"
DATA="/tmp/oxfs-r2-data"
META="/tmp/oxfs-r2-meta.db"
ENDPOINT="https://${R2_ACCOUNT_ID}.r2.cloudflarestorage.com"

PASSED=0
FAILED=0
TOTAL=0

assert() {
    TOTAL=$((TOTAL + 1))
    local desc="$1"; shift
    if "$@" >/dev/null 2>&1; then
        echo "  PASS: $desc"
        PASSED=$((PASSED + 1))
    else
        echo "  FAIL: $desc"
        FAILED=$((FAILED + 1))
        return 1
    fi
}

assert_eq() {
    TOTAL=$((TOTAL + 1))
    local desc="$1" expected="$2" actual="$3"
    if [ "$expected" = "$actual" ]; then
        echo "  PASS: $desc"
        PASSED=$((PASSED + 1))
    else
        echo "  FAIL: $desc (expected '$expected', got '$actual')"
        FAILED=$((FAILED + 1))
        return 1
    fi
}

cleanup() {
    umount "$MOUNT" 2>/dev/null || fusermount -u "$MOUNT" 2>/dev/null || true
    sleep 1
    rm -rf "$MOUNT" "$DATA" "$META"
}
trap cleanup EXIT

# Validate env
for var in R2_ACCOUNT_ID R2_ACCESS_KEY_ID R2_SECRET_ACCESS_KEY; do
    if [ -z "${!var:-}" ]; then
        echo "ERROR: $var not set"
        exit 1
    fi
done

mount_oxfs() {
    local prefix="${1:-$PREFIX}"
    local meta="${2:-$META}"
    rm -rf "$MOUNT" "$DATA"
    mkdir -p "$MOUNT" "$DATA"
    AWS_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID" \
    AWS_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY" \
    "$OXFS" mount "$MOUNT" \
        --backend s3 \
        --bucket "$BUCKET" \
        --region auto \
        --endpoint "$ENDPOINT/$prefix" \
        --meta-db "$meta" \
        --cache-mem-mb 64 &
    OXFS_PID=$!
    sleep 3
}

unmount_oxfs() {
    umount "$MOUNT" 2>/dev/null || fusermount -u "$MOUNT" 2>/dev/null || true
    wait "$OXFS_PID" 2>/dev/null || true
    sleep 1
}

# ============================================================
echo "=== Phase 1: Mount + Basic I/O ==="
mount_oxfs

assert "mount accessible" ls "$MOUNT"

echo "hello oxfs r2" > "$MOUNT/hello.txt"
CONTENT=$(cat "$MOUNT/hello.txt")
assert_eq "write+read content" "hello oxfs r2" "$CONTENT"

dd if=/dev/urandom of="$MOUNT/big.bin" bs=1024 count=10240 2>/dev/null
MD5_W=$(md5sum "$MOUNT/big.bin" | cut -d' ' -f1)
MD5_R=$(md5sum "$MOUNT/big.bin" | cut -d' ' -f1)
assert_eq "10MB write+read md5" "$MD5_W" "$MD5_R"

SIZE=$(stat -c%s "$MOUNT/big.bin" 2>/dev/null || stat -f%z "$MOUNT/big.bin")
assert_eq "10MB file size" "10485760" "$SIZE"

rm "$MOUNT/hello.txt" "$MOUNT/big.bin"

# ============================================================
echo "=== Phase 2: Directory Operations ==="

mkdir -p "$MOUNT/a/b/c"
assert "nested mkdir" test -d "$MOUNT/a/b/c"

echo "deep" > "$MOUNT/a/b/c/deep.txt"
assert "file in nested dir" test -f "$MOUNT/a/b/c/deep.txt"

mv "$MOUNT/a/b/c/deep.txt" "$MOUNT/a/moved.txt"
assert "rename: old gone" test ! -f "$MOUNT/a/b/c/deep.txt"
assert "rename: new exists" test -f "$MOUNT/a/moved.txt"

rm "$MOUNT/a/moved.txt"
rmdir "$MOUNT/a/b/c" "$MOUNT/a/b" "$MOUNT/a"
assert "rmdir: tree gone" test ! -d "$MOUNT/a"

mkdir "$MOUNT/tree"
for i in $(seq 1 10); do echo "f$i" > "$MOUNT/tree/f$i.txt"; done
rm -rf "$MOUNT/tree"
assert "rm -rf: tree gone" test ! -d "$MOUNT/tree"

# ============================================================
echo "=== Phase 3: Metadata Integrity ==="

echo "meta" > "$MOUNT/meta.txt"
chmod 755 "$MOUNT/meta.txt"
MODE=$(stat -c%a "$MOUNT/meta.txt" 2>/dev/null || stat -f%Lp "$MOUNT/meta.txt")
assert_eq "chmod 755" "755" "$MODE"

ln -s meta.txt "$MOUNT/link.txt"
TARGET=$(readlink "$MOUNT/link.txt")
assert_eq "symlink target" "meta.txt" "$TARGET"

MTIME1=$(stat -c%Y "$MOUNT/meta.txt" 2>/dev/null || stat -f%m "$MOUNT/meta.txt")
sleep 1
echo "updated" >> "$MOUNT/meta.txt"
MTIME2=$(stat -c%Y "$MOUNT/meta.txt" 2>/dev/null || stat -f%m "$MOUNT/meta.txt")
assert "mtime updated" test "$MTIME2" -gt "$MTIME1"

ln "$MOUNT/meta.txt" "$MOUNT/hard.txt"
NLINK=$(stat -c%h "$MOUNT/meta.txt" 2>/dev/null || stat -f%l "$MOUNT/meta.txt")
assert_eq "hard link nlink=2" "2" "$NLINK"
rm "$MOUNT/hard.txt"
NLINK=$(stat -c%h "$MOUNT/meta.txt" 2>/dev/null || stat -f%l "$MOUNT/meta.txt")
assert_eq "after unlink nlink=1" "1" "$NLINK"

rm "$MOUNT/meta.txt" "$MOUNT/link.txt"

# ============================================================
echo "=== Phase 4: Concurrent Access ==="

mkdir "$MOUNT/conc"
for t in $(seq 1 4); do
    (for i in $(seq 1 50); do echo "t${t}f${i}" > "$MOUNT/conc/t${t}_f${i}.txt"; done) &
done
wait
COUNT=$(ls "$MOUNT/conc/" | wc -l | tr -d ' ')
assert_eq "concurrent 4x50 files" "200" "$COUNT"

dd if=/dev/urandom of="$MOUNT/conc/shared.bin" bs=1024 count=1024 2>/dev/null
MD5_ORIG=$(md5sum "$MOUNT/conc/shared.bin" | cut -d' ' -f1)
for t in $(seq 1 4); do
    (MD5=$(md5sum "$MOUNT/conc/shared.bin" | cut -d' ' -f1); [ "$MD5" = "$MD5_ORIG" ]) &
done
wait
assert "concurrent reads same md5" true

rm -rf "$MOUNT/conc"

# ============================================================
echo "=== Phase 5: Prefix Isolation ==="

unmount_oxfs

mount_oxfs "${PREFIX}-iso-a" "/tmp/oxfs-meta-a.db"
echo "only in A" > "$MOUNT/a_file.txt"
assert "prefix A: file exists" test -f "$MOUNT/a_file.txt"
unmount_oxfs

mount_oxfs "${PREFIX}-iso-b" "/tmp/oxfs-meta-b.db"
assert "prefix B: file absent" test ! -f "$MOUNT/a_file.txt"
unmount_oxfs

mount_oxfs "${PREFIX}-iso-a" "/tmp/oxfs-meta-a.db"
CONTENT=$(cat "$MOUNT/a_file.txt")
assert_eq "prefix A: file survives remount" "only in A" "$CONTENT"
rm "$MOUNT/a_file.txt"
unmount_oxfs

rm -f /tmp/oxfs-meta-a.db /tmp/oxfs-meta-b.db

# ============================================================
echo "=== Phase 6: Crash Recovery ==="

mount_oxfs "${PREFIX}-crash"
for i in $(seq 1 100); do echo "file $i content" > "$MOUNT/crash_$i.txt"; done
kill -9 "$OXFS_PID" 2>/dev/null || true
sleep 2

mount_oxfs "${PREFIX}-crash" "/tmp/oxfs-crash-meta2.db"
RECOVERED=0
for i in $(seq 1 100); do
    if [ -f "$MOUNT/crash_$i.txt" ]; then
        CONTENT=$(cat "$MOUNT/crash_$i.txt")
        if [ "$CONTENT" = "file $i content" ]; then
            RECOVERED=$((RECOVERED + 1))
        fi
    fi
done
assert_eq "crash recovery: 100/100 files" "100" "$RECOVERED"
rm -f "$MOUNT"/crash_*.txt
unmount_oxfs
rm -f /tmp/oxfs-crash-meta2.db

# ============================================================
echo "=== Phase 7: Flamecast Workload ==="

mount_oxfs "${PREFIX}-workload"

# Simulate SDK workspace
export HOME="$MOUNT"
mkdir -p "$MOUNT/.claude/projects/test"
echo '{"session":"test","turn":1}' > "$MOUNT/.claude/projects/test/session.jsonl"
CONTENT=$(cat "$MOUNT/.claude/projects/test/session.jsonl")
assert_eq "SDK jsonl write+read" '{"session":"test","turn":1}' "$CONTENT"

# Git clone
git clone --depth 1 https://github.com/expressjs/express.git "$MOUNT/express" 2>/dev/null
assert "git clone: .git exists" test -d "$MOUNT/express/.git"
assert "git log works" git -C "$MOUNT/express" log --oneline -1

rm -rf "$MOUNT/express" "$MOUNT/.claude"
unmount_oxfs

# ============================================================
echo ""
echo "================================"
echo "Results: $PASSED/$TOTAL passed, $FAILED failed"
echo "================================"

[ "$FAILED" -eq 0 ] && exit 0 || exit 1
