# Plan: CI modelled on php-src's GitHub Actions

Status, 25 Nov 2026: upstream's layout is in place, but no run of it has happened yet.
- **Workflows:** `.github/matrix.php`, `.github/workflows/{test,test-suite,unit-tests,nightly-results}.yml` and `.github/actions/{apt-x64,setup-x64,setup-caddy,build-linux,test-linux}`.
- **Jobs:** `LINUX_X64` (on pull requests `RELEASE`; nightly `DEBUG`, `RELEASE`, `_REPEAT` and `_VARIATION`, each with all four test steps), `LINUX_ARM64`, `OPCACHE_VARIATION`, `COMMUNITY`, `BENCHMARKING` (pull requests), `LATEST_STABLE_RUST` (nightly), `UNIT_TESTS_LINUX_X64`, and the nightly failure issue.
- **The Rust compiler:** pinned at 1.95.0, except in `LATEST_STABLE_RUST`.
- **Replaced:** the earlier `x86_64-linux.yml`, `aarch64-linux.yml` and `nightly.yml`.
- **Still to do:**
  - the runner's upstream flags (step 1 below). Until then `actions/test-linux` uses the runner's own equivalents, and `OPCACHE_VARIATION` spells out `--file-cache-prime`/`--file-cache-use` as INI settings;
  - the CI environment list from the first runs (step 3).

## Principle

Start as close to php-src's CI as possible (`modules/php-src/.github` at the pin), and vary only where the difference is real:

- **Kept from upstream:** the workflow layout and file names, the job names, the triggers and schedule, the matrix logic and its `CI: …` labels, the test steps and their INI settings, the environment variables, the services and the host setup.
- **Varied, each for a stated reason:**
  - The build is Cargo, not autoconf and make.
  - The test driver is our `phpt-runner`, not `run-tests.php`, with our scope, baseline, environment and conflict files.
  - Jobs that test C-only machinery, or platforms and extensions out of scope, are left out.

## Layout (as upstream)

| Upstream file | Ours | Variation |
|---|---|---|
| `workflows/test.yml` | same | Same triggers: push to master, pull requests to any branch, nightly `cron: "0 1 * * *"`, `workflow_dispatch`. Same `paths-ignore` (`**/*.md`, `docs/**`, `LICENSE`, …) and the same `concurrency` group with `cancel-in-progress`. Only `master` in the branch list: we have no release branches. |
| `matrix.php` (`GENERATE_MATRIX`) | same script, our job list | Run with the runner image's preinstalled PHP, as upstream does. Same labels: `CI: No jobs`, `CI: All jobs`, `CI: All variations`, `CI: Linux X64`, `CI: Opcache Variation`, `CI: Community`, `CI: Benchmarking`. Nightly means all jobs and all variations, as upstream. |
| `workflows/test-suite.yml` | same, the jobs below | |
| `workflows/unit-tests.yml` (`UNIT_TESTS_LINUX_X64`) | same name | Its C unit tests become `cargo test --workspace`. |
| `actions/apt-x64` | same | The package list minus out-of-scope libraries (PostgreSQL, Firebird, ODBC, FreeTDS, LDAP, SNMP, tidy, enchant, …), plus the Rust toolchain. |
| `actions/setup-x64` | same | `locale-gen de_DE` kept; slapd, snmpd and the MSSQL logins left out (out of scope). |
| `actions/setup-caddy` | same | Kept: the HTTP tests use it. |
| `actions/test-linux` | same name and inputs | It runs `phpt-runner` with upstream's flags and environment (next section). |
| `actions/ccache` | `Swatinem/rust-cache` | The Cargo cache. |
| `actions/configure-x64`, `install-linux` | a "build" step | `cargo build` (`--release`, or the dev profile for `DEBUG`); no install: tests run on `target/<profile>/php`. |

## The test step (`actions/test-linux`), as upstream

Upstream runs:

```
sapi/cli/php run-tests.php -P -q <params> [-d opcache.enable_cli=1] -d opcache.jit=<type>
  -d opcache.protect_memory=1 -d opcache.jit_buffer_size=64M -j$(nproc)
  -g FAIL,BORK,LEAK,XLEAK --no-progress --offline --show-diff --show-slow 1000 --set-timeout 120
```

with `MYSQL_TEST_USER=root MYSQL_TEST_PASSWD=root`, `PDO_MYSQL_TEST_DSN="mysql:host=localhost;dbname=test"`, `SKIP_IO_CAPTURE_TESTS=1`, `STACK_LIMIT_DEFAULTS_CHECK=1` and `RUN_RESOURCE_HEAVY_TESTS=1`.

