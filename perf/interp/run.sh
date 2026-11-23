#!/bin/bash
# Instructions per operation (user space, perf stat), ours against C, for the
# operations ordinary application code runs all the time (calls of every
# kind, call_user_func_array(), global/static variables, array and string
# builtins, ...), with our JIT off: such code runs once or a few times per
# request, not in hot loops. The empty loop's count is subtracted.
#   perf/interp/run.sh [ours-php (target/release/php)] [ref-php (/usr/bin/php8.5)]
# Needs `sudo -n perf` (as perf/bench/acceptance.py does). Uses one CPU.
set -e
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
OURS=${1:-$ROOT/target/release/php}
REF=${2:-/usr/bin/php8.5}
N=200000
D=$(mktemp -d)
trap 'rm -rf "$D"' EXIT
python3 "$ROOT/perf/interp/gen.py" "$D" > /dev/null
count() { sudo -n perf stat -x, -e instructions:u -o "$D/.stat" -- "$@" > /dev/null 2>&1; awk -F, '/instructions/ {print $1}' "$D/.stat"; }
ob=$(BUZZS_JIT=0 count taskset -c 0 "$OURS" -n "$D/baseline.php")
cb=$(count taskset -c 0 "$REF" -n -d memory_limit=1G "$D/baseline.php")
printf '%-18s %10s %10s %7s\n' op ours ref ratio
for f in "$D"/*.php; do
    n=$(basename "$f" .php)
    [ "$n" = baseline ] && continue
    o=$(BUZZS_JIT=0 count taskset -c 0 "$OURS" -n "$f")
    c=$(count taskset -c 0 "$REF" -n -d memory_limit=1G "$f")
    awk -v n="$n" -v o="$o" -v c="$c" -v ob="$ob" -v cb="$cb" -v N=$N 'BEGIN { oo=(o-ob)/N; cc=(c-cb)/N; printf "%-18s %10.0f %10.0f %7.1f\n", n, oo, cc, (cc>0? oo/cc : 0) }'
done | sort -k4 -n -r
