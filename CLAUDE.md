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
- `crates/php-support` — INI parser, output-buffering stack, printf/sprintf format engine (ref: Zend/zend_ini_*, main/output.c, main/snprintf.c, ext/standard/formatted_print.c)
- `crates/php-hash`    — hash()/md5/sha*/crc32 etc. (ref: ext/hash, ext/standard/{md5,sha1,crc32}.c)
- `crates/php-random`  — Mt19937/PCG/Xoshiro/Secure engines, rand/mt_rand/lcg exact sequences (ref: ext/random)
- `crates/php-date`    — timelib port: strtotime parser, date() formatting, tz database, DateTime arithmetic (ref: ext/date/lib, ext/date/php_date.c)
- `crates/php-pcre`    — preg_* semantics over PCRE2: pattern/modifier parsing, replace/split/quote, error constants (ref: ext/pcre/php_pcre.c)
- `crates/php-stdstr`  — pure byte-level algorithms behind ext/standard string/html/url/math functions (ref: ext/standard/{string,html,url,base64,...}.c)
- `crates/php-streams` — stream layer: wrappers (file, php://, data:, glob), memory/temp streams, filters, fopen modes, stat (ref: main/streams/, ext/standard/file.c)

## Parallel work
Several agents work concurrently, one crate each. Only edit files inside your own crate (plus your row(s) in FEATURE_GAP.md). Never edit workspace Cargo.toml; crates are pre-registered. Commit your own crate's files with `git add crates/<yours>` only.
