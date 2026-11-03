#!/bin/bash
# Usage: perf/bench/compare.sh <baseline-binary> <candidate-binary> [label]
# A/B of two builds: same table as run.sh but the reference is <baseline-binary>,
# so ratio = candidate / baseline (0.80 = 20 % faster).  Same env knobs as run.sh.
dir=$(cd "$(dirname "$0")" && pwd)
[ $# -ge 2 ] || { echo "usage: $0 <baseline-binary> <candidate-binary> [label]" >&2; exit 2; }
REF=$(readlink -f "$1") exec "$dir/run.sh" "$2" "${3:-compare}"
