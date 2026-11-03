# Cycle-collector hooks (proposals for the php-vm / php-runtime / php-ext-spl owners)

The collector itself is committed in `crates/php-vm/src/gc.rs` + `gc/`
(registered by `pub mod gc;` in `lib.rs` and `crate::gc::register(vm);` in
`builtins/mod.rs`). On master **without** these diffs it already collects
every cycle that goes through an object, at explicit `gc_collect_cycles()`
calls (it scans `object::live_objects()`), including closures, generators
and fibers. The diffs below add what cannot live in gc.rs. Each applies
with `git apply` on master 3de9598 and was measured in a private worktree.

| # | Diff | Owner | What it enables |
|---|------|-------|-----------------|
| 01 | `01-php-runtime-reference-roots.diff` | php-runtime | reference cycles (`$a[] = &$a`) |
| 02 | `02-php-vm-register-reference-roots.diff` | php-vm (builtins/mod.rs) | wires 01 into the collector |
| 03 | `03-php-vm-safe-point-and-release-sites.diff` | php-vm (exec.rs, place.rs) | automatic (threshold) collection, `gc_status()["roots"]` |
| 04 | `04-php-vm-weakmap-traversal.diff` | php-vm (weakref.rs) | `WeakMap` entries as GC edges (gh10043-*, self-referencing maps) |
| 05 | `05-php-ext-spl-traversal.diff` | php-ext-spl | `ArrayObject`/`SplObjectStorage`/`SplFixedArray` cycles (gc_024) |
| 06 | `06-php-vm-fiber-traversal.diff` | php-vm (fiber.rs) | fibers whose callable closes a cycle (fibers/gc-cycle-callback) |

## 01 — php-runtime: possible roots for references
`crates/php-runtime/src/value.rs:318` (`RefInner` → `pub(crate)`),
`value.rs:424` (`PhpRef::downgrade`/`from_inner`, `impl Drop for PhpRef`),
new `crates/php-runtime/src/ref_roots.rs`, `lib.rs:41` (`pub mod ref_roots;`).

Rationale: a cycle made only of arrays and references has no object, so
the object store cannot find it, and its last external handle is often
released inside Rust drop glue (an array element of an array being freed,
`unserialize()` temporaries) where no VM hook can see it. php-src calls
`gc_check_possible_root()` on every `GC_DELREF` to non-zero; the `Drop`
impl does exactly that for `PhpRef` handles whose value is an array or
object (`Z_COLLECTABLE`), buffering a `Weak` once per reference. Cost per
`PhpRef` drop: one `strong_count` load + branch; recording is off until the
collector turns it on. No change to `Value`'s layout or API.

## 02 — php-vm `builtins/mod.rs:82`
Registers 01 as a `gc::RootProvider` and `gc::note_possible_root` as the
threshold callback. (Separate from 01 so that gc.rs compiles on master
without 01.)

## 03 — php-vm safe point + release sites
* after `exec.rs:862` (the pending-destructor poll, before the memlimit poll):
  ```rust
  // Root buffer reached the threshold (`gc_possible_root_when_full`).
  if crate::gc::wants_run() {
      self.gc_safe_point()?;
  }
  ```
  `wants_run()` is a single thread-local `Cell<bool>` load. The collector
  cannot run inside `Drop`, so the root that crosses the threshold only
  freezes the current buffer (exactly the roots php-src would collect) and
  the run happens here.
* after `exec.rs:110` (`cv_assign`) and `exec.rs:118` (`cv_bind`),
  `place.rs:716/721` (CV `AssignRef` / `Unset`): `crate::gc::possible_root(&old)`
  before dropping the old value — assignment overwrite / `unset()` of a
  variable holding an object (php-src: `zval_ptr_dtor` → `gc_check_possible_root`).
* after `exec.rs:1196` (`op_return_raw`): `crate::gc::frame_released(&frame);`
  before `drop(frame)` — scope exit (`i_free_compiled_variables`).

Further sites (property/element overwrite, `unset($o->p)`, `unset($a[k])`)
would make `gc_status()["roots"]` and automatic-run timing closer to
php-src; they are not needed for correctness (explicit runs scan all
objects).

## 04 — php-vm `weakref.rs:362` / `:779`
`impl GcTraverse for Rc<MapCell>` (`zend_weakmap_get_gc`): each entry is
reported with `GcVisitor::weak_entry(key, value)` — a conditional edge,
live only while both the map and the key are (php-src's
`GC_FROM_WEAKMAP`/`GC_FROM_WEAKMAP_KEY`). Storage shared with an active
iterator (`Rc` count > 1) is not reported. Registered in
`weakref::register`. When a key is garbage the collector calls
`weakref::object_freed(handle)` early, which drops the entries keyed by it
(they may hold the key itself).

## 05 — php-ext-spl `hooks.rs:56`, `dispatch.rs:289`
`get_gc` for `Rc<php_spl::SplObject>` via
`php_vm::gc::register_traverse_with` (the state type is foreign to
php-ext-spl, so the trait cannot be implemented there): `ArrayObject`
storage, `SplObjectStorage` entries, `SplFixedArray` elements.

## 06 — php-vm `fiber.rs:609` (before `register`) and `:610` (registration)
A complete `zend_fiber_object_gc`: the resolved call of a not-yet-started
fiber (`FiberData::call`, private) is reported along with the callable,
suspended frames, transfer value and return value, using the public
`GcVisitor::pending_call`/`GcVisitor::frame` helpers. gc.rs registers a
partial traversal (public fields only) that this one supersedes.

## The `get_gc` contract (`php_vm::gc::GcTraverse`)
Native state stored with `object::set_internal` is traversed when its type
is registered (`gc::register_traverse::<T>()` for a local `T: GcTraverse`,
`gc::register_traverse_with::<T>(f)` otherwise). The implementation must
report every `Value`/`Obj`/`ZArray`/`PhpRef` handle the state owns, once
per handle held; must not report handles owned through an `Rc` shared with
something else (check `Rc::strong_count == 1`); reports `WeakMap`-style
entries with `weak_entry`; must not run PHP code or mutate. Omitting a
handle is always safe (the target is treated as externally referenced);
over-reporting is not. When the object is garbage the collector drops the
state (`free_obj`). No `ClassHooks` field is needed: the registry is keyed
by the state's `TypeId`.

## Shared arrays and references
Arrays and references whose count is above the number of references
coming from inside the candidate subgraph are live (treated as opaque):
the collector never frees or clears anything that has a holder it did not
enumerate. A shared array/reference is freed only when *every* handle to
it is owned by garbage — the same condition php-src's trial deletion
checks.

## Measurements
`phpt-runner -j 4 -q` (debug build):

| | Zend/tests/gc | Zend/tests/weakrefs | Zend/tests/fibers |
|---|---|---|---|
| before (no collector) | 34/85 | 23/38 | — |
| master (gc.rs only) | 62/85 | 24/38 | 103/122 |
| master + 01..06 | 75/85 | 34/38 | 105/122 |

Plus `Zend/tests/magic_methods/bug71818.phpt` (memory_limit 20M with
100k self-referencing objects) passes with 03 applied. Remaining gc
failures: exact root counts / automatic-run timing where the VM clones
more than php-src (gc_023, gc_045, bug70805_1/2); bug77345_gc_2 (the
300k-node list alone takes ~120 MB in this VM, the GC graph tips it over
128M); non-GC diffs (gc_043, bug65372).