Ours runs the same INI settings, environment, `-j$(nproc)` and timeout through `phpt-runner --php target/<profile>/php`, plus `--baseline phpt-baseline.txt`. The PostgreSQL, Firebird, ODBC and DBLIB variables are dropped, as out of scope.

To keep the command line nearly identical, the runner gets upstream's spellings:

| Upstream flag | Runner today | Work |
|---|---|---|
| `-P`, `--no-progress` | not there | Accept them (`-P` is what `--php` does; ours prints no progress bar). |
| `--set-timeout 120` | `--timeout` | Add the alias. |
| `--offline` | not there | Add it: sets `SKIP_ONLINE_TESTS=1`, as run-tests.php does. |
| `-g FAIL,BORK,LEAK,XLEAK` | `-q` prints only non-passes | Add `-g` with run-tests.php's groups. |
| `--show-slow 1000` | not there | Add it: list the tests slower than N ms. |
| `--repeat 2` | there | — |
| `--file-cache-prime`, `--file-cache-use` | not there | Port them from run-tests.php: they set `opcache.file_cache` and `opcache.record_warnings=1`, and when using also `opcache.jit=0` and `opcache.validate_timestamps=0`; ext/opcache's own tests are skipped. |

## Jobs

| Upstream job | When upstream runs it | Ours | Variation |
|---|---|---|---|
| `LINUX_X64` | PR: `RELEASE_NTS` and `ASAN_DEBUG_ZTS`; nightly: `DEBUG`/`RELEASE` × `ZTS`/`NTS`, plus `_ASAN`, `_REPEAT`, `_VARIATION` | `LINUX_X64` | No `ZTS` axis: our VM is one per process, so one value. `DEBUG` is the dev profile (debug assertions, overflow checks), the counterpart of `--enable-debug`. Its services: `mysql:8.4` as upstream (root/root, database `test`). PostgreSQL, Firebird and MSSQL are left out (out of scope). |
| └ step "Test" | always | same | Opcache off and no JIT: `--env BUZZS_JIT=0`. |
| └ step "Test Tracing JIT" | nightly, ASAN | same | `opcache.enable_cli=1 opcache.jit=tracing`, with our trace JIT on. |
| └ step "Test OpCache" | nightly | same | `opcache.enable_cli=1`, `BUZZS_JIT=0`. |
| └ step "Test Function JIT" | PR and nightly, not ASAN | renamed "Test (default JIT)" | We have no function JIT. Its slot is the configuration users get: opcache off, our trace JIT on. |
| └ `_REPEAT` | nightly | same | `--repeat 2`: what a request leaves behind for the next one. |
| └ `_VARIATION` | nightly | same | The same `-d zend_test.observer.enabled=1 -d zend_test.observer.show_output=0`. Upstream's C `-D` defines (`ZEND_RC_DEBUG`, type-inference checks) have no counterpart. |
| └ `_ASAN` | PR and nightly | TODO, low priority | See "Interesting, low priority" below. |
| └ "Extra tests" | always | left out | `run-extra-tests.php` runs only opcache's C TLS tests. |
| └ "Verify generated files" | always | left out | C stubs and arginfo. |
| `OPCACHE_VARIATION` | nightly, label | same | Upstream's five steps as they are: file cache primed through shm; then used through shm, through the file, and with `file_cache_only=1`, primed and used. Needs the runner's file-cache options. |
| `COMMUNITY` | nightly, label | same | Upstream's projects and commands (AMPHP, Laravel, ReactPHP, Revolt, Symfony, PHPUnit, the Symfony demo with preloading, WordPress): `git clone --depth 1`, `composer install --ignore-platform-req=php+`, `vendor/bin/phpunit`. Its `asan` and `verify_type_inference` types don't apply, so it is one plain run. |
| `BENCHMARKING` | PRs | same | Upstream's `benchmark/benchmark.php`: instructions under valgrind for `Zend/bench.php` and the framework applications through `php-cgi`. The PR's counts are compared with its base commit's, and with the C counts that `php/benchmarking-data` holds for the pinned php-src commit. This is the CI's counterpart of our local acceptance check, which needs `perf`, and `perf` isn't reliably available on hosted runners. |
| `UNIT_TESTS_LINUX_X64` | push, PR | same name | `cargo test --workspace --no-fail-fast`. |
| `LINUX_X32` | PR (`DEBUG_ZTS`), nightly | later | An `i686-unknown-linux-gnu` build needs i386 builds of every C library we link: a project of its own. |
| `MACOS` | PR, nightly | later | Our build hasn't been made to work on macOS yet. |
| `ALPINE` | PR, nightly | later | musl. |
| `COVERAGE` | nightly (master) | later | `cargo llvm-cov`. |
| `MSAN` | nightly | left out | No practical Rust counterpart with the FFI libraries. |
| `LIBMYSQLCLIENT` | nightly | left out | Upstream builds mysqli on libmysqlclient instead of mysqlnd; our mysqlnd is Rust, with no such choice. |
| `WINDOWS`, `FREEBSD`, `SOLARIS`, `LINUX_PPC64` | various | left out | Out of scope, or no build. |
| — | — | `LINUX_ARM64` | Ours alone, from the current `aarch64-linux.yml`, renamed to upstream's naming. It covers the AArch64 build, the platform of the Cranelift JIT if #10 lands. |
| `nightly-results.yml` | after a failed nightly | same idea | Upstream posts to Slack. Ours would open or update a GitHub issue. |
| `verify-bundled-files.yml`, `docs.yml`, `windows-builds.yml` | — | left out | Bundled C libraries, the docs site, Windows. |
| `close-stale-prs.yml`, `labeler.yml`, the needs-feedback workflows | — | later, optional | Repository upkeep. |

