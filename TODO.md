# TODO — phprust (entire project)

The action-ordered work queue, by impact. It lists only work that is not finished: a
completed item is deleted, not ticked (finished work is in git history and
`FEATURE_GAP.md`). `FEATURE_GAP.md` is the per-area table with % and priority; its %
column is refreshed from a PHPT run by `scripts/update_feature_gap.py`.

Status: [ ] open · [~] in progress · [-] blocked/awaiting.
Last updated 22 Nov 2026.

## Where things stand

- **The deliverable** (README): the total runtime of ours on identical inputs, both at
  their defaults, is at most 1.10x C's, measured by `perf/bench/timing.py`. Measured
  22 Nov in the evidence run (`22-NOV-2026.full-test-run.log`, release build of `725007d4`)
  against `/usr/bin/php8.5` (PHP 8.5.12): **0.49x** wall-clock (1.637 s against 3.330 s),
  every one of the 31 inputs faster (0.33x `suite/calls.php` to 0.95x
  `suite/builtins.php`).
- **Conformance** (the same run, release binary): of 21,843 in-scope tests run, 21,827
  pass, 11 are XFAIL and 4 WARN, 1 fails (see C); 1,087 skip for host reasons
  (Windows-only, 32-bit-only, PDO drivers that are not installed, bundled-GD and
  older-library variants, online tests, readline, FPM); 1,134 are out of scope.
- **Per-workload guide**: `scripts/acceptance.sh` against the goal re-recorded from
  `/usr/bin/php8.5` (22 Nov, release build of `3fa1dfd7`, JIT on): 80/82 workloads within
  1.10x; over: `suite:builtins` 1.15, `zend:hash1` 1.12 (`zend:hash2` 1.09). Against the
  8.3 goal all 82 were within (21 Nov): 8.5 is 9% and 3% cheaper on those two.
- **Rust tests** (the same run, `cargo test --workspace --release`): 4,991 passed, 122
  ignored, 3 failed: `php-vm/tests/object_reuse.rs` asserted a count that only debug
  builds keep (fixed after the run: the count is checked in debug builds only).
- **Web servers**: end-to-end tests pass (22 Nov): nginx 1.24 -> FastCGI -> `php-cgi`
  (`crates/php-cli/tests/fastcgi.rs`, with a protocol test against `php-cgi -b`) and
  mod_php in Apache 2.4.58 (`crates/php-sapi-apache2/tests/apache_e2e.rs`: `apache2 -M`
  lists `php_module`, PHP through `http://127.0.0.1:<port>/`; the full mod_php test
  `--test httpd -- --ignored` passes every check). PHPT `sapi/cgi` and
  `sapi/cli`: 135 of 135 run pass.

## A. Next (22 Nov)

- [~] **FPM** (in scope since 22 Nov, owner): `php-fpm` is done (see FEATURE_GAP), with
  the end-to-end test `crates/php-cli/tests/fpm_e2e.rs` (nginx -> php-fpm). Open:
  - `get_browser()` is 30 times slower than C (1.1 s against 0.03 s on the test
    browscap.ini in a release build, 14 s in a debug build), so `gh12621.phpt` times
    out in debug runs (`php-stdmisc` browscap matching, not FPM).
  - The slowlog trace is written by the worker at its next safe point, so a builtin
    that blocks (`sleep()`) shows its caller, not itself (C reads the stopped worker
    by ptrace).
- [~] Re-derive the 8.3 data with 8.5.12 (owner, 22 Nov). Done: php-hash vectors, php-filter
  HTML/Unicode tables, php-ctype/php-json/php-filter `oracle.txt` (only filter's differed;
  its replay now matches 8.5 on every record). To do: the other oracle fixtures recorded
  from 8.3 (php-sodium, php-fileinfo, php-support fmt/ini cases,
  php-intl constants, php-mbstring and the php-stdstr/stdarr/stdmisc fixtures) with
  `/usr/bin/php8.5`, re-check the "(8.3)" expected outputs in the php-vm tests, and fix
  toward 8.5/master whatever then differs. About 70 lines still name 8.3 or 8.3.6.

## B. Performance beyond the benchmark inputs

- [ ] The two workloads over the 8.5 goal: `suite:builtins` (1.15x: string builtins in a
  loop, see the Strings item below) and `zend:hash1` (1.12x: string keys built at run time
  and `isset()` on them); `zend:hash2` is at 1.09x.

The deliverable is met on the 31 inputs. These are shapes real applications run that the
inputs do not cover, measured 21 Nov in instructions per loop iteration (ours / C);
candidates for `perf/bench` and for work.

