# TODO — phprust (entire project)

Companion to `FEATURE_GAP.md` (the authoritative gap table with %/pri per row;
keep it updated via `scripts/update_feature_gap.py`). This file is the
action-ordered work queue. Ordered by impact: biggest measured misses first,
then conformance closes, then in-scope unimplemented features.

Status: [ ] open · [~] in progress · [x] done · [-] blocked/awaiting

## A. Performance vs C (goal: all 82 goal.tsv workloads ≤ 1.10×)
Score: 75/82 meet (measured 2026-11-21 with `scripts/acceptance.sh`, JIT on by default; 21/82 with `BUZZS_JIT=0`).

- [x] JIT Step 1 — inline static-prop read/write/isset + class-const folds (`Ext` tape rows).
      Committed 2026-11-20 with ten row fixes; the wrong `empty()` answer was a `Bool`
      written at the `Int` payload offset (a `Bool`'s value is the byte after the tag).
      Sub-list: perf/TODO-jit-step1.md
- [~] JIT Step 2 — array/string dim rows. Done 2026-11-20 (`24eca59c`): int-offset array
      rows for int elements (`$t = $a[$i]`, the folded `$v = $a[$i]`, `$a[$i] = v`, `$a[] = v`,
      `$a[$i] op= v`) through helpers the trace calls, native `& | ^ << >>` and `NumBinC`.
      String-key reads done too (hash_read and x_eq_hash_v 1.45 -> 0.84, with the keys of
      compile-time arrays interned). Open: string keys built at run time (hash1/hash2),
      string-key writes, string offsets (x_eq_str_0),
      nested dims (matrix), float elements (heapsort), `isset`/`++` on an element, and an
      inline packed read in place of the helper call. An int-key hash insert with one
      probe (`HashTable::set_int_plain`) took ary 1.59 -> 1.10 (meets) and ary2 1.93 -> 1.48;
      `sort()`/`rsort()` of int lists and native int callbacks took suite:arrays 1.77 ->
      0.46. `$a[$i][$j]` read into a temporary runs through the tape's handler inside a
      trace (matrix 1.79, unchanged: its inner loop is 625 instructions against C's 352).
      Done 2026-11-20/21 (interpreted paths; a trace calls the same handlers): a lean
      `$a[$i][$k]` through two lists (matrix 1.79 -> 0.97), a read into a temporary, and
      behind the lean paths, each a small function of its own, arrays held by reference
      (heapsort 2.95 -> 2.21), hashes with int keys (ary2 1.48 -> 1.06 with the hot count
      by loop length) and string keys in variables and temporaries (hash1 1.97 -> 1.52,
      hash2 2.54 -> 1.66 with `foreach` on the tape, array_rw 1.81 -> 1.35).
      Open: heapsort needs floats in traces and the array rows of a trace to take a
      reference and float elements; hash1, hash2 and array_rw are interpreted loops whose
      string-key probe (`find_zstr`, ~80 instructions) and handler frames still cost
      about twice C's.
- [~] JIT Step 3 — call-heavy tail. Done 2026-11-20 (`f457925c`): pure int functions
      compiled whole with native recursion (ackermann 10.58 -> 0.18, fib 6.31 -> 0.19,
      fibo 6.27 -> 0.19). Done 2026-11-20: `self::`/`parent::`/`static::` calls prepared on
      the tape and inlined in a compiled loop (self_cc_f 4.64 -> 0.09); frameless builtins
      kept on the tape (`sqrt($x)` & co. as one instruction, the others called in the tape
      loop: float_math 1.95 -> 0.82); methods whose body reads or writes int properties of
      `$this` inlined (suite:calls 3.95 -> 0.11, suite:props 2.40 -> 0.82 with `self::$s++`
      compiled too); builtin calls prepared at a depth without a pooled frame; one-argument
      builtins with a computed argument (`chr(65 + $i % 26)`) and `dechex()` without a
      frame or handler (strings 2.16 -> 1.64, hash1 3.37 -> 1.95). Done 2026-11-20: a
      builtin call with computed arguments is one frameless op at its `DoCall`
      (`specialise::spread_frameless_call_at`), and `strpos`/`substr`/`max`/`min`/strict
      `in_array`/`str_contains` & co. are answered without their handler
      (`special::quick_eval_n`, tape row `FramelessQ`): suite:builtins 4.52 -> 1.55,
      suite:strings 1.64 -> 1.34. Done 2026-11-20: those loops compile (a trace calls the
      tape's own handler for a row it has no code for: `jit::plan_row`), and calls of two
      or three arguments are straight-line code (`special::quick_eval_2`/`quick_eval_3`):
      suite:builtins 1.55 -> 1.16, suite:strings 1.34 -> 1.18. Open: a handler call from a
      trace costs ~22 instructions around the handler (callee-saved registers for the
      trace's bases would make it ~8; `ops:strlen` is 1.08 compiled against 1.01
      interpreted), and the interpreter's user-call protocol (memory note
      `call-redesign-plan`).
- [~] JIT Step 4 — `New` rows (new_obj 3.27, micro:new_Foo 2.02, objects 4.64),
      elvis/ternary, `$this::CONST`, `$GLOBALS['v']`, `$_GET`. Done 2026-11-20: tape
      instructions for all of them (a ternary's arms, `?:`, `??`, non-scalar assignments,
      `$this::NAME` as one row, global reads run in the tape loop), which is not enough
      to flip them: x_eq_f_q_f_a 2.43, x_eq_a_elvis_null 1.86, x_eq_f_q_f_tmp 1.48,
      x_eq_f_elvis_tmp 1.40, x_eq_GLOBALS_v 2.02, x_eq__GET 2.60. `$this::CONST` is folded
      in a trace now (x_eq_Foo_cc_TEST 1.34 -> 0.26).
      Done 2026-11-20: temporaries of int/bool/null/any-scalar kind with joins in a trace,
      so the scalar forms compile (x_eq_f_elvis_tmp 1.40 -> 0.33, x_eq_f_q_f_tmp 1.48 ->
      0.38); a hand-written cached global read (x_eq__GET 2.60 -> 1.96, x_eq_GLOBALS_v
      2.02 -> 1.89).
      Done 2026-11-20: a trace copies and releases arrays (x_eq_f_q_f_a 2.43 -> 0.44,
      x_eq_a_elvis_null 1.86 -> 0.40) and calls the cached global read (x_eq__GET 1.96 ->
      1.79, x_eq_GLOBALS_v 1.89 -> 1.69).
      Done 2026-11-20: a trace reads a global slot itself (`C_GREAD`: the slots' addresses
      go into a table when the trace is entered): x_eq__GET 1.79 -> 0.27, x_eq_GLOBALS_v
      1.69 -> 0.25. `new` of a class without a constructor is a tape instruction
      (new_Foo 2.05 -> 1.71).
      Open: object allocation and release themselves (objects 4.04, new_obj 3.29, new_Foo
      1.71); strings and objects moved by a trace.
      Done earlier: `null`/bool constant folds (x_eq_TEST 2.37 -> 0.29).
- [x] JIT is the default (owner, 2026-11-20; `57aa54cc`): on for x86-64 Linux, `BUZZS_JIT=0`
      turns it off. `scripts/acceptance.sh` now measures it without a wrapper binary.
- [ ] JIT follow-ons from NOTE-jit-2026-11-16: guard-reload/loop-note machinery;
      floats in traces (the float loops meet the goal interpreted since `&&`/`||`
      conditions jump straight to their outcome: mandel 1.57 -> 0.78, mandel2 1.00,
      float_math 0.85; a trace would still be several times faster, and heapsort and
      suite:objects need it); property access inside inlined callee bodies;
      `self::`/`static::` scope guards for call inlining.
- [ ] Objects (objects 3.99, new_obj 3.30, new_Foo 1.72): `new C(...)` with a
      constructor that only assigns its parameters to properties as part of the `new`
      row (the constructor call is ~1,200 of new_obj's 2,090 instructions); allocation
      (`object::new_block` 172) and release (~290 across six functions) against C's
      ~135 and ~130; the interpreter's user-call protocol for the method call.
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
- [ ] tests/basic/oom_rfc1867_files_registration_full_path: needs Zend MM's placement of
      large blocks in 2 MiB chunks (the third 1.2 MiB copy of the file name needs a fifth
      chunk there); php-memlimit counts those blocks by size, so 8M is not reached.
- [x] ext/session rfc1867_incomplete_class_reserved_props (2026-11-20): a member with no
      declared property lost its name mangling in `Vm::write_mangled_prop`.
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
