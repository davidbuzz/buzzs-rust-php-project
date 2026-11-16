---
name: user-profile
description: "Who David Buzz is on this project and how they like to work with me (brief reports, runnable commands, many Opus subagents)"
metadata:
  node_type: memory
  type: user
  originSessionId: ebaf0a84-fb28-4fd1-bc69-f1147e28a4a5
  modified: 2026-11-06T03:15:16.892Z
---

David Buzz (davidbuzz@gmail.com) runs buzzs-rust-php-project (repo dir `/home/buzz/phprust`) as a hands-off director: short prompts ("report", "proceed with goal", "give me a command i can run") and expects me to coordinate agents autonomously.

- Wants code-writing subagents on Opus ("Opus 5") by default; up to 20 concurrent subagents (confirmed 6 Nov 2026). Agreed 6 Nov 2026 ("yes, do that"): Fable 5.1 for the riskiest performance packages' helpers and fix-up (valuesize, allocator, arrays, dispatch) and for build-capable fix-up agents; running Opus agents are not restarted. The main session itself moved to Fable 5.1 the same day; at ~12:55 the Fable usage credits ran out (every running agent died with HTTP 429), and the user switched back to Opus 5.5 (`/model opus`; commit attribution `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`). Agents can't be resumed with SendMessage after that: a resumed agent keeps its original model and fails again, so relaunch fresh agents (model inherited or "opus") pointed at the dead agent's worktree and status file.
- Wants README.md and FEATURE_GAP.md kept current with *measured* pass rates, plus runnable commands.
- Cares about machine stability (the box OOM-crashed once; see [[phprust-resource-rules]]); stops services on request and runs/permits `sudo apt install` of dev packages. Passwordless `sudo -n` works.
- Scope decisions are theirs; see [[phprust-scope-decisions]].
- HIGHLY sensitive to wasted time: expects tight timeouts, fail-fast execution, zero pointless waiting on dead/stalled processes, and rigorous, realistic timing estimates.
- Agents ALWAYS work in their own worktree and branch, never in the main checkout (CLAUDE.md "Parallel work"). When they say "fix it in master" they mean "land it in master", not "edit the shared checkout": on 6 Nov 2026 I started twelve fix-up agents directly in /home/buzz/phprust and they asked "these are supposed to be in worktrees?"; I moved them. Likewise "merge all, then ONE build": no per-branch or verifier builds (see [[phprust-one-build-rule]]).
