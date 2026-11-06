---
name: current-handover
description: "ACTIVE handover — read first if continuing this work; master fdf4df95 is clean and validated (5.21x C PHP, PHPT 99.9%); round ws29 (perf list + opcache) was stopped before any commits and needs relaunching"
metadata:
  node_type: memory
  type: project
  originSessionId: ebaf0a84-fb28-4fd1-bc69-f1147e28a4a5
  modified: 2026-11-06T09:05:50.652Z
---

# Handover, 6 Nov 2026 ~15:30 (work stopped: token budget at ~95%)

## Where we are
- Repo /home/buzz/phprust, branch `master`, no git remote configured, working tree clean. HEAD `fdf4df95` (README/FEATURE_GAP speed numbers).
  Before it: `3670e2f3` (fix: `$o->{$p} = $undef` warns for the value before the property name, ZEND_ASSIGN_OBJ order; fixes
  Zend/tests/gh10251.phpt), `0f5a015c` (FEATURE_GAP rows, phpt-baseline.txt with 4 entries, perf budgets from run12).
- Release binary: `target/release/php`, built 15:16 from 3670e2f3. Debug build current. Perf gate release copy:
  `~/.cache/phprust-perf-gate/release/php` (build of 0af145fa).
- Latest measurements (release 3670e2f3):
  - Perf gate geometric mean: 5.21x C PHP's user-space instructions over 82 workloads. The range is 1.43x (zend:strcat) to
    13.3x (ops:userfunc); none is at or below 1x.
  - Startup: 48.9M vs 18.8M instructions.
  - Last full PHPT run (run12 of 0af145fa): 20,169 of 20,174 run pass. With 3670e2f3 that should be 20,170; not rerun.
  - Rust tests: 4,507 pass / 5 fail. Four fail only because the bare test VM (`Vm::new`) lacks count(): coalesce_elvis_nullsafe_and_moves_on_cvs,
    class_constants_through_objects, slot_storage_of_every_size_class, unpacking_sends_each_element_as_the_iterator_yields_it. The fifth is
    concat_assign_on_elements_statics_and_chains_extends_in_place: nested `$a['n']['m'] .= 'x'` copies (1.001 allocations per iteration).

## What's in flight (all STOPPED, nothing committed)
Round ws29 implemented the owner's list of "biggest wins" plus opcache.
- Shared brief: `/tmp/claude-1000/-home-buzz-phprust/ebaf0a84-fb28-4fd1-bc69-f1147e28a4a5/scratchpad/ws20/BRIEF29.md`. It covers rules, file ownership
  and package scopes; the scratchpad is session-specific and may be gone.
- Worktrees `.../ws20/wt29-<pkg>`, branches `ws29/<pkg>`, all with 0 commits:
  - frames: calls + inline CV storage.
  - handlers: one small handler per opcode, plus safe points.
  - propops: dedicated property opcodes; has 3 uncommitted files.
  - opcache: script cache, API, INI and file cache. Skeleton crates crates/php-opcache and php-ext-opcache are already registered in the workspace.
  - optimizer: Zend/Optimizer study and passes.
  - modphp-vm: one VM per httpd worker thread instead of one per request.
  - fixup10: test failures; its status file has the count() diagnosis.
  - testcount: count()/sizeof() for the bare test VM; has 3 uncommitted files.
  - envpass: an environmental-pass list `phpt-environment.txt` plus a runner status.
- Never started: builtin-frames (slim builtin frames, after frames commits its frame API) and opcache preload (after modphp-vm).
- Also 4 finished, merged worktrees are kept because BRIEF29 points at their status notes: wt27-perf-calls, wt27-dispatch2, wt27-props2, wt27-builtins2.
- **Immediate next step** (if the owner wants to continue):
  1. Check each wt29 worktree (`git -C <wt> status`).
  2. Relaunch fresh Opus subagents on them with BRIEF29.md (edit-only; no per-branch builds).
  3. Merge all branches.
  4. Run ONE consolidated build/test/PHPT/perf run (`.../ws20/validate.sh <dir>`, then `finish.sh <dir> "<date>" <sha>`).

## Recent decisions (owner, 6 Nov 2026)
- Opcache is IN scope and scheduled now, ahead of the high/med rows. php-cgi stays OUT of scope; its 305 skipped tests stay skipped.
- Environmental failures that C PHP fails identically here count as PASS: ext/gd createfromstring-overflow (libgd has no GD2) and
  mysqli_fetch_field_types (MariaDB has no JSON type). See [[feedback-environmental-failures-pass]]. The runner support (envpass) is not done.
- AVIF: no bug. This libgd has only AVIF stubs; C PHP 8.5 claims support but fails at run time.
- The bare test VM should get a real count() (testcount design: a core constructor for with_extensions, count/sizeof only in `Vm::new`).
- In-house app test is private (/home/buzz/phprust-apptests, never in git). REPORT.md there lists the findings. The included-file `..` path bug
  was fixed by strings2, but the app suites were not rerun after the fix.
- Process rules: [[phprust-one-build-rule]], [[feedback-no-foreground-wait]], [[feedback-no-invented-labels]]; use subagents for edits.

## Pending
- No background tasks running; no PRs.
- Open owner questions: none. Research workflows (PHPT 99.9→100%, environmental pass) were stopped unfinished.
