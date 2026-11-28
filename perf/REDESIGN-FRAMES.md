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

## Step 2: the frame layout, prototyped as a light frame (28 Nov 2026)

Base a5887085, release builds in this worktree's target dir (`php-b2`:
f 513, g 529, alt 1,315, `$this->m()` 685, `$o->m()` 705, `self::s()` 627;
Laravel unit 131.58G, MW Api 266.87G).

### What was built

A partial switch is possible for one class of calls only: a callee whose
tape, from its first op to its returns, has nothing but instructions that
need neither the `Frame` nor the VM (`leaf_safe`: the numeric core rows,
jumps, `Return`). For those, a planned function call (`calls::light_call`)
puts the callee's compiled variables and temporaries at fixed offsets of
one preallocated slot block (`Vm::light_slots`, 64 slots), copies the
arguments there, stores the caller's resume point and nothing else: no
pooled frame is readied, no `Frame` field written, no `Rc` touched, no
frame pushed. `light_run` runs the callee's tape on that block with its own
copy of `run_core`, and its `Return` (`calls::light_return`) moves the
value to the caller's slot and forgets the scalars. Anything else the
callee stops at (an overflowing `+`, a poll, an undefined variable, a
value whose release could run user code) turns the light frame into the
real frame it stands for (`calls::light_materialize`) and the normal path
goes on from that op. Methods, closures, typed or defaulted parameters
and declared return types are not covered (they need `$this`, the scopes
or the frame's handles, i.e. the `Frame` itself).

The full diff is `perf/light-frame-prototype.diff` (the variant that
measured best on the apps, L6; not committed as code).

### Numbers

| Variant | f | g | alt | empty loop | methods | Laravel unit | MW Api |
|---|---:|---:|---:|---:|---:|---:|---:|
| baseline a5887085 | 513 | 529 | 1,315 | 46 | 685 / 705 / 627 | 131.58G | 266.87G |
| L1/L3: light frame run by `run_tape`'s own core (state checked at each core stop) | 454 | 483 | 967 | 51 | | 135.60G (+3.0%) | 275.33G (+3.2%) |
| L4: light frame on its own core (`light_run`), one test in `run_tape` | 416 | 449 | 893 | 46 | | 133.30G (+1.3%) | 270.74G (+1.5%) |
| L5: L4, the "may be a leaf" test inline | 419 | 453 | 899 | 45 | 706 / 726 / – | 132.19G (+0.46%) | 268.07G (+0.44%) |
| L6: `run_tape` untouched, the light run inside `call_plain` | 491 | 524 | 1,042 | 45 | 688 / 708 / 632 | 131.65G (+0.05%) | 267.06G (+0.07%) |

(Output of a battery of leaf, overflow, string/array/object-returning and
undefined-variable cases is identical to the baseline and to C 8.5, JIT
on and off.)

Per call of `f` in L4 (callgrind by symbol): `light_call` 133,
`light_return` 113 (+14 drop glue), `light_run` 98 (its prologue, the
`St`, the callee's own `Add` and dispatch, ~25 of it), `run_tape` 48.

### What this shows

* **The layout is worth it where it applies.** With no frame to ready or
  release, the alternating ruler drops by a third (1,315 -> 893): the
  ~105 per call of readying a pooled frame for another function and the
  ~15 field resets are gone, as predicted. `f` drops by a fifth
  (513 -> 416) even though `light_call`/`light_return` are still written
  as generic, checked Rust; straight-line versions should bring a leaf
  call to ~250.
* **It does not reach the apps as a side path.** Leaf functions are rare
  in Laravel and MediaWiki (their calls are methods reading properties),
  so the light path cannot win there, and every attempt to test for it
  costs the calls that are not leaves: 1.3% with an out-of-line test in
  `run_tape`, 0.45% with an inline one (~20 instructions per method call
  of register shuffling in `run_tape`, as before: the loop is fragile
  under any change), ~0.05% with the test moved into `call_plain`.
* **Core-loop changes are taxed again.** Running the light frame on
  `run_tape`'s core (L1-L3) changed its register allocation: the empty
  loop went 46 -> 51 and the apps lost 3%. Giving the light frame a core
  of its own (a second instance of the `run_core` loop in `light_run`)
  removed the tax entirely. That is the useful technique for any further
  fast path: run it on its own copy of the core, never add state to
  `run_tape`'s.

### Recommendation

The light frame stays a prototype (no app gain). The layout itself should
be done for real, for every user frame, because the cost it removes
(readying, releasing, `Rc` traffic, `Option`/bounds checks per slot) is
exactly the cost of the calls apps make, methods included. As a side
path it can only ever cover leaves.

What the full change takes (estimate):

1. A VM stack of slot blocks: frame header (raw `*const UserFunction`,
   op array, `$this`, scope, called class as handles owned by the frame,
   flags word, `ip`, return target, argument count, cold pointer) plus
   CVs and TMPs at fixed offsets; frames addressed by index or pointer;
   the pool and `ready_*`/`release_*`/`reinit_*` functions removed.
2. `Frame` users: `.cvs`/`.tmps` are touched at ~740 sites in 38 files,
   `vm.frames` at ~320, `Frame` named ~830 times (backtraces, reflection,
   `func_get_args`, `compact`/`extract`, symbol-table attach,
   generators/fibers, the GC's frame scan, exceptions, the JIT's slot
   bases). Most become accessor calls on a frame view; the symbol table,
   generators (frames moved into `GenData`) and fibers (own stacks) need
   real design.
3. Builtin frames on the same stack (header only), the call protocol
   (`call_plain`, `enter`, `push_*_from_native`) and returns rewritten
   against it; the light frame's `light_run` technique for the hot path.

That is several weeks for one person, done in stages behind the existing
API (first a `FrameView` accessor layer over today's `Frame`, then the
storage swap), with the PHPT dirs and both apps measured at each stage.
