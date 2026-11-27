#!/usr/bin/env bash
# shellcheck disable=SC2317 # the suites' and sites' functions are called by name, from the tables
#
# Part of buzzs-rust-php-project, under its licence (BSD-3-Clause; see LICENSE).
#
# Runs the test suites of real PHP projects under our `php` and under the C implementation, and
# lists the tests that fail here and pass there: each of those is a bug in this code base.
#
# NOTHING OF THOSE PROJECTS IS IN THIS REPOSITORY. This script holds only where each one lives
# and which revision to take. A project is fetched from its own upstream when it is not on
# disk yet, into app-tests/apps/ (ignored by git), and Composer installs its dependencies
# there. What is on disk already is used as it is: nothing is downloaded twice.
#
#   app-tests/run-all-app-tests.sh                     everything
#   app-tests/run-all-app-tests.sh reactphp-promise    only the suites named
#   app-tests/run-all-app-tests.sh --list              the suites, their pins, what is on disk
#   app-tests/run-all-app-tests.sh --fetch-only [...]  fetch and install what is missing, run nothing
#   app-tests/run-all-app-tests.sh --verify            check that git tracks nothing fetched
#
# Environment:
#   PHP_BIN      our binary (default: target/release/php, else target/debug/php)
#   REF_BIN      the C implementation (default: /usr/bin/php8.5); empty: run under ours only
#   APP_TIMEOUT  seconds one suite may run under one binary (default: 2700)
#   MYSQL_TEST_HOST, MYSQL_TEST_USER, MYSQL_TEST_PASSWD
#                a MySQL or MariaDB server for the suites that need one (the variables the PHPT
#                runner uses). Those suites are left out when they are not set. They create
#                and drop databases named apptests_*, and nothing else.
#
# One suite at a time, each a single process. The C binary always runs with memory_limit=1G
# (its child processes too, through an extra ini directory) and ours with 2G.

set -u

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
APPS=$HERE/apps
LOGS=$HERE/logs
APP_TIMEOUT=${APP_TIMEOUT:-2700}

# suite | checkout | upstream repository | pinned commit | needs | test command
#
# The command runs from the checkout's directory; one starting with @ names a function below.
# Suites that share a checkout share one fetch. "needs" is - or mysql. A pin is moved on
# purpose, by editing its line: results are comparable from run to run only against one
# revision of a project.
suites() {
    cat <<'EOF'
amphp-amp|amphp-amp|https://github.com/amphp/amp.git|73c38b323ff8d790abf0f76c56fcc892bda4111a|-|vendor/bin/phpunit
amphp-cache|amphp-cache|https://github.com/amphp/cache.git|1887e3e34a973ca38d9b661bedbeeb1e14ab2612|-|vendor/bin/phpunit
amphp-dns|amphp-dns|https://github.com/amphp/dns.git|fa11558d1638b7d5e4b4f5d794747b80cf74f82d|-|vendor/bin/phpunit
amphp-file|amphp-file|https://github.com/amphp/file.git|756b1cb6d6ed88949c94457865859d3a35d10813|-|vendor/bin/phpunit
amphp-http|amphp-http|https://github.com/amphp/http.git|1b0b2d471c25b095056f3ac408865cd5e3ba771f|-|vendor/bin/phpunit
amphp-parallel|amphp-parallel|https://github.com/amphp/parallel.git|ab0119110ce5c8f03670306a18d6a0b2084914e0|-|vendor/bin/phpunit
amphp-parser|amphp-parser|https://github.com/amphp/parser.git|3cf1f8b32a0171d4b1bed93d25617637a77cded7|-|vendor/bin/phpunit
amphp-pipeline|amphp-pipeline|https://github.com/amphp/pipeline.git|cf2d67696c2015ea7c7fd8f6ec5f8ed7c2d17c17|-|vendor/bin/phpunit
amphp-process|amphp-process|https://github.com/amphp/process.git|acfd64f68df5fa7f2d674c93d1986b2bf5d02770|-|vendor/bin/phpunit
amphp-serialization|amphp-serialization|https://github.com/amphp/serialization.git|025e0a6a70e4f99d732c4eea151ef762a59a8363|-|vendor/bin/phpunit
amphp-socket|amphp-socket|https://github.com/amphp/socket.git|a00859ec686d407094430b65e5f254f88d0a741c|-|vendor/bin/phpunit
amphp-sync|amphp-sync|https://github.com/amphp/sync.git|a696058be8845875d4ce177b4f2af13ad438b9e1|-|vendor/bin/phpunit
amphp-websocket-client|amphp-websocket-client|https://github.com/amphp/websocket-client.git|dc033fdce0af56295a23f63ac4f579b34d470d6c|-|vendor/bin/phpunit
amphp-websocket-server|amphp-websocket-server|https://github.com/amphp/websocket-server.git|8fa89ed203d31284ea95825694622abf2e827fc7|-|vendor/bin/phpunit
reactphp-async|reactphp-async|https://github.com/reactphp/async.git|592c7b351f0d9adc7028686cf8d8e579a80eb824|-|vendor/bin/phpunit
reactphp-cache|reactphp-cache|https://github.com/reactphp/cache.git|20629e6c4b8486b09bf10e40b0d3c848ac90b072|-|vendor/bin/phpunit
reactphp-child-process|reactphp-child-process|https://github.com/reactphp/child-process.git|8c3cc75202a67b3f983c90bbdbcfe211a20b85dd|-|vendor/bin/phpunit
reactphp-datagram|reactphp-datagram|https://github.com/reactphp/datagram.git|7b37becf2b56ea833f4a79fb25caa5a0b99910ca|-|vendor/bin/phpunit
reactphp-dns|reactphp-dns|https://github.com/reactphp/dns.git|d1d78a687a977d8ed5ec12bbe38398e471f75e6e|-|vendor/bin/phpunit
reactphp-event-loop|reactphp-event-loop|https://github.com/reactphp/event-loop.git|a5d7fa2d806cdc48ee81b1cea026bb4fbe19ed1f|-|vendor/bin/phpunit
reactphp-promise|reactphp-promise|https://github.com/reactphp/promise.git|146a40005ac07e876c9e5055be40e66242f23c97|-|vendor/bin/phpunit
reactphp-promise-stream|reactphp-promise-stream|https://github.com/reactphp/promise-stream.git|2f30290b9cf9425db89e438a0d235e936a7c1ca5|-|vendor/bin/phpunit
reactphp-promise-timer|reactphp-promise-timer|https://github.com/reactphp/promise-timer.git|60b1d852fe7e90df0b3e180d2fba81c947639055|-|vendor/bin/phpunit
reactphp-stream|reactphp-stream|https://github.com/reactphp/stream.git|f2d9bc62569dd683ac02d28f7ff48fdf87d9c209|-|vendor/bin/phpunit
revolt-event-loop|revolt-event-loop|https://github.com/revoltphp/event-loop.git|93999a4ffca70aaa3890c553001e9f8313a1895c|-|vendor/bin/phpunit --exclude-group skip
laravel-framework|laravel-framework|https://github.com/laravel/framework.git|321caf77d6c037577434e177c3bf280abb2b3513|-|vendor/bin/phpunit --exclude-group skip
phpunit|phpunit|https://github.com/sebastianbergmann/phpunit.git|21f5eb0ff77653a318bf6c4cbd5d2421a69bd224|-|./phpunit
phpunit-12-unit|phpunit-12|https://github.com/sebastianbergmann/phpunit.git|1d5a4a3c98fa1ca3a51402c74b66c2c8df85e0bb|-|./phpunit --testsuite unit
phpunit-12-end-to-end|phpunit-12|https://github.com/sebastianbergmann/phpunit.git|1d5a4a3c98fa1ca3a51402c74b66c2c8df85e0bb|-|./phpunit --testsuite end-to-end
doctrine-dbal|doctrine-dbal|https://github.com/doctrine/dbal.git|c435cd7ea33e17d2a89288caf994cf3d4549b879|-|vendor/bin/phpunit
doctrine-dbal-mysqli|doctrine-dbal|https://github.com/doctrine/dbal.git|c435cd7ea33e17d2a89288caf994cf3d4549b879|mysql|vendor/bin/phpunit -c ci/github/phpunit/app-tests-mysqli.xml
doctrine-orm|doctrine-orm|https://github.com/doctrine/orm.git|4487f850401c0e1f27891cce5897fa90b3055c2a|-|vendor/bin/phpunit
symfony-http-foundation|symfony-http-foundation|https://github.com/symfony/http-foundation.git|7fb73cffa06e2bcc2cb523fcccb1e8174106b206|-|vendor/bin/phpunit
symfony-http-kernel|symfony-http-kernel|https://github.com/symfony/http-kernel.git|94c57f4c5c45a311d244a0aeadb72b14ee41d25d|-|vendor/bin/phpunit
symfony-demo|symfony-demo|https://github.com/symfony/demo.git|8f84bf89426f47125010cbf2d4c6a920787bf482|-|vendor/bin/phpunit
symfony|symfony|https://github.com/symfony/symfony.git|9222647836686bff1283e060108107318f9db3e7|-|@symfony_components
wordpress-develop-formatting|wordpress-develop|https://github.com/WordPress/wordpress-develop.git|0afb74de8c3fe4c1869e44b62b0418d67b11aa4c|mysql|vendor/bin/phpunit --group formatting
wordpress-develop-query|wordpress-develop|https://github.com/WordPress/wordpress-develop.git|0afb74de8c3fe4c1869e44b62b0418d67b11aa4c|mysql|vendor/bin/phpunit --group query
EOF
}

