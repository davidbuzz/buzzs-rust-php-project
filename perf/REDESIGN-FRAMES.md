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

## Frame migration, stage 1: the slot accessor layer (28 Nov 2026)

Base 2b46fe36. The migration to one contiguous slot block per call starts
with an accessor layer, so that the storage swap changes one module.

### The API (`crates/php-vm/src/vm.rs`)

`Frame` no longer has `cvs` and `tmps` fields. It has `slots: FrameSlots`,
whose two vectors are private to `vm.rs`:

| Accessor | Gives |
|---|---|
| `f.slots.cvs()` / `f.slots.cvs_mut()` | `&[CvSlot]` / `&mut [CvSlot]` |
| `f.slots.tmps()` / `f.slots.tmps_mut()` | `&[Value]` / `&mut [Value]` |
| `f.slots.split_mut()` | `(&mut [CvSlot], &mut [Value])`: one op reads one kind and writes the other |
| storage, `vm.rs` only | `from_vecs`, `fit_cvs`/`fit_tmps` (readying), `cut_*`, `clear_*_buffer`, `shrink_released_*` (pool release) |
| `tmps_storage_mut()` | `#[doc(hidden)]`, only for the frame-pool tests (capacity checks) |

Why a field of its own rather than methods on `Frame`: a borrow of
`f.slots` leaves `f.code`, `f.this`, `f.iters`, ... free, just as the two
vector fields did. With accessors on `Frame`, a slot borrow would lock the
whole frame, and about 30 functions that hold a literal or `$this` while
writing a slot would need restructuring. Only four functions in the whole
crate held a variable and a temporary at once; they now use `split_mut`.

Everything returns slices, so the accessors compile to the same loads as
before. Measured against a release build of 2b46fe36 (JIT off per call;
apps JIT on, each measured twice alternating):

| | baseline 2b46fe36 | step 1 (6e823433) | step 2 (01cb0b89) |
|---|---:|---:|---:|
| f | 514 | 512 | 513 |
| alt (f+g) | 1,317 | 1,315 | 1,316 |
| `$this->m()` / `$o->m()` / `self::s()` | 684 / 705 / 628 | 686 / 706 / 628 | 685 / 706 / 627 |
| empty loop | 45-46 | 45-46 | 45-46 |
| Laravel unit | 131.486-131.488G | 131.490G (+0.003%) | 131.482G (-0.004%) |
| MW Api | 266.874-266.898G | 266.871-266.888G (noise) | 266.859-266.898G (noise) |

PHPT on the 17 directories: 0 of 12,636 results changed against the
baseline binary. `cargo test -p php-vm`: only the known float-math
failure. No clippy warning on a changed line.

### What is migrated and what is not

* All ~540 slot accesses outside `vm.rs`, in 30 files (the tape and its
  families, `exec.rs`/`hot.rs`/spec helpers, `call.rs`, `place.rs`, the
  generators, closures, reflection, the GC's frame scan, the JIT's
  frame-reading helpers, `php-ext-stdarr`'s symbol table, the tests), are
  migrated. The compiler enforces it: the vectors are private.
* Inside `vm.rs`, every slot read and write goes through the accessors,
  including the symbol-table attach and sync, the frame build from a
  symbol table and the release loops. The only code that touches the
  vectors is `FrameSlots`' own methods (the sizing and pool operations
  above) and `vm.rs`'s unit tests.
* Not behind the layer yet: the *sizing policy*. `FrameStack`'s pool
  (`push_in_place`, `reinit_*`, `ready_released_for`, `clear_for_reuse`,
  `release_*`, `drop_pool`) still decides when a frame's slots are
  resized, kept or given back, by calling the `FrameSlots` methods.
  Stage 2 replaces that policy, not just the storage.

### How the stage-2 storage swap plugs in

`FrameSlots` becomes `{ base: *mut Slot, ncv: u32, ntmp: u32 }` (or a
block index), pointing into the VM stack, a `Vec<MaybeUninit<Slot>>`
grown in chunks that never move (php-src's `zend_vm_stack` pages). The
accessors build their slices from it. Pushing a call means bumping the
stack top by `ncv + ntmp` slots and initialising them (CVs undefined,
TMPs null). Popping means releasing the counted values and lowering the
top; nothing is kept for the next call, so the pool's readying and release
functions mostly go away. The hard parts:

1. **Symbol table attach** (`attach_symbol_table`, sync, `extract`,
   `$$x`, `get_defined_vars`): it maps CV slots into a `VarStore`. It
   already works on `cvs_mut()` slices, so it is unaffected as long as a
   frame's slot block does not move while attached. It does not: blocks
   only move with their frame.
2. **Generators**: a generator's frame lives in `GenData` as a
   `Box<Frame>` and is swapped onto the stack at each resume
   (`push_boxed`/`pop_boxed`). Its slots cannot live on the shared stack
   between resumes. A generator's `FrameSlots` must own a heap block (a
   second variant, or a block allocated off-stack at creation), and
   `push_boxed` pushes only the header. This is also step 2 of the issue
   ("generators keep their frame in place").
