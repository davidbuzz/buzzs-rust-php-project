---
name: memories-in-project-repo
description: Store all memories directly in-project-repo under `memories/repo/` (never only in external tool directories)
metadata:
  node_type: memory
  type: feedback
  modified: 2026-11-07T08:58:00.000Z
---

# Store All Memories In-Project-Repo

All memories, handovers, operational rules, and agent guidelines MUST be stored directly in the project repository under `memories/repo/`.

1. **Location**: `memories/repo/` in the repository root (`/home/buzz/phprust/memories/repo/`).
2. **Index**: Kept indexed in `memories/repo/MEMORY.md`.
3. **Requirement**: Never store memories solely in external application or tool-specific paths (e.g. `~/.claude`, `~/.gemini`, etc.). The project repository is the single source of truth for all operational memories.

