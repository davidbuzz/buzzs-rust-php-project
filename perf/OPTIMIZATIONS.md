# Optimizations log (Laravel-driven, PR #17)

What has been sped up, by how much, and what is still slow. This page is
for anyone picking up performance work: check here before starting, so
the same ground is not covered twice, and add a row when a change lands.

## How the numbers are taken

- **Unit:** `perf stat -e instructions:u` (user-space instructions; wall
  clock is unreliable on the shared box).
- **Builds:** release build (`cargo build --release -p php-cli`).
- **Box:** AArch64 (WSL2). The x86-64 JIT and its acceptance set
  (`perf/bench/acceptance.py`) are separate.
- **Micro-benchmarks:** a loop inside a function in `namespace App` (as
  Laravel code is), run at two iteration counts N and 11N. The figure is
  the difference divided by 10N, so startup cancels out.
- **Laravel:** a real app, measured with
  `artisan route:list` ("rl") and `vendor/bin/phpunit tests/Unit` ("ut",
  needs `-d memory_limit=1G`).
- **C reference:** PHP **8.5** (8.5.13), the official `php:8.5-cli`
  Docker image (AArch64), with opcache off (its default for the CLI).
  Up to 25 Nov 2026 the reference was PHP 8.4 (ddev image): C figures in
  the tables below are 8.4's unless marked "8.5". 8.5 is somewhat faster
  (route:list 1.62G vs 1.79G). Where 8.5 and master differ, master is
  followed (e.g. 8.6 deprecations).
- **Validation for every commit:**
  - PHPT on Zend/tests, tests, ext/opcache, ext/standard,
    ext/reflection, ext/spl, ext/pcre, ext/mbstring and ext/json
    (13,991 tests), compared per test with the previous build.
  - The crate tests.
  - The acceptance set.
  - A new test whose expected output was taken from C.

## Totals

| | start of PR #17 | now | C 8.5 | C 8.4 |
|---|---|---|---|---|
| route:list | 4.796G | 4.279G (−10.8%) | 1.621G (we are 2.64×) | 1.786G |
| unit tests | 208.45G | 179.45G (−13.9%) | 49.86G (we are 3.60×) | 51.75G |

Our front end (lex/parse/compile, ≈1.2G of route:list) is close to C's.
The gap is execution.

## Where the remaining gap is (measured 25 Nov 2026, against C 8.5)

The unit tests take 180G instructions here and 50G under C 8.5. By
component (`perf record` of the run, grouped by symbol):

| Share | Component |
|---|---|
| 25% | tape interpreter (`run_tape`, `run_call`, `step_ext`) |
| 13% | other VM work (property and place walks, classes, misc) |
| 9% | value drop and clone (reference counts) |
| 6% | call machinery |
| 8% | lexer, parser and compiler |
| 3% | each: GC, hash tables, frames |

Per operation, with the JIT off (instructions, ours / C 8.5):

| Operation | ours / C 8.5 |
|---|---|
| empty loop | 27 / 34 |
| `$x + $i` | 59 / 71 |
| `$y = $s` (string copy) | 104 / 62 |
| `$a['k']` | 169 / 123 |
| `$o->p` | 189 / 84 |
| `$o->arr['k']` | 475 / 174 |
| `$s . 'x'` | 455 / 238 |
| `if ($s === 'abc')` | 228 / 101 |
| `isset($a['k'])` | 231 / 118 |
| `$a['z'] ?? null` | 578 / 174 |
| `f()` | 747 / 169 |
| `$o->m()` | 1,127 / 204 |

Integer work is at C's level. Anything touching reference-counted values
(strings, arrays, objects) is 2–3× C, and every call costs about 690
instructions more than C's.

A census of the tape's exits from its core loop in one unit-test run
(169M) puts calls and returns first: `Return` 29.9M, `InitMethodCall`
25.3M, `DoCall` 10.7M, frameless calls 10.4M, static and function calls
14.4M. After them come ops the core loop does not run itself (class
constants 5.5M, `instanceof` 3.8M, `foreach` start and end 7M, `$this`
3.3M) and string comparisons on the slow step. The patch for that census
is `perf/census/op-census.diff`. Apply it with `git apply` and build
release. Run with `DBG_OPS=1` for the ops reaching `Vm::run`'s generic
dispatch, plus `DBG_TAPE=1` for the tape's exits; the counts print to
stderr at exit. Revert it with `git apply -R` of the same file.

So the gap is not one thing. The plan, in order of instructions at
stake:

1. **The call protocol** (~20G): preparing, readying the frame, sending,
   entering and returning, outside the core loop.
2. **Per-site caches** for method calls, class constants and static
   properties. These use a cache per name with checks, where php-src
   reads a run-time cache slot per opline.
3. **Reference-counted values in the core loop**: string comparison and
   assignment, property reads, the release of objects (the GC's
   possible-root check).