- [ ] Ordinary (not loop-hot) code, measured 23 Nov by `perf/interp/run.sh`: instructions
  per operation with our JIT off, ours / C 8.5. Calls: a plain call 1,141 / 172, a method
  1,339 / 225, a static method 1,088 / 182, a closure 1,284 / 257, defaults filled in
  1,799 / 216, `new` with a constructor 3,323 / 755; `call_user_func_array()` 6,206 / 553
  (the builtin -> nested `execute()` path; `PendingCall` moves show as `memmove`);
  `func_get_args()` 3,860 / 513; `is_callable()` 3,122 / 370. Variables: `static $n` 4,104
  / 264, `global $g` 2,044 / 262, `compact()`+`extract()` 17,990 / 1,277. Builtins: the
  frameless wrapper `frameless_icall_op` costs ~340 per call (`trim` 1,140 / 285); `implode`
  3,357 / 615 is mostly its own UTF-8 tracking; `preg_match` 4,775 / 597; `instanceof`
  446 / 59. Element and property reads and writes are near or under C. A large framework
  page (warm opcache, one process) runs 1,706 M instructions against C's 251 M; its
  profile is flat: the generic interpreter paths, the call protocol and hash-table work.
- [ ] Code that runs with an attached symbol table (the top level of an included file, a
  function that uses `include`, `extract()`, `compact()` or `$$var`), 300 other variables
  in scope: an array write in a loop 12.0 ms against C's 2.1 (19.9 before 23 Nov's fixes;
  5.3 / 2.0 in a plain function), 200,000 calls of a small function 33.5 / 3.1 (0.6 / 2.7
  in a function: those calls take the generic `op_send`/`push_simple_ucall` path, not the
  tape's). 23 Nov removed the table round trip of writes in functions (024a945a), the
  global re-synchronisation after writes that do not touch `$GLOBALS` (7bf94dc6) and the
  name lookups of included files' frames (ff8f6d01); php-src points the attached frame's
  CVs into the table (`IS_INDIRECT`), so nothing is synchronised at all.
- [ ] Per-request state in long-running workers: since 23 Nov `php -S` (72a3245f),
  php-cgi's FastCGI loop and php-fpm (c3825834) keep one VM per worker and reset it
  (`Vm::request_reset`, as mod_php did) instead of a new VM per request (a one-line page
  22.0 M -> 1.9 M instructions; php-src 0.1 M). That exposed state that outlived the
  request (SPL's autoloader registry, 6599a1a1; objects freed after the store reset,
  6ab1cde9) and state dropped that php-src keeps (opcache's scripts, e2a8ca7e; PCRE's
  compiled patterns, 282cffca). 24 Nov: resource numbers restart per request, and
  request startup and shutdown follow `php_request_startup()` (the execution timer was
  never armed from INI, counted wall time and had no hard timeout; 4f90f9c0); caches
  keyed by a class or document address that outlived the object (SPL, DOM) and openssl
  callables kept past the request. The check: every php-src test script run in order on
  one `php -S` against a fresh server per script (scripts whose two fresh runs agree);
  still to do: act on its differences, and the remaining 1.8 M instructions of an empty
  request (`request_reset` rebuilds tables such as `CallBufs` and `ExtState`).
- [ ] Generators: `send()` 4,499 / 510, a `foreach` over a generator 2,556 / 450 (the
  frame is swapped onto the stack and back; the rest is the builtin-method call machinery
  around `send()` and the nested `execute()`).
- [ ] Calls and returns: a method that writes `$this->items[$k]` and returns `$this`
  2,498 / 479. Every return that is not a scalar from a variable or temporary of an
  untyped function goes through `call.rs::return_generic` (`return;`, a constructor's
  end, `return $this`, strings, arrays, typed returns).
- [ ] Objects: `new B(...)` with a constructor body 3,129 / 1,235 (`calls::new_over` and
  the site check fail for every class whose constructor has a body); `new S("abc", $i)`
  1,450 / 742 (an old object whose slots hold strings, arrays or objects is not made
  over in place).
- [ ] Arrays: `['id' => $i, 'name' => 'x', 'n' => $i]` 1,858 / 559, `[$i, $i]`
  1,029 / 404, a `foreach` over an array of arrays 370 / 158, over a list of strings
  181 / 140, with keys over a hash 174 / 141 (the general steps of `fe_fetch_into`).
- [ ] Strings: `strtoupper()`/`strpos()`/`str_contains()` 3,251 / 1,717
  (`strs::frameless_quick_n` costs about 165 around a two- or three-argument builtin;
  `strtoupper()` & co. are handler calls).
- [ ] Trace JIT coverage (`crates/php-vm/src/exec/tape/jit.rs`): float arithmetic into a
  temporary or another variable leaves the trace; property access inside inlined callee
  bodies; `self::`/`static::` scope guards for call inlining; closures; native-call
  inlining; heapsort's outer loop (a `return` inside it) and `gen_random` (constants
  defined at run time, a `global`) stay interpreted; a `foreach` step is no trace row;
  strings and objects moved by a trace; a handler call from a trace saves and restores
  five registers (callee-saved registers for the trace's bases would spare that).
- [ ] The interpreter's user-call protocol (the call-redesign plan: Rc-free prepare,
  `num_args`, fused enter, one flag word).
