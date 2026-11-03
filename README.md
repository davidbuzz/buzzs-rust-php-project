# phprust

A Rust re-implementation of PHP whose goal is to pass the php-src `.phpt` test suite.

The original PHP is written in C. This project ports it to idiomatic, best-practice Rust
(clippy-pedantic clean, `unsafe` only in audited FFI modules), matching PHP's observable
behaviour byte for byte: error messages, line numbers, `var_dump` layout, float printing.

## Status (4 Nov 2026)

The engine runs real PHP and every in-scope extension is bound to it. Full in-scope run of the
php-src suite (`Zend`, `tests`, `ext`, `sapi`) with our own `php` binary, measured 4 Nov 2026
(`scripts/update_feature_gap.py report.json --summary`); "ran" excludes skipped tests. 2,514
tests under out-of-scope directories ([phpt-scope.txt](phpt-scope.txt)) are not counted.

| Suite | tests | pass | fail | skip | pass % (ran) | pass % (all) |
|---|---|---|---|---|---|---|
| `Zend/tests` | 5,725 | 4,498 | 1,167 | 60 | 79.4% | 78.6% |
| `tests/` | 904 | 707 | 70 | 126 | 90.9% | 78.2% |
| `ext/standard` | 4,068 | 3,363 | 189 | 514 | 94.6% | 82.7% |
| other `ext/*` | 10,590 | 9,700 | 199 | 684 | 97.9% | 91.6% |
| `sapi/` | 127 | 110 | 2 | 15 | 98.2% | 86.6% |
| **in scope, total** | **21,414** | **18,378** | **1,627** | **1,399** | **91.8%** | **85.8%** |

Per-extension rows (measured from the same run, with skips excluded) are in
[FEATURE_GAP.md](FEATURE_GAP.md).

In progress: ext/zend_test (gates about 340 currently-skipped engine tests) and engine
work on magic methods and class diagnostics. Landed after the run above: typed references,
trait linking/variance rewrite, property-hook inheritance, native stack-limit checks —
405 additional tests pass across Zend/tests, tests/, ext/standard, ext/date and
ext/openssl (baseline refreshed 4 Nov: 1,628 → 1,226 known failures). What is
deliberately out of scope (opcache, FPM, pgsql,
ffi, …) and the row-by-row detail are in [FEATURE_GAP.md](FEATURE_GAP.md).

## Layout

A Cargo workspace; one crate per concern (engine) or per PHP extension (libraries). The full
list with the php-src files each one ports is in [CLAUDE.md](CLAUDE.md). The important ones:

| Crate | Role |
|---|---|
| `php-lexer`, `php-parser` | tokenizer and full PHP 8.x parser/AST (99.95% agreement with `php -l` on the corpus) |
| `php-runtime` | zval, zend_string, ordered hash table, operators, type juggling, var_dump/serialize |
| `php-vm`, `php-cli` | compiler, executor, classes, exceptions, generators, fibers; the `php` binary |
| `phpt-runner` | Rust port of `run-tests.php`; runs `.phpt` files and reports pass rates |
| `php-*` (the rest) | one library per extension, each with its own oracle-driven test suite |

## Building and running

```sh
cargo build -p php-cli -p phpt-runner           # builds target/debug/php and the runner
target/debug/php -r 'echo 1+2, "\n";'           # 3
target/debug/php script.php
```

Run the php-src suite (or any subdirectory of it) against our binary:

```sh
env -u LANGUAGE target/debug/phpt-runner --php target/debug/php -j 8 -q --no-artifacts --summary-depth 2 modules/php-src/tests
target/debug/phpt-runner --php target/debug/php -j 8 -q --no-artifacts modules/php-src/Zend/tests
```

`env -u LANGUAGE` matters only if your shell exports `LANGUAGE` (glibc's gettext lets it override
`LC_ALL`, which fails six ext/gettext tests under any PHP, including `/usr/bin/php`). Drop `-q` to list every test; add `--show-diff` to see expected-vs-actual for failures.
`--json report.json` plus `scripts/update_feature_gap.py report.json` rewrites the `%` column
of FEATURE_GAP.md from a run. `phpt-baseline.txt` lists known failures so CI only fails on
regressions (`--baseline phpt-baseline.txt`, refresh with `--update-baseline`).

Per-crate checks, as required before any work is declared done:

```sh
cargo fmt && cargo clippy --all-targets -p <crate> && cargo test -p <crate>
```

## Reference source

`modules/php-src/` is a READ-ONLY clone of https://github.com/php/php-src — the specification
this port is written against, and the source of the `.phpt` tests. It is gitignored and will be
deleted once the port is done (the tests will be copied in first; they are under the PHP
License 3.01).

It is a plain clone of upstream `master`, at commit `0c7255dc521` ("Merge branch 'PHP-8.6'").
To recreate it:

```sh
mkdir -p modules
git clone https://github.com/php/php-src modules/php-src
```

`/usr/bin/php` (PHP 8.3.6) is used as an oracle by the library test suites. Where 8.3 and
php-src master differ, the port follows master.

## Resource notes

Many agents build concurrently on one machine, so `.cargo/config.toml` caps `cargo` at 2
jobs, every oracle `php` invocation passes `-d memory_limit=1G`, and the `php` binary caps its
own address space at 4 GB. See the "Resource limits" section of CLAUDE.md.
