# The specialised instruction loop ("tape"): faster, mostly

Performance results of 10 November 2026 for the first version of the tape: the commit that adds
this file, on top of master `eae5e3dc`. Written as a record of where this work stands.

## Summary

- Plain loops and integer and float arithmetic now execute fewer instructions than C PHP 8.3.6
  (18-28% fewer on the gate's four workloads of that kind).
- Over the 82 workloads of the performance gate we are still **2.89x** slower than C PHP,
  down from 3.27x. The gate passes: every workload is inside its budget.
- 36 workloads got faster, 30 got slower (by 1-14%) and 16 did not change.
- The slower ones are dominated by function calls, array access and object creation. The new
  loop does not handle those itself yet, and handing them to the old code now costs one extra
  call.
- Two results need follow-up (see "Open findings"). The first keeps the functions of many
  programs off the new loop altogether.

## What changed

`crates/php-vm/src/exec/tape.rs` gives every compiled function a second form, built the first
time the function runs: an array of pre-decoded instructions, one per op, called the tape.

- The opcode of a tape instruction names the operation *and* the kinds of its operands
  (variable, temporary, small integer literal, other numeric literal).
- Its operands are byte offsets into the frame's variable and temporary slots, read without
  bounds checks. The decoder validates every offset once.
- php-src works the same way: `zend_vm_gen.php` generates one handler per opcode and operand
  kind (`ZEND_ADD_SPEC_TMPVARCV_CONST` and so on). Our existing ops are generic and were decoded
  again on every execution.

The tape has its own instructions for jumps, comparisons that branch (`<`, `<=`, `>`, `>=`,
`==`, `!=`), `+`, `-`, `*`, `+=`, `-=`, `*=`, `++`, `--`, assignment of numbers, bools and
null, and the loop step fused with its condition (`$i++` followed by `$i < $n`).

These instructions only handle plain ints and floats. Given anything else (a string, an array,
an object, a reference, an undefined variable, an integer overflow) an instruction changes
nothing and the op runs through the existing code. So does every op that has no tape
instruction.

## How it was measured

- `scripts/perf_gate.sh` (method in `perf/README.md`): user-space instruction counts from
  `perf stat -e instructions:u` on a release build, with start-up subtracted.
- Reference: `/usr/bin/php` 8.3.6. Both binaries run with
  `-d memory_limit=1G -d opcache.enable_cli=0`.
- "Before" is a release build of `149ba5a6`. "After" is `eae5e3dc` plus the tape changes
  (the commit that adds this file). The commits between the two add the opcache SSA dump and tests and delete old patch
  files.
- Ratio means our instructions divided by C PHP's. Below 1.00 we execute fewer instructions
  than C PHP.

## Results

### Overall

| Set | Workloads | Before | After |
|---|---:|---:|---:|
| All | 82 | 3.27 | 2.89 |
| `Zend/bench.php` | 18 | 3.57 | 2.96 |
| `Zend/micro_bench.php` | 35 | 2.96 | 2.86 |
| `perf/bench/suite/*.php` | 10 | 3.47 | 3.48 |
| Single operations, `perf/bench/*.php` | 19 | 3.47 | 2.61 |

The figures are geometric means of the ratio to C PHP. The "All" row is the gate's own figure;
the rows per set are computed from the per-workload ratios in the appendix.

### Single operations

Instructions per loop iteration, loop overhead included.

| Operation | Gate name | C PHP | Before | After | After ÷ C PHP |
|---|---|---:|---:|---:|---:|
| Empty `for` loop | `ops:empty_loop` | 29 | 94 | 23 | 0.79 |
| Integer add | `ops:int_add` | 69 | 198 | 50 | 0.72 |
| Float multiply | `ops:float_mul` | 97 | 309 | 80 | 0.82 |
| Compare and branch | `ops:is_smaller_jmp` | 51 | 165 | 42 | 0.82 |
| String append (`.=`) | `ops:concat_assign` | 235 | 298 | 279 | 1.19 |
| Array read | `ops:dim_read` | 124 | 429 | 467 | 3.77 |
| Array write | `ops:dim_write` | 143 | 426 | 465 | 3.25 |
| Hash read, string key | `ops:hash_read` | 147 | 385 | 366 | 2.49 |
| Property read | `ops:prop_read` | 82 | 325 | 305 | 3.72 |
| Property write | `ops:prop_write` | 97 | 342 | 322 | 3.32 |
| `strlen()` | `ops:strlen` | 73 | 199 | 180 | 2.47 |
| `abs()` | `ops:abs` | 150 | 219 | 219 | 1.46 |
| `count()` | `ops:count` | 78 | 199 | 180 | 2.31 |
| User function call | `ops:userfunc` | 141 | 1,022 | 1,135 | 8.05 |
| User function call, typed | `ops:userfunc_typed` | 187 | 1,072 | 1,185 | 6.34 |
| Method call | `ops:method` | 183 | 1,303 | 1,336 | 7.30 |
| Static method call | `ops:static_method` | 173 | 1,223 | 1,256 | 7.26 |
| Closure call | `ops:closure` | 243 | 1,356 | 1,390 | 5.72 |
| `new` object | `ops:new_obj` | 636 | 2,173 | 2,400 | 3.77 |

### Other loop shapes

These are not gate workloads. Each loop ran inside a function, at 1,000,000 and at 2,000,000
iterations; the figure is the difference in instructions divided by 1,000,000. `...` stands for
`for ($i = 0; $i < $n; $i++)`.

| Loop | C PHP | Before | After |
|---|---:|---:|---:|
| `for ($i = $n; $i > 0; $i--) { }` | 30 | 97 | 21 |
| `... { $s += $i; }` | 76 | 173 | 44 |
| `... { $y = $y * $x; }` (floats) | 75 | 209 | 54 |
| `... { if ($i > 10) { $c++; } }` | 55 | 221 | 58 |
| `while ($i < $n) { $s = $s + $i * 2 - 1; $i = $i + 1; }` | 146 | 433 | 291 |
| `... { $s = ($s ^ $i) & 1023; }` | 84 | 394 | 437 |

### Other workloads that moved most

Ratio to C PHP, for the gate workloads other than the single operations.

| Faster | Before | After |
|---|---:|---:|
| `zend:simplecall` | 3.03 | 0.70 |
| `zend:simple` | 3.62 | 0.97 |
| `zend:mandel2` | 3.67 | 1.39 |
| `suite:int_loop` | 3.14 | 2.07 |
| `zend:mandel` | 3.45 | 2.51 |
| `micro:empty_Foo_cc_x` | 3.09 | 2.52 |
| `micro:empty_self_cc_x` | 1.60 | 1.30 |
| `micro:empty_this_arrow_x` | 2.55 | 2.13 |

| Slower | Before | After |
|---|---:|---:|
| `zend:hash2` | 2.60 | 2.97 |
| `suite:array_rw` | 2.17 | 2.47 |
| `zend:sieve` | 2.56 | 2.90 |
| `zend:heapsort` | 3.47 | 3.92 |
| `zend:simpleucall` | 7.51 | 8.33 |
| `zend:simpleudcall` | 6.69 | 7.42 |
| `zend:ary2` | 2.79 | 3.02 |
| `micro:x_eq_Foo_cc_TEST` | 3.38 | 3.65 |

## Why 30 workloads got slower

An op without a tape instruction is handed to the old code through one more function call than
before, and the loop around it does more work per hand-off.

Measured with callgrind on the user-call loop (`for (...) { g(1); }` with
`function g($x) { return $x; }`), instructions per iteration by function:

| Function | Before | After |
|---|---:|---:|
| The loop itself (`run_hot` before, `run_tape` after) | 270 | 319 |
| Hand-off to the old code (`step_slow`) | - | 64 |
| `push_simple_ucall` | 236 | 236 |
| `step_ext` | 172 | 172 |
| `clear_for_reuse` | 155 | 155 |
| `return_simple` | 127 | 127 |
| `extend_trusted` | 46 | 46 |
| `get` | 16 | 16 |
| Total | 1,022 | 1,135 |

The call path itself is unchanged, and it is where most of the 1,135 instructions go. C PHP
needs 141 for the same iteration.

## Open findings

1. **A script that has used `require` or `include` keeps the functions it calls on the old
   loop.** `empty_loop($n)` of `Zend/micro_bench.php`, which the gate loads with `require`,
   still costs 95 instructions per iteration (`micro:empty_loop`, unchanged at 3.17). A minimal
   copy behaves the same: called from a script that loaded it with `require`, the function
   costs 95 per iteration; declared in the main file it costs 24.

   Cause, found after the measurements with a trace of the entries to the new loop: `require`
   attaches a symbol table to the main script's frame, and a frame with an attached symbol
   table runs on the old loop (`run_hot`). The old loop makes plain calls itself and carries on
   in the callee, so the callee never reaches the new loop. The trace shows the main frame
   refused (`symtab=true`) and no entry for the callee.

   `simple()` of `Zend/bench.php` did speed up (3.62 to 0.97) although it is loaded the same
   way. It takes no argument, and such a call is made by `Vm::run`, not by the old loop. This
   last part is read from the code, not confirmed by a trace.

   The gate runs every `Zend/bench.php` and `Zend/micro_bench.php` workload from a script that
   starts with `require`, so this holds those rows back. The rows that did not change at all
   (`micro:empty_loop`, `micro:int_func`, `zend:nestedloop`) fit this; it has not been checked
   row by row.
2. **A `while` loop stepped with `$i = $i + 1`** only went from 433 to 291 (C PHP: 146). Cause
   not established. Suspected: the op that fuses this assignment with the loop condition has no
   tape instruction.

## What is not done

- Function calls, array access, property access, object creation, string operations and bit
  operations have no tape instructions. They are where the remaining 2.89x comes from.
- The extra hand-off cost described above has to be removed before this is a gain for every
  workload.

## Correctness checks

- Targeted PHPT run of 13,935 tests on the debug build (`Zend/tests`, `tests`, opcache, spl,
  reflection, standard, pdo, session, mbstring, phar and others): 45 failures, none of them
  new. The same run before the change had 49.
- After that run, the decoder was extended to comparisons with the literal or the loop variable
  on the right (`$x > 5`, `$i > 0`). The unit tests cover this and pass, 7 of 7
  (`cargo test -p php-vm --lib tape`). The PHPT run has not been repeated since, and the
  measured release binary includes this addition.
- Not run: the full suite (`./run-all-tests.sh`), clippy, and the rest of the
  php-vm unit tests (9 were failing before this change and have not been re-checked).

## What the commit contains

- new: `crates/php-vm/src/exec/tape.rs`, `crates/php-vm/src/exec/tape/tests.rs`
- changed: `crates/php-vm/src/exec.rs`, `exec/hot.rs`, `opcode.rs`, `persistence.rs`,
  `compiler/mod.rs` (9 lines added and 2 removed in total)
- `proto.rs`: the stand-alone prototype in which the design was measured first
- this file

## Reproduce

```sh
GATE_TIMEOUT=40 scripts/perf_gate.sh                    # release build, then all 82 workloads
GATE_TIMEOUT=40 scripts/perf_gate.sh --filter '^ops:'   # the single operations only
cargo test -p php-vm --lib tape                         # the unit tests of the tape
```

## Appendix: every workload

Instruction counts; for the `ops:` rows, instructions per loop iteration. Change is the change
in our instruction count.

| Workload | C PHP | Before | After | Ratio before | Ratio after | Change |
|---|---:|---:|---:|---:|---:|---:|
| `zend:simple` | 78,013,608 | 282,012,135 | 76,007,642 | 3.62 | 0.97 | -73.0% |
| `zend:simplecall` | 30,015,510 | 91,010,114 | 21,009,557 | 3.03 | 0.70 | -76.9% |
| `zend:simpleucall` | 139,022,223 | 1,044,011,859 | 1,158,014,633 | 7.51 | 8.33 | +10.9% |
| `zend:simpleudcall` | 156,012,782 | 1,044,017,242 | 1,158,015,931 | 6.69 | 7.42 | +10.9% |
| `zend:mandel` | 656,740,311 | 2,264,541,508 | 1,649,019,183 | 3.45 | 2.51 | -27.2% |
| `zend:mandel2` | 729,005,689 | 2,676,857,038 | 1,011,271,251 | 3.67 | 1.39 | -62.2% |
| `zend:ackermann` | 158,052,200 | 1,525,437,616 | 1,565,843,718 | 9.65 | 9.91 | +2.6% |
| `zend:ary` | 19,469,426 | 53,174,028 | 53,627,445 | 2.73 | 2.75 | +0.9% |
| `zend:ary2` | 17,576,259 | 49,034,803 | 53,133,114 | 2.79 | 3.02 | +8.4% |
| `zend:ary3` | 352,700,685 | 889,847,796 | 945,553,377 | 2.52 | 2.68 | +6.3% |
| `zend:fibo` | 529,089,006 | 3,375,168,194 | 3,577,113,954 | 6.38 | 6.76 | +6.0% |
| `zend:hash1` | 49,818,937 | 179,858,399 | 178,864,717 | 3.61 | 3.59 | -0.6% |
| `zend:hash2` | 65,392,338 | 170,162,046 | 194,247,964 | 2.60 | 2.97 | +14.2% |
| `zend:heapsort` | 174,334,186 | 605,570,094 | 683,342,621 | 3.47 | 3.92 | +12.8% |
| `zend:matrix` | 190,405,065 | 483,227,908 | 433,144,409 | 2.54 | 2.27 | -10.4% |
| `zend:nestedloop` | 137,114,320 | 508,716,982 | 508,714,418 | 3.71 | 3.71 | -0.0% |
| `zend:sieve` | 117,471,428 | 301,231,818 | 341,082,819 | 2.56 | 2.90 | +13.2% |
| `zend:strcat` | 48,022,703 | 67,251,929 | 67,251,114 | 1.40 | 1.40 | -0.0% |
| `micro:empty_loop` | 9,005,976 | 28,528,635 | 28,531,731 | 3.17 | 3.17 | +0.0% |
| `micro:func` | 36,298,141 | 237,333,589 | 255,634,428 | 6.54 | 7.04 | +7.7% |
| `micro:undef_func` | 39,607,459 | 237,336,432 | 255,641,011 | 5.99 | 6.45 | +7.7% |
| `micro:int_func` | 14,992,224 | 47,730,929 | 47,731,478 | 3.18 | 3.18 | +0.0% |
| `micro:x_eq_self_cc_x` | 60,600,837 | 82,530,796 | 76,844,502 | 1.36 | 1.27 | -6.9% |
| `micro:self_cc_x_eq_0` | 53,707,459 | 87,339,759 | 81,645,389 | 1.63 | 1.52 | -6.5% |
| `micro:isset_self_cc_x` | 59,715,721 | 111,635,687 | 110,144,829 | 1.87 | 1.84 | -1.3% |
| `micro:empty_self_cc_x` | 61,807,821 | 98,734,616 | 80,440,648 | 1.60 | 1.30 | -18.5% |
| `micro:x_eq_Foo_cc_x` | 32,718,478 | 82,239,880 | 76,545,654 | 2.51 | 2.34 | -6.9% |
| `micro:Foo_cc_x_eq_0` | 24,303,204 | 87,040,902 | 81,341,627 | 3.58 | 3.35 | -6.5% |
| `micro:isset_Foo_cc_x` | 29,699,540 | 111,333,847 | 109,839,661 | 3.75 | 3.70 | -1.3% |
| `micro:empty_Foo_cc_x` | 31,815,920 | 98,429,926 | 80,142,717 | 3.09 | 2.52 | -18.6% |
| `micro:self_cc_f` | 62,704,298 | 276,337,656 | 276,343,982 | 4.41 | 4.41 | +0.0% |
| `micro:Foo_cc_f` | 45,909,026 | 257,431,686 | 257,442,110 | 5.61 | 5.61 | +0.0% |
| `micro:x_eq_this_arrow_x` | 23,104,538 | 95,144,366 | 89,452,993 | 4.12 | 3.87 | -6.0% |
| `micro:this_arrow_x_eq_0` | 26,711,869 | 99,945,189 | 94,250,927 | 3.74 | 3.53 | -5.7% |
| `micro:this_arrow_x_pluseq_2` | 43,216,750 | 96,946,681 | 91,252,883 | 2.24 | 2.11 | -5.9% |
| `micro:pp_this_arrow_x` | 38,120,410 | 79,543,413 | 73,854,999 | 2.09 | 1.94 | -7.2% |
| `micro:mm_this_arrow_x` | 37,801,276 | 79,548,468 | 73,853,487 | 2.10 | 1.95 | -7.2% |
| `micro:this_arrow_x_pp` | 38,116,337 | 79,546,847 | 73,848,219 | 2.09 | 1.94 | -7.2% |
| `micro:this_arrow_x_mm` | 37,813,362 | 79,547,146 | 73,853,057 | 2.10 | 1.95 | -7.2% |
| `micro:isset_this_arrow_x` | 41,722,513 | 124,847,260 | 123,350,415 | 2.99 | 2.96 | -1.2% |
| `micro:empty_this_arrow_x` | 43,808,149 | 111,645,711 | 93,351,646 | 2.55 | 2.13 | -16.4% |
| `micro:this_arrow_f` | 43,226,094 | 274,248,769 | 274,258,665 | 6.35 | 6.34 | +0.0% |
| `micro:x_eq_Foo_cc_TEST` | 37,518,931 | 126,946,101 | 136,851,608 | 3.38 | 3.65 | +7.8% |
| `micro:new_Foo` | 118,810,190 | 246,348,466 | 259,255,545 | 2.07 | 2.18 | +5.2% |
| `micro:x_eq_TEST` | 20,418,797 | 65,147,378 | 59,750,706 | 3.19 | 2.93 | -8.3% |
| `micro:x_eq__GET` | 41,719,816 | 154,846,858 | 141,052,133 | 3.71 | 3.38 | -8.9% |
| `micro:x_eq_GLOBALS_v` | 36,322,293 | 89,743,754 | 89,749,075 | 2.47 | 2.47 | +0.0% |
| `micro:x_eq_hash_v` | 44,416,236 | 115,543,875 | 115,553,330 | 2.60 | 2.60 | +0.0% |
| `micro:x_eq_str_0` | 37,222,845 | 100,253,322 | 100,253,919 | 2.69 | 2.69 | +0.0% |
| `micro:x_eq_a_elvis_null` | 33,900,607 | 83,145,054 | 83,151,808 | 2.45 | 2.45 | +0.0% |
| `micro:x_eq_f_elvis_tmp` | 33,614,230 | 109,843,915 | 109,846,423 | 3.27 | 3.27 | +0.0% |
| `micro:x_eq_f_q_f_a` | 29,719,415 | 103,249,687 | 103,257,982 | 3.48 | 3.47 | +0.0% |
| `micro:x_eq_f_q_f_tmp` | 29,407,602 | 110,145,154 | 110,149,977 | 3.74 | 3.75 | +0.0% |
| `suite:array_rw` | 320,003,201 | 695,396,063 | 789,634,560 | 2.17 | 2.47 | +13.6% |
| `suite:arrays` | 2,300,817,719 | 4,431,547,213 | 4,426,525,402 | 1.93 | 1.92 | -0.1% |
| `suite:builtins` | 481,761,032 | 2,483,124,100 | 2,651,130,282 | 5.15 | 5.50 | +6.8% |
| `suite:calls` | 396,172,573 | 2,404,445,847 | 2,514,546,535 | 6.07 | 6.35 | +4.6% |
| `suite:fib` | 531,830,251 | 3,414,364,528 | 3,616,310,334 | 6.42 | 6.80 | +5.9% |
| `suite:float_math` | 1,203,136,682 | 2,828,626,517 | 2,868,629,045 | 2.35 | 2.38 | +1.4% |
| `suite:int_loop` | 645,048,970 | 2,026,495,369 | 1,336,493,003 | 3.14 | 2.07 | -34.0% |
| `suite:objects` | 1,268,099,126 | 6,299,635,233 | 6,674,636,148 | 4.97 | 5.26 | +6.0% |
| `suite:props` | 587,622,558 | 1,774,685,694 | 1,893,194,274 | 3.02 | 3.22 | +6.7% |
| `suite:strings` | 1,248,596,493 | 3,358,728,640 | 3,473,731,442 | 2.69 | 2.78 | +3.4% |
| `ops:empty_loop` | 29 | 94 | 23 | 3.24 | 0.79 | -75.5% |
| `ops:int_add` | 69 | 198 | 50 | 2.87 | 0.72 | -74.7% |
| `ops:float_mul` | 97 | 309 | 80 | 3.19 | 0.82 | -74.1% |
| `ops:is_smaller_jmp` | 51 | 165 | 42 | 3.30 | 0.82 | -74.5% |
| `ops:concat_assign` | 235 | 298 | 279 | 1.26 | 1.19 | -6.4% |
| `ops:dim_read` | 124 | 429 | 467 | 3.46 | 3.77 | +8.9% |
| `ops:dim_write` | 143 | 426 | 465 | 3.00 | 3.25 | +9.2% |
| `ops:hash_read` | 147 | 385 | 366 | 2.60 | 2.49 | -4.9% |
| `ops:prop_read` | 82 | 325 | 305 | 3.96 | 3.72 | -6.2% |
| `ops:prop_write` | 97 | 342 | 322 | 3.49 | 3.32 | -5.8% |
| `ops:strlen` | 73 | 199 | 180 | 2.73 | 2.47 | -9.5% |
| `ops:abs` | 150 | 219 | 219 | 1.46 | 1.46 | +0.0% |
| `ops:count` | 78 | 199 | 180 | 2.58 | 2.31 | -9.5% |
| `ops:userfunc` | 141 | 1,022 | 1,135 | 7.20 | 8.05 | +11.1% |
| `ops:userfunc_typed` | 187 | 1,072 | 1,185 | 5.70 | 6.34 | +10.5% |
| `ops:method` | 183 | 1,303 | 1,336 | 7.16 | 7.30 | +2.5% |
| `ops:static_method` | 173 | 1,223 | 1,256 | 7.07 | 7.26 | +2.7% |
| `ops:closure` | 243 | 1,356 | 1,390 | 5.56 | 5.72 | +2.5% |
| `ops:new_obj` | 636 | 2,173 | 2,400 | 3.42 | 3.77 | +10.4% |
