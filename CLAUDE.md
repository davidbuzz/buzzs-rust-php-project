# phprust

A Rust re-implementation of PHP whose goal is to pass the php-src `.phpt` test suite.
`modules/php-src/` is a READ-ONLY reference clone of https://github.com/php/php-src (never edit it; it is gitignored).
`/usr/bin/php` (the C implementation) is installed and may be used as an oracle to check expected behaviour.

## Layout (cargo workspace)
- `crates/php-lexer`   — tokenizer (ref: Zend/zend_language_scanner.l)
- `crates/php-parser`  — AST + parser (ref: Zend/zend_language_parser.y, zend_ast.c)
- `crates/php-runtime` — values, strings, ordered hash table, operators/type juggling (ref: zend_types.h, zend_hash.c, zend_operators.c)
- `crates/php-vm`      — compiler + executor, classes, exceptions, output (ref: zend_compile.c, zend_execute.c)
- `crates/php-cli`     — the `php` binary (ref: sapi/cli)
- `crates/phpt-runner` — runs `.phpt` files from modules/php-src against our binary (ref: run-tests.php)

## Rules
- Idiomatic, best-practice Rust: clippy pedantic clean, no `unsafe` unless justified in a comment, no `unwrap()` in library code paths that can fail, thorough unit tests in every crate.
- Match PHP output byte-for-byte: error/warning message text, line numbers, `var_dump` formatting, float printing.
- Each crate owns its public API; coordinate via the types in `php-runtime`.
- Track progress in `FEATURE_GAP.md` (update the % column for rows you advance).
- Run `cargo fmt && cargo clippy --all-targets && cargo test` before declaring work done.
