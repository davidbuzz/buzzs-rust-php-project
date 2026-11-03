#!/bin/bash
# Usage: perf/bench/slowtests.sh <phpt-runner> <php-binary>...
# CPU time (task-clock of the runner and its children) of the tests that
# time out at 60 s in the debug build, run with a 600 s timeout.
src=$(cd "$(dirname "$0")/../.." && pwd)/modules/php-src
tests="ext/zlib/tests/inflate_add_basic.phpt ext/date/tests/date_diff.phpt ext/hash/tests/hash_pbkdf2_large_length.phpt ext/mbstring/tests/iso8859_encodings.phpt"
runner=$1; shift
for b in "$@"; do
    for t in $tests; do
        out=$(sudo -n perf stat -x, -e task-clock "$runner" --php "$b" -j 1 --timeout 600 --no-artifacts --no-color -q "$src/$t" 2>&1)
        res=$(echo "$out" | grep -E "^(PASS|FAIL|SKIP|BORK|TIMEOUT)" | head -1 | awk '{print $1}')
        [ -z "$res" ] && res=PASS
        cpu=$(echo "$out" | awk -F, '/task-clock/{printf "%.1f", $1/1000}')
        printf '%-20s %-40s %8s s  %s\n' "$(basename "$b")" "$t" "$cpu" "$res"
    done
done
