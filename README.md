# phprust

A Rust re-implementation of PHP whose goal is to pass the php-src `.phpt` test suite.

The original PHP is written in C. This project ports it to idiomatic, best-practice Rust
(clippy-pedantic clean, `unsafe` only in audited FFI modules), matching PHP's observable
behaviour byte for byte: error messages, line numbers, `var_dump` layout, float printing.

## Status (3 Nov 2026, 18:30)

The engine runs real PHP. Measured with our own `php` binary against the php-src suite:

| Suite | ran | pass | pass % |
|---|---|---|---|
| `tests/lang` | 282 | 248 | 88.6% |
| `tests/` (all, 904 files) | 770 | 555 | 72.1% |
| `Zend/tests` (5,725 files) | 5,558 | 2,961 | 53.3% |

Alongside the engine, 26 extension libraries are complete and oracle-verified against real PHP
(hash, random, pcre, date, json, mbstring, filter, bcmath, gmp, zlib, sqlite, libxml, …) but not
yet bound to the VM; binding them is the next phase. Row-by-row detail is in
[FEATURE_GAP.md](FEATURE_GAP.md).

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
target/debug/phpt-runner --php target/debug/php -j 8 -q --no-artifacts --summary-depth 2 modules/php-src/tests
target/debug/phpt-runner --php target/debug/php -j 8 -q --no-artifacts modules/php-src/Zend/tests
```

Drop `-q` to list every test; add `--show-diff` to see expected-vs-actual for failures.
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

Many agents build concurrently on one machine, so `.cargo/config.toml` caps `cargo` at 4
jobs, every oracle `php` invocation passes `-d memory_limit=1G`, and the `php` binary caps its
own address space at 4 GB. See the "Resource limits" section of CLAUDE.md.
