# TODO — phprust (entire project)

The action-ordered work queue, by impact. It lists only work that is not finished: a
completed item is deleted, not ticked (finished work is in git history and
`FEATURE_GAP.md`). `FEATURE_GAP.md` is the per-area table with % and priority; its %
column is refreshed from a PHPT run by `scripts/update_feature_gap.py`.

Status: [ ] open · [~] in progress · [-] blocked/awaiting.
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
- **Web servers**: end-to-end tests pass (22 Nov): nginx 1.24 -> FastCGI -> `php-cgi`
  (`crates/php-cli/tests/fastcgi.rs`, with a protocol test against `php-cgi -b`) and
  mod_php in Apache 2.4.58 (`crates/php-sapi-apache2/tests/apache_e2e.rs`: `apache2 -M`
  lists `php_module`, PHP through `http://127.0.0.1:<port>/`). PHPT `sapi/cgi` and
  `sapi/cli`: 135 of 135 run pass.

## A. Next (22 Nov)

- [ ] Evidence run `./run-all-tests.sh` (release build, `cargo test --workspace
  --release`, the full PHPT run on the release `php`, `timing.py` against
  `/usr/bin/php8.5`; it commits `<DD-MON-YYYY>.full-test-run.log`). The owner stopped the
  22 Nov 00:50 run during its Rust tests; run it again once the items below are done.
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

- [ ] apache2handler (FEATURE_GAP 75%): the full end-to-end test
  (`cargo test -p php-sapi-apache2 --test httpd -- --ignored --test-threads=1`, 22 Nov)
  fails 4 checks: `virtual('/hello.php')` prints nothing (the sub-request re-enters
  `request::run`, whose `PERSISTENT_VM.borrow_mut()` panics "RefCell already borrowed"
  in the child); session upload progress is `NULL`; the `max_execution_time` fatal of
  `slow/loop.php` is not in the error log; a `HEAD` response's body holds the header
  text (perhaps curl's `-I` output rather than the module). Fix them, then raise the row.
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

- [ ] Contribution policy (FEATURE_GAP 0%): CONTRIBUTING.md, code of conduct, security
  policy, before taking external contributions.
- [ ] Update this file after each evidence run.
- Rules that stay: never push unless the owner asks (`git push davidbuzz master`); JIT
  knobs are `BUZZS_*` (`BUZZS_JIT=0` off, `BUZZS_JIT_DEBUG=1` reports, `BUZZS_JIT_HOT=<n>`
  compiles a loop after n back edges instead of 1,000); after a JIT change also run the
  PHPT set with `--env BUZZS_JIT_HOT=100` and with `--env BUZZS_JIT=0`; PHPT runs use
  `-j 3`.
