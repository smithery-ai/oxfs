#!/bin/bash
set -euo pipefail

# oxfs vs tigrisfs benchmark
# Outputs JSON metrics to bench/results/<backend>.json
# Usage: ./bench/bench.sh --backend oxfs|tigris [--prefix <pfx>]

BACKEND="${1:---backend}"
shift || true
BACKEND="${1:-oxfs}"
shift || true

MOUNT="/tmp/oxfs-bench-mount"
DATA="/tmp/oxfs-bench-data"
META="/tmp/oxfs-bench-meta.db"
RESULTS_DIR="$(dirname "$0")/results"
ITERATIONS=5

# R2 config
if [ -z "${R2_ACCOUNT_ID:-}" ]; then
    FLAMECAST_DIR="${FLAMECAST_DIR:-$HOME/Documents/github/smithery/flamecast-agents}"
    eval "$(cd "$FLAMECAST_DIR" && \
        infisical run --env=prod --path=/apps/flamecast -- \
        bash -c 'echo "export R2_ACCOUNT_ID=$R2_ACCOUNT_ID"; echo "export R2_ACCESS_KEY_ID=$R2_ACCESS_KEY_ID"; echo "export R2_SECRET_ACCESS_KEY=$R2_SECRET_ACCESS_KEY"')"
fi

ENDPOINT="https://${R2_ACCOUNT_ID}.r2.cloudflarestorage.com"
BUCKET="connector-factory-workspaces"
PREFIX="bench-$(date +%s)"

mkdir -p "$RESULTS_DIR" "$MOUNT" "$DATA"

cleanup() {
    umount "$MOUNT" 2>/dev/null || fusermount -u "$MOUNT" 2>/dev/null || true
    pkill -f "oxfs mount.*bench" 2>/dev/null || true
    pkill -f "tigrisfs.*bench" 2>/dev/null || true
    sleep 1
}
trap cleanup EXIT

mount_backend() {
    cleanup
    rm -rf "$MOUNT" "$DATA"
    mkdir -p "$MOUNT" "$DATA"
    rm -f "$META"

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
    else
        echo "Unknown backend: $BACKEND"
        exit 1
    fi

    # Verify mount
    echo "test" > "$MOUNT/.bench_verify" && cat "$MOUNT/.bench_verify" >/dev/null && rm "$MOUNT/.bench_verify"
    echo "Mount verified ($BACKEND)"
}

# Timing helper: runs command N times, outputs median in ms
bench() {
    local name="$1"; shift
    local times=()

    for i in $(seq 1 $ITERATIONS); do
        local start=$(python3 -c "import time; print(int(time.time()*1000))")
        eval "$@" >/dev/null 2>&1
        local end=$(python3 -c "import time; print(int(time.time()*1000))")
        local elapsed=$((end - start))
        times+=($elapsed)
    done

    # Sort and take median
    IFS=$'\n' sorted=($(sort -n <<<"${times[*]}")); unset IFS
    local median=${sorted[$((ITERATIONS / 2))]}
    echo "  $name: ${median}ms (runs: ${times[*]})"
    echo "\"$name\": $median," >> "$RESULTS_DIR/${BACKEND}.json"
}

echo "=== oxfs benchmark: $BACKEND ==="
echo "Endpoint: $ENDPOINT"
echo "Prefix:   $PREFIX"
echo ""

mount_backend

echo "{" > "$RESULTS_DIR/${BACKEND}.json"

# ── Benchmark 1: Small file create + write + close ──
echo "--- Small file create (100 files) ---"
bench "small_file_create_100" '
for i in $(seq 1 100); do echo "file $i" > "$MOUNT/sf_$i.txt"; done
'

# ── Benchmark 2: Small file read (100 files) ──
echo "--- Small file read (100 files) ---"
bench "small_file_read_100" '
for i in $(seq 1 100); do cat "$MOUNT/sf_$i.txt" >/dev/null; done
'

# ── Benchmark 3: Small file delete (100 files) ──
echo "--- Small file delete (100 files) ---"
bench "small_file_delete_100" '
for i in $(seq 1 100); do rm "$MOUNT/sf_$i.txt"; done
# Recreate for next iteration
for i in $(seq 1 100); do echo "file $i" > "$MOUNT/sf_$i.txt"; done
'
rm -f "$MOUNT"/sf_*.txt 2>/dev/null

# ── Benchmark 4: Sequential write 1MB (128K blocks) ──
echo "--- Sequential write 1MB ---"
bench "seq_write_1mb" '
dd if=/dev/urandom of="$MOUNT/seq_write.bin" bs=131072 count=8 2>/dev/null
rm -f "$MOUNT/seq_write.bin"
'

# ── Benchmark 5: Sequential read 1MB ──
echo "--- Sequential read 1MB ---"
dd if=/dev/urandom of="$MOUNT/seq_read.bin" bs=131072 count=8 2>/dev/null
bench "seq_read_1mb" '
cat "$MOUNT/seq_read.bin" >/dev/null
'
rm -f "$MOUNT/seq_read.bin"

# ── Benchmark 6: Metadata: stat 100 files ──
echo "--- Stat 100 files ---"
for i in $(seq 1 100); do echo "s" > "$MOUNT/stat_$i.txt"; done
bench "stat_100" '
for i in $(seq 1 100); do stat "$MOUNT/stat_$i.txt" >/dev/null 2>&1; done
'
rm -f "$MOUNT"/stat_*.txt

# ── Benchmark 7: mkdir + rmdir ──
echo "--- mkdir+rmdir 50 dirs ---"
bench "mkdir_rmdir_50" '
for i in $(seq 1 50); do mkdir "$MOUNT/dir_$i"; done
for i in $(seq 1 50); do rmdir "$MOUNT/dir_$i"; done
'

# ── Benchmark 8: readdir (100 entries) ──
echo "--- readdir 100 entries ---"
for i in $(seq 1 100); do echo "r" > "$MOUNT/rd_$i.txt"; done
bench "readdir_100" '
ls "$MOUNT/" >/dev/null
'
rm -f "$MOUNT"/rd_*.txt

# ── Benchmark 9: Rename ──
echo "--- rename 50 files ---"
for i in $(seq 1 50); do echo "mv" > "$MOUNT/ren_src_$i.txt"; done
bench "rename_50" '
for i in $(seq 1 50); do mv "$MOUNT/ren_src_$i.txt" "$MOUNT/ren_dst_$i.txt"; done
for i in $(seq 1 50); do mv "$MOUNT/ren_dst_$i.txt" "$MOUNT/ren_src_$i.txt"; done
'
rm -f "$MOUNT"/ren_*.txt

# ── Benchmark 10: Git clone (real workload) ──
echo "--- git clone express.js ---"
bench "git_clone_express" '
git clone --depth 1 https://github.com/expressjs/express.git "$MOUNT/express_bench" 2>/dev/null
rm -rf "$MOUNT/express_bench"
'

# Close JSON
# Remove trailing comma from last entry
sed -i.bak '$ s/,$//' "$RESULTS_DIR/${BACKEND}.json" 2>/dev/null || \
    sed -i '' '$ s/,$//' "$RESULTS_DIR/${BACKEND}.json"
echo "}" >> "$RESULTS_DIR/${BACKEND}.json"
rm -f "$RESULTS_DIR/${BACKEND}.json.bak"

echo ""
echo "=== Results saved to $RESULTS_DIR/${BACKEND}.json ==="
cat "$RESULTS_DIR/${BACKEND}.json"
