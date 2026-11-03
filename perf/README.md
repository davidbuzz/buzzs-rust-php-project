# Interpreter and startup performance of the debug build

This is a performance analysis of `target/debug/php`, the binary the phpt
suite runs, plus proposed patches. **Nothing outside `perf/` is changed.**
The `.diff` files are proposals for the owners of php-vm, php-cli and the
workspace `Cargo.toml`.

* `perf/0*.diff`, `perf/10-*.diff`: against the **current shared working
  tree** (HEAD `49cf921` plus the uncommitted php-vm work-in-progress as of
  2026-11-03 22:30). Apply with `git apply perf/01-*.diff … perf/10-*.diff`
  in order.
* `perf/728fb5f/0*.diff`: the same call-path patches against commit
  `728fb5f`. This is the version that went through the regression run below.
* `perf/20-dev-profile-opt-levels.diff`: the proposed `Cargo.toml` profile
  change (against HEAD).
* `perf/bench/`: the benchmarks and the measuring scripts.

**Method.** The box ran at load average 20–35 on 16 cores the whole time, so
wall-clock numbers swing by ±50 %. Every A/B number below is either
**user-space instructions** (`perf stat -e instructions:u`, deterministic to
about 0.1 %) or **CPU time** (task-clock of the process and its children).
Per-iteration instruction counts cancel the startup cost by running each
benchmark at N and N/10 iterations (`bench/irun.sh`). They agree with
callgrind's `Ir`.

## TL;DR

**Startup** (`php -r 'echo 1;'`, the floor of every phpt test):

| build | instructions | CPU |
|---|---:|---:|
| shared debug binary today (WIP tree) | 1 927 M | 716 ms |
| + `10-startup-attribute-modules-at-insert.diff` | 569 M | 233 ms |
| + php-cli opt-level 2 / php-memlimit opt-level 3 | 479 M | 204 ms |
| + php-vm opt-level 1 | 357 M | 144 ms |
| + php-vm opt-level 2 | 172 M | 86 ms |
| `[profile.dev] opt-level = 1` for all crates (+ cli2) instead | 161 M | 76 ms |
| release build (728fb5f) | 61 M | 33 ms |
| `/usr/bin/php` | 40 M | 31 ms |

* **70 % of today's startup is `reflection::misc::record_module`**. It copies
  every function, class and constant name into three `HashSet<Vec<u8>>`
  before **and** after each of the ~40 extensions, then diffs the sets:
  O(extensions × symbols), about 1.3 G instructions. Patch 10 attributes
  symbols at insert time instead. Its output is identical: per-extension
  function, class and constant lists from `ReflectionExtension` and
  `get_defined_constants(true)` match byte for byte (`bench/attr.php`).
* The "~1 s per launch" seen by the runner is about 0.7 s of CPU plus queueing
  on the oversubscribed box. The `RLIMIT_STACK` re-exec in `php-cli` costs
  nothing measurable (identical instruction counts with `ulimit -s unlimited`).
* The < 50 ms target is not reachable with patches this size. After patch 10
  and the profile change, what is left is stub-signature parsing (21–34 %),
  `ClassBuilder::register` (30 %), dynamic-linker relocation of a 240 MB PIE
  with 139 k relative relocations and 51 shared libraries (13 %), and
  dropping the `Vm` at exit (7 %). See "Startup: what is left".

**Per-call overhead**, instructions per loop iteration on the WIP tree:

| | empty loop | `strlen($s)` | `g(1)` |
|---|---:|---:|---:|
| debug today | 4 420 | 29 717 | 22 113 |
| + patches 01–07 | 3 601 | 16 397 | 17 301 |
| + patches + php-vm opt-level 1 (+ php-cli 2) | 892 | 5 012 (**5.9×**) | 5 103 (**4.3×**) |
| + patches + php-vm opt-level 2 (+ php-cli 2) | 739 | 3 815 (**7.8×**) | 4 104 (**5.4×**) |

