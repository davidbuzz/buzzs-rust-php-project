# TODO — phprust (entire project)

Companion to `FEATURE_GAP.md` (the authoritative gap table with %/pri per row;
keep it updated via `scripts/update_feature_gap.py`). This file is the
action-ordered work queue. Ordered by impact: biggest measured misses first,
then conformance closes, then in-scope unimplemented features.

Status: [ ] open · [~] in progress · [x] done · [-] blocked/awaiting

## A. Performance vs C (goal: all 82 goal.tsv workloads ≤ 1.10×)
Score: 82/82 meet (measured 2026-11-21 with `scripts/acceptance.sh`, JIT on by default; 25/82 with `BUZZS_JIT=0`).

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
      Done 2026-11-21: the string-key steps make the probe themselves
      (`HashTable::sym_zstr_probe`, no `memcmp` call for a long key) and a string key goes
      straight to them after the lean int path missed (`dims::fetch_miss`,
      `assign_miss`): hash1 1.52 -> 1.44, hash2 1.66 -> 1.53, array_rw 1.35 -> 1.22. Then
      the lean handlers decode the offset once themselves and those two functions are
      gone (the second look cost an int key that misses the lean path about 10
      instructions): hash1 1.41, array_rw 1.19, heapsort without the JIT 2.34 -> 2.19.
      Done 2026-11-21: a trace's element rows take a variable that holds a reference
      or an array of floats (`jit_dim_get_wide`/`jit_dim_set_wide`), its comparisons
      floats (`J_CMPS`), and `$i = $j` / `--$l` with a used result are rows of its own:
      heapsort's inner loop compiles (2.35 -> 1.59).
      Done 2026-11-21 (later): a trace runs `a . b` into a temporary, `??` of a
      temporary and the element rows whose key is no int (a string in a variable or a
      temporary) through the tape's own handlers (`C_DIMKEY`, `jit_row_dim_keyed`), a
      read that raises nothing for a missing key gives null inside the trace
      (`dims::fetch_key_missing`), and a jump on a temporary takes an int as well as a
      bool: array_rw 1.19 -> 1.07 (meets the goal), and both loops of hash1 compile
      (1.41 -> 1.40: what is left is the handlers' own cost).
      Then `jit_dim_get_wide` (a third of heapsort: 50 calls an outer iteration at 98
      instructions) reads the element where it lies: heapsort 1.59 -> 1.48; and the
      trace reads a list's scalar element itself, without the call
      (`Emitter::emit_list_read`, the layout pinned by `list_layout` and
      `reference_layout`): heapsort 1.08 (meets the goal), dim_read 0.78 -> 0.32,
      ary3 0.93 -> 0.61, ary2 0.90, ary 0.83, sieve 0.82.
      Then the write in place (`Emitter::emit_list_write`: a scalar over a list's
      scalar element, or as the list's next element with room in the allocation; the
      layout pinned by `unshared_table_counts` and `list_append_layout`): heapsort
      0.97, sieve 0.82 -> 0.47, dim_write 0.82 -> 0.31, ary 0.83 -> 0.72, ary2
      0.90 -> 0.78 (with the overwrite alone the last two lost 9%: every write of theirs
      adds an element).
      Done 2026-11-21 (the tape's own handlers, lean paths that make no call, each
      in front of the handler it was): the step of a `foreach` over plain values
      (`dims::fe_step_plain`: 158 -> about 90 instructions a step), `$a[$key] += $int`
      with a string key (`assign_op_str`, `HashTable::lean_zstr_probe_mut`: 195 ->
      about 120), the read and the write of an element under a string key that was
      just built (`fetch_str_lean`, `assign_str_lean`: the key hashed in line, the
      temporary's string moved into a new bucket), and `dechex()` writing its digits
      into the string it returns (`ZStr::from_fn`): hash2 1.53 -> 1.19, hash1
      1.40 -> 1.22. Both still miss.
      Then (2026-11-21) the lean paths without stack frames: the byte comparison of
      two keys without the iterator calls of its long-key branch (they made every
      probe inlining it save six registers) and short keys first, `$a[$k] op= v`
      behind a frameless dispatcher (`assign_op_dim` -> `assign_op_dim_str` or the old
      handler), `dechex()` filling its digits from the end: hash2 1.19 -> 1.10
      (meets), hash1 1.22 -> 1.16, and the JIT-off ary3 back at 1.36.
      Then string-key rows for keys made as strings (`dims::made_str`), lean paths
      for keys that are integers in digits (`ArrayKey::small_int`), and keyed trace
      rows calling a helper made for their op: hash1 1.16 -> 1.08 (meets). Open:
      heapsort's outer loop (a `return` inside it) and `gen_random`
      (constants defined at run time, a `global`) stay interpreted; a `foreach` step
      is no trace row yet.
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
      suite:builtins 1.55 -> 1.16, suite:strings 1.34 -> 1.18. Done 2026-11-21: a trace
      calls the handlers themselves (they are `extern "C"`; the shims between are gone):
      ops:strlen 1.08 -> 0.97, ops:count 0.97 -> 0.88, suite:builtins 1.04. Open: a
      handler call from a trace still saves and restores five registers around the
      handler (callee-saved registers for the trace's bases would spare that), and the
      interpreter's user-call protocol (memory note `call-redesign-plan`).
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
      Open: object allocation and release themselves (see the Objects item below);
      strings and objects moved by a trace.
      Done earlier: `null`/bool constant folds (x_eq_TEST 2.37 -> 0.29).
- [x] JIT is the default (owner, 2026-11-20; `57aa54cc`): on for x86-64 Linux, `BUZZS_JIT=0`
      turns it off. `scripts/acceptance.sh` now measures it without a wrapper binary.
- [~] JIT follow-ons from NOTE-jit-2026-11-16: guard-reload/loop-note machinery;
      floats in traces. Done 2026-11-21: comparisons and `+=`/`-=`/`*=` of floats are
      compiled (by the operands' tags: `J_CMPS`, `J_ASSIGN_OPS`), and a math builtin may
      end an inlined method (`KQUICK`). Open: float arithmetic into a temporary or
      another variable (`$a * $b` of floats, a float literal operand) still leaves a
      loop to the interpreter (the float loops meet the goal interpreted since `&&`/`||`
      conditions jump straight to their outcome: mandel 0.78, mandel2 1.00, float_math
      0.84; a trace would still be several times faster); property access inside
      inlined callee bodies; `self::`/`static::` scope guards for call inlining.
- [x] Objects (all three meet the goal: new_obj 1.07, new_Foo 0.97, objects 0.78). Done
      2026-11-21: `new C(...)` with a constructor that only assigns its parameters to
      properties (a
      promoted constructor is one) is one tape instruction without the call
      (`calls::new_stored`, from `UserFunction::ctor_fold`), which a trace calls: new_obj
      3.30 -> 1.97, objects 3.99 -> 2.64. Then: an object of its class's defaults made by
      straight-line code (`object::new_defaults`), a temporary's value stored over an
      object and the object released directly, the free of a plain object in one
      function (new_Foo 1.73 -> 1.39, new_obj 1.76), and `$p->len()` inlined in the
      trace with its `sqrt` and the float `+=` (objects 2.64 -> 1.21).
      Then each `new` instruction keeps what the full checks found out at its place
      (`calls::NewSite`: the class, where each argument comes from, the slot and the
      type bits of each store) and a later `new` there reads the arguments where they
      are and stores them: `new_stored` 364 -> 254 instructions for `new R($i)`,
      objects 1.21 -> 1.02, new_obj 1.76 -> 1.55; with the stores writing the slots
      directly (`calls::store_arg`) objects 0.99, new_obj 1.52.
      What was left, by the profile of new_obj (964 instructions an iteration against
      C's 635): the possible-root bookkeeping of the new object (152), allocation (113
      in `object::new_defaults`) and release (~270 across seven functions) against C's
      ~135 and ~130. Then `$v = new C(...)` over the last handle of a plain object of
      `C` makes the new object in the old one's block (`object::reuse_for_new`: the
      handle a new object would be given, the old one back on the free list after it,
      the root entry moved to the slot the next root takes, memory use and its peak
      as an allocation and a free leave them; `calls::new_over`, `site_over`,
      `NEW_OVER`, `Emitter::emit_new_row`): new_Foo 1.39 -> 0.97, new_obj
      1.52 -> 1.07, objects 0.99 -> 0.78, with and without the JIT (it is the tape's
      own `new` instruction).
      Open: an old object whose slots hold strings, arrays or objects is not made
      over yet (its values would be released after the new object stands).
- [ ] Memory allocator: profile per-allocation cost claim (~81→~30 ins) on release build;
      update FEATURE_GAP row % from estimate to measured.
- [ ] Object creation/layout (0% row): New-row JIT + constructor fast paths; zval-layout
      items (one-alloc objects, reference size, 8B hash index) — design pass.
- [ ] Interpreter-level spec gaps: dim_read/dim_write/abs still over goal JIT-off.
- [ ] Shapes the 82 workloads do not cover, measured 2026-11-21 (instructions a loop
      iteration, ours against the C implementation's; candidates for `perf/bench`):
      a generator's `send()` 4,499 / 510 and a `foreach` over a generator 2,556 / 450
      (its frame is now swapped onto the stack and back; the rest is the builtin
      method call machinery around `send()` and the nested `execute()`);
      a method whose body writes `$this->items[$k]` and returns `$this` 2,498 / 479;
      `new B(...)` with a constructor body 3,129 / 1,235; `['id' => $i, 'name' => 'x',
      'n' => $i]` 1,858 / 559 and `[$i, $i]` 1,029 / 404 (were 3,039 and 1,585: array
      literals were `Vm::run` ops, now `InitArrayT`/`ArrayAddT` rows); a `foreach` over
      an array of arrays 370 / 158; `new S("abc", $i)` with a constructor of plain
      stores 1,450 / 742 (the old object holds a string, so its block is not made
      over); `strtoupper()`/`strpos()`/`str_contains()` 3,251 / 1,717; a `foreach`
      over a list of strings 181 / 140 and with keys over a hash 174 / 141 (were 241
      and 217). What is known of the causes: every return that is not a scalar out of
      a variable or temporary of an untyped function is `call.rs::return_generic`
      (`return;`, a constructor's end, `return $this`, strings, arrays, typed
      returns); `calls::new_over` and the site check fail on every `new` of a class
      whose constructor has a body; `strs::frameless_quick_n` costs about 165 around
      a two- or three-argument builtin, and `strtoupper()` & co. are handler calls;
      the general steps of `fe_fetch_into` for an array or object value.

## B. Conformance close-out to 100% in-scope phpt (currently 99.8%, 38 failures)
- [ ] Optimizer/opcache dump failures (30 of 38; 19 left in the targeted set of 13,935
      tests on 2026-11-21): dce_005, sccp_010/011/012/026,
      nullsafe_001/002, assign_dim_op_result_type, gh18107_1/2, preload_optimizer,
      array_map_foreach_optimization_007/008/013/014, named_parameter_new,
      gh9164, jit/ignored_opcodes. Done 2026-11-21: gh18050 (`in_array()` of a
      variable holding a known array is `ZEND_IN_ARRAY`: a variable sent by value to
      a function known at compile time no longer switches the propagation of
      variables off, `OptContext::known`, and a known variable stays known across a
      call that cannot reach it by name), opt/coalesce (`??`/`??=` of a local
      nothing has assigned goes on to the next op, and the first store of a literal
      goes when no later op names the variable), opt/dce_001 (a concatenation of
      scalar-typed parameters that nothing reads is dead, the concatenations it read
      with it) and opt/dce_007 (an array literal that nothing reads is dead when no
      element can raise anything), opt/dce_008 (a store of a locally built array of
      scalars into an array nothing reads: `compact::scalar_array_vars`) and
      opt/dce_009 (the loop of a `foreach` over an empty class constant, removed where
      the constant is put in place for the dump), opt/sccp_017 and opt/sccp_019 (the
      dead copy `$b = $a` of a locally built array of scalars and both arrays' element
      stores), opt/sccp_002 and opt/sccp_005 (an array literal with an element that
      is no constant is a partly known array: `sccp::partial_array_builds`) and
      opt/dce_004 (an array replaced before anything reads it goes with the stores
      into it: `dce::remove_overwritten_arrays`). What the open ones need: objects
      that do not escape (sccp_010/011/012, dce_005), a type narrowed by a type check
      along a branch (sccp_026), `JMP_NULL` on the variable itself with constants
      kept across calls (nullsafe), the SSA dump's types of `ASSIGN_DIM_OP`
      (assign_dim_op_result_type), and named arguments of known callees as positional
      sends (named_parameter_new).
- [x] Optimizer (2026-11-21): two wrong results with the optimizer on. An unread
      `$a[] = v` was dropped although it throws after the largest int key
      (`$a = [PHP_INT_MAX => 5]; $a[] = 1;` lost its Error): it goes only with a
      constant value and when no array of the variable can hold that key, as php-src
      keeps `ASSIGN_DIM ... NEXT` of a value it does not know. And array constants
      were compared with `===`, so `0.0` and `-0.0` in an element became one zero on
      both paths (php-src GH-24063): `sccp::same_constant` compares floats by their
      bits. Oracle scripts `extra_partarr.php` and `extra_negzero.php` cover both.
- [x] Optimizer (2026-11-21): an array literal that may be the last holder of an object
      keeps its store. Pass 13 dropped `$a = [new D];` of an unread `$a` (and
      `$b = [$o]; $o = null;`), the temporary array was freed at once and D's
      destructor ran before the rest of the function; PHP keeps it to the end, with
      and without its optimizer. `compact::build_is_inert` looks at what the array is
      built from. No PHPT covers it.
- [x] Optimizer (2026-11-21): the block pass folded `!` into a jump that keeps its
      result (`T = BOOL_NOT X; D = JMPZ_EX T` became `D = JMPNZ_EX X`), so with
      `opcache.enable_cli=1` the code after `!$a && !$b` and `!$a || !$c` saw the
      opposite result. php-src folds `BOOL_NOT` only into `JMPZ`/`JMPNZ`. No PHPT
      covers it; found by running scripts with known output through the optimizer.
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