## Where we must vary

1. **The baseline.** Upstream has none: a failing test fails the job. Ours runs with `--baseline phpt-baseline.txt` and `phpt-environment.txt`. The runner image's libraries differ from this machine's, so the first runs will show runner-only failures. Until a CI environment list holds them, the PHPT steps run with `continue-on-error`, as the AArch64 job's do today; after that, they gate.
2. **The Rust toolchain.** The gating jobs use the toolchain this repo builds with, pinned (a `rust-toolchain.toml`). One nightly job uses the latest stable, to see compiler changes early. The AArch64 job's rustc 1.100 laid `Value` out differently from our 1.95, which is what broke `jit_value_layout` today.
3. **The php-src tests.** Upstream tests its own checkout; ours fetches php-src at the pinned commit, as `aarch64-linux.yml` does now.
4. **MySQL.** Upstream uses MySQL 8.4; locally we test against MariaDB 10.11. CI follows upstream, so mysqli and pdo_mysql get checked against both.

## Order of work

1. [ ] The runner's upstream flags: the aliases, `--offline`, `-g`, `--show-slow`, `--file-cache-prime` and `--file-cache-use`, each with tests.
2. [x] `test.yml`, `matrix.php` and `test-suite.yml` with `LINUX_X64` (PR configuration), `UNIT_TESTS_LINUX_X64`, and `LINUX_ARM64` moved in. PHPT steps informational.
3. [ ] The CI environment list from the first runs; then the PHPT steps gate.
4. [x] The nightly variations: `_REPEAT`, `_VARIATION`, `DEBUG`, the four test steps, and `OPCACHE_VARIATION`.
5. [x] `COMMUNITY`.
6. [x] `BENCHMARKING`.
7. Later: `LINUX_X32`, `MACOS`, `ALPINE`, `COVERAGE`, the upkeep workflows.

## Interesting, low priority

- [ ] **`_ASAN`: AddressSanitizer on a nightly Rust build.**
  - **Why it's worth less to us than to php-src:** safe Rust's ownership and borrow rules already rule out most of the memory errors ASan finds in C (use after free, out-of-bounds access, double free).
  - **Where it could still find bugs:** the `unsafe` code, about 3,700 lines:
    - the bindings to about 15 C libraries (ICU, libxml2, OpenSSL, curl, sqlite, gd, pcre2, …);
    - the memory-limit allocator, Rust's global allocator here;
    - the tape interpreter's raw frame and instruction pointers;
    - the JIT;
    - the fiber stack switch, which is assembly.
  - **A real case:** the two pcre bugs in #11, an invalid pointer for an empty subject and a read past a subject that ended on a page boundary. Both came through `unsafe` FFI code, and the compiler could not catch them.
  - **Limits:**
    - It checks only code compiled with it: neither the system's C libraries (unless rebuilt with it) nor the JIT's machine code.
    - Rust's sanitizers need nightly (`-Zsanitizer=address`).
  - **When to do it:** worth it once a pinned nightly is in CI anyway, for example for a tail-call build if #5 goes ahead; one nightly job could do both.
  - **The cheaper alternative:** Miri on the allocator's and `Value`'s unit tests. It checks `unsafe` Rust precisely, but it cannot run code that calls C.

## Run time, roughly

A full PHPT run takes about 16 minutes on 3 cores of this machine. GitHub's standard runner has 4 cores.

- **A PR:** a release build plus two full PHPT runs ("Test" and "Test (default JIT)"), the unit tests, and the AArch64 job. Upstream's PRs run similar steps. Expect about 45 minutes.
- **A night:** all variations, `OPCACHE_VARIATION` and `COMMUNITY`, a few hours spread over parallel jobs. This is within GitHub's 6-hour limit per job.

GitHub's standard runners, ARM included, are free for public repositories.
