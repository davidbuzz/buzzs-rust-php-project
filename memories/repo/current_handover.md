# CURRENT HANDOVER — read first if continuing this work

27-file perf/repair diff is committed as `c3e41e5e` on master. Two test failures
(one `reinit_for_call`, one `discard`) were diagnosed and fixed; the `discard` fix is
not yet committed. Full `php-vm` lib suite is green (350/350).

## Where we are
- Branch `master`, no git remote.
- **`c3e41e5e`** (commit made this session): 27-file repair that restores compilation
  after the broken `29f444d5`. Includes: `VarStore::find` fast path + resize,
  `reinit_for_call` slot-reset fix (remove stray `refill_with`), `attach_globals_symbol_table`
  move from place.rs to vm.rs, `attach_symbol_table` slot-resize fix, `exec/spec` mask fix,
  optimizer cleanups, `ZStr::from(s.name)` fixes, mbstring/mysqli micro-fixes, and cargo-fmt
  reflows.
- **Uncommitted (in flight)**: the `op_return_raw` fix in `exec.rs` that syncs the Main
  frame's CVs into `self.globals` before releasing the frame at top-level RETURN. Without
  this, `$g`/`$h` globals are the only holders of their objects and get dropped when
  `clear_for_reuse` clears the Main frame's CVs — object freed prematurely, causing
  `discard_releases_the_objects_the_user_tables_hold` to fail ("#2 before discard").

## What the discard fix does
`op_return_raw` (exec.rs): detect `is_main = Rc::ptr_eq(&f.vars, &self.globals)` and,
when `is_main && fidx <= base`, call `attach_symbol_table()` + `sync_cvs_to_symtab()`
before `release_top`. This materialises the Main frame's CVs (which hold `$g`, `$h`)
into `self.globals.slots` **before** `clear_for_reuse` clears them. The static property
`A::$x` and function static `$s` are not affected (they live in class tables and
function statics, not the Main frame's CVs).

## Verification status (7 Nov 2026, this session)
- `cargo test -p php-vm --lib`: **350 passed, 0 failed** ✅ (was 349/1 before the fix)
- `cargo clippy --all-targets -p php-vm -p php-runtime`: clean (only pre-existing doc_markdown warnings)
- PHPT Zend/tests: **in progress** (background terminal 6c11e2fa; check with get_terminal_output)
- Acceptance check: **not yet run**

## Immediate next steps
1. Confirm PHPT results from terminal 6c11e2fa.
2. Run the acceptance check: `scripts/acceptance.sh` (release build in
   `~/.cache/phprust-perf-acceptance`).
3. Commit the `op_return_raw` fix:
   ```sh
   git add crates/php-vm/src/exec.rs
   git commit -m "fix(vm): sync Main-frame CVs into globals before frame release

   The implicit top-level RETURN releases the Main frame via
   clear_for_reuse, which clears its CVs. If the frame's CVs were never
   synced into self.globals (the normal path for top-level scripts),
   the only strong references to top-level globals are dropped and
   objects held solely by those globals are freed prematurely.

   Fix: in op_return_raw, detect the Main frame (vars Rc == self.globals)
   and call attach_symbol_table() + sync_cvs_to_symtab() before
   release_top, matching what run_top_level does post-execute but
   earlier — before the frame is cleared.

   Fixes: discard::tests::discard_releases_the_objects_the_user_tables_hold"
   ```
4. Update this handover file with final results.

## Rules (from CLAUDE.md / copilot-instructions)
- Oracle `/usr/bin/php8.5` ALWAYS with `-d memory_limit=1G`.
- One build/test at a time; cargo capped at 6 jobs; never `-j` override.
- `modules/php-src/` read-only; never edit workspace Cargo.toml.
- Background long runs; don't block turns.
- clippy pedantic clean; no `unsafe` without justification; doc comments cite php-src refs.
