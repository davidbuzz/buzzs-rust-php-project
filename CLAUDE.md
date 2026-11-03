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
- `crates/php-stdarr`  — ext/standard array functions over php-runtime Value/ZArray (ref: ext/standard/array.c)
- `crates/php-mbstring` — libmbfl port: encodings, conversion, mb_* string algorithms (ref: ext/mbstring, ext/mbstring/libmbfl)
- `crates/php-json`     — json_encode/json_decode/json_validate over php-runtime Value (ref: ext/json)
- `crates/php-filter`   — filter_var/filter_input validation+sanitize filters (ref: ext/filter)
- `crates/php-ctype`    — ctype_* (ref: ext/ctype)
- `crates/php-uri`      — RFC 3986 + WHATWG URL parsers behind Uri\\Rfc3986\\Uri / Uri\\WhatWg\\Url (ref: ext/uri)
- `crates/php-bcmath`   — arbitrary-precision decimal arithmetic for bc* functions and BcMath\\Number (ref: ext/bcmath)
- `crates/php-gmp`      — big-integer engine behind GMP objects/functions (ref: ext/gmp)
- `crates/php-calendar` — calendar conversions (ref: ext/calendar)
- `crates/php-zlib`     — zlib binding with ext/zlib semantics (gz*, deflate/inflate contexts, zlib.* filters)
- `crates/php-bz2`      — libbz2 binding with ext/bz2 semantics
- `crates/php-iconv`    — glibc iconv binding with ext/iconv semantics
- `crates/php-posix`    — ext/posix over libc
- `crates/php-openssl`  — OpenSSL binding with ext/openssl semantics
- `crates/php-sqlite`   — libsqlite3 binding behind ext/sqlite3 and ext/pdo_sqlite
- `crates/php-libxml`   — libxml2 FFI base + ext/xml (expat-style) + ext/xmlwriter; dom/simplexml/xmlreader/xsl build on it
- `crates/php-intl`     — ICU binding with ext/intl semantics (ref: ext/intl)
- `crates/php-exif`     — EXIF/JPEG/TIFF metadata parser with ext/exif semantics (ref: ext/exif)
- `crates/php-sockets`  — ext/sockets over libc
- `crates/php-sysv`     — ext/sysvmsg, sysvsem, sysvshm, shmop over libc
- `crates/php-gettext`  — ext/gettext over libc
- `crates/php-pcntl`    — ext/pcntl over libc (signal dispatch hooks for the VM)
- `crates/php-spl`      — SPL data structures, iterators, ArrayObject core over php-runtime (ref: ext/spl)
- `crates/php-session`  — session core: serializers, save handlers, ID generation, INI semantics (ref: ext/session)
- `crates/php-mysqlnd`  — pure-Rust MySQL client protocol (ref: ext/mysqlnd); `php-ext-mysqli` binds ext/mysqli on it
- `crates/php-sapi-apache2` — mod_php: Apache 2.4 module SAPI (ref: sapi/apache2handler)
- `crates/php-zip`      — libzip binding with ext/zip semantics
- `crates/php-fileinfo` — libmagic binding with ext/fileinfo semantics
- `crates/php-sodium`   — libsodium binding with ext/sodium semantics
- `crates/php-xsl`      — libxslt binding over php-libxml with ext/xsl semantics
- `crates/php-memlimit` — counting global allocator + memory_limit accounting for the "Allowed memory size exhausted" fatal (ref: Zend/zend_alloc.c semantics)
- `crates/php-cli-server` — the CLI built-in web server (`php -S`) (ref: sapi/cli/php_cli_server.c)
- `crates/php-curl` + `crates/php-ext-curl` — libcurl binding with ext/curl semantics (ref: ext/curl)
- `crates/php-ftp` + `crates/php-ext-ftp` — FTP client protocol with ext/ftp semantics (ref: ext/ftp)
- `crates/php-stdmisc`  — ext/standard: crypt/password, exec/proc_open, network/dns, env/info, highlight, getopt, http_build_query/parse_str, locale
- `crates/php-phar`     — phar/tar/zip archive formats, manifest, signatures, stub, phar:// semantics (ref: ext/phar)
- `crates/php-dom`      — ext/dom (legacy DOMDocument API + PHP 8.4 Dom\\* HTML5 API) over php-libxml (ref: ext/dom, ext/lexbor)
- `crates/php-simplexml` — SimpleXMLElement semantics over php-libxml (ref: ext/simplexml)
- `crates/php-xmlreader` — XMLReader pull parser over php-libxml (ref: ext/xmlreader)
- `crates/php-pdo`      — PDO core: PDO/PDOStatement semantics, fetch modes, error modes, driver trait (ref: ext/pdo)
- `crates/php-soap`     — SOAP core: WSDL/XSD parser, SOAP 1.1/1.2 encoding of values, packet build/parse, HTTP transport glue (ref: ext/soap)
- `crates/php-gd`       — libgd binding with ext/gd semantics (system libgd 2.3.3; GD_BUNDLED is false) (ref: ext/gd)
- `crates/php-ext-zendtest` — ext/zend_test: the engine test extension gating `--EXTENSIONS-- zend_test` tests (ref: ext/zend_test)

## Parallel work
Several agents work concurrently, one crate each. Only edit files inside your own crate (plus your row(s) in FEATURE_GAP.md). Never edit workspace Cargo.toml; crates are pre-registered. Commit your own crate's files with `git add crates/<yours>` only.

## Resource limits (the box has OOM-crashed once)
- `.cargo/config.toml` caps `cargo` at 2 jobs; never override it with `-j`.
- EVERY invocation of `/usr/bin/php` (oracle scripts, fixture generators) MUST pass `-d memory_limit=1G`. Ubuntu's CLI php.ini sets `memory_limit=-1`, and one unbounded script took the machine down.
- Do not run more than one `cargo test`/`cargo build` at a time from a single agent; do not run the whole php-src suite with `--php /usr/bin/php` (use subsets).

## Test services
- MariaDB 10.11 is installed for ext/mysqli tests: database `test`, user `phptest` / password `phptest` on 127.0.0.1:3306 (set `MYSQL_TEST_HOST=127.0.0.1 MYSQL_TEST_USER=phptest MYSQL_TEST_PASSWD=phptest MYSQL_TEST_DB=test` when running those tests). Start/stop with `sudo systemctl start|stop mariadb`.
- Apache 2.4 + apache2-dev (`apxs`) are installed for the mod_php SAPI; keep `apache2` stopped unless testing it.
