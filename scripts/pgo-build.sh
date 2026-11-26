#!/bin/bash
# buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
# Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
#
# Profile-guided release build: the release binaries (php, php-cgi, php-fpm), compiled a
# second time with a record of how PHP code actually ran in the first, so that the compiler
# lays out and inlines the interpreter for the paths requests take. Nothing in the source
# changes; the result is the same program, faster.
#
#   1. an instrumented release build, in target/pgo/gen (its own build directory)
#   2. training: that build runs php-src's Zend/bench.php and Zend/micro_bench.php,
#      perf/bench/suite, and the php-src test folders ordinary application code leans on
#      (Zend, tests, ext/standard, spl, pcre, json, date, mbstring), with the JIT on and off
#   3. the optimised release build from the merged profile, in target/release as usual
#
# Usage: scripts/pgo-build.sh
# Env:   PGO_TRAIN=<command>  one more training step: run by `sh -c` after the others, with
#                             PGO_PHP, PGO_PHP_CGI and PGO_PHP_FPM set to the instrumented
#                             binaries (serve your own application's pages with them, for
#                             example). A long-running server must end through its own exit
#                             path (SIGINT or SIGTERM, not SIGKILL): the profile is written then.
#        PGO_JOBS=<n>         PHPT workers during training (default 3)
#        PGO_KEEP=1           keep target/pgo (the instrumented build and the raw profiles)
# Needs: the compiler's llvm-profdata (`rustup component add llvm-tools`) and the php-src
#        tests (scripts/fetch-phpt-tests.sh).
# Note:  a later plain `cargo build --release` builds without the profile again: the profile
#        is passed in RUSTFLAGS, which this script sets for its own builds only.
# Exit:  0 built, 2 setup or build error.
set -u
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root" || exit 2
host=$(rustc -vV | sed -n 's/^host: //p')
profdata=$(rustc --print sysroot)/lib/rustlib/$host/bin/llvm-profdata
[ -x "$profdata" ] || { echo "pgo-build: no llvm-profdata for this compiler: run 'rustup component add llvm-tools'" >&2; exit 2; }
[ -f modules/php-src/Zend/bench.php ] || { echo "pgo-build: no php-src tests: run scripts/fetch-phpt-tests.sh" >&2; exit 2; }
work=$root/target/pgo
raw=$work/raw
rm -rf "$raw" "$work/php.profdata"
mkdir -p "$raw" || exit 2
jobs=${PGO_JOBS:-3}

echo "pgo-build: 1/3 instrumented build ($(rustc --version))"
# -vp-counters-per-site: room for the value profile (the targets of indirect calls) of a
# program this size; with the default the runtime runs out and says so on every exit.
RUSTFLAGS="-Cprofile-generate=$raw -Cllvm-args=-vp-counters-per-site=8" \
    cargo build --release -p php-cli --target-dir "$work/gen" || exit 2
# The runner is built plainly: it only starts the tests.
cargo build --release -p phpt-runner || exit 2
gen=$work/gen/release

echo "pgo-build: 2/3 training"
# %m: every process merges its counts into the one file of its binary.
export LLVM_PROFILE_FILE="$raw/%m.profraw"
for jit in 1 0; do
    for script in modules/php-src/Zend/bench.php modules/php-src/Zend/micro_bench.php perf/bench/suite/*.php; do
        BUZZS_JIT=$jit "$gen/php" -d memory_limit=1G "$script" > /dev/null 2>&1
    done
done
# The tests are run for the code they exercise, not for their verdicts (the profiling
# runtime may write to a test's output).
env -u LANGUAGE target/release/phpt-runner --php "$gen/php" -j "$jobs" -q --no-artifacts \
    --env "LLVM_PROFILE_FILE=$raw/%m.profraw" \
    modules/php-src/Zend/tests modules/php-src/tests modules/php-src/ext/standard/tests \
    modules/php-src/ext/spl/tests modules/php-src/ext/pcre/tests modules/php-src/ext/json/tests \
    modules/php-src/ext/date/tests modules/php-src/ext/mbstring/tests > "$work/train-phpt.log" 2>&1
echo "pgo-build: tests run for training: $(grep -c '' "$work/train-phpt.log") lines of output in $work/train-phpt.log"
if [ -n "${PGO_TRAIN:-}" ]; then
    echo "pgo-build: PGO_TRAIN"
    PGO_PHP=$gen/php PGO_PHP_CGI=$gen/php-cgi PGO_PHP_FPM=$gen/php-fpm sh -c "$PGO_TRAIN"
fi
unset LLVM_PROFILE_FILE
[ -n "$(ls "$raw")" ] || { echo "pgo-build: the training wrote no profile" >&2; exit 2; }
"$profdata" merge -o "$work/php.profdata" "$raw" || exit 2

echo "pgo-build: 3/3 optimised build"
RUSTFLAGS="-Cprofile-use=$work/php.profdata" cargo build --release -p php-cli || exit 2
[ -n "${PGO_KEEP:-}" ] || rm -rf "$work/gen" "$raw"
echo "pgo-build: done: target/release/php, php-cgi and php-fpm are profile-guided builds (profile: $work/php.profdata)"
