---
name: phprust-resource-rules
description: "Hard-won operational rules for running many build agents on this box (OOM history, Cargo.toml sweep, scratchpad etiquette, worktrees)"
metadata:
  node_type: memory
  type: feedback
  originSessionId: ebaf0a84-fb28-4fd1-bc69-f1147e28a4a5
  modified: 2026-11-06T01:55:14.976Z
---

Rules learned the hard way while coordinating 15-20 agents on /home/buzz/phprust:

- The box OOM-rebooted once (/usr/bin/php at 6.5 GB + VS Code). Every oracle php call needs `-d memory_limit=1G`; cargo is capped at 6 jobs in .cargo/config.toml (never override with -j); php-cli caps its own address space with RLIMIT_AS. User decision 6 Nov 2026: the cap becomes adaptive (max of 4 GiB and 75% of RAM+swap; `PHPRUST_MAX_AS_MB` still overrides) so ext/gmp gmp_setbit_long can run; memory_limit remains the real bound.
- MariaDB: user decision 6 Nov 2026 to `GRANT ALL ON *.* TO 'phptest'@<host>` (done) so mysqli gh22854 / mysqli_get_client_stats can pass; mysqli_fetch_field_types stays failing (MariaDB has no JSON type). The user declined running ext/mysqli single-threaded in the suite, so its two shared-table races stay flaky under -j 12.
- Always `git diff --stat Cargo.toml` before `git add`: I once swept an agent's uncommitted workspace-member line into my commit and HEAD stopped building.
- Never delete other agents' files in the shared scratchpad (I deleted `*.py` there once and broke running agents). Agents must use private worktrees under the scratchpad with their own CARGO_TARGET_DIR because the shared tree is often mid-edit.
- Agents lose uncommitted work when the VS Code session restarts: tell them to commit every milestone / 30 min with `git commit -- <paths>`.
- VS Code locked up (4 Nov 01:40) after the phpt-runner briefly reached 10 GB RSS and ~100 GB virtual (runaway test output + a PCRE2 JIT-stack leak, both fixed in 78c2a11 / 98db4af); a restart kills every agent's background tasks but keeps transcripts: resume each agent with SendMessage pointing at its worktree.
- `rm -f $S/*.py` style globs trip the safety check; use literal absolute paths.

**Why:** stability of the shared machine and of HEAD matter more than any one agent's speed.
**How to apply:** put these in every agent brief; verify HEAD builds in a private worktree before reporting numbers. Related: [[user-profile]].
- Killing stray processes: `pkill -f <pattern>` matches my own `bash -c` command line when the pattern appears in it and kills my shell mid-script; use `pkill -x <name>` (cargo, rustc, phpt-runner, php) or kill a process tree from a pid. Two coordinator sessions can coexist after a VS Code lock-up (the old process survives); check `pgrep -f native-binary/claude` and ListAgents peers before resuming agents, or duplicates collide in shared worktrees.