3. **Fibers**: a fiber's frames are moved off the shared stack on
   suspension (`split_off`/`drain`/`extend` in `fiber.rs`). Slot pointers
   into the VM stack would dangle. Each fiber needs its own VM stack
   segment (php-src gives every fiber its own `zend_vm_stack`), so the
   stack must be per execution context, not per `Vm`.
4. **GC frame scan** (`gc/traverse.rs`, `gc/collect.rs`, `gc::cvs_released`):
   it reads slices through the accessors and keeps working unchanged. Only
   frames saved by fibers and generators need their blocks visited, as
   today.
5. **JIT slot bases**: the tape core and the traces take raw `cvs`/`tmps`
   base pointers (`St`, the trace ABI `f(st.cvs, st.tmps, frame)`). Those
   become `base` and `base + ncv`, the same ABI. A trace that calls a
   function (not done today) would have to reload them after the stack
   grows, unless chunks never move, which is why the stack must be
   chunked.
6. **Frame headers**: `Frame` itself (≈280 bytes, ~30 fields) stays in
   `FrameStack.vec` in stage 2. Moving the header into the block, with
   raw function and op-array pointers and a flags word, is a separate
   stage 3. It touches the ~320 `vm.frames` uses (backtraces, reflection,
   `func_get_args`, exceptions), which should first get their own
   accessor layer (`FrameStack` already hides most of it behind
   `last`/`get`/`iter`).

Effort: stage 2 (slots on a chunked VM stack, generators owning blocks,
per-fiber stack segments) is about one to two weeks. Stage 3 (headers in
the block, protocol rewrite, the light-frame core technique on the hot
path) is several more.

### The frame stack behind its own layer (stage 1, part 2)

Base f2265649. The ~320 direct `vm.frames` uses now go through
`FrameStack` methods only; its storage (`vec`, `live`) is private to
`vm.rs`, and it no longer derefs to `[Frame]`.

| Accessor | For |
|---|---|
| `len()`, `is_empty()`, `last()`/`last_mut()`, `first()`/`first_mut()`, `get(i)`/`get_mut(i)`, `stack[i]` (`Index<usize>`) | the live frames, by depth |
| `iter()` / `iter_mut()` → `FrameIter` / `FrameIterMut` | walks (backtraces, the GC's frame scan, `debug_backtrace`, the symbol-table sync); newtypes over the slice iterators, double-ended and exact-size, so callers never see a slice. An `impl Iterator` return was tried first: the borrow it holds lasts to the end of scope (it might have a destructor), which broke two extensions' code. |
| `both_mut(i)` / `both(i)` | frames `i-1` and `i`, both present: a call's caller and the released frame it readies, a return's caller and callee |
| `pair_mut(i)` | the same as two options (three call sites that use one side alone) |
| `pooled(i)`, `pooled_mut(i)`, `pooled_at(i)`, `pooled_at_mut(i)` | a frame live or released (the pool slot a call is readied in) |
| `enter_pooled(live)`, `leave_released()` | the readied frame becomes the top; the released top leaves the live frames |
| `split_pooled_mut(i)` | the planned call and the plain return only: `both_mut`'s pair left an unfolded null test there (+2 to +6 per call, the register-allocation lottery), so those two keep the raw split |
| `push`, `pop`, `truncate`, `push_in_place`, `push_builtin`, `ensure_pooled`, `release_*`, `pop_cleared_top`, `push_boxed`/`pop_boxed`, `drop_pool` | as before (they were already methods) |

Not behind the layer: whole-stack moves. Fibers (`fiber.rs`: `insert`,
`splice`, `drain`, `split_off`, `extend`) and the reflection call that
runs code under a borrowed stack (`reflection/misc.rs`: `split_off`,
`drain`, `From<Vec<Frame>>`) move `Frame` values in and out of
`Vec<Frame>`s. Moving the headers onto a VM stack makes those moves of
stack segments; they are the fiber design question listed above.

Measured (release build of f2265649 as baseline, JIT off per call, apps
JIT on, each twice alternating):

| | baseline f2265649 | part 1 (`vec`/`live` private) | part 2 (no `Deref`) |
|---|---:|---:|---:|
| f / alt | 513 / 1,316 | 513 / 1,317 | 513 / 1,317 |
| `$this->m()` / `$o->m()` / `self::s()` | 685 / 706 / 628 | 685 / 706 / 628 | 684 / 705 / 628 |
| empty loop | 45-46 | 45-46 | 45-46 |
| Laravel unit | 131.479-131.480G | 131.425G (-0.04%) | 131.425G (-0.04%) |
| MW Api | 266.57-266.88G | 266.77G (-0.04%) | 266.42-266.46G (-0.06%) |

(The MediaWiki baseline moved from 266.85G to 266.57G between the two
rounds: the box's disk filled up in between. Each column is compared with
the baseline readings alternating with it.)

PHPT on the 17 directories: 0 of 12,636 results changed against the
baseline binary. `cargo test -p php-vm`: only the known float-math
failure. `cargo test --workspace --no-run` builds; it was run without
debug info (`CARGO_PROFILE_DEV_DEBUG=0`), because with it the test
binaries of the whole workspace are ~60G and filled the disk once. No
clippy warning on a changed line.
