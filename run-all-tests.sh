#!/bin/bash
# The full test run, recorded as evidence. One command builds everything from
# the current source, runs every test and the runtime comparison against the C
# implementation, writes all of the output to ./<DD-MON-YYYY>.full-test-run.log
# (the layout of 16-NOV-2026.full-test-run.log, plus PHASE 4 and a summary),
# and commits that one file to git. The commit stays local: nothing is pushed.
#
#   PHASE 1  cargo build --workspace --release
#   PHASE 2  cargo test --workspace --release --no-fail-fast
#   PHASE 3  the full in-scope php-src PHPT run on the release php: every
#            PASS/FAIL line, a diff for each failure, checked against
#            phpt-baseline.txt
#   PHASE 4  python3 perf/bench/timing.py --no-build: total runtime of ours
#            against /usr/bin/php on identical inputs (the deliverable)
#   SUMMARY  the numbers of the four phases (also the commit message)
#
# usage: ./run-all-tests.sh [--no-commit]
#   --no-commit  write the log but do not commit it
#
# At most 3 CPUs (the owner shares the box): cargo uses 3 jobs
# (.cargo/config.toml), the PHPT run 3 workers, and the PHPT and timing runs
# are pinned to the last three cores. Run it on an otherwise quiet box: PHASE 4
# measures wall-clock time. A second run on the same day replaces that day's
# log (git keeps the earlier version). Exit status: 0 when every phase passed,
# 1 when any failed, 2 on a usage or setup error.
set -uo pipefail
cd "$(dirname "$0")" || exit 2

commit=1
case "${1:-}" in
'') ;;
--no-commit) commit=0 ;;
-h | --help)
    sed -n '2,/^set -uo/{/^set -uo/d;s/^# \{0,1\}//;p}' "$0"
    exit 0
    ;;
*)
    echo "usage: $0 [--no-commit]" >&2
    exit 2
    ;;
esac

day=$(LC_ALL=C date +%d-%b-%Y | tr '[:lower:]' '[:upper:]')
log="$day.full-test-run.log"
ncpu=$(nproc --all)
cores="$((ncpu - 3))-$((ncpu - 1))"
work=$(mktemp -d /tmp/full-test-run.XXXXXX) || exit 2
trap 'rm -rf "$work" 2>/dev/null' EXIT

# The php-src test copy is not committed (see scripts/fetch-phpt-tests.sh).
# Check it exists and is at the revision phpt-baseline.txt is written against
# before spending an hour on a run that would report the wrong pass rate.
if ! pin=$(scripts/fetch-phpt-tests.sh --verify 2>&1); then
    echo "$pin" >&2
    exit 2
fi

# run <name> <command...>: prints "$ command", runs it with its output on the
# terminal (so in the log) and in $work/<name>.out, returns its exit status.
run() {
    local name=$1
    shift
    printf '$ %s\n' "$*"
    "$@" 2>&1 | tee "$work/$name.out"
    return "${PIPESTATUS[0]}"
}

summary() {
    local rc_build=$1 rc_test=$2 rc_phpt=$3 rc_time=$4
    echo "cargo build --workspace --release: exit $rc_build"
    awk -v rc="$rc_test" '
        /^test result: / { bins++; p += $4; f += $6; i += $8; if ($3 != "ok.") bad++ }
        END { printf "cargo test --workspace --release: %d passed, %d failed, %d ignored, in %d test binaries (%d failing); exit %d\n", p, f, i, bins, bad, rc }
    ' "$work/test.out"
    echo "PHPT, in-scope php-src suite on the release php: exit $rc_phpt"
    grep -E '^(Directory  |TOTAL |Number of tests|Out of scope|Tests (skipped|warned|failed|passed)|Expected fail|Time taken|Baseline:)' \
        "$work/phpt.out" | sed 's/^/    /'
    echo "runtime against C, perf/bench/timing.py: exit $rc_time"
    grep -E '^(input  |TOTAL |# criterion)' "$work/timing.out" | sed 's/^/    /'
}

