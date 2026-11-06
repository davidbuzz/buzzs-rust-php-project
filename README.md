# phprust

A Rust re-implementation of PHP whose goal is to pass the php-src `.phpt` test suite.

The original PHP is written in C. This project ports it to idiomatic, best-practice Rust
(clippy-pedantic clean, `unsafe` only in audited FFI modules), matching PHP's observable
behaviour byte for byte: error messages, line numbers, `var_dump` layout, float printing.

Current implementation coverage, test results, and remaining work are tracked in
[FEATURE_GAP.md](FEATURE_GAP.md).

## Layout

A Cargo workspace; one crate per concern (engine) or per PHP extension (libraries). The full
list with the php-src files each one ports is in [CLAUDE.md](CLAUDE.md). The important ones:

| Crate | Role |
|---|---|
| `php-lexer`, `php-parser` | tokenizer and PHP 8.x parser/AST |
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

Use these incremental dev-profile builds for correctness checks and scoped PHPT
runs. Reserve `--release` for performance measurements: the release profile uses
thin LTO and one codegen unit, which makes rebuilding test executables expensive.
Keep build directories consistent and avoid `cargo clean` during edit/test loops.
For multiple affected crates, combine their `-p` selectors in one command.
`ccache` caches C/C++ compilation, not Rust; a local-only `sccache` setup can help
repeated non-incremental Rust compilation, but does not cache final linking or
replace Cargo's incremental dev builds.

With `sccache` installed and configured for local disk storage, use it explicitly
for non-incremental release builds rather than disabling incremental dev builds:

```sh
RUSTC_WRAPPER=sccache cargo build --release -p php-cli
sccache --show-stats
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

## Related work

Projects with a related thesis or implementation:

- **[francescotinti/php-rust](https://github.com/francescotinti/php-rust)** /
  [phprust.com](https://phprust.com/) — a Rust PHP implementation targeting PHP 8.5,
  using the mago parser and a HIR-to-bytecode pipeline. A read-only reference clone lives
  at `modules/php-rust/` (gitignored) for comparison and study; its code is never edited
  or copied.
- **[jolicode/rphp](https://github.com/jolicode/rphp)** — AI-assisted experiment
  ([write-up](https://jolicode.com/blog/writing-the-php-virtual-machine-in-rust-with-a-lot-of-help-from-ai))
  exploring a Rust PHP VM and copy-on-write request forking.
- Earlier/smaller efforts: `rust-php/php`, `tagua-vm` (archived), various experimental
  interpreters and parsers (`wudi/php-parser`, `jorgsowa/rust-php-parser`);
  [mago](https://github.com/carthage-software/mago) is a PHP toolchain (not an engine)
  in Rust.

This project follows php-src `master`, organizes extensions as per-extension crates,
and uses `.phpt` tests and per-workload instruction counts as its compatibility and
performance measures.

## Compared with php-rust (phprust.com)

Both projects implement PHP in Rust. They differ in what they treat as the definition of
correct, and that choice shows in the results. Figures for php-rust are its own (README and
COVERAGE.md, clone of 2 Nov 2026); figures for this project were measured on 6 Nov 2026.

### Two approaches

- **php-rust** reimplements PHP from its observable behaviour on a new architecture: the mago
  parser, its own HIR and bytecode VM, six crates and about 154,000 lines of Rust. It targets
  PHP 8.5.7, and its main proof of correctness is real applications (WordPress, Composer,
  PHPUnit, Doctrine, Symfony) plus a frozen list of known test failures.
- **This project** ports php-src's own design, file by file: the compiler, executor, hash
  tables, memory allocator, garbage collector and the bundled libraries (timelib, libmbfl,
  libmagic rules and others), in 100 crates and about 746,000 lines of Rust. It follows
  php-src `master`. Its definition of correct is php-src's own `.phpt` suite, byte for byte,
  across the whole tree, and every crate cites the C file it ports.

### Conformance

| | php-rust (its own figures) | This project (6 Nov 2026) |
|---|---|---|
| `Zend/tests` (the engine suite) | 2,655 pass of 4,067 runnable (65.3%); 1,412 fail | 5,665 pass of 5,665 runnable (100%); 60 of 5,725 skipped |
| Whole php-src suite | not reported | 20,170 pass of 20,174 run (99.9%) across the 21,582 in-scope tests: `Zend/tests`, `tests/`, ext/standard, every other in-scope extension, the CLI SAPI |
| Internal functions | 1,017 of 2,143 (47%) | 1,962 functions, 347 classes, 54 extensions |
| Rust test suite | 1,748 tests | 4,290 tests |

The difference is depth as well as count. Many php-src tests check details that only a port
of the mechanisms reproduces: the exact text and order of every warning and deprecation,
`memory_limit` failures at the same allocation as C PHP (with the same "tried to allocate N
bytes"), garbage-collector root order and destructor timing, object handle numbers in
`var_dump` output, stack-overflow limits, and the bundled libraries' edge cases. An engine
can run WordPress correctly and still fail those tests; this one passes them because it
reproduces the mechanisms behind them. The remaining failures are listed in
`phpt-baseline.txt`; two are environmental (Ubuntu's libgd is built without GD2, MariaDB has
no JSON column type).

### Where php-rust is ahead

- **Speed.** php-rust reports 2.4–4.6x PHP's CPU time on six micro-benchmark categories and
  about 1.77x on the full WordPress test suite. This project is at 5.21x PHP's user-space
  instruction count (geometric mean over the 82 workloads of `scripts/perf_gate.sh`; release
  build of `3670e2f3`, 6 Nov 2026), down from about 18x on 4 Nov 2026 and 6.4x on the morning
  of 6 Nov. It is still slower than C PHP on every workload: from 1.4x on string concatenation
  to 13.3x on user function calls, with start-up at 2.6x. The performance work is in progress.
  The metrics and workloads differ, so the numbers are not directly comparable, but php-rust is
  faster today.
- **Real applications.** php-rust has shown WordPress, Composer, PHPUnit, Doctrine and Symfony
  running on its engine. This project has not measured real applications yet.

### Why the approach matters

A port of php-src's design starts from PHP's exact semantics, so its remaining work is mostly
making the same design faster: operand-specialised handlers, per-opcode runtime caches,
16-byte values and a zend_mm-style allocator, each measured against C PHP. A behaviour-driven
engine reaches "fast and working" sooner, but each further conformance point means finding
and reproducing another mechanism it does not share with C PHP; for php-rust, its 1,412
failing engine tests are that list.

In short: php-rust shows that PHP applications can run on a Rust engine; this project is a
Rust implementation of PHP that passes 99.9% of php-src's own test suite, with its
performance work under way toward C PHP's speed.

## Resource notes

Many agents build concurrently on one machine, so `.cargo/config.toml` caps `cargo` at 6
jobs, every oracle `php` invocation passes `-d memory_limit=1G`, and the `php` binary caps its
own address space (at the larger of 4 GiB and 75% of RAM plus swap; `PHPRUST_MAX_AS_MB`
overrides it). See the "Resource limits" section of CLAUDE.md.
