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

## Resource notes

Many agents build concurrently on one machine, so `.cargo/config.toml` caps `cargo` at 2
jobs, every oracle `php` invocation passes `-d memory_limit=1G`, and the `php` binary caps its
own address space at 4 GB. See the "Resource limits" section of CLAUDE.md.
