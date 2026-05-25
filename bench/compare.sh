#!/bin/bash
set -euo pipefail

# Compare oxfs vs tigrisfs benchmark results
# Usage: ./bench/compare.sh

DIR="$(dirname "$0")/results"

if [ ! -f "$DIR/oxfs.json" ] || [ ! -f "$DIR/tigris.json" ]; then
    echo "Missing results. Run:"
    echo "  ./bench/bench.sh --backend oxfs"
    echo "  ./bench/bench.sh --backend tigris"
    exit 1
fi

python3 - "$DIR/oxfs.json" "$DIR/tigris.json" << 'PYEOF'
import json, sys

with open(sys.argv[1]) as f: oxfs = json.load(f)
with open(sys.argv[2]) as f: tigris = json.load(f)

# Weights for composite score (sum to 1.0)
weights = {
    "small_file_create_100": 0.20,
    "small_file_read_100": 0.15,
    "small_file_delete_100": 0.10,
    "seq_write_1mb": 0.10,
    "seq_read_1mb": 0.10,
    "stat_100": 0.10,
    "mkdir_rmdir_50": 0.05,
    "readdir_100": 0.05,
    "rename_50": 0.05,
    "git_clone_express": 0.10,
}

print(f"{'Metric':<25} {'oxfs (ms)':>10} {'tigris (ms)':>12} {'ratio':>8} {'winner':>8}")
print("-" * 68)

composite = 0.0
for key, weight in weights.items():
    o = oxfs.get(key, 0)
    t = tigris.get(key, 0)
    if t > 0:
        ratio = o / t
        winner = "oxfs" if o < t else "tigris" if t < o else "tie"
        composite += weight * (t / o if o > 0 else 0)  # higher is better for oxfs
    else:
        ratio = 0
        winner = "?"
        composite += weight
    print(f"{key:<25} {o:>10} {t:>12} {ratio:>7.2f}x  {winner:>7}")

print("-" * 68)
print(f"Composite score (oxfs vs tigris): {composite:.2f}")
print(f"  1.0 = parity, >1.0 = oxfs faster, <1.0 = tigris faster")

# Write composite for autoresearch
with open(sys.argv[1].replace('.json', '_composite.txt'), 'w') as f:
    f.write(f"{composite:.4f}\n")
PYEOF