The tests that time out (CPU s): `inflate_add_basic` 158 → 26,
`iso8859_encodings` 133 → 27, `date_diff` 32 → 12 with patches + php-vm
opt-level 1. All are well under 60 s.

**Recommendation.**
1. Apply patch 10 (startup, 3.4×).
2. Apply `20-dev-profile-opt-levels.diff`: php-vm opt-level 1, php-cli 2,
   php-memlimit 3.
3. Apply 01–07 (call path).

Together: about 5× on startup and about 6× on builtin calls, at the price of
about 20 s more per php-vm edit-rebuild (17–24 s → 41 s). If the owner can
live with 61 s rebuilds, php-vm opt-level 2 gives another 1.3× on calls and
1.7× on startup. A workspace-wide `[profile.dev] opt-level = 1` is fastest
for the slow tests, but costs a 9-minute full rebuild and 55 s edit-rebuilds.

## 1. Benchmarks

`bench/*.php` hold micro-benchmarks. Each is
`for ($i = 0; $i < $n; $i++) { <body>; }` and prints `hrtime` ns/op.

* `bench/irun.sh <php> <bench> <n>`: instructions per iteration
  (deterministic).
* `bench/table.sh <scale> <php>...`: ns/op table, best of 3.
* `bench/startup.sh <php>...`: startup instructions, CPU and wall time.
* `bench/slowtests.sh <runner> <php>...`: CPU seconds of the four tests that
  time out.
* `bench/phpt/`: 21 trivial tests for the runner-overhead measurement.
* `bench/attr.php`: checks the extension attribution that patch 10 touches.

### 1a. Instructions per iteration, commit 728fb5f (lower is better)

The `empty_loop` column is the loop's own cost and is included in every other
column. "+P" means patches 01–07. The vmO2+P and allO1+P columns were
measured with 01–06 only (before patch 07, which only affects
`static_method`).

| bench | O0 | O0+P | vmO1 | vmO1+P | vmO2 | vmO2+P | allO1+P | release | release+P |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| empty_loop | 3 923 | 3 095 | 1 148 | 838 | 946 | 712 | 747 | 786 | 595 |
| strlen | 20 606 | 11 374 | 10 150 | 4 488 | 5 640 | 3 316 | 3 691 | 4 484 | 2 767 |
| abs | 20 236 | 11 329 | 9 865 | 4 508 | 5 770 | 3 494 | 3 704 | 4 468 | 2 785 |
| count | 22 937 | 13 537 | 11 906 | 6 160 | 7 392 | 4 881 | 5 118 | 5 784 | 3 915 |
| userfunc | 16 956 | 12 659 | 7 289 | 4 666 | 4 579 | 3 726 | 4 091 | 3 736 | 3 108 |
| userfunc_typed | 23 366 | 13 220 | 11 053 | 4 801 | 6 589 | 3 847 | 4 203 | 5 331 | 3 208 |
| method | 19 478 | 16 989 | 8 283 | 7 223 | 5 149 | 4 637 | 5 064 | 4 233 | 3 875 |
| static_method | 22 695 | 16 881 | 10 577 | 6 717 | 5 794 | (5 279) | (5 794) | 4 760 | 3 712 |
| closure | 18 611 | 16 303 | 7 123 | 6 081 | 5 020 | 4 520 | 5 036 | 4 074 | 3 732 |

Builtin-call overhead (`strlen` − `empty_loop`): **16 683** at O0, 8 279
with the patches (2.0×), 8 997 at vmO1 (1.9×), **3 650** at vmO1+P (4.6×),
2 604 at vmO2+P (6.4×), 2 944 at allO1+P (5.7×). Release goes from 3 698 to
2 172 (1.7×).

