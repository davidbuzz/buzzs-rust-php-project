---
name: phprust-worktrees
description: Exclusively use git worktrees for subagents (each worker gets its own worktree, branch, and bounded package; never share the main working directory)
metadata:
  node_type: memory
  type: feedback
  modified: 2026-11-07T08:55:00.000Z
---

# Exclusively Use Git Worktrees for Subagents

Subagents must ALWAYS use private git worktrees. We have been using exclusively git worktrees for all subagents:

1. **Every worker gets its own git worktree and branch**:
   - Created from an agreed integration commit before launching the worker.
   - Bounded work package per worker (workers edit only their assigned crate/package).
   - Never let subagents work directly in `/home/buzz/phprust` (the shared tree).
2. **Edit-only workers**:
   - Subagents do edits, analysis, and reviews in their worktrees.
   - Per [[phprust-one-build-rule]], subagents do not run redundant per-branch builds. Merge all worker branches into the integration branch first, then run ONE consolidated build/test/PHPT validation pass.
3. **Lifecycle & Cleanliness**:
   - Remove worker worktrees only after their changes are cleanly merged or explicitly retained, and only when clean. Never force-remove uncommitted work.