say() { printf '%s\n' "$*"; }
die() { printf 'run-all-app-tests: %s\n' "$*" >&2; exit 2; }

# The rule this directory lives by: git tracks nothing that was fetched or produced here.
verify() {
    local bad
    git -C "$ROOT" check-ignore -q "$APPS/x" || die "app-tests/apps/ is not ignored by git; refusing to fetch into it"
    git -C "$ROOT" check-ignore -q "$LOGS/x" || die "app-tests/logs/ is not ignored by git"
    bad=$(git -C "$ROOT" ls-files -- "$APPS" "$LOGS")
    [ -z "$bad" ] || die "git tracks fetched or produced files, which must never be committed:
$bad"
}

have_mysql() { [ -n "${MYSQL_TEST_HOST:-}" ] && [ -n "${MYSQL_TEST_USER:-}" ]; }
mysql_do() { MYSQL_PWD=${MYSQL_TEST_PASSWD:-} mysql -h "$MYSQL_TEST_HOST" -u "$MYSQL_TEST_USER" "$@"; }

# Scripted edits to a fetched checkout, made once after its fetch: a test that cannot run
# anywhere is put in the group the test command leaves out. Edits are made by command, never
# kept as patch files, so that no line of a project's source is copied into this repository.
prepare() {
    local checkout=$1 dir=$2
    case $checkout in
    laravel-framework)
        # Hangs under any PHP (upstream's own CI skips it the same way).
        skip_test "$dir/tests/Filesystem/FilesystemTest.php" 'public function testSharedGet()'
        ;;
    revolt-event-loop)
        # Broken since php-src pull request 14452 (upstream's CI skips it too).
        skip_test "$dir/test/Driver/StreamSelectDriverTest.php" 'public function testTooLargeFileDescriptorSet(): void'
        ;;
    esac
}

# Puts the test method declared by the line `$2` of file `$1` into PHPUnit's group "skip".
skip_test() {
    local file=$1 decl=$2
    [ -f "$file" ] || return 0
    if ! grep -B1 -F "$decl" "$file" | grep -q "Group('skip')"; then
        DECL=$decl perl -0pi -e 's/^([ \t]*)\Q$ENV{DECL}\E/$1#[\\PHPUnit\\Framework\\Attributes\\Group(\x27skip\x27)]\n$1$ENV{DECL}/m' "$file"
    fi
}

# Composer's part of a fetch. A library's own test runner is not always among what it
# requires: it is added here, in the checkout only.
install_deps() {
    local checkout=$1 dir=$2
    local -a composer=("${COMPOSER_PHP[@]}" "$COMPOSER")
    case $checkout in
    symfony-http-foundation | symfony-http-kernel)
        # These are tested inside Symfony's own tree upstream; standing alone they need a
        # PHPUnit and the bridge that provides the clock mock.
        # (A checkout fetched by its commit has no tag for Composer to read its version from.)
        (cd "$dir" && COMPOSER_ROOT_VERSION=7.4.23 "${composer[@]}" require --dev --no-progress --no-interaction --ignore-platform-req=php+ 'phpunit/phpunit:^12' 'symfony/phpunit-bridge:^7.4')
        ;;
    symfony)
        # Its runner installs the PHPUnit it wants into the checkout.
        (cd "$dir" && COMPOSER_ROOT_VERSION=8.2.x-dev "${composer[@]}" install --no-progress --no-interaction --ignore-platform-req=php+ &&
            COMPOSER_BINARY=$LOGS/composer.sh "${COMPOSER_PHP[@]}" ./phpunit install)
        ;;
    *)
        (cd "$dir" && "${composer[@]}" install --no-progress --no-interaction --ignore-platform-req=php+)
        ;;
    esac
}

# A checkout that is on disk at its pinned commit is left alone: no network is touched.
fetch() {
    local checkout=$1 url=$2 sha=$3 dir=$APPS/$1
    if [ -d "$dir/.git" ] && [ "$(git -C "$dir" rev-parse -q --verify HEAD 2> /dev/null)" = "$sha" ]; then
        say "  $checkout: on disk at ${sha:0:12}, not fetched"
    else
        say "  $checkout: fetching ${sha:0:12} from $url"
        mkdir -p "$dir" || return 1
        [ -d "$dir/.git" ] || { git init -q "$dir" && git -C "$dir" remote add origin "$url"; } || return 1
        git -C "$dir" fetch -q --depth 1 origin "$sha" || return 1
        git -C "$dir" checkout -q -f --detach FETCH_HEAD || return 1
        # Another revision's dependencies and scripted edits do not carry over.
        rm -rf "${dir:?}/vendor" "${dir:?}/.app-tests-prepared"
    fi
    if [ ! -f "$dir/.app-tests-prepared" ]; then
        prepare "$checkout" "$dir" && : > "$dir/.app-tests-prepared"
    fi
    if [ -f "$dir/vendor/autoload.php" ] && [ -f "$dir/.app-tests-installed" ]; then
        say "  $checkout: dependencies on disk, not installed again"
    else
        say "  $checkout: installing dependencies with Composer"
        install_deps "$checkout" "$dir" > "$LOGS/$checkout.composer.txt" 2>&1 ||
            { say "  $checkout: installing failed, see app-tests/logs/$checkout.composer.txt"; return 1; }
        : > "$dir/.app-tests-installed"
    fi
}

# The command line of one of the two binaries, in PHP_CMD. Both read the same settings
# (settings_ini) from an ini directory of their own, so that the processes a suite starts get
# them too; the C binary keeps its own directory besides (its extensions are loaded from it).
# Ours gets the memory limit `$2` (default 2G; a site, whose pages show the setting, gets C's).
php_cmd() {
    local memory=${2:-2G}
    if [ "$1" = ours ]; then
        PHP_CMD=(env "PHP_INI_SCAN_DIR=$LOGS/ours-ini-$memory" "$PHP_BIN" -d "memory_limit=$memory")
    else
        # PHPRC: an empty php.ini stands in for the distribution's.
        PHP_CMD=(env "PHPRC=$LOGS/ref-php-ini" "PHP_INI_SCAN_DIR=$REF_SCAN" "$REF_BIN" -d memory_limit=1G)
    fi
}

