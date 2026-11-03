#!/bin/bash
# Performance gate: builds the release `php`, then runs perf/bench/gate.py,
# which counts user-space instructions per workload (Zend/bench.php tests,
# Zend/micro_bench.php sections, perf/bench/suite/*.php, perf/bench/*.php
# micro-ops) for our php and /usr/bin/php and fails when the ratio
# ours/php of any workload exceeds its budget in perf/bench/budgets.tsv.
#
# Usage: scripts/perf_gate.sh [gate.py options: --ratchet | --init | --filter RX]
# Env:   PHP_BIN=<path>       skip the build and gate this binary
#        CARGO_TARGET_DIR     build directory (default ~/.cache/phprust-perf-gate,
#                             never inside the repository)
#        GATE_SET             zend,micro,suite,ops (default: all)
#        BACKEND=callgrind    use valgrind instead of `sudo -n perf stat`
# Exit:  0 within budget, 1 over budget / failing workload, 2 build or setup error.
set -u
root=$(cd "$(dirname "$0")/.." && pwd)
if [ -z "${PHP_BIN:-}" ]; then
    export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/.cache/phprust-perf-gate}
    (cd "$root" && cargo build --release -p php-cli) || exit 2
    PHP_BIN=$CARGO_TARGET_DIR/release/php
fi
[ -x "$PHP_BIN" ] || { echo "perf_gate: $PHP_BIN is not executable" >&2; exit 2; }
python3 -I "$root/perf/bench/gate.py" "$PHP_BIN" "$@"
