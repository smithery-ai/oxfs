#!/bin/bash
set -euo pipefail

# Run all FUSE filesystem benchmarks against R2
# Requires: R2_ACCOUNT_ID, R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY in env

for var in R2_ACCOUNT_ID R2_ACCESS_KEY_ID R2_SECRET_ACCESS_KEY; do
    if [ -z "${!var:-}" ]; then
        echo "ERROR: $var not set"
        exit 1
    fi
done

ENDPOINT="https://${R2_ACCOUNT_ID}.r2.cloudflarestorage.com"
BUCKET="connector-factory-workspaces"
MOUNT="/tmp/fuse-bench"
RESULTS="/bench/results"
ITERATIONS=3

mkdir -p "$RESULTS" "$MOUNT"

time_ms() { python3 -c "import time; print(int(time.time()*1000))"; }

cleanup_mount() {
    umount "$MOUNT" 2>/dev/null || fusermount -u "$MOUNT" 2>/dev/null || true
    pkill -f "oxfs mount" 2>/dev/null || true
    pkill -f tigrisfs 2>/dev/null || true
    pkill -f geesefs 2>/dev/null || true
    pkill -f juicefs 2>/dev/null || true
    sleep 1
    rm -rf "$MOUNT"
    mkdir -p "$MOUNT"
}

mount_backend() {
    local backend="$1"
    local prefix="bench-${backend}-$(date +%s)"
    cleanup_mount

    case "$backend" in
        oxfs)
            AWS_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID" \
            AWS_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY" \
            RUST_LOG=warn \
            oxfs mount "$MOUNT" \
                --backend s3 --bucket "$BUCKET" --region auto \
                --endpoint "$ENDPOINT" --prefix "$prefix" \
                --meta-db "/tmp/oxfs-bench.db" --cache-mem-mb 256 &
            sleep 3
            ;;
        tigris)
            AWS_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID" \
            AWS_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY" \
            AWS_REGION=auto AWS_S3_PATH_STYLE=true \
            tigrisfs --endpoint "$ENDPOINT" -f --enable-specials \
                "${BUCKET}:${prefix}" "$MOUNT" &
            sleep 5
            ;;
        geesefs)
            AWS_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID" \
            AWS_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY" \
            geesefs --endpoint "$ENDPOINT" --region auto \
                "${BUCKET}:${prefix}" "$MOUNT" &
            sleep 5
            ;;
        juicefs)
            # JuiceFS needs a metadata engine; use sqlite for local bench
            juicefs format --storage s3 \
                --bucket "${ENDPOINT}/${BUCKET}" \
                --access-key "$R2_ACCESS_KEY_ID" \
                --secret-key "$R2_SECRET_ACCESS_KEY" \
                "sqlite3:///tmp/juicefs-meta.db" "bench-vol" 2>/dev/null || true
            juicefs mount -d "sqlite3:///tmp/juicefs-meta.db" "$MOUNT" \
                --cache-size 256 --buffer-size 256 2>/dev/null
            sleep 3
            ;;
    esac

    # Verify
    echo "verify" > "$MOUNT/.verify" && cat "$MOUNT/.verify" >/dev/null && rm "$MOUNT/.verify"
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
    printf "  %-30s %6dms\n" "$name" "$med"
    echo "\"$name\": $med," >> "$CURRENT_RESULT"
}

bench_suite() {
    local backend="$1"
    CURRENT_RESULT="$RESULTS/${backend}.json"
    echo "{" > "$CURRENT_RESULT"

    echo "=== $backend ==="
    mount_backend "$backend"

    run_bench "single_file_roundtrip" '
    echo "hello" > "$MOUNT/s.txt"; cat "$MOUNT/s.txt" >/dev/null; rm "$MOUNT/s.txt"'

    run_bench "create_10_files" '
    for i in $(seq 1 10); do echo "f$i" > "$MOUNT/c$i.txt"; done'
    rm -f "$MOUNT"/c*.txt 2>/dev/null

    for i in $(seq 1 10); do echo "r$i" > "$MOUNT/r$i.txt"; done
    run_bench "read_10_files" '
    for i in $(seq 1 10); do cat "$MOUNT/r$i.txt" >/dev/null; done'
    rm -f "$MOUNT"/r*.txt

    run_bench "write_256k" '
    dd if=/dev/urandom of="$MOUNT/w.bin" bs=65536 count=4 2>/dev/null; rm "$MOUNT/w.bin"'

    for i in $(seq 1 10); do echo "s" > "$MOUNT/s$i.txt"; done
    run_bench "stat_10" '
    for i in $(seq 1 10); do stat "$MOUNT/s$i.txt" >/dev/null 2>&1; done'
    rm -f "$MOUNT"/s*.txt

    run_bench "mkdir_rmdir_5" '
    for i in $(seq 1 5); do mkdir "$MOUNT/d$i"; done
    for i in $(seq 1 5); do rmdir "$MOUNT/d$i"; done'

    for i in $(seq 1 5); do echo "mv" > "$MOUNT/mv_s$i.txt"; done
    run_bench "rename_5" '
    for i in $(seq 1 5); do mv "$MOUNT/mv_s$i.txt" "$MOUNT/mv_d$i.txt"; done
    for i in $(seq 1 5); do mv "$MOUNT/mv_d$i.txt" "$MOUNT/mv_s$i.txt"; done'
    rm -f "$MOUNT"/mv_*.txt

    run_bench "symlink_roundtrip" '
    echo "t" > "$MOUNT/st.txt"; ln -s st.txt "$MOUNT/sl.txt"
    readlink "$MOUNT/sl.txt" >/dev/null; cat "$MOUNT/sl.txt" >/dev/null
    rm "$MOUNT/sl.txt" "$MOUNT/st.txt"'

    sed -i '$ s/,$//' "$CURRENT_RESULT"
    echo "}" >> "$CURRENT_RESULT"

    cleanup_mount
    echo ""
}

# Run all backends
for backend in oxfs tigris geesefs juicefs; do
    if command -v "$backend" >/dev/null 2>&1 || [ "$backend" = "oxfs" ]; then
        bench_suite "$backend" || echo "  $backend: FAILED"
    else
        echo "=== $backend: not installed, skipping ==="
    fi
done

# Compare
echo "============================================"
echo "=== COMPARISON ==="
echo "============================================"
python3 - "$RESULTS" << 'PYEOF'
import json, sys, os

results_dir = sys.argv[1]
data = {}
for f in os.listdir(results_dir):
    if f.endswith('.json'):
        name = f.replace('.json', '')
        with open(os.path.join(results_dir, f)) as fh:
            data[name] = json.load(fh)

if not data:
    print("No results found")
    sys.exit(1)

metrics = list(next(iter(data.values())).keys())
backends = sorted(data.keys())

header = f"{'Metric':<30}" + "".join(f"{b:>10}" for b in backends) + "   winner"
print(header)
print("-" * len(header))

for m in metrics:
    vals = {b: data[b].get(m, 99999) for b in backends}
    best = min(vals, key=vals.get)
    row = f"{m:<30}" + "".join(f"{vals[b]:>8}ms" for b in backends) + f"   {best}"
    print(row)
PYEOF