**Even the release build does about 600–800 instructions per empty loop
iteration** (real PHP: about 15). Its profile is
`run` 49 %, `read_cv` 13 %, `resolve_place` 13 %, and `Value`/`RBase`/`Vec<RSeg>`
drops 11 %. Per opcode, the loop pays `self.frames.last().expect()`, a
`RefCell` borrow of the `VarStore`, a `Value` clone with refcount traffic and
a thread-local read in `has_pending_destructors`, and the `run` frame is over
1 KB of stack. That is a design-level cost; see "Larger follow-ups".

### 1b. Wall time, ns/op (best of 3, `bench/table.sh 0.05`; loaded box, ±50 %)

All 728fb5f builds. Treat these as indicative only, and use the instruction tables for A/B comparisons. Some O0+P cells are slower than O0 purely from load (`method`, `preg_match`); their instruction counts are lower.

| bench | debug O0 | O0 + P | vmO1 + P | allO1 + P(01–06) | release | release + P | /usr/bin/php |
|---|---:|---:|---:|---:|---:|---:|---:|
| abs | 13331.9 | 6095.6 | 2096.0 | 1008.4 | 1132.1 | 807.0 | 22.2 |
| array_set | 5646.5 | 5104.9 | 1381.4 | 1337.6 | 1739.2 | 1270.8 | 31.2 |
| closure | 26591.7 | 20267.1 | 3915.9 | 2366.7 | 2681.2 | 1772.0 | 56.3 |
| concat | 7100.6 | 3324.2 | 1363.5 | 339.6 | 643.9 | 635.5 | 38.3 |
| count | 20483.2 | 9575.2 | 2353.3 | 3193.1 | 2374.3 | 2757.2 | 12.7 |
| date | 58309.1 | 20423.4 | 26770.2 | 5526.7 | 6592.7 | 3014.6 | 1092.6 |
| empty_loop | 4097.1 | 1888.2 | 265.7 | 334.8 | 334.7 | 166.9 | 4.6 |
| int_add | 4208.4 | 2421.2 | 818.2 | 317.2 | 517.3 | 342.4 | 11.0 |
| method | 11739.1 | 22960.0 | 4170.1 | 2726.8 | 1485.6 | 1815.3 | 29.9 |
| preg_match | 47343.1 | 73393.0 | 24221.1 | 4192.0 | 7590.7 | 4820.1 | 415.7 |
| sprintf | 47741.5 | 20251.0 | 11823.7 | 4009.5 | 4464.9 | 2690.3 | 183.1 |
| static_method | 44609.4 | 8765.2 | 3032.6 | 2088.7 | 2886.6 | 1629.7 | 26.2 |
| strlen | 17098.5 | 11829.0 | 2383.2 | 1971.9 | 1294.8 | 731.3 | 13.0 |
| str_repeat | 46977.0 | 31588.4 | 7047.5 | 3679.3 | 1747.8 | 2628.8 | 202.1 |
| userfunc | 15766.5 | 10115.6 | 2080.5 | 1934.5 | 1844.3 | 2178.2 | 21.8 |
| userfunc_typed | 13434.5 | 12826.1 | 2335.8 | 1504.0 | 2500.1 | 1160.3 | 27.9 |

### 1c. The tests that time out (CPU s, runner + php, 600 s timeout)

| build | inflate_add_basic | date_diff | hash_pbkdf2_large_length | iso8859_encodings |
|---|---:|---:|---:|---:|
| debug O0, 728fb5f | 158.0 | 32.4 | 1.8 | 133.0 |
| O0 + patches 01–06 | 106.7 | 26.9 | 2.4 | 83.8 |
| php-vm opt-level 1 | 37.0 | 14.1 | 2.3 | 41.2 |
| php-vm opt-level 1 + patches 01–07 | **25.8** | **11.5** | 2.2 | **26.7** |
| php-vm opt-level 2 + patches 01–06 | 17.1 | 8.7 | 2.2 | 20.3 |
| all crates opt-level 1 + patches 01–06 | 15.2 | 4.6 | 2.1 | 15.8 |
| release | 13.7 | 3.6 | 2.2 | 14.4 |
| release + patches 01–07 | 10.5 | 3.2 | 2.1 | 11.2 |
| `/usr/bin/php` | 1.0 | 0.6 | 0.8 | 1.6 |

