# CURRENT HANDOVER — read first if continuing this work
13-file perf/cleanup diff is uncommitted on master; 2 php-vm test failures diagnosed and need fixing before PHPT/perf/commit.

## Where we are
- Branch `master`, no git remote. HEAD `29f444d5` (sapi-apache2 output-start/abort/upload-progress).
- Working tree: one coherent uncommitted diff across 13 files (see "What's in flight").
  `git status`: M in php-ext-mbstring/funcs.rs, php-ext-mysqli/lib.rs, php-vm/{builtins/array.rs, call.rs,
  classlink.rs, compiler/optimizer/{pass1.rs,sccp.rs}, exec/spec/{props.rs,statics.rs}, ext.rs, place.rs,
  reflection/misc.rs, vm.rs}.

## What's in flight
A performance/cleanup round (continuing the 5.21x -> parity effort). The diff:
1. **Hot-path micro-cleanups** (clippy pedantic + small perf):
   - `exec/spec/props.rs`: `mask::FLOAT` -> `mask::DOUBLE` (FLOAT does not exist; DOUBLE = 1<<4 in
     `types.rs`), `Some(Some(r))` double-Option closures, and the isset/empty cache path collapsed to
     reuse the shared `plain_truth()` helper (also derefs References).
   - `exec/spec/statics.rs`: `r.try_borrow().ok().map(...)` -> `r.try_borrow().map(...)` (same result).
   - `builtins/array.rs` (`in_array`): `php_stdarr::ops::is_identical/is_equal` -> `php_runtime::ops::*`
     (identical functions, right crate layer).
   - `classlink.rs` + `reflection/misc.rs`: `ZStr::from(s.name.as_str())` -> `ZStr::from(s.name)`
     (Sig name is already `String`, avoids one `&str` round-trip).
   - `ext.rs`: hoist `since`/`message` `ZStr` allocation out of the `format_suffix` call (skip alloc when empty).
   - `pass1.rs`: `Count` fold avoids a temporary `Value::Array` clone (reads len directly).
   - `sccp.rs`: unused-import cleanup.
   - `php-ext-mbstring/funcs.rs`: `call(vm,a,|st| Ok(st.mb_check_encoding(...)))` -> drop redundant `Ok`
     (trait already returns `Result`).
   - `php-ext-mysqli/lib.rs`: `*i` -> `i` (cop) in `mysqli_real_connect` flags.
2. **`VarStore::find` (vm.rs)**: linear scan kept under 8 names, else build index once; new slot
   bounds-check + resize so a found index is always backed by a slot. `find_or_add` consistency checked.
3. **Frame pooling (vm.rs)**:
   - `reinit_for_call`: NEW - resets every existing slot to `None` in place (`for slot in vs.slots.iter_mut()
     { *slot = None; }`) instead of clear+refill when the VarStore is uniquely owned.
   - `reinit_for_user`: slots cleared then refilled with `None` (unchanged behaviour).
   - `init_cvs`: slot resize moved out of the `names.is_empty()` branch (resizes whenever short).
   - `attach_globals_symbol_table` moved from `place.rs` into `vm.rs` (now also syncs CVs->symtab for Main
     frame or globals-shared frame); `place.rs` copy deleted; `op_bind_global` and 8 call sites in
     place.rs use the new one.
4. `call.rs`: `CallTarget::Static(&'a Class, ..)` -> `&'a Rc<Class>`.

## Verification status (7 Nov 2026, this session)
- `cargo fmt` clean; `cargo clippy --all-targets -p php-vm -p php-ext-mbstring -p php-ext-mysqli`: only
  pre-existing warnings (doc_markdown etc. in files the diff doesn't touch).
- `cargo test -p php-ext-mbstring -p php-ext-mysqli`: PASS (32 + 8).
- `cargo test -p php-vm`: **348 pass, 2 FAIL**:
  1. `vm::tests::reinit_for_call_leaves_the_compiled_variables_to_the_caller` (vm.rs:1858) - asserts
     `vars.slots.is_empty()` right after `reinit_for_call`. The new in-place reset (item 3) keeps the
     slots at length ncv with `None`, so the assertion now fails. The test encodes the OLD contract
     ("leaving the compiled variables empty for the caller to fill"); the new contract is
     "slots present but undefined". Decide: the in-place reset looks intentional (avoids refill churn);
     most likely fix is updating the test to expect `len()==ncv, all None` - BUT check callers of
     `reinit_for_call` (frame reuse in `call.rs` / exec) to confirm nothing depends on slots being
     absent (e.g. `find()` returning None vs Some(None) - in `VarStore::find`, `Some(None)` slots still
     report the name as found; `find_or_add` treats both as found. `init_cvs` then fills them. Should be
     safe, but verify `symtab_attached`/`sync_cvs_to_symtab` paths treat `Some(None)` and absent identically).
  2. `discard::tests::discard_releases_the_objects_the_user_tables_hold` (discard.rs:211, "#2 before
     discard") - objects (static prop A::$x, static var $s, global $g) still live after `vm.discard()`.
     Likely cause: the moved/changed `attach_globals_symbol_table` (now also calls `sync_cvs_to_symtab()`,
     copying CVs into the globals symtable on Main/globals frames) is creating new strong references in
     `self.globals` that outlive the request, OR the `init_cvs` resize change changed which slots exist
     for the Main frame's globals. Diagnose: statics live in class tables (should be dropped by discard
     regardless); the global `$g` is in the Main frame's VarStore/symtable. Check what `vm.discard()`
     walks and whether `sync_cvs_to_symtab` during shutdown (request reset path) re-roots the globals.

## Immediate next steps
1. Fix the 2 test failures (see above; start by reading `discard()` in `crates/php-vm/src/discard.rs`
   and the `sync_cvs_to_symtab` interaction; `git stash` A/B to confirm each failure's cause).
2. `cargo test -p php-vm` green, then scoped PHPT: `target/debug/phpt-runner --php target/debug/php
   --no-artifacts modules/php-src/Zend/tests` (5665 tests) plus a fast smoke
   (`modules/php-src/ext/json`).
3. Perf gate: `scripts/perf_gate.sh` (release build; budgets in `perf/bench/budgets.tsv`).
   Workloads most affected: micro:prop_*, isset_*, x_eq_Foo_cc_*, empty_loop, func/userfunc.
4. If green: commit (`git add` the 13 files; message along the lines of
   `perf(vm): in-place frame slot reset, VarStore find fast path, hot-path cleanups`).
5. Update FEATURE_GAP.md perf rows only if the gate changes numbers (coordinator role, verified results).

## Rules (from CLAUDE.md / copilot-instructions)
- Oracle `/usr/bin/php` ALWAYS with `-d memory_limit=1G`.
- One build/test at a time; cargo capped at 6 jobs; never `-j` override.
- `modules/php-src/` read-only; never edit workspace Cargo.toml.
- Background long runs; don't block turns.
- clippy pedantic clean; no `unsafe` without justification; doc comments cite php-src refs.
