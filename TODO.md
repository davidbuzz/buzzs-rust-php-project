# TODO — phprust (entire project)

Companion to `FEATURE_GAP.md` (the authoritative gap table with %/pri per row;
keep it updated via `scripts/update_feature_gap.py`). This file is the
action-ordered work queue. Ordered by impact: biggest measured misses first,
then conformance closes, then in-scope unimplemented features.

Status: [ ] open · [~] in progress · [x] done · [-] blocked/awaiting

## A. Performance vs C (goal: all 82 goal.tsv workloads ≤ 1.10×)
Score: 48/82 meet (measured 2026-11-20 with `scripts/acceptance.sh`, JIT on by default; 17/82 with `BUZZS_JIT=0`).

- [x] JIT Step 1 — inline static-prop read/write/isset + class-const folds (`Ext` tape rows).
      Committed 2026-11-20 with ten row fixes; the wrong `empty()` answer was a `Bool`
      written at the `Int` payload offset (a `Bool`'s value is the byte after the tag).
      Sub-list: perf/TODO-jit-step1.md
- [~] JIT Step 2 — array/string dim rows. Done 2026-11-20 (`24eca59c`): int-offset array
      rows for int elements (`$t = $a[$i]`, the folded `$v = $a[$i]`, `$a[$i] = v`, `$a[] = v`,
      `$a[$i] op= v`) through helpers the trace calls, native `& | ^ << >>` and `NumBinC`.
      Open: string keys (hash_read, x_eq_hash_v, hash1/hash2), string offsets (x_eq_str_0),
      nested dims (matrix), float elements (heapsort), `isset`/`++` on an element, and an
      inline packed read in place of the helper call. ary (1.59) and ary2 (1.92) compile now
      and are still over: what is left is table growth and hash inserts in php-runtime.
- [ ] JIT Step 3 — call-heavy tail: frameless recursion (ackermann 10.67, fib 6.43,
      fibo 6.39), closure inlining (ops:closure 4.15, ops:new_obj adjacent),
      native-call inlining (builtins 5.36, calls 5.09, int_func 2.15).
- [ ] JIT Step 4 — `New` rows (new_obj 3.28, micro:new_Foo 2.12, objects 4.63),
      elvis/ternary (`JmpZC` + `Step` rows: x_eq_f_q_f_a 3.49, x_eq_a_elvis_null 2.92,
      x_eq_f_elvis_tmp 2.51, x_eq_f_q_f_tmp 1.94), `$this::CONST` (x_eq_Foo_cc_TEST 3.00: a
      dynamic class operand), and the loops the tape never enters, so no trace is attempted:
      `self::f()` (self_cc_f 4.64), `$GLOBALS['v']` (2.60), `$_GET` (3.80).
      Done 2026-11-20: `null`/bool constant folds (x_eq_TEST 2.37 -> 0.29).
- [x] JIT is the default (owner, 2026-11-20; `57aa54cc`): on for x86-64 Linux, `BUZZS_JIT=0`
      turns it off. `scripts/acceptance.sh` now measures it without a wrapper binary.
- [ ] JIT follow-ons from NOTE-jit-2026-11-16: guard-reload/loop-note machinery;
      floats in traces (mandel 1.60, float_mul, float_math); property access inside
      inlined callee bodies; `self::`/`static::` scope guards for call inlining.
- [ ] Memory allocator: profile per-allocation cost claim (~81→~30 ins) on release build;
      update FEATURE_GAP row % from estimate to measured.
- [ ] Object creation/layout (0% row): New-row JIT + constructor fast paths; zval-layout
      items (one-alloc objects, reference size, 8B hash index) — design pass.
- [ ] Interpreter-level spec gaps: dim_read/dim_write/abs still over goal JIT-off.

## B. Conformance close-out to 100% in-scope phpt (currently 99.8%, 38 failures)
- [ ] Optimizer/opcache dump failures (30 of 38): DCE regression in
      `crates/php-vm/src/compiler/optimizer/compact.rs` (~1073–1168) is the top item, then
      dce_*, sccp_*, nullsafe_*, coalesce, assign_dim_op_result_type, gh18107_1/2,
      preload_optimizer, array_map_foreach_optimization_007/008/013/014,
      named_parameter_new, gh18050, gh9164, jit/ignored_opcodes.
- [ ] tests/basic/oom_rfc1867_files_registration_full_path.
- [ ] ext/session rfc1867_incomplete_class_reserved_props.
- [ ] ext/gettext 6 failures: host locale `.mo` setup (environment, not engine) — script
      a locale fixture setup so they run.
- [ ] Debug-binary-only opcache failures (sccp_003/004/009/016/031/032,
      ct_eval_frameless_002).
- [ ] Verify (not cause of JIT work, flagged earlier): `PHP Warning:` CLI prefix /
      leading-newline difference vs C PHP in our stderr formatting.
- [ ] mysqli_result_references (refcount-1 reference dumped as `&`) — closes mysqli 99%→100%.

## C. In-scope but unimplemented (0% rows)
- [ ] `php-cgi` binary (sapi/cgi port) + phpt-runner CGI sections → unlocks ~305 skipped
      tests (ext/phar 102, tests/basic 80, ext/standard 47, filter 21, session 21, zlib 11,
      soap 10, mbstring 8, Zend 3, iconv 1, lang 1).
- [ ] ext/pdo_mysql driver over php-mysqlnd (169 tests, MariaDB env).

## D. Smaller residuals to 100% (med/low, from FEATURE_GAP "Missing" notes)
- [ ] apache2handler (75%): output-start file:line in header warnings, implicit_flush,
      abort bailout, header_register_callback before implicit sends, session upload
      progress, real interruptible timeout.
- [ ] php -S server: session cookies, header callback with output-started location,
      ignore_user_abort bailout.
- [ ] ext/opcache: preload (66 tests), file cache (3), INI-validation warnings,
      native machine-code JIT emission (our own JIT is separate; this is opcache's).
- [ ] sodium: SensitiveParameter reflection marker, trace-arg scrub hook (sodium_error_001),
      AEGIS/IPCrypt/XOF/ML-KEM (needs libsodium ≥1.0.19 — host pkg question).
- [ ] filter: `uri_parser_class` php-uri hook wiring.
- [ ] ctype: setlocale(LC_CTYPE) table selection (needs de_DE locale).
- [ ] zip: callbacks on last-ref stream close; newer-libzip paths (seekable, torrentzip, zstd).
- [ ] posix: open_basedir checks (access/mkfifo/mknod).
- [ ] streams/http: `ftp://` proxying.
- [ ] finfo: libmagic 5.48 bundled descriptions (5 phpt wording diffs).
- [ ] zend_test: 3 opcache-dependent tests (gh17899, observer_preload, optimizer_register_pass).
- [ ] ext/session upload progress / rfc1867 items shared with apache row.

## E. Housekeeping / process
- [ ] Refresh FEATURE_GAP.md scoreboard rows after JIT Step 1 commit (37/82 → 43/82;
      const/static-lookup row 3/12 → 9/12; property/const micro numbers).
- [ ] Commit cadence: checkpoint the moment lib tests are green; never push (user rule).
- [ ] JIT knobs stay `BUZZS_*`, never `PHPRUST_*`: `BUZZS_JIT=0` (off), `BUZZS_JIT_DEBUG`
      (dump), `BUZZS_JIT_HOT=<n>` (compile after n back edges instead of 20,000; for test runs).
- [ ] PHPT runs exercise the JIT by default now; after every JIT step also run the set with
      `--env BUZZS_JIT_HOT=100` (every loop compiled) and with `--env BUZZS_JIT=0`.

## Done recently (do not redo)
- [x] Trace JIT base → call inlining → property read/write rows → three cached-property
      row classes (37/82) — chain 45bde918..ccef215d.
- [x] JIT Step 1 static props/class consts code + measurement (43/82, zero regressions).
