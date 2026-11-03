# phpt-runner

A Rust port of php-src's `run-tests.php`. It runs `.phpt` tests from `modules/php-src` against a
PHP binary (ours by default, or any other with `--php`) and compares the output with exactly the
rules `run-tests.php` uses.

## Usage

```
cargo build -p php-cli -p phpt-runner
target/debug/phpt-runner [OPTIONS] <PATHS>...
```

| Option | Meaning |
|---|---|
| `--php <PATH>` | Binary under test. Default: `$TEST_PHP_EXECUTABLE`, else `php` or `php-cli` next to the runner (`target/debug/`). |
| `-j, --jobs <N>` | Parallel workers (default: number of CPUs). |
| `--filter <SUBSTR>` | Only tests whose path contains the substring (repeatable). |
| `--list` | Print the selected tests and exit. |
| `--show-diff` | Print the diff of each failure (automatic when running a single test). |
| `--json <OUT>` | Write a JSON report (totals, per-directory counts, every result). |
| `--baseline <FILE>` | Known-failures file; see below. |
| `--update-baseline` | Rewrite the baseline from this run. |
| `--timeout <SECS>` | Per-process timeout (default 60). |
| `--srcdir <DIR>` | php-src root (default: the nearest ancestor of the first path containing `run-tests.php`). Tests run with it as the working directory and are reported relative to it. |
| `-d NAME=VALUE` | Extra INI setting for every test, like `run-tests.php -d`. |
| `-q, --quiet` | Only print non-PASS/SKIP result lines. |
| `--no-artifacts` | Do not write `.out`/`.exp`/`.diff` or keep the generated `.php` for failures. |
| `--summary-depth <N>` | Group the summary table by the first N path components. |
| `--skip-slow` | Export `SKIP_SLOW_TESTS=1`. |
| `--no-color` | Plain output (also when `NO_COLOR` is set or stdout is not a terminal). |
| `--scope <FILE>` | Out-of-scope prefixes (default: `phpt-scope.txt` in the php-src root or an ancestor, i.e. the repository root). |
| `--no-scope` | Run and count every selected test. |
| `--env KEY=VALUE` | Extra environment variable for every test (repeatable). |
| `--no-snapshot` | Run the binary in place. By default it is hard-linked (or copied) into a private temp directory first, so a concurrent `cargo build` relinking `target/debug/php` cannot break a long run. |
| `--resume <FILE>` | Append each result to a JSON-lines log as it completes; tests already in the log are not rerun (their results are reused), so a killed full-suite run can be continued. |

Examples:

```
# Our binary on the Zend and language suites, compact output, per-suite table:
target/debug/phpt-runner -q --summary-depth 2 modules/php-src/Zend/tests modules/php-src/tests

# One test with its diff:
target/debug/phpt-runner modules/php-src/Zend/tests/add_001.phpt

# Validate the runner itself against the C implementation:
target/debug/phpt-runner --php /usr/bin/php modules/php-src/tests/run-test
```

Output is one `PASS`/`FAIL`/`SKIP`/`XFAIL`/`WARN`/`BORK` line per test in run-tests.php's format,
then a per-directory table (total, pass, fail, skip, xfail, bork, pass% of the tests that ran)
and run-tests.php's summary block. For failing tests `<name>.out`, `<name>.exp`, `<name>.diff`
are written next to the test and the generated `<name>.php` is kept (all gitignored, inside the
php-src clone). Without a baseline the exit code is 1 if anything FAILs or BORKs.

## Scope

`phpt-scope.txt` at the repository root lists php-src path prefixes that are out of scope by
project decision (FEATURE_GAP.md rows with Pri `out`: opcache/JIT, ffi, server-backed DB and
protocol clients except mysqli, readline/tidy/enchant/com_dotnet/imap, php-src's test-helper
extensions, non-CLI SAPIs except apache2handler). Tests under those prefixes are not run; they
are reported as `OUT` (a separate count in the table, summary and JSON), excluded from every
pass-rate denominator and never written to the baseline. `--no-scope` disables this.

## Environment for service-backed suites

The ext/mysqli tests need the local MariaDB (see CLAUDE.md, "Test services"); without these
variables they skip:

```
target/debug/phpt-runner -q -j 4 \
    --env MYSQL_TEST_HOST=127.0.0.1 --env MYSQL_TEST_USER=phptest \
    --env MYSQL_TEST_PASSWD=phptest --env MYSQL_TEST_DB=test \
    modules/php-src/ext/mysqli
```

(`--env` is a convenience; exported variables reach the tests too.)

Like run-tests.php (master and 8.3), SKIPIF and CLEAN run with the base INI settings plus `-d`
options but *not* the test's `--INI--` section. So the four LOAD DATA LOCAL tests, which set
`mysqli.allow_local_infile=1` only in `--INI--`, skip in upstream CI too (their SKIPIF probe
needs it). To exercise them anyway, pass it to every process: `-d mysqli.allow_local_infile=1`.

## Extension directory

