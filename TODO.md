# TODO — phprust (entire project)

The action-ordered work queue: open work first, by impact; finished work is summarised at
the end (the details are in git history and `FEATURE_GAP.md`). `FEATURE_GAP.md` is the
per-area table with % and priority; its % column is refreshed from a PHPT run by
`scripts/update_feature_gap.py`.

Status: [ ] open · [~] in progress · [x] done · [-] blocked/awaiting.
Last updated 22 Nov 2026.

## Where things stand

- **The deliverable** (README): the total runtime of ours on identical inputs, both at
  their defaults, is at most 1.10x C's, measured by `perf/bench/timing.py`. Last measured
  21 Nov (release build of `d3380559`, against C PHP 8.3.6): **0.53x** wall-clock (1.847 s
  against 3.517 s), every one of the 31 inputs faster (0.32x `suite/calls.php` to 0.98x
  `suite/float_math.php`). The reference is now `/usr/bin/php8.5` (PHP 8.5.12); the first
  measurement against it is PHASE 4 of today's `./run-all-tests.sh`.
- **Conformance**: full in-scope PHPT run of 21 Nov (debug binary, `1ffce758`, and again
  on `d3380559`): **0 failures**. Of 21,844 tests run, 21,829 pass, 11 are XFAIL and 4 WARN
  (XFAIL tests that pass); 1,086 skip for host reasons (Windows-only, 32-bit-only, PDO
  drivers that are not installed, bundled-GD and older-library variants, online tests,
  readline, FPM); 1,134 are out of scope (`phpt-scope.txt`).
- **Per-workload guide**: `scripts/acceptance.sh` 82/82 workloads within 1.10x of
  `perf/bench/goal.tsv` with the JIT on (25/82 with `BUZZS_JIT=0`), 21 Nov. `goal.tsv`
  still holds counts measured from C PHP 8.3.
- **Rust tests**: `cargo test --workspace` 4,980 passed, 122 ignored (21 Nov; the run's two
  failures fixed and rechecked).

## A. In flight (22 Nov)

- [~] Evidence run (owner, started 00:50): `./run-all-tests.sh` = release build,
  `cargo test --workspace --release`, the full PHPT run on the release `php`,
  `timing.py` against `/usr/bin/php8.5`; commits `22-NOV-2026.full-test-run.log`.
- [~] Branch `e2e-web` (worktree `/tmp/claude-1000/wt-e2e`), edited only, to be merged
  after the evidence run, then one build and the checks below:
  - FastCGI in `php-cgi` (ref: main/fastcgi.c): `-b <addr:port>|<port>|<path>`, a
    listening socket on standard input, keep-alive, `GET_VALUES`,
    `PHP_FCGI_MAX_REQUESTS`, `PHP_FCGI_CHILDREN`, `FCGI_WEB_SERVER_ADDRS`,
    `fastcgi.logging`.
  - End-to-end tests: `crates/php-cli/tests/fastcgi.rs` (the protocol against
    `php-cgi -b`; nginx 1.24 passing `*.php` to `php-cgi` over a Unix socket) and
    `crates/php-sapi-apache2/tests/apache_e2e.rs` (mod_php in the system apache2:
    `apache2 -M` lists `php_module`, PHP through `http://127.0.0.1:<port>/` URLs).
  - The C reference `/usr/bin/php` -> `/usr/bin/php8.5` in tests, scripts and docs.
  - Removed: TAPE_FASTER_MOSTLY.md, perf/RELEASE_BENCH_2026-11-04.md,
    perf/NOTE-jit-2026-11-16.md, perf/TODO-jit-step1.md, the root copy of
    REVIEW-tape-series-2026-11-13.md, crates/php-ext-pcre/PROPOSALS.md.
  - Then: `cargo fmt`, `clippy --all-targets` and `cargo test` of php-cli,
    php-cli-server, php-sapi-apache2 and php-vm; the tests that run the reference and
    now run 8.5 (php-pcntl, php-posix, php-sockets, php-sysv, php-session, php-mysqlnd,
    phpt-runner); PHPT `sapi/cgi`.
- [ ] Re-derive the 8.3 data with 8.5.12 (owner, 22 Nov): regenerate the oracle fixtures
  recorded from 8.3 (php-hash vectors, php-ctype/php-json/php-filter `oracle.txt`,
  php-filter HTML/Unicode tables, php-sodium, php-fileinfo, php-support fmt/ini cases,
  php-intl constants, php-mbstring and the php-stdstr/stdarr/stdmisc fixtures) with
  `/usr/bin/php8.5`, re-check the "(8.3)" expected outputs in the php-vm tests, and fix
  toward 8.5/master whatever then differs. About 70 lines still name 8.3 or 8.3.6.
