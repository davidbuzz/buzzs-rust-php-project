---
name: phprust-status-2026-11-06-ws29
description: "State at 6 Nov 2026 ~15:10 when work stopped (token budget): master 0f5a015c validated (run12), round ws29 stopped before any commits"
metadata:
  node_type: memory
  type: project
  originSessionId: ebaf0a84-fb28-4fd1-bc69-f1147e28a4a5
  modified: 2026-11-06T05:06:28.745Z
---

Work stopped at 95% token usage on 6 Nov 2026 (~15:10, the owner's request). Resume from here.

**Master 0f5a015c** (clean). Validated as run12 on 0af145fa: build clean, Rust tests 4,505 pass / 6 fail, PHPT 20,169/20,174 run pass (99.9%), perf gate geometric mean 5.21x C PHP (was 6.44x), startup 49.0M vs 18.8M instructions. FEATURE_GAP rows, phpt-baseline.txt (5 entries) and perf budgets were refreshed in 0f5a015c. Scratchpad (session-specific, may be gone): ws20/BRIEF29.md, FOLLOWUPS.md, AGENTS.md, status/, run12/.

**Open failures:**
- 4 of the 6 Rust test failures are only the bare test VM lacking count().
- The other 2, plus gh10251.phpt, are real: nested `.=` copies the string; property assignment must read the value before the property name (ZEND_ASSIGN_OBJ, bug #79599).
- PHPT: arginfo_zpp_mismatch, concat_003 (load-flaky), and two environmental ones that count as PASS per [[feedback-environmental-failures-pass]].

**Round ws29 was stopped before any commits.** Worktrees in scratchpad ws20/wt29-*, branches ws29/*, from 9d65a9da/1ed67623:
- frames, handlers, propops: the owner's list of biggest performance wins.
- opcache, optimizer: opcache, now scheduled.
- modphp-vm: one VM per process.
- fixup10: the test failures. Its status file holds the diagnosis.
- testcount: count() for the bare test VM.
- envpass: the environmental-pass list and runner status.
Only propops and testcount had partial uncommitted edits. builtin-frames and opcache preload were never started. To resume, relaunch fresh agents on the same worktrees with BRIEF29.md, merge, then ONE consolidated run ([[phprust-one-build-rule]]).

**Why:** next session needs to know nothing in ws29 landed and where to restart.
**How to apply:** check `git -C <worktree> status` before relaunching; delete stale worktrees only when clean. Related: [[phprust-scope-decisions]], [[phprust-performance-goal]].
