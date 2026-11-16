#!/bin/bash
# Full php-src PHPT run; every part of its output (stdout, stderr, the build,
# and the timings) is captured in ./test_results.log (override with
# PHPT_LOG=path). At most 3 CPUs (the owner shares the box): 3 workers,
# pinned to 3 cores.
set -euo pipefail
cd "$(dirname "$0")"

log="${PHPT_LOG:-./test_results.log}"

main() {
    # The php-src test copy is not committed (see scripts/fetch-phpt-tests.sh). Check it
    # exists and is at the revision phpt-baseline.txt is written against; a run against
    # any other php-src revision reports the wrong pass rate.
    scripts/fetch-phpt-tests.sh --verify

    sudo systemctl start mariadb

    # Names read by ext/mysqli (tests/connect.inc) and ext/pdo_mysql
    # (tests/inc/config.inc). phptest/phptest is the test-only MariaDB account
    # (see CLAUDE.md); MariaDB must be running (sudo systemctl start mariadb).
    export MYSQL_TEST_HOST=127.0.0.1 MYSQL_TEST_USER=phptest MYSQL_TEST_PASSWD=phptest MYSQL_TEST_DB=test
    export PDO_MYSQL_TEST_DSN='mysql:host=127.0.0.1;dbname=test' PDO_MYSQL_TEST_USER=phptest \
           PDO_MYSQL_TEST_PASS=phptest PDO_MYSQL_TEST_HOST=127.0.0.1 PDO_MYSQL_TEST_DB=test
    # LANGUAGE overrides LC_ALL in gettext and fails ext/gettext under any PHP.
    unset LANGUAGE

    # Record what produced the log, so a file revisited weeks later can be
    # lined up with a build.
    echo "== run-all-tests.sh: $(date -Is), git $(git rev-parse --short HEAD) ($(git rev-parse --abbrev-ref HEAD))"

    cargo build -p php-cli -p phpt-runner
    time taskset -c 13-15 target/debug/phpt-runner --php target/debug/php -j 3 --no-artifacts \
        --baseline phpt-baseline.txt --summary-depth 2 modules/php-src

    sudo systemctl stop mariadb
}

# One tee for the whole run: the terminal still shows everything, and the log
# file holds stdout and stderr interleaved in the order they happened.
main 2>&1 | tee "$log"
