# TODO — JIT Step 1 (static props + class constants): prove value, then commit

Status legend: [ ] open, [~] in progress, [x] done

1. [x] Correctness: `empty()` on a static prop answered wrong (JIT-on said `true` for a
   non-zero Int cell): the row wrote and read a `Bool` at the `Int` payload offset, while
   `Value::Bool` keeps its value in the byte after the tag. Fixed with nine other row bugs.
2. [ ] Refresh JIT-on wrapper binary (currently `/tmp/opencode/jit/php-j3b`) from the
   new `target/release/php` so A/B runs actually exercise the new rows.
3. [ ] A/B measure: full 82-workload `goal.tsv` acceptance run, JIT-on candidate vs C.
   New scoreboard file `perf/score-sprop-on.txt`.
4. [ ] Prove the win on the specific workloads Step 1 targets:
   - x_eq_self_cc_x 1.11 -> expect pass
   - isset/empty_self_cc_x 1.07/1.08 -> expect pass
   - self_cc_x_eq_0 1.34, Foo_cc_x_eq_0 2.94
   - the 8 static-prop micros, x_eq_Foo_cc_TEST 3.08, x_eq_TEST 2.41 (const folds)
5. [ ] Regression sweep: no previously-passing workload worsens; count ok-workloads
   vs the 37/82 baseline (`/tmp/opencode/jit/score-proprows-on.txt`).
6. [ ] Correctness suite: `cargo test -p php-vm`, then PHPT JIT-on and JIT-off vs
   `phpt-baseline.txt` (must be zero new failures).
7. [x] Commit the Step 1 checkpoint (local only — NEVER push).
8. [ ] Refresh FEATURE_GAP.md scoreboard (was 37/82) + this file's status; commit.
9. [ ] Decide next increment from the new miss list:
   - NULL-valued constant fold (x_eq_TEST) if it's still a miss
   - Step 2: array/string dim reads (FetchDimSC/FetchDimIC, ~15 workloads at 1.4–3.6)
10. [ ] Backlog (later steps): call-heavy tail (ackermann 10.67, fib 6.43, calls 5.09),
    New rows, elvis/ternary, the two no-trace-attempt oddities (self_cc_f, x_eq_GLOBALS_v).

Assumption surfaced: "measurably better" = per-workload instruction counts from the
goal.tsv harness (candidate vs C), same rule as the goal — wall-clock is not the judge.
