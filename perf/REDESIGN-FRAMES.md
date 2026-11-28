# Call frames and the call protocol: prototype measurements (issue #45, step 1)

Branch `perf/redesign-frames` on 70713cc0. aarch64, instruction counts
(`instructions:u`), JIT off for the micro-benchmarks
(`percall.sh ... mwbench/mc2.php`, loop subtracted), JIT on (the default)
for the applications. Binaries: release with line tables
(`CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`); the baseline build of
70713cc0 measures the same as `php-rb39` (f 584 vs 583).

## Where a plain call goes (baseline, `f($i)`, body `return $x + 1`)

Per call, by ELF symbol (address-based count from callgrind
`--dump-instr=yes`; callgrind's own function names mislabel inlined code
here, so the counts were re-attributed through `nm`):

| Part | Instructions |
|---|---:|
| `run_tape`: core exit at `CallInit`, the `WHY_CALL` arm, a frame-loop round (`frames.last_mut`, reload, `tape.get`, `JitCtx::maybe_for_tape`, `St`), core re-entry; the same again at `Return` (`break`, `match ops.get(at)`, `flush`, plain test), plus the callee's `Add`/`Return` dispatch and the caller's assignment | 220 |
| `calls::call_plain` (plan lookup, guards, two passes over the arguments, frame readying, field stores) | 201 |
| `Vm::return_plain::<false>` (tests, result move, `release_plain`, pop) | 163 |
| **total** | **584** (C 8.5: 132) |

By kind: loads 144, branches 137, compares 56, moves 59, arithmetic 69,
spills 46, stores 34, prologue/epilogue saves 27. No single step is
large: the cost is ~12 steps of 15-40 instructions each.

Inside `call_plain`, the second argument pass (`plan_arg` returning
`Option<Value>`, then `CvSlot::replace` and the drop test of the old
value) was ~50 of the 201, mostly moves of the `Option<Value>` through the
stack.

## Approaches tried

| Variant | f | g | alt (f+g) | empty loop | Laravel unit | MW Api |
|---|---:|---:|---:|---:|---:|---:|
| baseline 70713cc0 | 584 | 629 | 1,485 | 45 | 133.35G | 271.52G |
| P1: call and return made inside `run_core` (new arms in the core loop, `St` reassigned, callee/caller tape taken up there) | 618 | 660 | 1,551 | 49 | | |
| P2: P1 with `call_plain`/`return_plain` inlined into the core's helpers | 587 | 626 | 1,486 | 49 | | |
| P3: P2 + lean argument pass | 559 | 569 | 1,409 | 50 | | |
| P4: P3 + callee entry cached in the plan | 546 | 557 | 1,379 | 49 | | |
| P5: P4 + return type check reads the scopes lazily | 540 | 553 | 1,370 | 49 | **138.13G (+3.6%)** | **280.01G (+3.1%)** |
| A1: frame switch in `run_tape`'s inner loop (core untouched) + lean argument pass + lazy scopes | 514 | 530 | 1,316 | 45 | 132.21G (-0.86%) | 268.51G (-1.11%) |
| **A2 (committed): A1, and a return the loop declined still gets the base-frame attempt after it** | **512** | **528** | **1,316** | **46** | **131.87G (-1.11%)** | **268.20G (-1.22%)** |

Methods and static calls with A2 (JIT off; baseline in brackets): `$this->m()`
684 (757), `$o->m()` 705 (778), `self::s()` 627 (700), `O::s()` 570 (644).

(Apps: each binary measured twice, alternating with the baseline; the
readings of each binary agree to 0.02%. PHPT on Zend/tests, tests/lang,
tests/classes, ext/standard general_functions + array, ext/spl,
ext/reflection: 0 of 8,888 results changed.)

### P1-P5: the call inside the core loop (rejected)

`run_core` got an extra parameter (`CoreEnv`: VM, base, whether a call is
being prepared) and two arms: a `CallInit` with a plan made the call
through an out-of-line `core_call` and continued in the callee with the
`St` and instruction pointer it returned; the `Exit` of an `Op::Return`
(marked in its `k`) did the same through `core_return`. Exits of the core
after such a switch were handed to `run_tape` as a pending stop, handled
once the new frame's locals were looked up.

What it showed:

* The frame-loop round trips really are ~125 per call: `run_tape` went
  from 220 to 95 per call.
