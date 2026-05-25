#!/bin/bash
set -euo pipefail

# Quick oxfs vs tigrisfs benchmark (sized for R2 latency)
# Usage: ./bench/quick.sh oxfs|tigris

BACKEND="${1:-oxfs}"
MOUNT="/tmp/oxfs-qbench"
META="/tmp/oxfs-qbench.db"
RESULTS_DIR="$(dirname "$0")/results"
ITERATIONS=3

# R2 config
if [ -z "${R2_ACCOUNT_ID:-}" ]; then
    FLAMECAST_DIR="${FLAMECAST_DIR:-$HOME/Documents/github/smithery/flamecast-agents}"
    eval "$(cd "$FLAMECAST_DIR" && \
        infisical run --env=prod --path=/apps/flamecast -- \
        bash -c 'echo "export R2_ACCOUNT_ID=$R2_ACCOUNT_ID"; echo "export R2_ACCESS_KEY_ID=$R2_ACCESS_KEY_ID"; echo "export R2_SECRET_ACCESS_KEY=$R2_SECRET_ACCESS_KEY"')"
fi

ENDPOINT="https://${R2_ACCOUNT_ID}.r2.cloudflarestorage.com"
BUCKET="connector-factory-workspaces"
PREFIX="qbench-${BACKEND}-$(date +%s)"

mkdir -p "$RESULTS_DIR"

cleanup() {
    pkill -f "oxfs mount.*qbench" 2>/dev/null || true
    pkill -f "tigrisfs.*qbench" 2>/dev/null || true
    umount "$MOUNT" 2>/dev/null || fusermount -u "$MOUNT" 2>/dev/null || true
    sleep 1
    rm -rf "$MOUNT" "$META"
}
trap cleanup EXIT

cleanup
mkdir -p "$MOUNT"

export PATH="$HOME/bin:$PATH"

if [ "$BACKEND" = "oxfs" ]; then
    AWS_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID" \
    AWS_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY" \
    RUST_LOG=warn \
    ./target/release/oxfs mount "$MOUNT" \
        --backend s3 --bucket "$BUCKET" --region auto \
        --endpoint "$ENDPOINT" --prefix "$PREFIX" \
        --meta-db "$META" --cache-mem-mb 256 &
    sleep 3
elif [ "$BACKEND" = "tigris" ]; then
    AWS_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID" \
    AWS_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY" \
    AWS_REGION=auto \
    AWS_S3_PATH_STYLE=true \
    tigrisfs --endpoint "$ENDPOINT" -f --enable-specials \
        "${BUCKET}:${PREFIX}" "$MOUNT" &
    sleep 5
fi

echo "test" > "$MOUNT/.verify" && cat "$MOUNT/.verify" >/dev/null && rm "$MOUNT/.verify"
echo "=== $BACKEND benchmark (prefix: $PREFIX) ==="

time_ms() {
    python3 -c "import time; print(int(time.time()*1000))"
}

run_bench() {
    local name="$1"; shift
    local times=()
    for i in $(seq 1 $ITERATIONS); do
        local s=$(time_ms)
        eval "$@" >/dev/null 2>&1
        local e=$(time_ms)
        times+=($((e - s)))
    done
    IFS=$'\n' sorted=($(sort -n <<<"${times[*]}")); unset IFS
    local med=${sorted[$((ITERATIONS / 2))]}
    printf "  %-30s %6dms  (runs: %s)\n" "$name" "$med" "${times[*]}"
    echo "\"$name\": $med," >> "$RESULTS_DIR/${BACKEND}.json"
}

echo "{" > "$RESULTS_DIR/${BACKEND}.json"

# 1. Single file write+read
run_bench "single_file_roundtrip" '
echo "hello benchmark" > "$MOUNT/single.txt"
cat "$MOUNT/single.txt" >/dev/null
rm "$MOUNT/single.txt"
'

# 2. Create 10 small files
run_bench "create_10_files" '
for i in $(seq 1 10); do echo "f$i" > "$MOUNT/c$i.txt"; done
'
rm -f "$MOUNT"/c*.txt 2>/dev/null

# 3. Read 10 small files
for i in $(seq 1 10); do echo "r$i" > "$MOUNT/r$i.txt"; done
run_bench "read_10_files" '
for i in $(seq 1 10); do cat "$MOUNT/r$i.txt" >/dev/null; done
'
rm -f "$MOUNT"/r*.txt

# 4. Write 256KB file
run_bench "write_256k" '
dd if=/dev/urandom of="$MOUNT/med.bin" bs=65536 count=4 2>/dev/null
rm "$MOUNT/med.bin"
'

# 5. Stat 10 files
for i in $(seq 1 10); do echo "s" > "$MOUNT/s$i.txt"; done
run_bench "stat_10" '
for i in $(seq 1 10); do stat "$MOUNT/s$i.txt" >/dev/null 2>&1; done
'
rm -f "$MOUNT"/s*.txt

# 6. mkdir + rmdir
run_bench "mkdir_rmdir_5" '
for i in $(seq 1 5); do mkdir "$MOUNT/d$i"; done
for i in $(seq 1 5); do rmdir "$MOUNT/d$i"; done
'

# 7. Rename 5 files
for i in $(seq 1 5); do echo "mv" > "$MOUNT/mv_s$i.txt"; done
run_bench "rename_5" '
for i in $(seq 1 5); do mv "$MOUNT/mv_s$i.txt" "$MOUNT/mv_d$i.txt"; done
for i in $(seq 1 5); do mv "$MOUNT/mv_d$i.txt" "$MOUNT/mv_s$i.txt"; done
'
rm -f "$MOUNT"/mv_*.txt

# 8. Symlink create+read
run_bench "symlink_roundtrip" '
echo "target" > "$MOUNT/sym_tgt.txt"
ln -s sym_tgt.txt "$MOUNT/sym_lnk.txt"
readlink "$MOUNT/sym_lnk.txt" >/dev/null
cat "$MOUNT/sym_lnk.txt" >/dev/null
rm "$MOUNT/sym_lnk.txt" "$MOUNT/sym_tgt.txt"
'

sed -i.bak '$ s/,$//' "$RESULTS_DIR/${BACKEND}.json" 2>/dev/null || \
    sed -i '' '$ s/,$//' "$RESULTS_DIR/${BACKEND}.json"
echo "}" >> "$RESULTS_DIR/${BACKEND}.json"
rm -f "$RESULTS_DIR/${BACKEND}.json.bak"

echo ""
echo "=== Results: $RESULTS_DIR/${BACKEND}.json ==="
cat "$RESULTS_DIR/${BACKEND}.json"
