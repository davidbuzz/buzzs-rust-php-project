---
name: phprust-one-build-rule
description: Integration validation = exactly ONE full build/test/PHPT pass after ALL fixes and edits are in; no overlapping or incremental background builds
metadata:
  node_type: memory
  type: feedback
  originSessionId: ebaf0a84-fb28-4fd1-bc69-f1147e28a4a5
  modified: 2026-11-06T01:31:56.185Z
---

When integrating or finishing work, make every fix and edit first, then run ONE full build followed by tests/PHPT in a single chained job. The user corrected me sharply (5 Nov 2026) after I ran several background builds and partial test runs while still editing (each new edit invalidated the previous run, and builds overlapped a PHPT run).

**Why:** overlapping builds load the shared box (OOM history, see [[phprust-resource-rules]]) and slow PHPT into timeout noise, and runs started before the last edit measure nothing useful.
**How to apply:** batch all edits; only then `cargo build` -> `cargo test --workspace` -> phpt-runner in one background command; never start a second build while one is running; if the run finds failures, fix them all, then run the one consolidated pass again. Workers do edits/review only (see [[user-profile]]). On 6 Nov 2026 I let four Fable verifiers and a fix-up agent build per branch through a flock queue (`scratchpad/ws20/buildq.sh`, lock `/tmp/phprust-build.lock`); it produced 12 queued builds and the user stopped it: "explain why you have 12 builds queued? merge all the code, then do ONE build." So per-branch verification builds are out too, however risky the branch: merge everything textually, then one consolidated run; fix, then one run again. The queue script stays useful only to serialise my own validate.sh against any stray cargo. Related: [[phprust-performance-goal]].