main() {
    local rc_build rc_test rc_phpt rc_time started_db=0 dirty=''
    if [ -n "$(git status --porcelain --untracked-files=no -- . ":!$log")" ]; then
        dirty=' (plus uncommitted changes)'
    fi
    echo "=================================================================="
    echo "buzzs-rust-php-project - FULL TEST RUN - $day (release)"
    echo "git commit: $(git rev-parse HEAD)$dirty"
    echo "host: $(uname -a)  cpus: $ncpu"
    echo "rustc: $(rustc --version)  cargo: $(cargo --version)"
    echo "php-src tests: ${pin#modules/php-src: }"
    echo "C reference: /usr/bin/php $(/usr/bin/php -d memory_limit=1G -r 'echo PHP_VERSION;')"
    echo "started: $(date -Is)"
    echo "=================================================================="

    # The mysqli and pdo_mysql tests need MariaDB (the test-only phptest
    # account, see CLAUDE.md); it is stopped again before the timing phase.
    if ! systemctl is-active --quiet mariadb; then
        sudo systemctl start mariadb && started_db=1
    fi
    # Names read by ext/mysqli (tests/connect.inc) and ext/pdo_mysql
    # (tests/inc/config.inc).
    export MYSQL_TEST_HOST=127.0.0.1 MYSQL_TEST_USER=phptest MYSQL_TEST_PASSWD=phptest MYSQL_TEST_DB=test
    export PDO_MYSQL_TEST_DSN='mysql:host=127.0.0.1;dbname=test' PDO_MYSQL_TEST_USER=phptest \
        PDO_MYSQL_TEST_PASS=phptest PDO_MYSQL_TEST_HOST=127.0.0.1 PDO_MYSQL_TEST_DB=test
    # LANGUAGE overrides LC_ALL in gettext and fails ext/gettext under any PHP.
    unset LANGUAGE

    echo
    echo "### PHASE 1: cargo build --release (all binaries from current source)"
    run build cargo build --workspace --release
    rc_build=$?
    echo "build exit code: $rc_build"
    if [ "$rc_build" -ne 0 ]; then
        [ "$started_db" -eq 1 ] && sudo systemctl stop mariadb
        echo
        echo "ended: $(date -Is) (the build failed: no tests run)"
        return 1
    fi

    echo
    echo "### PHASE 2: cargo test --release (entire workspace, all crates, no-fail-fast)"
    run test cargo test --workspace --release --no-fail-fast
    rc_test=$?
    echo "cargo test exit code: $rc_test"

    # A fresh TMPDIR: the tests create and remove many temporary files, and a
    # /tmp holding hundreds of thousands of stale entries slows them into
    # timeouts.
    mkdir "$work/tmp"
    echo
    echo "### PHASE 3: full PHPT conformance run (all in-scope tests, release php,"
    echo "### every PASS/FAIL line printed, diffs shown for failures)"
    run phpt env TMPDIR="$work/tmp" taskset -c "$cores" target/release/phpt-runner \
        --php target/release/php --no-color --show-diff --no-artifacts -j 3 \
        --scope phpt-scope.txt --baseline phpt-baseline.txt modules/php-src
    rc_phpt=$?
    echo "phpt exit code: $rc_phpt"
    [ "$started_db" -eq 1 ] && sudo systemctl stop mariadb

    echo
    echo "### PHASE 4: runtime against the C implementation (the deliverable: the"
    echo "### total runtime of ours <= 1.10 x C's on identical inputs, both at defaults)"
    run timing python3 perf/bench/timing.py --no-build
    rc_time=$?
    echo "timing exit code: $rc_time"

    echo
    echo "### SUMMARY"
    summary "$rc_build" "$rc_test" "$rc_phpt" "$rc_time" | tee "$work/summary.txt"
    echo "$rc_test $rc_phpt $rc_time" >"$work/rc"
    echo
    echo "ended: $(date -Is)"
    [ "$rc_test" -eq 0 ] && [ "$rc_phpt" -eq 0 ] && [ "$rc_time" -eq 0 ]
}

# One tee for the whole run: the terminal still shows everything, and the log
# file holds stdout and stderr interleaved in the order they happened.
main 2>&1 | tee "$log"
status=${PIPESTATUS[0]}

# Commit the evidence only when the run got to its summary (a failed build
# leaves nothing worth recording).
if [ "$commit" -eq 1 ] && [ -f "$work/rc" ]; then
    read -r rc_test rc_phpt rc_time <"$work/rc"
    sum="$work/summary.txt"
    tests=$(sed -n 's/^cargo test[^:]*: \([0-9]*\) passed, \([0-9]*\) failed.*/\1 passed, \2 failed/p' "$sum")
    phpt=$(awk '/^ *Number of tests/ { run = $NF } /^ *Tests passed/ { pass = $4 } /^ *Tests failed/ { fail = $4 }
        END { printf "%s of %s pass, %s fail", pass, run, fail }' "$sum")
    ratio=$(sed -n 's/^ *# criterion: .*: \(.*\) (\([0-9.]*\) wall.*/\2 x C, \1/p' "$sum")
    {
        echo "Full test run evidence $day (release): PHPT $phpt; runtime $ratio"
        echo
        echo "Produced by ./run-all-tests.sh: cargo build and cargo test of the whole"
        echo "workspace (release), the full in-scope PHPT run on the release php"
        echo "(every PASS/FAIL line and failure diffs, php-src ${pin#modules/php-src: at }),"
        echo "and perf/bench/timing.py against /usr/bin/php. cargo test: $tests."
        echo "Exit codes: cargo test $rc_test, PHPT $rc_phpt, timing $rc_time."
        echo
        cat "$sum"
    } >"$work/msg"
    if git add -- "$log" && git commit -q -F "$work/msg" -- "$log"; then
        echo "committed $log as $(git rev-parse --short HEAD) (local only, not pushed)"
        if upstream=$(git rev-parse --abbrev-ref '@{u}' 2>/dev/null); then
            echo "to publish it: git push ${upstream%%/*} $(git rev-parse --abbrev-ref HEAD)"
        fi
    else
        echo "run-all-tests.sh: committing $log failed" >&2
        status=1
    fi
fi
exit "$status"