# php_cmd, and a `php` first on the PATH that is that command: a suite that starts `php` by
# name must get the binary under test with its settings, not whichever PHP the system has.
# The caller puts the PATH back from SAVED_PATH.
php_on_path() {
    php_cmd "$@"
    mkdir -p "$LOGS/bin-$1" || return 1
    printf '#!/bin/sh\nexec %s "$@"\n' "$(printf '%q ' "${PHP_CMD[@]}")" > "$LOGS/bin-$1/php"
    chmod +x "$LOGS/bin-$1/php"
    SAVED_PATH=$PATH
    PATH=$LOGS/bin-$1:$PATH
}

# The settings that differ from php-src's own defaults on purpose, and the defaults a
# distribution's php.ini most often changes, spelled out. The C binary is kept from reading
# the distribution's php.ini at all (php_cmd): under it, which errors are shown, whether
# assertions run and whether $_ENV is filled differ from ours, and the two binaries would be
# compared under different settings.
settings_ini() {
    cat <<EOF
memory_limit=$1
error_reporting=-1
display_errors=1
display_startup_errors=1
log_errors=0
html_errors=0
zend.assertions=1
short_open_tag=1
variables_order=EGPCS
request_order=
zend.exception_ignore_args=0
session.gc_probability=1
date.timezone=UTC
opcache.enable_cli=0
EOF
}

# Settings a suite takes from the environment, written into the checkout before each run, and
# state one binary's run must not hand to the other's.
configure() {
    local name=$1 dir=$2
    case $name in
    symfony-demo)
        git -C "$dir" checkout -q -- data # the SQLite databases its tests write to
        ;;
    doctrine-dbal-mysqli)
        mysql_do -e 'DROP DATABASE IF EXISTS apptests_dbal; CREATE DATABASE apptests_dbal' || return 1
        perl -pe 's/(name="db_host" value=")[^"]*/$1$ENV{MYSQL_TEST_HOST}/;
                  s/(name="db_dbname" value=")[^"]*/${1}apptests_dbal/;
                  s{^(\s*)<var name="db_user" value="[^"]*"\s*/>}{$1<var name="db_user" value="$ENV{MYSQL_TEST_USER}"/>\n$1<var name="db_password" value="$ENV{MYSQL_TEST_PASSWD}"/>};
                  $_ = "" if /name="db_password"/ && !/db_user/' \
            "$dir/ci/github/phpunit/mysqli.xml" > "$dir/ci/github/phpunit/app-tests-mysqli.xml"
        ;;
    wordpress-develop-*)
        mysql_do -e 'CREATE DATABASE IF NOT EXISTS apptests_wpdev' || return 1
        perl -pe 's/youremptytestdbnamehere/apptests_wpdev/; s/yourusernamehere/$ENV{MYSQL_TEST_USER}/;
                  s/yourpasswordhere/$ENV{MYSQL_TEST_PASSWD}/;
                  s/(\x27DB_HOST\x27,\s*\x27)localhost/$1$ENV{MYSQL_TEST_HOST}/' \
            "$dir/wp-tests-config-sample.php" > "$dir/wp-tests-config.php"
        ;;
    esac
}

# One PHPUnit run under PHP_CMD, from the directory `$1`: the report goes to `$2`, the user
# and system CPU seconds are added to `$3`. The rest is the command and its arguments.
phpunit_run() {
    local dir=$1 report=$2 cpu=$3
    shift 3
    (cd "$dir" && exec /usr/bin/time -a -o "$cpu" -f '%U %S' timeout -s KILL "$APP_TIMEOUT" "${PHP_CMD[@]}" "$@" --log-junit "$report") < /dev/null
}

# Symfony's own runner, one component after the other (as upstream's CI runs them).
symfony_components() {
    local name=$1 which=$2 dir=$3 component rc worst=0
    export SYMFONY_DEPRECATIONS_HELPER='max[total]=999' COMPOSER_BINARY=$LOGS/composer.sh
    while IFS= read -r component; do
        say "== $component"
        phpunit_run "$dir" "$LOGS/$name.$which.${component//\//_}.xml" "$LOGS/$name.$which.cpu" ./phpunit "$component" \
            --exclude-group tty --exclude-group benchmark --exclude-group intl-data --exclude-group transient
        rc=$?
        [ "$rc" -gt 128 ] && { say "== $component ended with status $rc"; worst=$rc; }
    done < <(cd "$dir" && find src/Symfony -mindepth 2 -type f -name phpunit.xml.dist -printf '%h\n' | sort)
    unset SYMFONY_DEPRECATIONS_HELPER COMPOSER_BINARY
    return "$worst"
}

# Runs a suite under one binary; the JUnit reports and the console log go to logs/.
run_suite() {
    local name=$1 which=$2 cmd=$3 dir=$4 started rc
    php_on_path "$which"
    # A framework's compiled cache is built by the binary that uses it (Symfony: APP_CACHE_DIR).
    rm -rf "${dir:?}/var/app-tests-cache-$which"
    rm -f "${LOGS:?}/${name:?}.$which".*
    if ! configure "$name" "$dir" > "$LOGS/$name.$which.txt" 2>&1; then
        PATH=$SAVED_PATH
        printf '%s %s\n' "could-not-be-configured" 0 > "$LOGS/$name.$which.status"
        return
    fi
    started=$(date +%s)
    export APP_CACHE_DIR="$dir/var/app-tests-cache-$which"
    if [ "${cmd#@}" != "$cmd" ]; then
        "${cmd#@}" "$name" "$which" "$dir" >> "$LOGS/$name.$which.txt" 2>&1
    else
        # shellcheck disable=SC2086 # the test command is split into its words on purpose
        phpunit_run "$dir" "$LOGS/$name.$which.xml" "$LOGS/$name.$which.cpu" $cmd >> "$LOGS/$name.$which.txt" 2>&1
    fi
    rc=$?
    unset APP_CACHE_DIR
    PATH=$SAVED_PATH
    printf '%s %s\n' "$rc" "$(($(date +%s) - started))" > "$LOGS/$name.$which.status"
}

# One line per suite from the JUnit reports of both runs, and the tests that fail only here.
compare() {
    python3 - "$1" "$LOGS" "${REF_BIN:+ref}" <<'PY'
import glob, sys, xml.etree.ElementTree as ET
name, logs, has_ref = sys.argv[1], sys.argv[2], sys.argv[3] == "ref"

def load(which):
    """(results by test, exit status, wall seconds, CPU seconds) of one run; results is None
    when the run left no report."""
    try:
        rc, secs = open(f"{logs}/{name}.{which}.status").read().split()
    except OSError:
        return None, "?", "?", None
    cpu = None
    try:
        cpu = sum(float(x) for line in open(f"{logs}/{name}.{which}.cpu") for x in line.split()[:2]
                  if x.replace(".", "").isdigit())
    except OSError:
        pass
    out = {}
    reports = glob.glob(f"{logs}/{name}.{which}.xml") + glob.glob(f"{logs}/{name}.{which}.*.xml")
    for report in reports:
        try:
            root = ET.parse(report).getroot()
        except (OSError, ET.ParseError):
            continue
        for case in root.iter("testcase"):
            key = f"{case.get('classname') or case.get('class') or case.get('file') or ''}::{case.get('name')}"
            kinds = {child.tag for child in case}
            out[key] = "fail" if kinds & {"failure", "error"} else "skip" if "skipped" in kinds else "pass"
    return (out or None), rc, secs, cpu

def cell(run):
    results, rc, secs, cpu = run
    if results is None:
        return f"no report (exit {rc}, {secs}s)"
    bad = sum(1 for v in results.values() if v == "fail")
    return f"{len(results)} tests, {bad} fail, {cpu:.1f}s CPU" if cpu is not None else f"{len(results)} tests, {bad} fail"

ours = load("ours")
ref = load("ref") if has_ref else (None, "-", "-", None)
compared = ours[0] is not None and ref[0] is not None
only_here = []
if compared:
    only_here = sorted(k for k, v in ours[0].items() if v == "fail" and ref[0].get(k) == "pass")
    with open(f"{logs}/{name}.fails-only-here.txt", "w") as f:
        f.writelines(k + "\n" for k in only_here)
line = f"{name}\tours: {cell(ours)}"
if has_ref:
    line += f"\tC: {cell(ref)}"
    if compared and ours[3] and ref[3]:
        line += f"\tCPU {ours[3] / ref[3]:.1f}x C"
    line += f"\t{len(only_here)} fail only here" if compared else "\tnot compared"
print(line)
for k in only_here[:8]:
    print(f"    {k}")
if len(only_here) > 8:
    print(f"    ... {len(only_here) - 8} more in app-tests/logs/{name}.fails-only-here.txt")
done = ours[0] is not None and (compared or not has_ref)
sys.exit(0 if done and not only_here else 1)
PY
}

