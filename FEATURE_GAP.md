# FEATURE_GAP

What has to be built for the Rust port to pass the php-src test suite, and how far along each item is.
Right now it is the TODO list. Once items are done, it becomes the itemised record of what was completed.

- **%**: estimated completion. Where possible, this should track the share of that item's `.phpt` tests that pass.
- **Pri**: `high` blocks most other work or most tests; `med` is a widely used feature; `low` is niche, platform-specific or optional.
- **Source / tests**: where the reference lives in `modules/php-src/` and how many `.phpt` files cover it, as counted at commit `0c7255dc521`. The whole tree has 23,792 `.phpt` files.

Keep rows when they reach 100%; they become the completion record. Add rows as gaps are found.

| Item | % | Pri | Description |
|---|---|---|---|
| **Infrastructure** | | | |
| Cargo workspace layout | 0% | high | Crate split (lexer, parser, compiler, vm, runtime, ext/*, sapi/cli), shared lints, `rustfmt`/`clippy` config, CI. |
| `.phpt` test runner | 90% | high | `crates/phpt-runner` (Rust port of `run-tests.php`): all sections parsed; SKIPIF (skip/info/warn/xfail/xleak/flaky, cache, `nocache`), INI defaults + `--INI--` (`{PWD}`/`{TMP}`/`{ENV:}`/`{MAIL:}`), ENV, ARGS, STDIN, CAPTURE_STDIO, CLEAN, XFAIL, EXTENSIONS, CONFLICTS + batched parallel scheduling, REDIRECTTEST, flaky retry, timeouts, `Termsig=`, core dumps disabled, exact EXPECT/EXPECTF/EXPECTREGEX via PCRE2, run-tests-format `.diff`, baseline (`phpt-baseline.txt`), per-dir table, JSON. Validated against php-src 8.3.6 tests with `/usr/bin/php` 8.3.6: identical classification to that version's `run-tests.php` on 18,173 tests except deliberate master behaviours (offline/IO-capture skips); 99.9% of Zend/tests+tests that ran pass. Missing: CGI/POST/GET/COOKIE/EXPECTHEADERS and PHPDBG tests are parsed but SKIPPED (no such SAPIs), no valgrind/ASAN leak checks (XLEAK), `--repeat`, opcache file-cache/preload modes, JUnit output. Ref: `run-tests.php`, `tests/run-test/` (18). |
| Pass-rate reporting | 100% | med | `phpt-runner --json` + `scripts/update_feature_gap.py report.json [--dry-run] [--denominator all\|ran]` rewrites the % of mapped rows (directory-prefix → row mapping in the script; unmapped rows untouched). |
| **Language front end** | | | |
| Lexer | 95% | high | Tokens, inline HTML, `<?php`/`<?=`/`?>`, heredoc/nowdoc, string interpolation, casts, comments, `__halt_compiler`. Ref: `Zend/zend_language_scanner.l`. **Done** (`crates/php-lexer`): full php-src master token set incl. 8.4+ tokens, tokenizer and parser modes, flexible heredoc/nowdoc with scan-ahead, numeric overflow rules, escape processing, exact scanner error messages/lines, warnings, `token_get_all()` API. `token_get_all()` output is identical to PHP 8.3 for all 23,476 comparable `.phpt` FILE sections. **Missing**: `zend.multibyte`/`declare(encoding=...)` input re-encoding and `zend.detect_unicode` BOM detection; `TOKEN_PARSE` mode needs parser feedback (ext/tokenizer). |
| Parser and AST | 0% | high | Full PHP 8.x grammar, AST and source positions for errors. Ref: `Zend/zend_language_parser.y`, `Zend/zend_ast.c`, `Zend/tests/grammar`. |
| Compiler (AST → IR/opcodes) | 0% | high | Scope resolution, constant folding, compile-time errors that exactly match PHP's messages. Ref: `Zend/zend_compile.c`. |
| Compile-time diagnostics | 0% | high | Fatal/parse error text and line numbers that match byte for byte, since most tests compare output exactly. Ref: `Zend/tests/errmsg`. |
| **Runtime core** | | | |
| Value model (zval) | 0% | high | null/bool/int/float/string/array/object/resource/reference, refcounting and copy-on-write semantics. |
| Strings (zend_string) | 0% | high | Binary-safe byte strings, interning, numeric-string rules. |
| Ordered hash table (arrays) | 0% | high | Insertion-ordered map with int/string keys, packed arrays, iteration that survives modification, `next_free_element`. Ref: `Zend/zend_hash.c`. |
| Type juggling and comparison | 0% | high | PHP 8 comparison rules, `==`/`<=>`, int overflow to float, string↔number conversion, float printing (`precision`, `serialize_precision`). Ref: `Zend/zend_operators.c`, `Zend/tests/comparison`. |
| Executor / VM | 0% | high | Opcode dispatch, call frames, by-ref args, variadics, named args, defaults. Ref: `Zend/zend_execute.c`, `Zend/zend_vm_def.h`. |
| Error and warning system | 0% | high | `E_*` levels, `error_reporting`, `@` suppression, `set_error_handler`, deprecations, uncaught exception output. Ref: `Zend/tests/error_handler`, `Zend/tests/error_reporting`. |
| Exceptions | 0% | high | `throw`, try/catch/finally, `Throwable` hierarchy, chaining, stack traces in the exact format PHP prints. Ref: `Zend/zend_exceptions.c`, `Zend/tests/exceptions`. |
| Garbage collection | 0% | med | Cycle collection, destructor ordering, `gc_collect_cycles`, `WeakReference`/`WeakMap`. Ref: `Zend/zend_gc.c`, `Zend/tests/gc`. |
| Memory limit and resource limits | 0% | low | `memory_limit`, `max_execution_time`, stack-overflow detection. Ref: `Zend/zend_alloc.c`, `Zend/zend_call_stack.c`. |
| INI system | 0% | high | `php.ini` parsing, `ini_get`/`ini_set`, per-test `--INI--`. Ref: `Zend/zend_ini*.c`, `main/php_ini.c`. |
| **Language features** | | | |
| Functions and closures | 0% | high | User functions, closures, `use`, arrow functions, static vars, first-class callable syntax, `callable` checks. Ref: `Zend/tests/closures`, `arrow_functions`, `first_class_callable`. |
| Classes and objects | 0% | high | Properties, methods, constants, static, visibility, `readonly`, constructor promotion, `clone`, magic methods. Ref: `Zend/zend_object_handlers.c`, `tests/classes` (283). |
| Inheritance, interfaces, traits | 0% | high | Signature compatibility checks, abstract, final, LSB, trait conflict resolution. Ref: `Zend/zend_inheritance.c`, `Zend/tests/traits`, `Zend/tests/inheritance`. |
| Type system | 0% | high | Scalar, union, intersection and DNF types, nullable, `never`, `static`, coercion vs `strict_types`. Ref: `Zend/tests/type_declarations`. |
| Namespaces and `use` | 0% | high | Name resolution, group use, function and constant fallback. Ref: `Zend/tests/namespaces`. |
| Autoloading | 0% | high | `spl_autoload_register`, class loading order. Ref: `Zend/zend_autoload.c`. |
| Enums | 0% | med | Pure and backed enums, `cases()`, `from`/`tryFrom`, interface implementation. Ref: `Zend/zend_enum.c`, `Zend/tests/enum`. |
| Generators | 0% | med | `yield`, `yield from`, send/throw, generators as iterators. Ref: `Zend/zend_generators.c`, `Zend/tests/generators`. |
| Fibers | 0% | med | `Fiber` start/suspend/resume, needs a separate stack or a CPS design. Ref: `Zend/zend_fibers.c`, `Zend/tests/fibers`. |
| Attributes | 0% | med | Parsing, `#[Attribute]` validation, reflection access, built-ins (`Override`, `Deprecated`, `SensitiveParameter`…). Ref: `Zend/zend_attributes.c`. |
| `match`, nullsafe, spread, list/destructuring | 0% | high | Modern expression syntax. Ref: `Zend/tests/match`, `nullsafe_operator`, `list`, `array_unpack`. |
| Property hooks and asymmetric visibility | 0% | med | PHP 8.4 `get`/`set` hooks, `private(set)`. Ref: `Zend/tests/property_hooks`, `asymmetric_visibility`. |
| Lazy objects | 0% | low | PHP 8.4 lazy ghosts and proxies. Ref: `Zend/tests/lazy_objects`. |
| Built-in iterators and interfaces | 0% | high | `Traversable`, `Iterator`, `IteratorAggregate`, `ArrayAccess`, `Countable`, `Stringable`. Ref: `Zend/zend_interfaces.c`. |
| `include`/`require`, `eval`, `goto`, `declare` | 0% | high | File inclusion and include path, `__halt_compiler`, ticks. Ref: `Zend/tests/jump`, `Zend/tests/declare`. |
| Zend built-in functions | 0% | high | `strlen`, `func_get_args`, `get_class`, `define`, `class_exists`, `debug_backtrace`… Ref: `Zend/zend_builtin_functions.c`. |
| Zend test suite overall | 0% | high | All of `Zend/tests` (5,725 `.phpt`). |
| PHP language test suite overall | 0% | high | `tests/lang` (294), `tests/basic` (140), `tests/classes` (283), `tests/output` (95), `tests/func` (14), `tests/strings` (9), `tests/security` (51). |
| **main/ (PHP runtime services)** | | | |
| Output buffering | 0% | high | `echo`/`print`, `ob_*` stack, handlers, flush semantics. Ref: `main/output.c`, `tests/output`. |
| Streams layer | 0% | high | Plain files, `php://` wrappers, memory/temp streams, contexts, filters, user stream wrappers, glob. Ref: `main/streams/`. |
| Network streams / sockets transport | 0% | med | `tcp://`, `udp://`, `unix://`, `stream_socket_*`. Ref: `main/streams/xp_socket.c`, `main/network.c`. |
| Superglobals and request variables | 0% | high | `$_SERVER`, `$_GET`, `$_POST`, `$_COOKIE`, `$_FILES`, `$argv`, `$GLOBALS` semantics. Ref: `main/php_variables.c`, `main/rfc1867.c`. |
| printf-family formatting | 0% | high | PHP's own `snprintf`/`spprintf`, float formatting. Ref: `main/snprintf.c`, `main/spprintf.c`. |
| **SAPIs** | | | |
| CLI (`php` binary) | 0% | high | Arguments, `-r`, `-d`, `-n`, `-l`, STDIN/STDOUT/STDERR constants, exit codes. Ref: `sapi/cli` (127). |
| CLI built-in web server | 0% | low | `php -S`. Ref: `sapi/cli/php_cli_server.c`. |
| CGI | 0% | low | Ref: `sapi/cgi` (24). |
| FPM | 0% | low | Ref: `sapi/fpm` (151). |
| phpdbg | 0% | low | Ref: `sapi/phpdbg` (74). |
| Embed / apache2handler / litespeed / fuzzer | 0% | low | No tests of their own; probably out of scope. |
| **Core extensions (always enabled)** | | | |
| ext/standard | 0% | high | Strings, arrays, math, files, `var_dump`/`print_r`/`var_export`, `serialize`, `sprintf`, `preg`-free string functions, URLs, base64, crc32, md5/sha1, `proc_open`, `mail`… The largest extension (4,068 `.phpt`). Split into sub-rows as work starts. |
| ext/spl | 0% | high | Data structures, iterators, `ArrayObject`, `SplFileObject`, exceptions, autoload (820). |
| ext/pcre | 0% | high | `preg_*` with PCRE2-compatible semantics; choose between binding pcre2 and a pure Rust engine (178). |
| ext/date | 0% | high | `DateTime*`, timezones, `strtotime` parser, `date()` formats (744). |
| ext/reflection | 0% | high | Full Reflection API (577). |
| ext/json | 0% | high | `json_encode`/`json_decode`/`json_validate`, `JsonSerializable` (111). |
| ext/random | 0% | high | `Random\Randomizer`, engines (Mt19937, PCG, Xoshiro), `rand`/`mt_rand` that reproduce seeded sequences exactly (71). |
| ext/hash | 60% | high | `hash()` algorithms, HMAC, HKDF, incremental hashing (87). `crates/php-hash` is complete as a library: all 60 algorithms in `hash_algos()` order, bit-exact against `/usr/bin/php` (one-shot, chunked, HMAC, murmur/xxh seeds, xxh3/xxh128 secrets), `HashContext` init/update/final/copy with `HASH_HMAC`, `__serialize`/`__unserialize` state arrays byte-identical to PHP (incl. error codes), `hash_hkdf`, `hash_pbkdf2`, `hash_equals`, `mhash*`, `md5`/`sha1`/`crc32` helpers, exact error/deprecation texts. Remaining: VM bindings (functions, `HashContext` class, `hash_file`/`hash_update_stream` over streams). |
| ext/ctype | 0% | med | `ctype_*` (49). |
| ext/tokenizer | 0% | med | `token_get_all`, `PhpToken`; reuses the lexer (54). |
| ext/uri | 0% | med | RFC 3986 and WHATWG URL parsing (608). |
| **Common extensions** | | | |
| ext/mbstring | 0% | med | Multibyte string functions, encodings (453). |
| ext/iconv | 0% | med | (83). |
| ext/filter | 0% | med | `filter_var` and related functions (129). |
| ext/session | 0% | med | Sessions, save handlers (290). |
| ext/bcmath | 0% | med | Arbitrary precision decimals, `BcMath\Number` (171). |
| ext/gmp | 0% | low | Big integers (106). |
| ext/zlib | 0% | med | gzip/deflate functions and stream wrapper (170). |
| ext/bz2 | 0% | low | (34). |
| ext/zip | 0% | low | `ZipArchive` (153). |
| ext/phar | 0% | low | Phar archives (597). |
| ext/openssl | 0% | med | Crypto, TLS stream transport (283). |
| ext/sodium | 0% | low | libsodium API (43). |
| ext/curl | 0% | med | (208). |
| ext/ftp | 0% | low | (66). |
| ext/sockets | 0% | low | Low-level sockets API (133). |
| ext/fileinfo | 0% | low | libmagic-compatible detection (66). |
| ext/exif | 0% | low | (107). |
| ext/calendar | 0% | low | (55). |
| ext/gettext | 0% | low | (19). |
| ext/intl | 0% | low | ICU-backed i18n; large (595). |
| ext/gd | 0% | low | Image processing (341). |
| **XML stack** | | | |
| ext/libxml | 0% | med | Shared libxml layer (33). |
| ext/dom | 0% | med | DOM Level 3 and the PHP 8.4 HTML5 `Dom\*` API; uses lexbor (914). |
| ext/simplexml | 0% | med | (168). |
| ext/xml | 0% | med | Expat-style SAX parser (71). |
| ext/xmlreader / ext/xmlwriter | 0% | low | (64 / 52). |
| ext/xsl | 0% | low | (88). |
| ext/soap | 0% | low | (663). |
| **Databases** | | | |
| ext/pdo (core) | 0% | med | PDO abstraction (137). |
| ext/pdo_sqlite / ext/sqlite3 | 0% | med | SQLite support, self-contained so testable in CI (95 / 108). |
| ext/mysqli, ext/mysqlnd, ext/pdo_mysql | 0% | low | Need a live MySQL server to test (495 / 0 / 169). |
| ext/pgsql, ext/pdo_pgsql | 0% | low | Need a live PostgreSQL server to test (105 / 96). |
| ext/odbc, pdo_odbc, pdo_dblib, pdo_firebird, dba | 0% | low | (54 / 26 / 33 / 51 / 61). |
| ext/ldap, ext/snmp | 0% | low | Need external services (142 / 42). |
| **POSIX / OS** | | | |
| ext/posix | 0% | low | (61). |
| ext/pcntl | 0% | low | Process control, signals (69). |
| ext/readline | 0% | low | Interactive shell `php -a` (36). |
| ext/shmop, sysvmsg, sysvsem, sysvshm | 0% | low | SysV IPC (7 / 9 / 3 / 16). |
| **Engine extras** | | | |
| ext/opcache | 0% | low | Many of its 1,038 tests check optimizer/JIT internals; decide which behaviour-only tests to keep. Ref: `Zend/Optimizer`. |
| Optimizer passes | 0% | low | Only needed where they change observable behaviour, e.g. some warnings. Ref: `Zend/Optimizer/`. |
| ext/ffi | 0% | low | C FFI from PHP (109). |
| ext/zend_test, dl_test, skeleton | 0% | low | Test-only helper extensions that some Zend tests depend on (168 / 3 / 3). |
| ext/tidy, enchant, com_dotnet, imap | 0% | low | Niche or Windows-only; probably out of scope (53 / 33 / 42 / 1). |