A binary that loads no php.ini (ours) gets `-d extension_dir=/nonexistent/phpt-runner-extension-dir`,
so its built-in `extension_dir` (which may point at the system PHP's modules) cannot make
`--EXTENSIONS--` lines or `dl()`-based SKIPIF checks pick up the system's `.so` files: tests
needing an extension the binary lacks are SKIPPED. Binaries that load php.ini files (e.g.
`/usr/bin/php`, whose `extension=` lines need the real directory) keep their own.
`-d extension_dir=...` overrides either way.

## Baseline workflow

`phpt-baseline.txt` at the repository root lists the in-scope tests (paths relative to
php-src) known to FAIL or BORK with our binary across the whole suite
(`Zend`, `tests`, `ext`, `sapi`):

```
target/debug/phpt-runner -q -j 4 --no-artifacts --baseline phpt-baseline.txt \
    --env MYSQL_TEST_HOST=127.0.0.1 --env MYSQL_TEST_USER=phptest \
    --env MYSQL_TEST_PASSWD=phptest --env MYSQL_TEST_DB=test \
    modules/php-src/Zend modules/php-src/tests modules/php-src/ext modules/php-src/sapi
```

A full run takes over an hour with the debug binary; add `--resume /tmp/full.jsonl` so it can be
continued if it is interrupted. A subset run (e.g. just `modules/php-src/Zend/tests`) can be
checked against the same file.

The run fails (exit 1) only if a test outside the baseline fails (`NEW FAILURE`) or a baseline
test now passes (`UNEXPECTED PASS`, so the baseline can shrink). After making tests pass, rerun
with `--update-baseline` and commit the smaller file. Entries for tests that were not part of the
run are kept, so updating from a partial run is safe.

## Updating FEATURE_GAP.md

```
target/debug/phpt-runner -q --json /tmp/results.json modules/php-src/Zend/tests modules/php-src/tests
scripts/update_feature_gap.py /tmp/results.json --dry-run      # show what would change
scripts/update_feature_gap.py /tmp/results.json                # rewrite the % column
scripts/update_feature_gap.py /tmp/results.json --summary      # headline table for README.md
```

The script maps directory prefixes to row names (the `MAPPING` table in the script); rows
without a mapping, with no tests in the report, or entirely out of scope are left alone.
Out-of-scope tests are never counted. The percentage is passed/total by default
(`--denominator ran` excludes skipped/borked tests). `--summary` prints in-scope
total/pass/fail/skip and pass rates per suite (Zend/tests, tests/, ext/, sapi/) and overall.

## Fidelity notes

- EXPECTF is converted with the same `preg_quote` + `%r...%r` + `strtr` algorithm and matched
  with PCRE2 in byte mode with `DOTALL`, as `preg_match('/^...$/s')` does. Output and
  expectation are `trim()`med and CRLF-normalised first.
- INI defaults are run-tests.php's `$ini_overwrites`; `error_reporting` uses the binary's own
  `E_ALL` (falling back to master's 30719).
- The environment matches run-tests.php (`TEST_PHP_EXECUTABLE`, `TEST_PHP_EXTRA_ARGS`,
  `SKIP_ONLINE_TESTS=1`, `SKIP_IO_CAPTURE_TESTS` when not on a terminal,
  `TEST_PHP_SHARED_CACHE_DIR`, empty variables unset, request variables removed for SKIPIF and
  CLEAN).
- Tests are dispatched in batches of up to 4 consecutive files, and `CONFLICTS` keys (file or
  directory) are honoured; tests conflicting with `all` run alone at the end.
- Child processes run in their own process group (killed as a group on timeout) with
  `RLIMIT_CORE=0`, an `RLIMIT_CPU` backstop just above the timeout and a parent-death signal
  (so nothing is orphaned if the runner is killed); transient spawn failures (ENOMEM, EAGAIN,
  ETXTBSY) are retried with backoff; a death by signal appends `Termsig=N` like run-tests.php.
- Deliberate deviation: a test that timed out is not retried as "flaky".
- Not supported: CGI (`--GET--`, `--POST--`, `--COOKIE--`, `--EXPECTHEADERS--`, ...) and
  `--PHPDBG--` tests are parsed but SKIPPED; no valgrind/ASAN leak detection, `--repeat`,
  opcache file-cache/preload modes or JUnit output.

## Validation

With `/usr/bin/php` (8.3.6) on the php-src 8.3.6 test tree, this runner and that version's
`run-tests.php` classified 18,173 tests (Zend, tests, ext, sapi) identically, except for tests
skipped on purpose by master's defaults (offline network tests, I/O-capture tests without a
terminal). On `Zend/tests` + `tests/`, 99.9% of the tests that ran pass; the 3 failures also fail
under run-tests.php (Ubuntu build differences). On the master test tree that `modules/php-src`
holds, the same binary passes 73.6% because the tests target PHP 8.6.

`cargo test -p phpt-runner` covers the parser, matcher, diff, scheduler and baseline logic, runs
fixture tests for every section type against `/usr/bin/php`, and checks `tests/run-test` plus a
sample of `Zend/tests` (all skipped if `/usr/bin/php` is absent).