# ---------------------------------------------------------------------------------------------
# Sites: applications and tools that are installed and exercised rather than unit-tested. What
# each binary prints and serves is compared, item by item.
# ---------------------------------------------------------------------------------------------

# Release archives and single files, each checked against the SHA-256 pinned here.
# name | address | SHA-256 | directory inside the archive (- : a single file, used as it is)
downloads() {
    cat <<'EOF'
composer|https://getcomposer.org/download/2.11.1/composer.phar|6f1b891e1d4927d62740276c540fb490e47689e19e102c49e8da849794882f8c|-
wp-cli|https://github.com/wp-cli/wp-cli/releases/download/v2.12.0/wp-cli-2.12.0.phar|ce34ddd838f7351d6759068d09793f26755463b4a4610a5a5c0a97b68220d85c|-
wordpress|https://wordpress.org/wordpress-7.1.4.tar.gz|881e53a4681ff2cd911651b11cccdede5ae477ddf0765af5d6a1a8b219a1d835|wordpress
mediawiki|https://releases.wikimedia.org/mediawiki/1.43/mediawiki-1.43.12.tar.gz|ff2b4361425a0634e8be21c6fa6531b96bb96b7b4c291979908d98b1da59dd90|mediawiki-1.43.12
matomo|https://builds.matomo.org/matomo-6.0.0.tar.gz|9ac196f8ee12e261c251daf49dd1c64417197bbd18283280698a3fcca73541f0|matomo
EOF
}

# site | needs | what it uses: names from downloads(), or git:<checkout> of a suite | port
sites() {
    cat <<'EOF'
composer|-|composer|-
wordpress|mysql|wordpress wp-cli|8791
mediawiki|mysql|mediawiki|8792
matomo|mysql|matomo|8793
symfony-demo-pages|-|git:symfony-demo|8794
EOF
}

