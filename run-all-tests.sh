#!/bin/bash
# Full php-src PHPT run (about 8 minutes); results in ./test_results.log.
set -euo pipefail
cd "$(dirname "$0")"

sudo systemctl start mariadb

# Names read by ext/mysqli (tests/connect.inc) and ext/pdo_mysql
# (tests/inc/config.inc). phptest/phptest is the test-only MariaDB account
# (see CLAUDE.md); MariaDB must be running (sudo systemctl start mariadb).
export MYSQL_TEST_HOST=127.0.0.1 MYSQL_TEST_USER=phptest MYSQL_TEST_PASSWD=phptest MYSQL_TEST_DB=test
export PDO_MYSQL_TEST_DSN='mysql:host=127.0.0.1;dbname=test' PDO_MYSQL_TEST_USER=phptest \
       PDO_MYSQL_TEST_PASS=phptest PDO_MYSQL_TEST_HOST=127.0.0.1 PDO_MYSQL_TEST_DB=test
# LANGUAGE overrides LC_ALL in gettext and fails ext/gettext under any PHP.
unset LANGUAGE

cargo build -p php-cli -p phpt-runner
time target/debug/phpt-runner --php target/debug/php -j 12 --no-artifacts \
    --summary-depth 2 modules/php-src | tee ./test_results.log


sudo systemctl stop mariadb