- [ ] Memory allocator (FEATURE_GAP 50%): the per-allocation cost (about 81 -> 30
  instructions) is a design estimate; profile the release build and set the % from it.
- [ ] Value layout (FEATURE_GAP 90%): one allocation per object (slots inline), the
  reference size (72 B against php's 32), the 12-byte-per-slot hash index.
- [ ] With the JIT off (`BUZZS_JIT=0`, not the default): 25/82 workloads meet the goal;
  dim_read, dim_write and abs are among those over it. Low priority: the deliverable is
  measured at the default, JIT on.

## C. Conformance and features

- [ ] SAPIs (from PR #9, whose `php-sapi` crate gave every SAPI one `RequestInfo`): the
  SAPI-internal request structs (the CLI's `Request`, CGI's `RequestInfo`, the built-in
  server's `Request`, Apache's accessors) still exist; move them onto `RequestInfo` with a
  `SapiModule` trait after `sapi_module_struct`. `crates/php-vm/src/include.rs` still
  resolves include paths with `std::fs::canonicalize`, not the realpath cache
  (`tsrm_realpath`); measure before changing it, it is on the include path.
- [ ] Startup warnings in php-cgi: an invalid INI value given with `-d` (`pcre.backtrack_limit
  =xyz`) is reported before the output and as text; php-cgi 8.5 prints it after the output
  and as HTML (`html_errors=1` in CGI).
- [ ] The opcache file cache in file-cache-only mode serves the first cached copy of an
  include the script rewrote within the same second
  (`Zend/tests/partial_application/cache_freed_declaring_site_005`, with run-tests'
  `opcache.file_update_protection=0` and `validate_timestamps=0`); check what php-src's
  `zend_file_cache_script_load` does there.
- [ ] A long-running `php -S` dies (`_exit(137)` from the memory limit's hard-limit path)
  after scripts that exhaust a lowered `memory_limit` (`Zend/tests/oom_*`,
  `json_decode_oom_utf8_substitute`); C ends only the request with "Allowed memory size
  exhausted". Found by the shared-server check (every php-src test script in order on one
  `php -S`), which otherwise showed 19 differences against fresh servers before
  `c6419b5a` fixed 5 of them (mbstring regex state), 11 of them also in C's own `php -S`;
  the rest: `Zend/tests/gc/bug70805*`, `generators/gh9801`. Run it again after this.

- [ ] `gc_status()["roots"]` counts objects only: php-src also buffers an array whose count
  drops to a non-zero value (`$b = $a; unset($b);` is 1 root there, 0 here; `implode()`
  releases its array with `GC_TRY_DTOR_NO_REF`). `gc_collect_cycles()` results are not
  affected (cycles always pass through an object or a reference here).
- [ ] `$g .= $g` with `$g` undefined and an error handler that throws: C reads the right
  operand first (the handler throws), suppresses the left operand's warning while the
  exception is pending and stores `""`; ours stops at the first warning. (The other
  compound assignments now store their result before throwing, as php-src does.)

- [ ] `php -S` (CLI built-in web server row): its "Missing" note (session cookies, the
  header callback when output sends the headers, the "output started at" location, the
  `ignore_user_abort` bailout) dates from 3 Nov, and every run `sapi/cli` test passes;
  check each and fix or remove it.
- [ ] opcache's own JIT (`opcache.jit=tracing|function`) reports "JIT is not available",
  so 5 `ext/opcache/tests/jit` tests skip (bug80426, gh13400, gh21710,
  preload_restart_function_jit, preload_restart_hot_counters). C's CLI default has it
  disabled; our trace JIT is a separate thing.
- [ ] Needs host libraries this box does not have (the tests skip): sodium
  AEGIS/IPCrypt/XOF/ML-KEM (libsodium >= 1.0.19/1.0.21; 9 tests); `setlocale(LC_CTYPE)`
  table selection in ext/ctype (lc_ctype_inheritance needs a de_DE locale);
  newer-libzip paths (seekable streams, torrentzip, zstd).
- [ ] No test covers (FEATURE_GAP "Missing"): http `ftp://` proxying; ext/zip callbacks
  when a stream closes the last reference.
- [ ] New php-src tests (pin cb90f9a2377) still failing:
  - `ext/fileinfo` gh25051, hwpx_mime: master's bundled libmagic database (5.48, format
    21; the system libmagic 5.45 reads format 18) gives `font/ttf` for TrueType and
    detects HWPX/NuGet zips; port its entries into `php-fileinfo`'s overlay.

## D. Housekeeping

- [ ] Update this file after each evidence run.
- [ ] CI: reuse what php-src's GitHub Actions do (reviewed 24 Nov in `modules/php-src/.github`
  at the pin: `workflows/test-suite.yml`, `actions/*`). Our CI so far is
  `.github/workflows/aarch64-linux.yml`. Worth taking, roughly in this order:
  - The variants of a test run (`actions/test-linux`): the suite again with
    `-d opcache.enable_cli=1`, and with `--repeat 2` (every test twice in one process: the
    per-request state; `php --repeat` and the runner's `--repeat` exist); run-tests.php's
    `-g FAIL,BORK,LEAK,XLEAK --offline --show-slow 1000 --set-timeout 120` and the
    environment `SKIP_IO_CAPTURE_TESTS=1 STACK_LIMIT_DEFAULTS_CHECK=1
    RUN_RESOURCE_HEAVY_TESTS=1`.
  - `OPCACHE_VARIATION`: the file cache primed, then used (after shared memory, and with
    `opcache.file_cache_only=1` both ways). The runner needs `--file-cache-prime` and
    `--file-cache-use` (run-tests.php sets `opcache.file_cache`, `opcache.record_warnings=1`,
    `opcache.jit=0`, `opcache.validate_timestamps=0` when using, and skips the opcache tests).
  - The host setup that turns on tests that skip here (`actions/apt-x64`, `setup-x64`):
    the `de_DE` locale (`language-pack-de`, `locale-gen de_DE`: ext/ctype
    lc_ctype_inheritance), MySQL 8.4 as a service (mysqli and pdo_mysql against MySQL as
    well as MariaDB), the Caddy server for HTTP tests (`actions/setup-caddy`), newer
    library versions on a newer image (libsodium >= 1.0.19 for AEGIS/ML-KEM).
  - The 32-bit job (`LINUX_X32`): the 32-bit-only tests that skip here, on an
    `i686-unknown-linux-gnu` build.
  - `COMMUNITY`: the test suites of real projects run on the built `php` (AMPHP, Laravel,
    ReactPHP, Revolt, Symfony, PHPUnit, ...: `git clone --depth 1`, `composer install
    --ignore-platform-req=php+`, `vendor/bin/phpunit`), and the Symfony demo with preloading.
  - `BENCHMARKING`: `benchmark/benchmark.php` counts instructions under valgrind for
    `Zend/bench.php` and two framework applications through `php-cgi` (50 warm-ups, 50 runs),
    and php-src keeps every commit's numbers in `php/benchmarking-data`: a C baseline per
    commit to compare `perf/bench` with.
  - Scheduling (`workflows/test.yml`, `.github/matrix.php`): a nightly run of every job;
    pull-request labels (`CI: All jobs`) select the heavy ones.
  - Later: sanitizer jobs (ours would be nightly Rust's address sanitizer over the FFI
    code), macOS, Alpine (musl) and FreeBSD builds, the stale-PR and labeler workflows.
    Not applicable: stub/arginfo verification (`verify-generated-files`), the
    bundled-library checks, Windows builds, the C unit tests, the self-hosted benchmark
    (`real-time-benchmark.yml`).
- Rules that stay: never push unless the owner asks (`git push davidbuzz master`); JIT
  knobs are `BUZZS_*` (`BUZZS_JIT=0` off, `BUZZS_JIT_DEBUG=1` reports, `BUZZS_JIT_HOT=<n>`
  compiles a loop after n back edges instead of 1,000); after a JIT change also run the
  PHPT set with `--env BUZZS_JIT_HOT=100` and with `--env BUZZS_JIT=0`; PHPT runs use
  `-j 3`.