`hash_pbkdf2_large_length` is already fast in this tree, because php-hash is
built at opt-level 3. Its timeout must come from load, or from a different
build. Profiles of `inflate_add_basic` and `date_diff` (`perf record`, O0)
are **interpreter-bound**:

| test | std generics instantiated at O0 | php_vm | php_runtime | the extension's own code |
|---|---:|---:|---:|---:|
| inflate_add_basic | 53 % | 29 % | 8 % | zlib: 1 % |
| date_diff | 52 % | 21 % | 9 % | date: 7 % |

### 1d. phpt-runner overhead

21 trivial tests (`bench/phpt`) with `-j 1` spawn 25 `php` processes: one
per test, plus one SKIPIF, one CLEAN and two probes. The runner's total CPU
is 2.80 s with the 728fb5f debug binary against 0.79 s with `/usr/bin/php`,
about 112 ms per process. That is exactly the php startup cost; the runner's
own work (diffing, INI argument building) is in the noise. **Per-test
overhead is php startup.** At today's 716 ms × ~21 k tests, that is about
4 CPU-hours per full suite run, and after patch 10 plus the profile change,
about 50 CPU-minutes.

## 2. Profile summary

### 2a. Builtin call: `strlen($s)` at O0, 728fb5f (callgrind; 20 600 instructions per iteration)

| instructions per call | where | what |
|---:|---|---|
| 10 235 | `call_builtin` | total, of which: |
| 2 076 | ↳ `Iterator::collect` ×2 | `f.args = values.iter().take(n).cloned().map(Some).collect()`, done twice (three times in the WIP) |
| 1 121 | ↳ `Frame::with_vars` | a full `Frame` for the builtin. `vec![Null; 0]`, `(0..0).map().collect()` and `vec![FinState::None; 0]` still run the generic code |
| 943 | ↳ `Vec::truncate` | dropping that `Frame` |
| 668 | ↳ `TypeDecl::clone` | `let ty = ty.clone()` per typed parameter. Unneeded: `b` is not borrowed from `self` |
| ~750 | ↳ `func_display` + `to_string_lossy` + `String` drops | **function and parameter names formatted on every call**, used only in error messages |
| 500 | ↳ `drop(Args)` | |
| 3 073 | `op_init_fcall` | `self.functions.get(&lc)`: hashbrown with SipHash, monomorphised at O0, **on every call** |
| 2 056 | `op_send` | by-ref checks, `calls.last()` ×3, `push_arg`; for `f($x)` also `place_fetch` → generic `resolve_place` (1 150) |
| ~600 | `has_pending_destructors` | thread-local `Cell` read after every opcode, 86 instructions at O0 (≈2 when optimised) |

`$i++` alone costs 1 244: `place_incdec` resolves the place generically and
clones the `RBase` before it reaches its own integer fast path.

* **User calls** (`g(1)`, 13 000 per call): `push_user_frame` (`Frame::new`
  builds a `VarStore` that clones every CV name with `names.to_vec()`),
  `op_return_raw` (frame drop), the `op_init_fcall` hash lookup and `RECV`.
* **Typed user calls** pay another 6 400. `recv_check` clones the `TypeDecl`,
  and `verify_return` builds `func_display` and **`TypeDecl::display` on every
  return**, even when the value passes.
* **Static calls** (`D::m()`) pay 3 400 in `lookup_class`, which lowercases
  the name into a new allocation and SipHashes it on every call.
* Even at opt-level 2, about 7 malloc/free pairs per builtin call remain
  before the patches (4 after).

### 2b. Startup, WIP tree (`perf record --call-graph=lbr`)