# A download that is on disk with the pinned checksum is not fetched again, and an archive
# that is unpacked from it is not unpacked again.
download() {
    local name=$1 url=$2 sha=$3 inner=$4 file=$APPS/_downloads/${2##*/}
    mkdir -p "$APPS/_downloads" || return 1
    if [ -f "$file" ] && [ "$(sha256sum "$file" | cut -d' ' -f1)" = "$sha" ]; then
        say "  $name: ${url##*/} on disk, not downloaded"
    else
        say "  $name: downloading $url"
        curl -fsSL --retry 2 -o "$file.part" "$url" || return 1
        if [ "$(sha256sum "$file.part" | cut -d' ' -f1)" != "$sha" ]; then
            say "  $name: what $url serves is not the pinned file (its SHA-256 differs)"
            return 1
        fi
        mv "$file.part" "$file" || return 1
    fi
    [ "$inner" = - ] && return 0
    if [ "$(cat "$APPS/$name/.app-tests-sha256" 2> /dev/null)" != "$sha" ]; then
        say "  $name: unpacking"
        rm -rf "${APPS:?}/${name:?}" "${APPS:?}/_unpack"
        mkdir -p "$APPS/_unpack" && tar -xzf "$file" -C "$APPS/_unpack" &&
            mv "$APPS/_unpack/$inner" "$APPS/$name" || return 1
        rm -rf "${APPS:?}/_unpack" # an archive may hold more than the directory (a how-to page)
        echo "$sha" > "$APPS/$name/.app-tests-sha256"
    fi
}

SERVER_PID=

# One command of a site's script, run in its working directory: what it prints and its exit
# status are kept for the comparison.
step() {
    local label=$1
    shift
    (cd "$WORK" && "$@") > "$OUT/$label.txt" 2>&1 < /dev/null
    echo "exit status $?" >> "$OUT/$label.txt"
}

# Serves the directory `$1` (through the router script `$2`, if given) under PHP_CMD.
serve() {
    local i
    if ss -ltn 2> /dev/null | grep -q "[:.]$PORT "; then
        say "port $PORT is in use"
        return 1
    fi
    (cd "$1" && exec "${PHP_CMD[@]}" -S "127.0.0.1:$PORT" ${2:+"$2"}) > "$OUT/_server.txt" 2>&1 < /dev/null &
    SERVER_PID=$!
    for i in $(seq 1 100); do
        if ! kill -0 "$SERVER_PID" 2> /dev/null; then
            say "the server ended at once, see $OUT/_server.txt"
            SERVER_PID=
            return 1
        fi
        (exec 3<> "/dev/tcp/127.0.0.1/$PORT") 2> /dev/null && return 0
        sleep 0.1
    done
    say "the server did not open port $PORT ($i tries)"
    return 1
}

stop_serving() {
    [ -n "$SERVER_PID" ] || return 0
    kill "$SERVER_PID" 2> /dev/null
    wait "$SERVER_PID" 2> /dev/null
    SERVER_PID=
}

# One request to the served site: the body and the status are kept for the comparison. The
# cookies carry from one request to the next.
page() {
    local label=$1 path=$2
    shift 2
    curl -s --max-time 300 -b "$OUT/_cookies" -c "$OUT/_cookies" -o "$OUT/$label.body" \
        -w 'HTTP status %{http_code}\n' "$@" "http://127.0.0.1:$PORT$path" > "$OUT/$label.status" 2>&1
}

# The hidden fields of the form in a fetched page, as curl arguments (one per line).
hidden_fields() {
    python3 - "$1" <<'PY'
import re, sys
html = open(sys.argv[1], encoding="utf-8", errors="replace").read()
for field in re.finditer(r"<input[^>]*type=.hidden.[^>]*>", html):
    name = re.search(r'name="([^"]*)"', field.group(0))
    value = re.search(r'value="([^"]*)"', field.group(0))
    if name:
        print("--data-urlencode\n" + name.group(1) + "=" + (value.group(1) if value else ""))
PY
}

# Fetches the form at `$2`, then posts it back with its hidden fields and the fields given.
form() {
    local label=$1 path=$2
    local -a hidden
    shift 2
    page "$label-form" "$path" -L
    mapfile -t hidden < <(hidden_fields "$OUT/$label-form.body")
    page "$label" "$path" -L "${hidden[@]}" "$@"
}

# A value out of a JSON answer that may be preceded by a notice: the path `$2` (dots) in `$1`.
json_value() {
    python3 - "$1" "$2" <<'PY'
import json, sys
text = open(sys.argv[1], encoding="utf-8", errors="replace").read()
try:
    value = json.loads(text[text.index("{"):])
    for key in sys.argv[2].split("."):
        value = value[key]
    print(value)
except (ValueError, KeyError, TypeError):
    print("")
PY
}

site_composer() {
    local -a composer=("${PHP_CMD[@]}" "$APPS/_downloads/composer.phar" --no-interaction --no-ansi)
    mkdir -p "$WORK/tests" || return 1
    # Its own home and cache, so that each binary really downloads and unpacks what it requires.
    export COMPOSER_HOME=$WORK/.composer-home COMPOSER_CACHE_DIR=$WORK/.composer-cache
    step version "${composer[@]}" --version
    step diagnose "${composer[@]}" diagnose
    step require-monolog "${composer[@]}" require --no-progress monolog/monolog
    cat > "$WORK/log.php" <<'PHP'
<?php
require __DIR__ . '/vendor/autoload.php';
use Monolog\Formatter\LineFormatter;
use Monolog\Handler\StreamHandler;
use Monolog\Level;
use Monolog\Logger;
use Monolog\Processor\IntrospectionProcessor;

$log = new Logger('app');
$handler = new StreamHandler('php://stdout', Level::Debug);
$handler->setFormatter(new LineFormatter("[%channel%.%level_name%] %message% %context% %extra%\n", null, true, true));
$log->pushHandler($handler);
$log->pushProcessor(new IntrospectionProcessor());
$log->info('hello', ['user' => 'someone', 'n' => 3]);
$log->warning('array context', ['list' => [1, 2, ['x' => true]]]);
try {
    throw new RuntimeException('boom');
} catch (Throwable $e) {
    $log->error('caught', ['exception' => $e->getMessage()]);
}
$log->withName('child')->debug('renamed channel');
echo get_class($log->getHandlers()[0]), ' ', count($log->getProcessors()), " processor(s)\n";
PHP
    step monolog-run "${PHP_CMD[@]}" log.php
    step require-phpunit "${composer[@]}" require --dev --no-progress 'phpunit/phpunit:^12'
    cat > "$WORK/tests/SampleTest.php" <<'PHP'
<?php
use PHPUnit\Framework\Attributes\DataProvider;
use PHPUnit\Framework\Attributes\RunInSeparateProcess;
use PHPUnit\Framework\TestCase;

interface Greeter
{
    public function greet(string $name): string;
}

final class SampleTest extends TestCase
{
    public function testAssertions(): void
    {
        $this->assertSame(3, 1 + 2);
        $this->assertEquals(['a' => 1, 'b' => [2, 3]], ['b' => [2, 3], 'a' => 1]);
        $this->assertStringContainsString('ell', 'hello');
        $this->assertMatchesRegularExpression('/^h\w+o$/', 'hello');
        $this->assertCount(2, [1, 2]);
        $this->assertInstanceOf(ArrayObject::class, new ArrayObject([]));
        $this->assertEqualsWithDelta(0.3, 0.1 + 0.2, 1e-9);
        $this->assertJsonStringEqualsJsonString('{"a":1,"b":2}', '{"b":2,"a":1}');
    }

    public static function additions(): array
    {
        return ['small' => [1, 2, 3], 'zero' => [0, 0, 0], 'negative' => [-1, -2, -3], 'float' => [0.5, 0.25, 0.75]];
    }

    #[DataProvider('additions')]
    public function testAdd(int|float $a, int|float $b, int|float $sum): void
    {
        $this->assertSame($sum, $a + $b);
    }

    public function testException(): void
    {
        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('bad value');
        throw new InvalidArgumentException('bad value 42');
    }

    public function testMock(): void
    {
        $greeter = $this->createMock(Greeter::class);
        $greeter->expects($this->once())->method('greet')->with('World')->willReturn('Hi World');
        $this->assertSame('Hi World', $greeter->greet('World'));
    }

    #[RunInSeparateProcess]
    public function testIsolated(): void
    {
        define('ONLY_IN_CHILD', 1);
        $this->assertTrue(defined('ONLY_IN_CHILD'));
    }

    public function testDeliberateFailure(): void
    {
        $this->assertSame(['x' => 1, 'y' => 2], ['x' => 1, 'y' => 3], 'arrays differ');
    }
}
PHP
    step sample-tests "${PHP_CMD[@]}" vendor/bin/phpunit --no-configuration --colors=never tests
    step vendor-tree sh -c 'find vendor -type f | LC_ALL=C sort'
    unset COMPOSER_HOME COMPOSER_CACHE_DIR
}

site_wordpress() {
    local -a wp=("${PHP_CMD[@]}" "$APPS/_downloads/wp-cli-2.12.0.phar" "--path=$WORK" --no-color)
    cp -a "$APPS/wordpress" "$WORK" || return 1
    mysql_do -e "DROP DATABASE IF EXISTS \`$DB\`; CREATE DATABASE \`$DB\`" || return 1
    step wp-config-create "${wp[@]}" config create "--dbname=$DB" "--dbuser=$MYSQL_TEST_USER" \
        "--dbpass=${MYSQL_TEST_PASSWD:-}" "--dbhost=$MYSQL_TEST_HOST" --skip-check
    # No request leaves the machine while the pages are fetched, and nothing runs on a timer.
    step wp-config-no-external "${wp[@]}" config set WP_HTTP_BLOCK_EXTERNAL true --raw
    step wp-config-no-cron "${wp[@]}" config set DISABLE_WP_CRON true --raw
    step wp-config-no-updater "${wp[@]}" config set AUTOMATIC_UPDATER_DISABLED true --raw
    step wp-core-install "${wp[@]}" core install "--url=http://127.0.0.1:$PORT" '--title=App Tests' \
        --admin_user=admin --admin_password=apptests-pass-123 --admin_email=admin@example.com --skip-email
    serve "$WORK" || return 1
    page home /
    page post '/?p=1'
    page page '/?page_id=2'
    page feed '/?feed=rss2'
    page search '/?s=hello'
    page not-found '/?p=999999'
    page rest-root /wp-json/
    page rest-posts /wp-json/wp/v2/posts
    page login-form /wp-login.php
    page login-post /wp-login.php --cookie 'wordpress_test_cookie=WP%20Cookie%20check' \
        --data 'log=admin&pwd=apptests-pass-123&wp-submit=Log+In&redirect_to=%2Fwp-admin%2F&testcookie=1'
    page admin /wp-admin/
    page admin-posts /wp-admin/edit.php
    page admin-new-post /wp-admin/post-new.php
    page admin-options /wp-admin/options-general.php
    page admin-plugins /wp-admin/plugins.php
    page admin-themes /wp-admin/themes.php
    page admin-users /wp-admin/users.php
    page admin-health /wp-admin/site-health.php
    page admin-ajax-heartbeat /wp-admin/admin-ajax.php --data 'action=heartbeat&interval=60'
    stop_serving
}

site_mediawiki() {
    local token
    cp -a "$APPS/mediawiki" "$WORK" || return 1
    mysql_do -e "DROP DATABASE IF EXISTS \`$DB\`; CREATE DATABASE \`$DB\`" || return 1
    step install "${PHP_CMD[@]}" maintenance/run.php install --dbtype=mysql "--dbserver=$MYSQL_TEST_HOST" \
        "--dbname=$DB" "--dbuser=$MYSQL_TEST_USER" "--dbpass=${MYSQL_TEST_PASSWD:-}" \
        "--installdbuser=$MYSQL_TEST_USER" "--installdbpass=${MYSQL_TEST_PASSWD:-}" \
        "--server=http://127.0.0.1:$PORT" --scriptpath= --pass=apptests-pass-123 --lang=en 'App Tests' Admin
    serve "$WORK" || return 1
    page main '/index.php?title=Main_Page'
    page version '/index.php?title=Special:Version'
    page recent-changes '/index.php?title=Special:RecentChanges'
    page all-pages '/index.php?title=Special:AllPages'
    page not-found '/index.php?title=No_Such_Page_Xyz'
    page api-siteinfo '/api.php?action=query&meta=siteinfo&format=json&formatversion=2'
    page api-opensearch '/api.php?action=opensearch&search=Main&format=json'
    page load-startup '/load.php?modules=startup&only=scripts&lang=en&skin=vector-2022'
    page rest-search '/rest.php/v1/search/page?q=main&limit=5'
    page login-token '/api.php?action=query&meta=tokens&type=login&format=json'
    token=$(json_value "$OUT/login-token.body" query.tokens.logintoken)
    page login-post /api.php --data-urlencode action=clientlogin --data-urlencode username=Admin \
        --data-urlencode password=apptests-pass-123 --data-urlencode "loginreturnurl=http://127.0.0.1:$PORT/" \
        --data-urlencode "logintoken=$token" --data-urlencode format=json
    page csrf-token '/api.php?action=query&meta=tokens&format=json'
    token=$(json_value "$OUT/csrf-token.body" query.tokens.csrftoken)
    page edit-post /api.php --data-urlencode action=edit --data-urlencode title=Sandbox \
        --data-urlencode text=$'== App tests sandbox ==\nDeterministic content line one.\n* a bullet\n[[Main Page]]' \
        --data-urlencode 'summary=app-tests round trip' --data-urlencode "token=$token" --data-urlencode format=json
    page sandbox '/index.php?title=Sandbox'
    page sandbox-history '/index.php?title=Sandbox&action=history'
    page watchlist '/index.php?title=Special:Watchlist'
    page preferences '/index.php?title=Special:Preferences'
    stop_serving
}

site_matomo() {
    local token=0123456789abcdef0123456789abcdef salt hash schema=Mysql
    cp -a "$APPS/matomo" "$WORK" || return 1
    mysql_do -e "DROP DATABASE IF EXISTS \`$DB\`; CREATE DATABASE \`$DB\`" || return 1
    mysql_do -N -e 'SELECT VERSION()' | grep -qi mariadb && schema=Mariadb
    serve "$WORK" || return 1
    # The installation wizard, page by page.
    page install-welcome '/index.php?action=welcome' -L
    page install-system-check '/index.php?action=systemCheck' -L
    form install-database '/index.php?action=databaseSetup' --data-urlencode "host=$MYSQL_TEST_HOST" \
        --data-urlencode "username=$MYSQL_TEST_USER" --data-urlencode "password=${MYSQL_TEST_PASSWD:-}" \
        --data-urlencode "dbname=$DB" --data-urlencode tables_prefix=matomo_ --data-urlencode adapter=MYSQLI \
        --data-urlencode "schema=$schema" --data-urlencode 'submit=Next »'
    page install-tables '/index.php?action=tablesCreation&module=Installation' -L
    form install-superuser '/index.php?action=setupSuperUser&module=Installation' --data-urlencode login=admin \
        --data-urlencode password=apptests-pass-123 --data-urlencode password_bis=apptests-pass-123 \
        --data-urlencode email=admin@example.com --data-urlencode subscribe_newsletter_piwikorg=0 \
        --data-urlencode subscribe_newsletter_professionalservices=0 --data-urlencode 'submit=Next »'
    form install-first-site '/index.php?action=firstWebsiteSetup&module=Installation' \
        --data-urlencode 'siteName=App Tests' --data-urlencode url=http://example.com --data-urlencode timezone=UTC \
        --data-urlencode ecommerce=0 --data-urlencode 'submit=Next »'
    page install-tracking-code '/index.php?action=trackingCode&module=Installation' -L
    form install-finished '/index.php?action=finished&module=Installation' --data-urlencode do_not_track=1 \
        --data-urlencode anonymise_ip=1 --data-urlencode 'submit=Continue to Matomo »'
    # A fixed API token for the reports: Matomo stores the SHA-512 of the token and its salt.
    salt=$(sed -n 's/^salt = "\{0,1\}\([0-9a-f]*\).*/\1/p' "$WORK/config/config.ini.php" 2> /dev/null | head -1)
    hash=$(printf '%s%s' "$token" "$salt" | sha512sum | cut -d' ' -f1)
    mysql_do "$DB" -e "INSERT IGNORE INTO matomo_user_token_auth (login, description, password, hash_algo, system_token, secure_only, date_created) VALUES ('admin', 'app-tests', '$hash', 'sha512', 0, 0, NOW())"
    page login-page '/index.php?module=Login&action=index'
    page login-post '/index.php?module=Login&action=index' -L --data-urlencode form_login=admin \
        --data-urlencode form_password=apptests-pass-123 \
        --data-urlencode "form_nonce=$(sed -n 's/.*name="form_nonce"[^>]*value="\([^"]*\)".*/\1/p' "$OUT/login-page.body" | head -1)"
    page dashboard '/index.php?module=CoreHome&action=index&idSite=1&period=day&date=today' -L
    page track-1 '/matomo.php?idsite=1&rec=1&url=http%3A%2F%2Fexample.com%2Fpage1&action_name=Page1&_id=aaaaaaaaaaaaaaaa&send_image=0'
    page track-2 '/matomo.php?idsite=1&rec=1&url=http%3A%2F%2Fexample.com%2Fpage2&action_name=Page2&_id=aaaaaaaaaaaaaaaa&send_image=0'
    page track-gif '/matomo.php?idsite=1&rec=1&url=http%3A%2F%2Fexample.com%2Fpage1&action_name=Page1&_id=aaaaaaaaaaaaaaaa'
    page api-version "/index.php?module=API&method=API.getMatomoVersion&format=JSON&token_auth=$token"
    page api-visits "/index.php?module=API&method=VisitsSummary.get&idSite=1&period=day&date=today&format=JSON&token_auth=$token"
    page api-actions "/index.php?module=API&method=Actions.getPageUrls&idSite=1&period=day&date=today&format=JSON&token_auth=$token"
    page api-live "/index.php?module=API&method=Live.getLastVisitsDetails&idSite=1&period=day&date=today&format=JSON&token_auth=$token&filter_limit=5"
    step visits-in-database mysql_do -N "$DB" -e 'SELECT COUNT(*) FROM matomo_log_visit'
    stop_serving
    step console-archive "${PHP_CMD[@]}" console core:archive "--url=http://127.0.0.1:$PORT/"
    step console-diagnostics "${PHP_CMD[@]}" console diagnostics:run
}

site_symfony_demo_pages() {
    cp -a "$APPS/symfony-demo" "$WORK" || return 1
    git -C "$WORK" checkout -q -- data
    rm -rf "${WORK:?}/var/cache" "${WORK:?}/var/log"
    export APP_ENV=prod APP_DEBUG=0
    step cache-clear "${PHP_CMD[@]}" bin/console cache:clear --no-ansi
    serve "$WORK/public" "$WORK/public/index.php" || { unset APP_ENV APP_DEBUG; return 1; }
    page home /
    page home-en /en
    page blog /en/blog/
    page blog-rss /en/blog/rss.xml
    page blog-page-2 /en/blog/page/2
    page blog-search '/en/blog/search?q=lorem'
    page blog-french /fr/blog/
    page login /en/login
    page admin-needs-login /en/admin/post/
    page not-found /en/no-such-page
    stop_serving
    unset APP_ENV APP_DEBUG
}

# Runs a site's script under one binary, in a fresh working copy and a fresh database. Both
# binaries get the same directory and the same database name, one after the other: what an
# application derives from its path (the name of a compiled asset) is then the same.
run_site() {
    local name=$1 which=$2 rc started
    php_on_path "$which" 1G
    PORT=$3
    OUT=$LOGS/$name.$which
    WORK=$APPS/_work/$name
    DB=apptests_${name//-/_}
    rm -rf "${OUT:?}" "${WORK:?}"
    mkdir -p "$OUT" "$APPS/_work" || { PATH=$SAVED_PATH; return 1; }
    started=$(date +%s)
    "site_${name//-/_}" > "$OUT/_script.txt" 2>&1
    rc=$?
    stop_serving
    PATH=$SAVED_PATH
    printf '%s %s\n' "$rc" "$(($(date +%s) - started))" > "$OUT/_status"
}

# One line per site: the items both runs produced, and the ones that differ once what has to
# differ (times, tokens, paths, the two version numbers) is taken out.
compare_site() {
    python3 - "$1" "$LOGS" "$APPS" "$PHP_BIN" "$REF_BIN" <<'PY'
import difflib, os, re, subprocess, sys
name, logs, apps, ours_bin, ref_bin = sys.argv[1:6]
runs = ("ours", "ref")

def version(binary):
    try:
        return subprocess.run([binary, "-d", "memory_limit=1G", "-r", "echo PHP_VERSION;"],
                              capture_output=True, text=True, timeout=30).stdout.strip()
    except OSError:
        return ""

versions = {"ours": version(ours_bin), "ref": version(ref_bin)}
binaries = {"ours": ours_bin, "ref": ref_bin}

# Notices php-src master raises and 8.5 does not: a difference between the two C versions, not
# between ours and C.
MASTER_ONLY = [
    # (As a line of a JSON log first: the next rule would cut such a line in two.)
    r"^\{\"message\":\"Deprecated: [^\n]*deprecated since 8\.6[^\n]*\n",
    r"(?:^\n)?^(?:<br />\n)?(?:<b>)?Deprecated(?:</b>)?: +[^\n]*deprecated since 8\.6[^\n]*\n",
    r"(?:^\n)?^(?:<br />\n)?(?:<b>)?Warning(?:</b>)?: +Class [A-Za-z0-9_\\]+ implementing SessionHandlerInterface is missing the (?:create_sid|validateId)\(\) method[^\n]*\n",
]
EVERYWHERE = [
    (r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?", "TIMESTAMP"),
    (r"(?:Mon|Tue|Wed|Thu|Fri|Sat|Sun), \d{1,2} (?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) \d{4} \d{2}:\d{2}:\d{2} (?:GMT|[+-]\d{4})", "TIMESTAMP"),
    (r"\b\d{1,2}:\d{2}(?::\d{2})?(?: ?[ap]m)?\b", "TIME"),
    (r"\b[0-9a-f]{10,128}\b", "HEX"),
    (r"Time: [^,\n]+, Memory: [^\n]+", "Time: T, Memory: M"),
    (r"\b\d+(?:\.\d+)? ?(?:ms|milliseconds?|seconds?|secs?|MiB|MB|KiB|kB)\b", "N UNITS"),
]
PER_SITE = {
    "wordpress": [
        (r'(<div class="(?:hh|mn|ss)">)\d+', r"\1N"),
        (r'((?:name|id)="(?:hidden_|cur_)?(?:hh|mn|ss)"[^>]{0,120}?value=")\d+', r"\1N"),
        # A known difference of the host, not of the two engines: where the system's libgd has
        # no AVIF support, C's GD still says it has (and fails when asked to write one); ours
        # says it has not, and WordPress then warns about AVIF uploads.
        (r'"avif_upload_error":true,', ""),
    ],
    "mediawiki": [
        (r'"wgRequestId":"[^"]*"', '"wgRequestId":"X"'),
        (r'"wgBackendResponseTime":\d+', '"wgBackendResponseTime":X'),
        (r"<!--\s*\nNewPP limit report.*?-->", "<!--NEWPP-->"),
        (r"<!--\s*\nTransclusion expansion time report.*?-->", "<!--TRANSCLUSION-->"),
        (r"<!-- Saved in parser cache.*?-->", "<!--PARSER CACHE-->"),
        (r'"(cpu|wall)time":"[^"]*"', r'"\1time":"X"'),
        (r'"timestamp":"\d{14}"', '"timestamp":"X"'),
        (r"(?<=from=)\d{14}", "X"),
        (r"(?<=from&quot;:&quot;)\d{14}", "X"),
    ],
    "matomo": [
        (r'"(serverTimePretty|serverDatePretty|serverDatePrettyFirstAction|serverTimePrettyFirstAction|firstActionTimestamp|lastActionTimestamp|lastActionDateTime|visitServerHour|timestamp|prettyDate)":"?[^",]*"?', r'"\1":X'),
        (r'"(visitDuration|visitDurationPretty|secondsSinceFirstVisit|secondsSinceLastVisit|daysAgo)":"?[^",]*"?', r'"\1":X'),
        (r'"(generationTimeMilliseconds|generationTime)":"?[^",]*"?', r'"\1":X'),
        (r'"(idVisit|pageId|pageIdAction|timeSpent|sum_time_spent|avg_time_on_page|avg_time_on_site|sum_visit_length|serverTimestamp)":\d+', r'"\1":X'),
        (r'"(entry_sum_visit_length|timeSpentPretty|visitLocalTime|visitLocalHour)":"[^"]*"', r'"\1":"X"'),
        (r"MATOMO_SESSID=[^;\"]*", "MATOMO_SESSID=X"),
        (r"Time elapsed: [\d.]+s", "Time elapsed: Ns"),
        (r"Peak memory: [\d.]+ ?[KMG]", "Peak memory: N M"),
        (r"\b\d+s ago", "Ns ago"),
        (r"^(INFO|ERROR|WARNING)(\s+\[[^\]]*\])\s+\d+", r"\1\2 PID"),
        (r"\d{4}-\d{2}-\d{2}", "DATE"),
    ],
}

def normal(text, which):
    for a, b in ((f"{logs}/{name}.{which}", "OUT"), (f"{logs}/ours-ini-1G", "INI"), (f"{logs}/ref-ini", "INI"),
                 (f"{logs}/ref-php-ini", "INI"), (binaries[which], "PHP")):
        text = text.replace(a, b)
    if versions[which]:
        text = text.replace(versions[which], "PHP_VERSION")
    for pattern in MASTER_ONLY:
        text = re.sub(pattern, "", text, flags=re.M)
    for pattern, to in PER_SITE.get(name, []) + EVERYWHERE:
        text = re.sub(pattern, to, text, flags=re.S | re.M)
    return text

def lines(text):
    """The lines of an item for the listing of differences: a very long one (a page's inline
    JSON) is cut where its parts meet, so that the differing part shows."""
    out = []
    for line in text.splitlines():
        out.extend(re.sub(r'(?<=>)(?=<)|(?<=,)(?=")|(?<=,)(?=&quot;)', "\n", line).split("\n") if len(line) > 300 else [line])
    return out

def items(which):
    """What one run produced, by label: page bodies with their status, and step transcripts."""
    out, folder = {}, f"{logs}/{name}.{which}"
    try:
        names = sorted(os.listdir(folder))
    except OSError:
        return None
    for file in names:
        if file.startswith("_") or not file.endswith((".body", ".txt")):
            continue
        text = open(f"{folder}/{file}", "rb").read().decode("utf-8", "replace")
        if file.endswith(".body"):
            try:
                text = open(f"{folder}/{file[:-5]}.status").read() + text
            except OSError:
                pass
        out[file.rsplit(".", 1)[0]] = normal(text, which)
    return out

def status(which):
    try:
        return open(f"{logs}/{name}.{which}/_status").read().split()
    except OSError:
        return ["?", "?"]

ours, ref = items("ours"), items("ref")
if not ours or not ref:
    print(f"{name}\tours: {len(ours or {})} items (exit {status('ours')[0]})\tC: {len(ref or {})} items (exit {status('ref')[0]})\tnot compared")
    sys.exit(1)
labels = sorted(set(ours) | set(ref))
differ = [l for l in labels if ours.get(l) != ref.get(l)]
with open(f"{logs}/{name}.differences.txt", "w") as f:
    for label in differ:
        a, b = ref.get(label), ours.get(label)
        if a is None or b is None:
            f.write(f"=== {label}: only under {'ours' if a is None else 'C'}\n")
            continue
        diff = list(difflib.unified_diff(lines(a), lines(b), "C", "ours", lineterm="", n=1))
        f.write(f"=== {label}: {sum(1 for d in diff if d[:1] in '+-' and d[:3] not in ('+++', '---'))} lines differ\n")
        f.writelines(line[:400] + "\n" for line in diff[:80])
print(f"{name}\t{len(labels)} items, ours {status('ours')[1]}s, C {status('ref')[1]}s\t{len(differ)} differ from C")
if differ:
    print("    " + ", ".join(differ[:14]) + (f", ... {len(differ) - 14} more" if len(differ) > 14 else ""))
    print(f"    details in app-tests/logs/{name}.differences.txt")
sys.exit(1 if differ else 0)
PY
}

mode=run
names=()
for arg in "$@"; do
    case $arg in
    --list) mode=list ;;
    --fetch-only) mode=fetch ;;
    --verify) mode=verify ;;
    -h | --help) sed -n '5,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) die "unknown option $arg" ;;
    *) names+=("$arg") ;;
    esac
