# phprust

A Rust re-implementation of PHP whose goal is to pass the php-src `.phpt` test suite.

The original PHP is written in C. This project ports it to idiomatic, best-practice Rust
(clippy-pedantic clean, `unsafe` only in audited FFI modules), matching PHP's observable
behaviour byte for byte: error messages, line numbers, `var_dump` layout, float printing.

## Status (3 Nov 2026, late evening)

The engine runs real PHP and every in-scope extension is bound to it. Pass rates below are
over non-skipped tests, measured with our own `php` binary against the php-src suite. The
engine rows are from the last full measurement earlier today; a resumable full in-scope run
is in progress and will replace them (the owner's partial runs since then are around 76% for
`tests/` + `Zend/tests` combined).

| Suite | pass % | detail |
|---|---|---|
| `tests/lang` | 88.6% | 248/282 |
| `tests/` (all) | 72.1% | 555/770 |
| `Zend/tests` | 53.3% | 2,961/5,558 (lazy objects now 81.6%, attributes 91.2%) |
| ext/standard strings, arrays, files, dir | 95–100% | |
| ext/date, json, random, bcmath, uri, iconv, tokenizer | 98–99% | |
| ext/pcre, hash, mbstring, intl, gmp, openssl, sodium, zlib, filter | 95–98% | |
| ext/pdo + pdo_sqlite + sqlite3 | 98.5% | 323/327 |
| ext/dom | 97.4% | 876/899 |
| ext/reflection | 94.2% | 536/569 |
| ext/spl | 93.0% | 759/820 |
| ext/simplexml, xmlreader, xmlwriter, xml, libxml, xsl, soap | 82–100% | |
| ext/mysqli, session, curl, ftp, sockets, phar, zip, bz2, fileinfo | 90–98% | |
| ext/exif, calendar, gettext, posix, sysv*, shmop, ctype, pcntl | 98–100% | |
| sapi/cli | 88.3% | 98/111; the built-in server (`php -S`) 77% |
| ext/gd | 99.6% | 281/282 (system libgd 2.3.3) |

In progress: ext/zend_test (gates about 340 currently-skipped engine tests) and typed
references in the engine. What is deliberately out of scope (opcache, FPM, pgsql,
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

Many agents build concurrently on one machine, so `.cargo/config.toml` caps `cargo` at 2
jobs, every oracle `php` invocation passes `-d memory_limit=1G`, and the `php` binary caps its
own address space at 4 GB. See the "Resource limits" section of CLAUDE.md.