4. **Beyond that**, closing the gap to C needs architecture work, not
   more fast paths. One route is a baseline JIT for whole functions: the
   trace JIT already beats C on hot loops. Another is a leaner frame and
   dispatch, with guaranteed tail calls (nightly Rust's `become`). Both
   are decisions for the project's owner.

## Calls

The biggest lever, because Laravel is all namespaced code calling small
functions.

| Change | Commit | Before → after (instr/call) | C | Laravel |
|---|---|---|---|---|
| Namespaced calls prepared on the tape | 7a25bdc6 | `is_string()` 976 → 617; user fn 1,143 → 990 | 178 / ~150 | rl −1.3%, ut −1.3% |
| Variadic calls prepared | 7473b1f1 | 4,336 → 2,916 | | ut −0.7% |
| By-reference arguments of prepared builtins | 9989d124 | `preg_match($re,$s,$m)` 6,721 → 5,322; `end()` 2,941 → 1,760 | | rl −0.4% |
| Literal defaults bound with the frame | 70522a4a | `fd($i)` 1,230 → 1,091; `f(...$list)` 3,036 → 1,093 | | small |
| Pure builtin methods called without a frame (reflection getters) | 180f70c2, b15fccc9 | `getName()` 1,180 → 714 | 209 | rl −1.9%, ut −1.8% |
| `ArrayAccess` dims call `offsetSet`/`offsetGet` directly | 58c44d52 | `offsetSet` 5,249 → 3,250 | | ut −1.3% |
| User methods called from native code get their frame directly (`__get`, `ArrayAccess`, `Iterator`, `__toString`) | 82471e25 | `__get` 4,151 → 3,697; `$aa['k']=$v` 3,273 → 2,801 | 589 | ut −0.65% |
| Callbacks from builtins (`usort`, `array_map` …) get their frame directly | 16b41ddb | `usort` 300 ints 5.28M → 4.57M | 1.37M | small |
| `call_user_func()` direct frame | d01d03a2 | 4,659 → 4,467 | 961 | small |
| **Namespaced frameless calls (`ZEND_JMP_FRAMELESS`)** | a0d9be25 | `in_array` 1,431 → 782; `str_contains` 1,225 → 733 | 283 / 275 | **rl −1.3%, ut −1.6%** |
| Namespaced `is_*()`, `strlen()`, `count()` frameless | ae8734e2 | `is_string` 649 → 408; `count` 657 → 407 | 178 / 221 | rl −0.7%, ut −0.6% |
| `array_key_exists()`, `strtolower/upper()` frameless | 1d6dad71 | `array_key_exists` 1,743 → 663; `strtolower` 999 → 689 | 290 / 412 | rl −0.3%, ut −0.7% |
| 2-/3-argument frameless calls read their args in place | 082036fd | `substr` 1,198 → 836; `array_key_exists` 664 → 504 | 368 / 290 | −0.2% |
| `$this->name` arguments sent from the property cache (calls with them are prepared) | 23adafca | `ObjectArray::has()` 3,130 → 2,788 | | rl −0.35% |
| `RECV_VARIADIC` on the tape (untyped/`mixed`); value-less returns in `return_simple` | 98a9e086 | variadic call 1,328 → 1,017; Doctrine `triggerIfCalledFromOutside` 3,022 → 2,766 | | ut −0.4% |
| Static property elements (`isset(self::$a[$k])`, `static::$a[k]`) and `self/static/parent::class` without the generic ops | 4c61cbd9 | 2,035 → 1,393; `isset(static::$b[static::class])` 1,927 → 1,342 | 473 | ut −0.2% |
| `$a instanceof $b` of two variables without the generic op | eeb13391 | `instanceof $name` 749 → 447; `instanceof $obj` 617 → 308 | 8.5: 181 / 115 | ut −0.3%, rl −0.1% |
| `$this` as a call's argument goes to prepared calls | 6b2bdd15 | `method_exists($this, …)` 5,431 → 4,342; `get_class($this)` 3,513 → 2,429 | 8.5: 1,217 / 585 | ut −0.35% |
| `$cv += $array` merged in place (no symbol-table attach, no copy) | ba8c572b | 4,886 → 1,588 | 8.5: 581 | ut −0.8%, rl −0.3% |
| Returns of objects as `: static`, `self`, `parent` checked against the frame's classes (were the generic `Return`) | 8454da47 | 2,135 → 1,479 | 8.5: 285 | none measurable |

## Ops, compiler, tape

| Change | Commit | Effect |
|---|---|---|
| Magic constants (`__DIR__`) fold at compile time | a252e1cf | Composer classmap no longer runs ~18.5k `ArrayAdd`s; rl −1.9% |
| Array literal shapes worked out in one pass (was quadratic) | 0fc84026, 6b712559 | including `autoload_classmap.php`: 1.37G → 281M (C 288M) |
| Op arrays give back spare capacity | ca1732ee | boot memory 57.0 → 52.9 MB |
| `$cv[$k] ?? $d` miss stays on the tape | 6a088e27 | 1,470 → 525 |
| `$this->p[$k] ?? $d` miss stays fast | f15eae7e | 2,266 → 482 (C 232) |
| `instanceof` of a non-object without the class | def9bef7 | `Arr::get` 10,435 → 9,764 |
| `assert()` reads its INI once per change; INI lookups by FxHash | e5bc64c9, f3b4ca14 | `assert($x)` 2,905 → 1,958 |
| Property guards in a small vector | 65e3937a | `__get` 4,332 → 4,148 |

## Library functions

All of these are 1,000-element inputs unless noted.

| Change | Commit | Before → after | C |
|---|---|---|---|
| `in_array()` of a string: byte loop | b9790651 | 30 strings: 4,359 → 2,168 | |
| `mb_substr()` slices valid UTF-8 without decoding | f321d9f8 | 3,879 → 2,565 | 798 |
| `HashTable::push_all` (packed fill): `array_merge`, `array_keys`, `array_slice`, `to_list` | b39b9211 | `array_merge` 231,881 → 128,791; `array_keys` 102,432 → 49,393 | 42,764 / 22,638 |
| `ZArray::from_values` packed fill; `implode()` reads pieces in place; `explode` one-byte count | 6a7e629c | `explode` 238,287 → 138,362; `implode` 241,337 → 173,221 | 73,333 / 67,483 |
| VM `serialize()` without `format!`; stack budget cached | d6a7837d | 1,492,875 → 439,053 | 239,318 |
| SWAR `memnstr()` (word-at-a-time `memchr`) | 18817d97 | `strpos` on 2.4 KB 29,423 → 6,027; `explode(",")` → 102,302 | 2,114 / 73,333 |
| `array_unique()` keyed by `ZStr`; `array_column()` keys made once | a86b841f | `array_unique` 372,484 → 184,672 | 120,704 |
| `json_encode()`: inline capacity check | b811f35d | 2,560,703 → 1,799,438 | 1,099,762 |
| SPL `class_uses()` & co. off the generic dispatch; no user-method search on internal classes | 9e844a75, 02679327 | `spl_object_hash` 9,820 → 1,472; `iterator_to_array` 14,666 → 11,804 | |

## Still slow (C 8.4 for comparison, namespaced code)

| Construct | ours | C | Notes |
|---|---|---|---|
| Plain user call | ~650–990 | ~150 | The protocol itself; see "Dead ends" |
| Builtin call through the generic prepared path | ~500 protocol + handler | ~150 | `Args` owns `Vec<Value>` copies; frameless classes avoid it |
| `getName()` / `isStatic()` | ~700 | ~200 | `pure_method_now` is already lean |
| `__get` | 3,700 | 589 | nested `execute` |
| `foreach` over a user `Iterator` | ~4× | | 5 method calls per element |
| `preg_match` | 3,173 | 690 | |
| `mb_strrpos` | 2,354 | 540 | |
| `strrpos` (not frameless) | 1,803 | 294 | generic protocol |
| generators | 16.8k | 2.3k | |
| named args, spread, `compact`/`extract` | ~7× | | |
| `unserialize` | 5.3× | | two-pass node tree |
| memory | 157 MB | 85 MB | unit tests need `memory_limit=1G` |

## Dead ends (don't retry without a new idea)

- **Fused call/return, a "plain" frame state word, threaded dispatch.**
  All net losses (Nov 2026). Code added around `run_tape` slows every op
  in the loop.
- **New arms in `hot::step_ext` / `run_tape`.** The micro-benchmarks
  improve, but Laravel gets 0.1–0.3% slower each time. New fast paths
  belong in out-of-line handlers.
- **Moving `$this`/called into `Args` and skipping pools in `call_pure`.**
  No measurable change.
- **Caching `ArrayAccess` methods on `Class`.** No measurable change: the
  profiler's attribution on this box is skewed. Trust totals, not
  per-symbol percentages.
- **Fast paths picked from micro-benchmarks alone** (25 Nov 2026). Four
  in a row looked good in a micro-benchmark and did nothing for Laravel:
  `InitArray`/`ArrayAdd` as `VmOp` rows (the generic ones run only in
  frames with an attached symbol table), one method-cache lookup for user
  methods (the pure-builtin path it skipped bails out early on most of
  Laravel's calls), batching a call's `Send`s (one argument per call has
  nothing to batch), and the `: static` return above. Measure the real
  workload first, with a counter where the profile is unclear
  (`perf/census/op-census.diff` has the hook), then pick the target.
- **Fusing calls whose arity can never run in place** (e.g.
  `count($a, 1)`). Slower than the prepared generic call, so these are
  left unfused at compile time (`frameless_arity`).

## Method notes

- **Slow-op census.** The patch `perf/census/op-census.diff` (see "Where
  the remaining gap is"). It finds the ops that still reach the generic
  dispatch and the tape's exits. It is temporary instrumentation: keep
  it out of commits.
- **Per-function profile.** A temporary observer in php-ext-zendtest
  reads a perf counter per call. It overstates call costs (~2.7k per
  observed call).
- **Lesson.** Laravel code is namespaced: any fast path keyed on global
  function names misses it. Check the namespaced form of every such path.