- [ ] Re-record `perf/bench/goal.tsv` from `/usr/bin/php8.5` (`acceptance.py --record`)
  and re-run `scripts/acceptance.sh` (owner, 22 Nov). 8.5 may be faster than 8.3 on
  some workloads, so the 82/82 may change.
- [ ] README and FEATURE_GAP numbers from today's evidence run (timing against 8.5.12,
  cargo test, PHPT), replacing the 8.3.6 figures (owner, 22 Nov).

## B. Performance beyond the benchmark inputs

The deliverable is met on the 31 inputs. These are shapes real applications run that the
inputs do not cover, measured 21 Nov in instructions per loop iteration (ours / C);
candidates for `perf/bench` and for work.

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

- [ ] apache2handler (FEATURE_GAP 75%): the row's "Missing" list dates from 3 Nov; output
  start, the header callback, implicit flush, abort polling, the CPU timer and upload
  progress were implemented on 6-7 Nov (`19de417e`, `29f444d5`). Run
  `cargo test -p php-sapi-apache2 --test httpd -- --ignored --test-threads=1`, then
  update the row.
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

## D. Housekeeping

- [ ] FEATURE_GAP's "Remaining"/"Missing" notes still name tests that pass in the 21 Nov
  full run: Fibers (destructors_004 & co.), Attributes (005_objects, `#[\NoDiscard]`),
  ext/session (bug79031 & co.), ext/sodium (sensitive_parameter, sodium_error_001),
  zend_test (gh17899, observer_preload, optimizer_register_pass), ext/uri
  (`zend/internal_api_*`), mysqli_result_references, ext/fileinfo (libmagic
  descriptions); the `.phpt` runner row says CGI sections are not done and the
  Superglobals row calls CGI request parsing out of scope (php-cgi exists since 10 Nov);
  the Strings row says `ZStr` has no cached UTF-8 validity flag (it has:
  `is_valid_utf8()`/`mark_valid_utf8()`). Prune them against the full run.
- [ ] Contribution policy (FEATURE_GAP 0%): CONTRIBUTING.md, code of conduct, security
  policy, before taking external contributions.
- [ ] Update this file after each evidence run.
- Rules that stay: never push unless the owner asks (`git push davidbuzz master`); JIT
  knobs are `BUZZS_*` (`BUZZS_JIT=0` off, `BUZZS_JIT_DEBUG=1` reports, `BUZZS_JIT_HOT=<n>`
  compiles a loop after n back edges instead of 1,000); after a JIT change also run the
  PHPT set with `--env BUZZS_JIT_HOT=100` and with `--env BUZZS_JIT=0`; PHPT runs use
  `-j 3`.

## Done (summary)

- The deliverable: 0.53x C's total runtime on the 31 inputs (21 Nov, against 8.3.6);
  `perf/bench/timing.py` measures it; `./run-all-tests.sh` records the build, the Rust
  tests, the PHPT run and the timing in a dated log committed to git (22 Nov).
- The trace JIT, on by default since 20 Nov (`57aa54cc`): whole loops as x86-64 code;
  inlined function, static and instance-method calls; property rows; static properties
  and class constants; array element rows (int and string keys, lists read and written
  in place); float comparisons and `op=`; global reads; array copies; pure-int
  functions compiled whole with native recursion.
- Interpreter fast paths: string-key probes, `foreach` steps, `new` sites and
  constructor folds, objects made over in place: 82/82 acceptance workloads (21 Nov).
- Optimizer: every opcache optimizer and dump test passes (SCCP, DCE, `JMP_NULL` chains,
  escape analysis, named arguments of known callees, PFA dumps, type narrowing on
  branches), and three wrong results with the optimizer on fixed (21 Nov).
- Conformance: the 38 failures of 16 Nov down to 0 (21 Nov), including the rfc1867
  memory test (large blocks take a chunk each), the session reserved-properties test,
  ext/gettext (`LANGUAGE` unset) and the debug-binary-only opcache tests.
- SAPIs and drivers: php-cgi (CGI/1.1, 10 Nov), ext/pdo_mysql (296 run tests pass),
  ext/mysqli + mysqlnd, mod_php (prefork).
