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
| Value model (zval) | 70% | high | null/bool/int/float/string/array/object/resource/reference, refcounting and copy-on-write semantics. `php-runtime::Value` done (Rc-based COW arrays, `PhpRef` references, `Obj` trait objects, resources); objects/GC/typed-reference sources are the VM's job. |
| Strings (zend_string) | 70% | high | Binary-safe byte strings, interning, numeric-string rules. `php-runtime::ZStr` (cached DJB hash, interning, in-place append) and `is_numeric_string_ex`/`zend_strtod` ports done; string *functions* belong to ext/standard. |
| Ordered hash table (arrays) | 85% | high | Insertion-ordered map with int/string keys, packed arrays, iteration that survives modification, `next_free_element`. Ref: `Zend/zend_hash.c`. `php-runtime::ZArray` mirrors Zend's layout (packed/hash, holes, table sizes, iterator remapping, `zend_sort`, splice/shift/pop/unshift); property-tested against a model. Missing: `HASH_FLAG_HAS_EMPTY_IND`/INDIRECT slots (VM symbol tables). |
| Type juggling and comparison | 80% | high | PHP 8 comparison rules, `==`/`<=>`, int overflow to float, string↔number conversion, float printing (`precision`, `serialize_precision`). Ref: `Zend/zend_operators.c`, `Zend/tests/comparison`. `php-runtime::{ops,convert,float}`: 23k-case operator matrix and cast table verified against the oracle; object handlers (`cast_object`, `compare`, `do_operation`) are hooks the VM must implement. |
| Executor / VM | 0% | high | Opcode dispatch, call frames, by-ref args, variadics, named args, defaults. Ref: `Zend/zend_execute.c`, `Zend/zend_vm_def.h`. |
| Error and warning system | 0% | high | `E_*` levels, `error_reporting`, `@` suppression, `set_error_handler`, deprecations, uncaught exception output. Ref: `Zend/tests/error_handler`, `Zend/tests/error_reporting`. |
| Exceptions | 0% | high | `throw`, try/catch/finally, `Throwable` hierarchy, chaining, stack traces in the exact format PHP prints. Ref: `Zend/zend_exceptions.c`, `Zend/tests/exceptions`. |
| Garbage collection | 0% | med | Cycle collection, destructor ordering, `gc_collect_cycles`, `WeakReference`/`WeakMap`. Ref: `Zend/zend_gc.c`, `Zend/tests/gc`. |
| Memory limit and resource limits | 0% | low | `memory_limit`, `max_execution_time`, stack-overflow detection. Ref: `Zend/zend_alloc.c`, `Zend/zend_call_stack.c`. |
| INI system | 55% | high | `php.ini` parsing, `ini_get`/`ini_set`, per-test `--INI--`. Ref: `Zend/zend_ini*.c`, `main/php_ini.c`. **Done in `php-support::ini`:** scanner/parser port (Bison tables, byte-identical syntax errors, NORMAL/RAW/TYPED, `${VAR:-fb}`, constants hook, expressions, `a[]=`), `parse_ini_string()` arrays (125 oracle cases incl. the phpt fixtures), `zend_ini_parse_quantity`/`parse_bool`, `IniRegistry` with core directives, access levels, typed modify handlers, `ini_get/set/restore/get_all`. **Missing:** `php.ini` file discovery/loading (`main/php_ini.c`), `[PATH=]`/`[HOST=]` sections, extension directives beyond core/standard samples, VM wiring of the userland functions. |
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
| Output buffering | 60% | high | `echo`/`print`, `ob_*` stack, handlers, flush semantics. Ref: `main/output.c`, `tests/output`. **Done in `php-support::output`:** full `main/output.c` port behind an `OutputHost` trait (re-entrant user handlers, modes, chunked auto-flush, buffer_size accounting, failure→passthrough, lock fatal + bailout, `Producing output…` deprecation, exact notices, request startup/shutdown); unit tests mirror ~45 of the `tests/output` phpts. **Missing:** VM wiring, `ob_gzhandler`/`mb_output_handler`/url-rewriter aliases and handler conflicts, headers integration, `output_add_rewrite_var`, the `stream_isatty`/vt100 tests (streams). |
| Streams layer | 0% | high | Plain files, `php://` wrappers, memory/temp streams, contexts, filters, user stream wrappers, glob. Ref: `main/streams/`. |
| Network streams / sockets transport | 0% | med | `tcp://`, `udp://`, `unix://`, `stream_socket_*`. Ref: `main/streams/xp_socket.c`, `main/network.c`. |
| Superglobals and request variables | 0% | high | `$_SERVER`, `$_GET`, `$_POST`, `$_COOKIE`, `$_FILES`, `$argv`, `$GLOBALS` semantics. Ref: `main/php_variables.c`, `main/rfc1867.c`. |
| printf-family formatting | 65% | high | PHP's own `snprintf`/`spprintf`, float formatting. Ref: `main/snprintf.c`, `main/spprintf.c`. **Done in `php-support::fmt`:** `zend_gcvt`/`php_conv_fp`/`format_double` (precision incl. -1 shortest, `1.0E+25`, `-0`, INF/NAN), `sprintf`/`vsprintf` engine of `formatted_print.c` with exact `ValueError`/`ArgumentCountError` messages (1428 oracle cases), `round()` (all modes, master algorithm) and `number_format()`. **Missing:** VM wiring (`printf`/`fprintf`/`vfprintf` output), locale-aware decimal point source, the C-level `spprintf` `%Z`/`%S` conveniences (VM concern). |
| **SAPIs** | | | |
| CLI (`php` binary) | 0% | high | Arguments, `-r`, `-d`, `-n`, `-l`, STDIN/STDOUT/STDERR constants, exit codes. Ref: `sapi/cli` (127). |
| CLI built-in web server | 0% | low | `php -S`. Ref: `sapi/cli/php_cli_server.c`. |
| CGI | 0% | low | Ref: `sapi/cgi` (24). |
| FPM | 0% | low | Ref: `sapi/fpm` (151). |
| phpdbg | 0% | low | Ref: `sapi/phpdbg` (74). |
| Embed / apache2handler / litespeed / fuzzer | 0% | low | No tests of their own; probably out of scope. |
| **Core extensions (always enabled)** | | | |
| ext/standard | 0% | high | Strings, arrays, math, files, `var_dump`/`print_r`/`var_export`, `serialize`, `sprintf`, `preg`-free string functions, URLs, base64, crc32, md5/sha1, `proc_open`, `mail`… The largest extension (4,068 `.phpt`). Split into sub-rows as work starts. |
| ext/standard — string/url/math algorithms (php-stdstr) | 60% | high | `crates/php-stdstr` is complete as a library of byte-exact ports (plain `&[u8]`/`i64`/`f64` APIs, `StdError` for throws, `Notice` buffer for warnings/deprecations, master wording): case/trim (`..` ranges)/pad/`wordwrap`/`chunk_split`/`nl2br`/`strrev`/`str_repeat`/`str_split`/`strtr` (both forms)/`str_replace`/`str_ireplace`/`substr`/`substr_replace`/`substr_count`/`strpos` family/`strstr`/`strrchr`/`strpbrk`/`strspn`/`explode`/`strtok` (stateful)/`str_increment`/`hebrev`/`basename`/`dirname`/`pathinfo`, `strcmp` family/`strnatcmp`/`substr_compare`, `similar_text`/`levenshtein`/`soundex`/`metaphone`/`str_word_count`/`count_chars`, slashes/`quotemeta`, `strip_tags`, `chr`/`ord`/`bin2hex`/`hex2bin`/`utf8_*`, `html*` entity functions (all flags/doctypes/charsets, generated tables), `urlencode` family/`parse_url`, base64/uuencode/quoted-printable, CSV (`str_getcsv`/`fgetcsv` core/`fputcsv`), `sscanf`, `pack`/`unpack`, `version_compare`, `base_convert`/`bindec`/…/`intdiv`/`fmod`/`fdiv`/`log`/round-mode parsing. ~74k oracle cases vs `/usr/bin/php` (`tests/fixtures/*.txt`, mapped to master). Remaining: VM bindings (argument coercion, array forms, alias names), `implode`/`http_build_query`/`parse_str`/`str_shuffle` (value-level, VM), locale-dependent paths (non-ASCII `basename`, `setlocale`). |
| ext/standard — array functions (php-stdarr) | 60% | high | `crates/php-stdarr` is complete as a library over `php-runtime` values: every function of `ext/standard/array.c` (master), i.e. all sorts with every `SORT_*` flag and the exact `php_array_*_compare*` comparators (public for SPL), `usort`/`uasort`/`uksort` (copy-then-assign, bool-return deprecation + swapped retry), `array_multisort` (arg parsing/errors, by-ref write-back), internal pointer functions, `count` (`COUNT_RECURSIVE` with recursion warning), `min`/`max` (int/float fast paths), search, `extract` (all `EXTR_*` incl. `EXTR_REFS`, CV-undefined slots) / `compact` through a `SymbolTable` trait, `range` (8.3+ algorithm and errors), `shuffle`/`array_rand`/`str_shuffle` over an engine `range()` closure, push/pop/shift/unshift/splice, slice/merge/merge_recursive/replace(_recursive) with reference and recursion semantics, the full `array_diff*`/`array_intersect*` families (fast paths, master's hash-based `array_intersect`, sort-based algorithms incl. the shared-callback-slot quirk), `array_unique` (all flags, enum grouping), walk/walk_recursive (live iteration with registered iterators), map/filter/reduce/find/any/all, column/combine/chunk/pad/fill/flip/count_values/sum/product. Verified by 5,600 oracle cases replayed against `/usr/bin/php` in both hash and packed layouts (`tests/data/oracle.txt`, master fix-ups) plus unit tests for references, recursion, the internal pointer and php-src regressions. Remaining: VM bindings (zpp, callables, by-ref args), objects in `array_walk`/`current()`&co. operate on a property-table snapshot, `Random\BrokenRandomEngineError` is reported as `Error`, `SORT_LOCALE_STRING` uses the C locale. |
| ext/standard: var_dump/print_r/var_export/serialize | 60% | high | `php-runtime::{dump,serialize}`: exact output for scalars/arrays/references, recursion detection, object output via `RuntimeContext::object_dump`/`serialize_object` hooks. Missing: `debug_zval_dump` refcounts, `__PHP_Incomplete_Class`, `allowed_classes`, `__wakeup`/`__unserialize` semantics (VM). |
| ext/spl | 0% | high | Data structures, iterators, `ArrayObject`, `SplFileObject`, exceptions, autoload (820). |
| ext/pcre | 60% | high | `preg_*` with PCRE2-compatible semantics (178). **Library done** (`crates/php-pcre`, binds the real PCRE2 via `pcre2-sys`, links the system lib when pkg-config finds it, else the bundled 10.46): exact port of `php_pcre.c` — delimiter/bracket/modifier parsing with master's warning texts (incl. `r` on PCRE2 ≥ 10.43, `\C`+`u` message), 4096-entry cache with PHP's eviction, locale-aware cache keys + `pcre2_maketables`, `pcre.backtrack_limit`/`recursion_limit`/`jit` (192K per-thread JIT stack, JIT NOMEMORY/compile warnings), match/match_all (all flag/order shapes, named-group key order and `/J` rules, `MARK`, trailing-group trimming, negative offsets, NOTEMPTY_ATSTART retry + UTF-8 advancement, UTF-8 / offset errors, "Get subpatterns list failed"), replacement parsing (`\n`/`$n`/`${n}`/escapes), replace/limit/count, callback (re-entrant, flags), array patterns, filter, split (all flags/limits), grep, quote, `preg_last_error(_msg)` codes, `PCRE_VERSION*`/JIT info. Verified by 411 oracle cases replayed against `/usr/bin/php` (normalised to master) plus unit tests. **Missing**: VM bindings (`preg_*` builtins, `$matches` by-ref, array subjects, `preg_replace_callback_array` loop, ValueError/TypeError texts, phpinfo rows) — then the % should track the .phpt pass rate; OOM-limit tests (`memory_limit`) not modelled. |
| ext/date | 60% | high | `DateTime*`, timezones, `strtotime` parser, `date()` formats (744). `crates/php-date` is complete as a library (timelib 2026.02 port): bundled *slim* timezonedb 2026.6 embedded byte-for-byte (`tools/gen_tzdb.py`; identifiers/BC/per-country lists, POSIX-footer transitions, `getTransitions`/`getLocation`, abbreviation + fallback maps, `timezone_name_from_abbr`), the `parse_date.re` grammar via an re2c-compatible longest-match scanner (lazy DFA) with every action, exact error/warning texts and positions (`date_parse`), `timelib_parse_from_format` (all specifiers, `!`/`|`/`+`/`*`/`#`/`?`), ISO 8601 durations/periods, tm↔unixtime with relative application/special weekdays/DST disambiguation, `diff`/`add`/`sub` (civil + wall), `date()`/`format()`/`DateInterval::format()`, glibc-compatible `strftime`, `idate`, `mktime`, `getdate`/`localtime`, `date_sun_info`/`date_sunrise`/`date_sunset`, `DatePeriod` iteration, `modify`, `setISODate`/`setTimestamp`/…, serialization helpers. Verified by 15.1k oracle cases vs `/usr/bin/php` (`tests/data/*.tsv`, normalised to master), the 45-file DST diff/add/sub/days phpt suite replayed from the `.inc` data, and hand-ported phpt expectations. Remaining: VM bindings (classes, properties/var_dump, exceptions with `Class::method(): ` prefixes, `date.timezone` INI, `__set_state`/`__serialize` glue, `DatePeriod` property handlers). |
| ext/reflection | 0% | high | Full Reflection API (577). |
| ext/json | 0% | high | `json_encode`/`json_decode`/`json_validate`, `JsonSerializable` (111). |
| ext/random | 60% | high | `Random\Randomizer`, engines (Mt19937, PCG, Xoshiro), `rand`/`mt_rand` that reproduce seeded sequences exactly (71). `crates/php-random` is complete as a library: Mt19937 (both modes), PcgOneseq128XslRr64 (int/16-byte seeds, `jump`), Xoshiro256StarStar (splitmix64/32-byte seeds, `jump`/`jumpLong`), Secure, userland engines (1..8-byte outputs), `range32/64`, legacy `MT_RAND_PHP` scaling, every `Randomizer` method (γ-section `getFloat`, `getBytesFromString` fast/slow paths, `pickArrayKeys` with hash-table holes), `mt_srand`/`mt_rand`/`rand`/`shuffle`/`str_shuffle`/`array_rand` on the global Mt19937, `lcg_value`, `random_int`/`random_bytes`, `uniqid`, engine `__serialize`/`__unserialize` state formats and exact error texts; replayed bit-exact against 8,000 `/usr/bin/php` results (`tests/fixtures/oracle.txt`). Remaining: VM bindings (classes, functions, `IntervalBoundary` enum, deprecations). |
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
