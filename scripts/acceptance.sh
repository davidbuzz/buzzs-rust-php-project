#!/bin/bash
# Performance acceptance check: builds the release `php`, then runs
# perf/bench/acceptance.py, which counts user-space instructions per workload
# (Zend/bench.php tests, Zend/micro_bench.php sections, perf/bench/suite/*.php,
# perf/bench/*.php micro-ops) for our php and compares each count against the
# recorded goal — the instruction counts measured for the C implementation
# (/usr/bin/php) in perf/bench/goal.tsv. Fails when any workload exceeds
# 1.10x its goal.
#
# Usage: scripts/acceptance.sh [acceptance.py options: --record | --filter RX]
# Env:   PHP_BIN=<path>       skip the build and check this binary
#        CARGO_TARGET_DIR     build directory (default: the workspace's target/, so the
#                             binary is target/release/php, as `cargo build --release` makes it)
#        ACCEPTANCE_SET       zend,micro,suite,ops (default: all)
#        BACKEND=callgrind    use valgrind instead of `sudo -n perf stat`
# Exit:  0 every workload within goal, 1 over goal / failing workload, 2 build or setup error.
set -u
root=$(cd "$(dirname "$0")/.." && pwd)
if [ -z "${PHP_BIN:-}" ]; then
    (cd "$root" && cargo build --release -p php-cli) || exit 2
    PHP_BIN=${CARGO_TARGET_DIR:-$root/target}/release/php
fi
[ -x "$PHP_BIN" ] || { echo "acceptance: $PHP_BIN is not executable" >&2; exit 2; }
python3 -I "$root/perf/bench/acceptance.py" "$PHP_BIN" "$@"