* But the out-of-line helpers cost what they saved (76 + 82 per call in
  P1: prologues, the frame and tape looked up again, `Core` returned
  through memory).
* Worst: the empty loop went from 45 to 49 instructions per iteration
  in every variant. Adding a parameter and making `st` loop-carried
  changed `run_core`'s register allocation, and that tax on every tape
  instruction cost the applications 3-4%. This is the trap
  `perf-call-path-experiments` describes (code added near `run_core`
  slows every instruction), now measured: +4 per loop iteration costs
  more than -45 per call gives back.

### A: switch frames in `run_tape`'s inner loop (kept)

`run_core` stays as it was. In `run_tape`'s inner loop, after a planned
call (`call_plain`) and after a plain return (`return_plain::<false>`),
the new current frame's op array, tape and resume instruction are looked
up (`take_up`: safe point due? tape decoded and without live JIT tables?
frame `usable`?) and the loop goes on with `c`, `ops`, `lits`, `tbase`,
`fp` and `p` replaced (`continue`), instead of a round through the outer
`'frames` loop. A JIT-hooked tape (`jctx` is `Some`) on either side keeps
the old path. A return the inner loop declines is not tried again after
the loop.

Combined with:

* the lean argument pass in `call_planned`: each argument written into
  the (released, undefined) compiled variable with `ptr::write`, read by
  kind in one match, without `Option<Value>` round trips (f -25, g -60);
* `return_plain`'s return-type closure capturing the scopes by reference
  (read only for a declared type).

Covered: every call a plan makes (functions, methods, static calls), every
return of a plain frame to a caller above the run's base, on tapes the
JIT does not hook. Not covered: calls made through `run_call` (prepared,
unplanned), builtins, the base frame of a nested execution, JIT-hooked
tapes (those take the old path).

## What remains of the 512, and what the next step must remove

After A2, per call of `f` (callgrind, by symbol): `run_tape` 184 (was
220; ~25 of it is the callee's `Add` and `Return` dispatch and the
caller's assignment), `call_plain` 175 (was 201), `return_plain` 154 (was
163). On the alternating ruler (`f` then `g`, per pair): `call_plain`
442, `run_tape` 360, `return_plain` 303, `ready_frame_slow` 211.

The plain call is still ~4x C. With the frame switch done, the remaining
cost is in the frame itself:

1. **Readying a pooled frame for a changed callee** (alt ruler: 1,316 for
   two calls vs 514 + 530 = 1,044 alone: ~135 per call). The frames are
   pooled per depth; 83% of planned calls in the apps find the pooled
   frame last used by another function: four `Vec` fits, three `Rc`
   exchanges (`func`, `ufunc`, `code` all hold the same function), the
   variable store's names. A contiguous VM-stack frame sized from the op
   array (php-src's `zend_vm_stack_push_call_frame`) has none of this.
2. **Field resets at release** (~15 stores in `release_plain`) for fields
   every entry writes again: write on entry, not on exit.
3. **The temporaries loop at return** (~25): php-src frees no TMP at
   return because every TMP is consumed by its user; ours can hold
   values.
4. **Twelve layers of `Option`/bounds tests** on `Vec` slots: with slots
   at fixed offsets after a frame header, the checks become offsets.
5. **Prologues** of `call_plain` and `return_plain` (~32): they are
   out of line because inlining them into `run_tape` spilled the
   loop's registers (measured before, `call-plans-design`).

## Recommendation

* Keep approach A (commit on this branch): it is the
  `ZEND_VM_ENTER`/`ZEND_VM_LEAVE` part of the protocol, it moves both
  apps by 1.1-1.2%, and it does not touch `run_core`. It is ready for
  review as it is (tests, clippy on the changed lines and PHPT pass);
  the JIT-on micro battery (x86 acceptance) was not run on this box.
* Never add state or arms to `run_core` for calls: the 4-instruction
  loop tax outweighs any per-call saving at application call densities.
* The next step that can reach the issue's exit criterion (≤ 200) is the
  frame layout, not the protocol: a contiguous slot block per call on a
  VM stack (header with raw `*const UserFunction`/op array pointers kept
  alive by the function table or the closure the frame holds, CVs and
  TMPs at fixed offsets), so that readying is a pointer bump and the
  `St` of the callee is `frame + const`. Items 1-4 above go away together;
  measured separately (cold state behind a pointer, lazy vars, this
  prototype's lean loop) each moves the ruler by 5-25 only.
