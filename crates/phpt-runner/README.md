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

## Baseline workflow

`phpt-baseline.txt` at the repository root lists tests (paths relative to php-src) known to
FAIL or BORK with our binary on `Zend/tests` and `tests/`:

```
target/debug/phpt-runner -q --no-artifacts --baseline phpt-baseline.txt \
    modules/php-src/Zend/tests modules/php-src/tests
```

The run fails (exit 1) only if a test outside the baseline fails (`NEW FAILURE`) or a baseline
test now passes (`UNEXPECTED PASS`, so the baseline can shrink). After making tests pass, rerun
with `--update-baseline` and commit the smaller file. Entries for tests that were not part of the
run are kept, so updating from a partial run is safe.

## Updating FEATURE_GAP.md

```
target/debug/phpt-runner -q --json /tmp/results.json modules/php-src/Zend/tests modules/php-src/tests
scripts/update_feature_gap.py /tmp/results.json --dry-run      # show what would change
scripts/update_feature_gap.py /tmp/results.json                # rewrite the % column
```

The script maps directory prefixes to row names (the `MAPPING` table in the script); rows
without a mapping, or with no tests in the report, are left alone. The percentage is
passed/total by default (`--denominator ran` excludes skipped/borked tests).

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
  `RLIMIT_CORE=0`; a death by signal appends `Termsig=N` like run-tests.php.
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
