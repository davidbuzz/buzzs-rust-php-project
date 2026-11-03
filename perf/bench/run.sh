#!/bin/bash
# Usage: perf/bench/run.sh <php-binary> <label>
# Runs Zend/bench.php, Zend/micro_bench.php, startup and perf/bench/suite/*.php
# on <php-binary> and on /usr/bin/php (opcache off, memory_limit=1G), interleaved,
# pinned to one core, and prints a TSV table with best wall/CPU seconds and the
# ratio <php-binary> / /usr/bin/php.  Env: REPS (3), CORE (last cpu; 'none'
# disables taskset), FILTER (regex on bench names), SUBS=0 hides per-test rows,
# REF (reference binary, default /usr/bin/php), OUT (also save the table there).
dir=$(cd "$(dirname "$0")" && pwd)
[ $# -ge 2 ] || { echo "usage: $0 <php-binary> <label>" >&2; exit 2; }
bin=$(readlink -f "$1")
if [ -n "$OUT" ]; then
    python3 -I "$dir/harness.py" "$bin" "$2" "${REF:-/usr/bin/php}" | tee "$OUT"
else
    exec python3 -I "$dir/harness.py" "$bin" "$2" "${REF:-/usr/bin/php}"
fi