The shared debug binary is 240 MB, too large for valgrind ("mmap failed in
UME"), and DWARF unwinding failed on it, so the profile uses LBR call
stacks. Inclusive share of user cycles:

| | today | after patch 10 | after patch 10 + vmO1 + cli2 |
|---|---:|---:|---:|
| `Vm::with_extensions` | 90 % | 54 % | 72 % |
| ↳ `record_module` / `snapshot` | **71 % / 48 %** | — | — |
| ↳ `ClassBuilder::register` | 11 % | 33 % | 30 % |
| ↳ `sig::parse_sig` (functions and methods) | 10 % | 32 % | 21 % |
| ↳ `ModuleBuilder::constant` + `note_constant` | 3 % | 10 % | 13 % |
| ↳ `note_new` (patch 10's attribution) | — | 7 % | 8 % |
| counting allocator (`CountingAlloc::alloc`/`dealloc`, generic, instantiated in php-cli at O0) | 17 % | 19 % | 4 % |
| dynamic linker (`_dl_relocate_object`, `do_lookup_x`) | 2 % | 7 % | 13 % |
| `drop_in_place<Vm>` at exit | — | — | 7 % |

## 3. Proposed patches

Instruction counts per iteration are at O0 on 728fb5f, cumulative
(`bench/irun.sh`). All patches keep behaviour unchanged. Regression check
with the 728fb5f set (01–07):

* `Zend/tests` (5 725 tests): identical per-test results to the unpatched
  build.
* `ext/standard/tests/{array,general_functions,strings}` + `ext/spl/tests`
  (2 754 tests): identical, except `ext/spl/tests/bug71153.phpt`
  ("Performance Degradation in ArrayIterator"), which goes from FAIL (timeout)
  to PASS.

The WIP-tree set (`perf/0*.diff` plus `perf/10-*.diff`) was compared
against the unpatched WIP build on `Zend/tests`,
`ext/standard/tests/{array,general_functions}`, `ext/spl/tests` and
`ext/reflection/tests`: 8 295 tests, 6 687 vs 6 684 PASS. All 5 tests that
differed (`fibers/silence-operator-outside-fiber`, `type_casts/cast_to_int`,
`type_casts/cast_to_void`, `ReflectionClass_export_array_bug72222`,
`ReflectionClass_getExtensionName_basic`) **pass with both binaries when
re-run alone**. They were collisions from running two runners at once in the
shared php-src tree, which writes the generated `.php` next to each test.

### 01: `call_builtin`: lazy names, side-effect-free type fast path

* `fname` becomes a closure, evaluated only on the error paths. `pname`
  moves into the slow path. The `ty.clone()` goes.
* Before calling `verify_type`, `accepts_without_coercion(mask, v)` checks
  the type mask (`Int` → `LONG`, `Str` → `STRING`, `Array` →
  `ARRAY|ITERABLE`, `Object` → `OBJECT`, and so on). That is exactly the
  subset of `type_accepts` that returns `true` without callable or class
  checks, so `verify_type` would accept the value unchanged. References fall
  through to the slow path.
* The second "backtrace copy" of the arguments is only redone when a
  coercion actually happened.

strlen 20 606 → 16 990 (−3 616). Risk: low. The fast path is a strict subset
of what `verify_type` accepts as-is.

### 02: `InitFCall` call-site cache

`OpArray::fcall_cache: RefCell<Vec<Option<CachedFunc>>>`, indexed by the
name literal, works like php-src's runtime-cache slot.

* Only **primary-name** hits are cached. Literals are shared between call
  sites, and a later `ns\foo` declaration must still win at a site that has
  not run yet, so the global fallback of a namespaced call is not cached.
* User functions are held as `Weak`. A strong `Rc` would form a cycle for a
  recursive function, whose op array would cache itself.
* Functions are never removed from `vm.functions`.

strlen 16 990 → 14 509 (−2 481, the SipHash lookup).

### 03: place fast paths

* `place_fetch(Read)` of a plain CV calls `read_cv` directly. That covers
  `f($x)`, which sends through a place because the argument might be
  by-reference.
* `place_incdec` runs its int fast path before `resolve_place`.

empty_loop 3 923 → 3 095, strlen −1 431.

### 04: `RECV` and return-type fast paths

* `recv_check` returns early for an untyped parameter or an
  `accepts_without_coercion` value, without cloning the `TypeDecl`, scope or
  called class.
* `verify_return` returns early for no return type, or for an accepted
  value that is not void/never and not in a generator, without formatting
  `func_display`/`TypeDecl::display`.

userfunc_typed 19 873 → 14 224 (−28 %). Depends on 01, which defines
`accepts_without_coercion`.

### 05: cheap frame construction

* `Frame::with_vars` skips `vec![…; 0]`/`collect()` for empty
  `tmps`/`iters`/`fins`.
* `frame_args()` is a plain loop instead of
  `iter().take().cloned().map(Some).collect()`.
* In the WIP version, the post-defaults copy is only redone when
  `call.has_named`, because only named-argument gaps differ from the copy
  taken at frame push.

strlen −1 519, userfunc −884.

### 06: `SEND` positional by-value fast path

For a positional argument of a call that is not magic, has no named
arguments yet, and whose parameter is not by-ref, `op_send` reads the value
and pushes it in one step. That is exactly what the general path computes.
strlen −195.

### 07: `ClassSpec::Named` cache

`OpArray::class_cache: RefCell<Vec<Weak<Class>>>`, indexed by literal and
used by `resolve_class_spec`. Classes are never unregistered after startup
(the only `classes.remove` is in `fiber.rs` at registration time).
`static_method` 20 207 → 16 882 at O0, and 9 512 → 6 718 at php-vm
opt-level 1.

A tried-and-dropped variant skipped the lowercase allocation in
`method_call_on` when the name is already lowercase. It made no measurable
difference.

### 10: startup: attribute symbols to extensions at insert time

* `record_module(begin)` now just opens a window: `Modules::current = name`.
* `builtins::reg`/`alias`/`constant`, `ModuleBuilder::constant` and
  `constant_deprecated`, and `Vm::register_class` call
  `reflection::note_new(table, key)` when their `insert` returned `None` (a
  new key). That is exactly the "keys present after but not before" set the
  snapshot diff computed.

Startup 1 927 M → 569 M instructions (716 → 233 ms CPU). The output of
`bench/attr.php` (per-extension function/class/constant lists and
`get_defined_constants(true)` counts) is identical to the unpatched binary.
Risk: a future insert site that bypasses these helpers during
`Extension::register` would not be attributed. Today every insert during
registration goes through them; the remaining direct inserts in php-cli's
`sapi.rs` and in `include.rs` run outside the window.

### 20: dev-profile opt-levels (`Cargo.toml`)

```toml
[profile.dev.package.php-vm]
opt-level = 1
[profile.dev.package.php-cli]
opt-level = 2      # the counting allocator is monomorphised here
[profile.dev.package.php-memlimit]
opt-level = 3
```

`php-memlimit`'s `CountingAlloc<A>` is generic, so
`[profile.dev.package.php-memlimit]` alone does nothing (measured: 569 M →
569 M). Its code is compiled in php-cli, where `install_global_allocator!`
instantiates it, so php-cli's own opt-level matters (569 M → 479 M, and php-cli
relinks in about 20 s). A cleaner fix inside php-memlimit is to move the
`admit`/`add_live`/`release`/`bump` bodies into non-generic functions.

## 4. Profile settings: compile time against runtime

The edit-rebuild column changes one function body in `php-vm/src/call.rs` and
rebuilds `php-cli`. It is wall time on the loaded box with 3 jobs. The
full-rebuild column is the cost after switching the setting.

| setting | edit-rebuild | full rebuild | strlen (instructions/iteration, +P) | startup (WIP + patch 10 + cli2) | slow tests (+P, CPU s) |
|---|---:|---:|---:|---:|---|
| today (php-vm O0) | 17–24 s | — | 11 374 | 479 M / 204 ms | 107 / 27 / 84 |
| php-vm opt-level 1 | 41 s | 2–2.6 min | 4 488 | 357 M / 144 ms | 26 / 12 / 27 |
| php-vm opt-level 2 | 61 s | 2.3–2.9 min | 3 316 | 172 M / 86 ms | 17 / 9 / 20 |
| `[profile.dev] opt-level = 1` (all crates) | 55 s | **8.9 min** | 3 691 | 161 M / 76 ms | 15 / 5 / 16 |

`debug-assertions = false` for php-vm on top of opt-level 1 changed nothing
measurable (4 488 → 4 445), so keep it on.

## 5. Startup: what is left (for < 50 ms)

1. **Lazy builtin signatures.** `parse_sig` re-parses every function stub
   (about 1 700) and every method stub of about 320 classes on every start (21–34 % of startup). Store the `&'static str`
   in `Builtin` and parse into a `OnceCell` on first use, or generate the
   tables in a `build.rs`. This touches all 94 `.params` users, so it is a
   refactor for the php-vm owner, not a drop-in patch.
2. **`ClassBuilder::register`** (30 %) copies inherited consts, methods and
   props into fresh `HashMap`s per class, plus method `parse_sig`. Laziness
   would help here too, for example materialising an internal class on
   first lookup.
3. **Dynamic linking** (13 % at vmO1). The binary is PIE with 139 521
   `R_X86_64_RELATIVE` relocations, `BIND_NOW` and 51 shared libraries.
   `-Wl,-z,pack-relative-relocs` and lazy binding for dev builds are linker
   flags in `php-cli/build.rs` (`cargo:rustc-link-arg-bins=`) and relink only
   php-cli. **Tried:** `-z lazy` + `-z pack-relative-relocs` (RELR applied, BIND_NOW gone) gave identical instruction counts and CPU time (160.8 M / 78 ms), so the remaining loader cost is in the shared libraries (ICU, libxml2, OpenSSL …) and page faults. Not worth a patch.
4. **`drop_in_place<Vm>` at exit** (7 %). Freeing every class and function
   `Rc` one by one. php-src skips most of this with arena teardown. Rust
   cannot just `forget` the VM, because resources with buffered writers must
   still flush, but the function/class/constant tables could be leaked
   deliberately after shutdown has run.
5. The `RLIMIT_STACK` re-exec costs nothing measurable. The "1 s" wall seen
   by the runner is run-queue wait on the oversubscribed box.

## 6. Larger follow-ups (not patched)

* **Interpreter loop design** (affects release too, which is about 50× slower
  than php in loops). Cache the current frame's `code`/`tmps`/`vars` in
  locals of `run` instead of `self.frames.last().expect()` per access. Store
  CVs in the frame (no `Rc<RefCell<VarStore>>` borrow per read). Read operands
  by reference instead of cloning `Value`s. Check `has_pending_destructors`
  only after ops that can drop the last reference.
* **`VarStore` names.** Share `cv_names` as `Rc<[ZStr]>` and copy on the
  first dynamic variable. That saves one `Vec` plus N refcount bumps per user
  call.
* **Builtin frames.** Push a small call record instead of a full ~30-field
  `Frame`, and reuse the `values`/`passed` buffers.
* **Hashing.** Every by-name table uses SipHash. A faster hasher (FxHash/ahash,
  or reuse the PHP hash `ZStr` can compute) helps every remaining lookup,
  startup inserts included.
* **Method calls.** Add an inline cache keyed on (class pointer, name
  literal) in `op_init_method_call`.
