#!/bin/bash
# Usage: perf/bench/irun.sh <php-binary> <bench> <n>
# User-space instructions per loop iteration of perf/bench/<bench>.php:
# (instructions at n - instructions at n/10) / (0.9 n), which cancels the
# process startup. Deterministic to ~0.1 %, unlike wall time on a busy box.
dir=$(cd "$(dirname "$0")" && pwd)
bin=$1; b=$2; n=$3
ins() { sudo -n perf stat -x, -e instructions:u timeout --kill-after=0.5s "${BENCH_TIMEOUT:-3s}" "$bin" -d memory_limit=1G -d opcache.enable_cli=0 "$dir/$b.php" "$1" 2>&1 >/dev/null | awk -F, '/instructions/{print $1}'; }
a=$(ins "$n"); c=$(ins $((n / 10)))
echo "$(basename "$bin") $b: $(( (a - c) / (n - n / 10) ))"