done

verify
[ "$mode" = verify ] && { say "git tracks nothing under app-tests/apps/ or app-tests/logs/"; exit 0; }

if [ "$mode" = list ]; then
    while IFS='|' read -r name checkout url sha needs cmd; do
        if [ -d "$APPS/$checkout/.git" ] && [ "$(git -C "$APPS/$checkout" rev-parse -q --verify HEAD 2> /dev/null)" = "$sha" ]; then
            state="on disk"
        else
            state="not fetched"
        fi
        printf '%-30s %s  %-11s  %-5s  %s\n' "$name" "${sha:0:12}" "$state" "$needs" "$url"
    done < <(suites)
    while IFS='|' read -r name needs uses port; do
        printf '%-30s %-12s  %-11s  %-5s  %s\n' "$name" "(site)" "" "$needs" "uses: $uses"
    done < <(sites)
    exit 0
fi

for name in "${names[@]}"; do
    { suites; sites; } | grep -q "^$name|" || die "no suite or site named $name (see --list)"
done

PHP_BIN=${PHP_BIN:-$([ -x "$ROOT/target/release/php" ] && echo "$ROOT/target/release/php" || echo "$ROOT/target/debug/php")}
REF_BIN=${REF_BIN-/usr/bin/php8.5}
[ -x "$PHP_BIN" ] || die "no php binary at $PHP_BIN (build it, or set PHP_BIN)"
[ -z "$REF_BIN" ] || [ -x "$REF_BIN" ] || die "no reference binary at $REF_BIN (set REF_BIN, or REF_BIN= to run without one)"
COMPOSER=$(command -v composer) || die "composer is not installed"
mkdir -p "$APPS" "$LOGS/ref-ini" "$LOGS/ref-php-ini" "$LOGS/ours-ini-1G" "$LOGS/ours-ini-2G" || die "cannot create $APPS"
settings_ini 1G > "$LOGS/ours-ini-1G/zz-app-tests.ini"
settings_ini 2G > "$LOGS/ours-ini-2G/zz-app-tests.ini"
echo '; empty on purpose: see settings_ini in run-all-app-tests.sh' > "$LOGS/ref-php-ini/php.ini"

