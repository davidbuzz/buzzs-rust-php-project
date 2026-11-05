# CURRENT HANDOVER — read first if continuing this work
Perf program vs competing "php-rust" project: sealed-numeric lever landed (geomean 17.88→16.39); builtin-call buffer pooling is HALF-IMPLEMENTED and uncompiled in the working tree; two subagents are (were) running on mimalloc and ext/standard fixes.

## Where we are
- Branch `master`, many commits ahead of any remote (local-only repo). Recent: `8dba934` (sealed numeric Op::Bin forms, budgets ratcheted to geomean 16.39), `c3e39c0`/`23fe8c1`/`8d2942d` (competitor audit in README + read-only clone at `modules/php-rust`), `3784951`/`f0d71b1` (NoDiscard + deprecated-identifier test clusters, baseline 1,188).
- **Uncommitted working-tree state is a MIX of three workstreams** (check `git status`):
  1. **Mine, php-vm, INCOMPLETE/UNCOMPILED**: builtin-call buffer pooling in `crates/php-vm/src/{call.rs,vm.rs,lib.rs}` — `CallBufs` pools on Vm (`call_bufs.args/values/passed`), `push_pending()`/`put_arg_vec()` helpers, pooled takes in `call_builtin`, reclaim of `call_args` after ZPP and of `values`/`passed` at the old `drop(args)` site. **Missing: `put_val_vec`/`put_bool_vec` are referenced but NOT YET DEFINED** (define next to `put_arg_vec` in call.rs, clear + cap pool at 16). Not built, not tested.
  2. **Subagent "mimalloc-lever"** (background Coder agent): `crates/php-memlimit/*`, `Cargo.lock` — making php-memlimit delegate to mimalloc. Acceptance: full gate PASS with geomean strictly < 16.39, 0 new phpt failures, else revert. It commits on success.
  3. **Subagent "stdlib-fixes"** (background Coder agent): `crates/php-stdstr/src/csv.rs` so far — fixing ext/standard baseline failures in stdstr/stdarr/support crates only; commits its crates + phpt-baseline.txt on success.
- If the agents are gone in the new session: inspect their uncommitted diffs, finish/validate per the criteria above, or `git checkout --` their files if broken.

## What's in flight (immediate next steps)
1. Finish the pooling lever: define `put_val_vec`/`put_bool_vec`, `cargo build -q -p php-vm`, `cargo test -q -p php-vm`, then release build `CARGO_TARGET_DIR=$HOME/.cache/phprust-perf-gate cargo build --release -p php-cli` and full gate `PHP_BIN=$HOME/.cache/phprust-perf-gate/release/php scripts/perf_gate.sh` — targets: ops:count (was 61×→ expect big drop), ops:strlen 46×, ops:userfunc 26×; geomean must go < 16.39 with no workload over budget. Then corpus check `env -u LANGUAGE target/debug/phpt-runner --php target/debug/php -j 6 -q --no-artifacts --baseline phpt-baseline.txt modules/php-src/Zend/tests modules/php-src/tests modules/php-src/ext/standard` → must be 0 new failures. Ratchet (`scripts/perf_gate.sh --ratchet`) + commit if green.
2. Next levers (ranked by measured gap vs competitor): user-call frame pooling (ops:userfunc/method 26× vs their 7.5–9×; Frame construct/drop ~5% of count workload), property inline caches keyed on class-ptr + epoch (ops:prop_read 26× vs their 5.6×), RefCell-free CV slots.

## Recent decisions & findings (not obvious from git)
- Competitor audit (their binary at `~/.cache/phpr-audit/release/phpr`, build needs `libtidy-dev` — installed): same-corpus conformance **phpr 47.6% vs ours ~82%**; same-gate instructions **phpr ~6× geomean vs ours 16.4×**; their parser can't parse `Zend/micro_bench.php` (35/82 workloads crash). Oracle probes show their hot-path semantics are byte-correct — their speed is engineering (sealed forms, ICs, lean call protocol, mimalloc), their failures are missing surface (pure VM-stateless builtins are their structural ceiling; ours take `&mut Vm`).
- Sealed-numeric experiment proved the user's key question: **we can absorb their leanness with 0 new test failures** (full corpus validated).
- Perf measurement is layout-sensitive (strcat ±6% from code layout): use out-of-line slow paths (`bin_slow` pattern) and re-measure; stable across 3 runs = real.
- phpr's audit run CONTAMINATED modules/php-src test tree once (leftover `nul:`/`NUL` files, open_basedir dirs → 2 false "new failures"); fixed via `git clean -fdq tests Zend/tests` in modules/php-src. Do that if weird security-test failures appear.
- `modules/php-rust` is a READ-ONLY reference clone (gitignored): study ideas/methodology (PERF_MAP.md, REGOLE.md), NEVER copy code (PHP License 3.01, and repo rule in CLAUDE.md).

## User rules from this session (binding)
- **Adoption bar: any change taken from the competitor must make OUR numbers better** — gate geomean down AND 0 new baseline failures; otherwise revert.
- Only performance and test-completeness matter to the user right now.
- Subagents are authorized for pulling in competitor-inspired work (keep them off each other's crates).
- Never foreground-wait on builds — background them and do docs/commits/planning meanwhile (also in .github/copilot-instructions.md).
- cargo job cap is 6 (committed); one cargo build/test at a time per agent; oracle `/usr/bin/php` always with `-d memory_limit=1G`.
- Mandatory new-project web search memory saved at `~/.config/Code/User/prompts/new-project-research.instructions.md`.

## Pending / active context
- Background agents (IDs from this session, likely dead in a new one): mimalloc `6aa23059-de49-46f5-b494-f562ca7f5e79`, stdlib-fixes `630f3ce7-3dcc-4e86-9691-04f2b5719f54`. Their uncommitted file edits are in the tree either way.
- Binaries: ours release `~/.cache/phprust-perf-gate/release/php`, competitor `~/.cache/phpr-audit/release/phpr`.
- Current pinned numbers: gate geomean 16.39 (budgets.tsv ratcheted at `8dba934`), phpt baseline 1,188 known failures, Zend/tests ~82% of ran, full in-scope suite 91.8%.
- Profiles: `/tmp/el.perf` (empty_loop), `/tmp/cnt.perf` (count) — perf record files, re-record if stale.
- Strategic milestone agreed in README: WordPress-core-suite validation over our native mysqli as the next application axis; rename project before publishing (phprust.com name collision).