# Composer runs under the reference binary when there is one (installing is not what is tested).
if [ -n "$REF_BIN" ]; then
    settings_ini 1G > "$LOGS/ref-ini/zz-app-tests.ini"
    # Its own ini directory stays in force (the extensions are loaded from there), ours is added.
    REF_SCAN=$("$REF_BIN" -d memory_limit=1G -r 'echo PHP_CONFIG_FILE_SCAN_DIR;')
    REF_SCAN=${REF_SCAN:+$REF_SCAN:}$LOGS/ref-ini
    COMPOSER_PHP=("$REF_BIN" -d memory_limit=1G)
else
    REF_SCAN=
    COMPOSER_PHP=("$PHP_BIN" -d memory_limit=2G)
fi
# For the runners that call Composer themselves.
printf '#!/bin/sh\nexec %s "$@"\n' "$(printf '%q ' "${COMPOSER_PHP[@]}" "$COMPOSER")" > "$LOGS/composer.sh"
chmod +x "$LOGS/composer.sh"

say "ours: $PHP_BIN"
say "C:    ${REF_BIN:-(none)}"
status=0
summary=()
declare -A fetched=()
while IFS='|' read -r name checkout url sha needs cmd; do
    if [ ${#names[@]} -gt 0 ] && ! printf '%s\n' "${names[@]}" | grep -qx "$name"; then
        continue
    fi
    say "$name"
    if [ "$needs" = mysql ] && ! have_mysql; then
        say "  $name: left out, it needs a database (set MYSQL_TEST_HOST, MYSQL_TEST_USER, MYSQL_TEST_PASSWD)"
        summary+=("$name	left out: needs a database")
        continue
    fi
    if [ -z "${fetched[$checkout]:-}" ]; then
        fetch "$checkout" "$url" "$sha" && fetched[$checkout]=yes || fetched[$checkout]=no
    fi
    if [ "${fetched[$checkout]}" = no ]; then
        summary+=("$name	could not be fetched or installed")
        status=1
        continue
    fi
    [ "$mode" = fetch ] && continue
    say "  $name: running under ours"
    run_suite "$name" ours "$cmd" "$APPS/$checkout"
    if [ -n "$REF_BIN" ]; then
        say "  $name: running under C"
        run_suite "$name" ref "$cmd" "$APPS/$checkout"
    fi
    line=$(compare "$name") || status=1
    summary+=("$line")
done < <(suites)

while IFS='|' read -r name needs uses port; do
    if [ ${#names[@]} -gt 0 ] && ! printf '%s\n' "${names[@]}" | grep -qx "$name"; then
        continue
    fi
    say "$name"
    if [ "$needs" = mysql ] && ! have_mysql; then
        say "  $name: left out, it needs a database (set MYSQL_TEST_HOST, MYSQL_TEST_USER, MYSQL_TEST_PASSWD)"
        summary+=("$name	left out: needs a database")
        continue
    fi
    ok=yes
    for use in $uses; do
        if [ "${use#git:}" != "$use" ]; then
            checkout=${use#git:}
            if [ -z "${fetched[$checkout]:-}" ]; then
                IFS='|' read -r _ _ url sha _ _ < <(suites | grep "^[^|]*|$checkout|" | head -1)
                fetch "$checkout" "$url" "$sha" && fetched[$checkout]=yes || fetched[$checkout]=no
            fi
            [ "${fetched[$checkout]}" = yes ] || ok=no
        else
            IFS='|' read -r _ url sha inner < <(downloads | grep "^$use|")
            download "$use" "$url" "$sha" "$inner" || ok=no
        fi
    done
    if [ "$ok" = no ]; then
        summary+=("$name	could not be fetched")
        status=1
        continue
    fi
    [ "$mode" = fetch ] && continue
    say "  $name: running under ours"
    run_site "$name" ours "$port"
    if [ -z "$REF_BIN" ]; then
        summary+=("$name	ran under ours only: its output is in app-tests/logs/$name.ours/")
        continue
    fi
    say "  $name: running under C"
    run_site "$name" ref "$port"
    line=$(compare_site "$name") || status=1
    summary+=("$line")
done < <(sites)

[ "$mode" = fetch ] && exit "$status"
say
say "Results (logs and JUnit reports in app-tests/logs/):"
printf '%s\n' "${summary[@]}"
exit "$status"
