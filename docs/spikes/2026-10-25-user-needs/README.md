# What users of similar tools ask for — synthesis and plan (2026-10-25)

Six reports in this folder (`ade.md`, `agents-cli.md`, `terminals.md`, `editors.md`, `multi-agent.md`, `assistant-memory.md`), GitHub issues ranked by reactions over Orca, vibe-kanban, crystal, claude-squad, Codex, Claude Code, OpenCode, Crush, Warp, Wave, Ghostty, cmux, Zed, Helix, Lapce, Cursor's forum, Superset, agent-orchestrator, Cline, Roo, Continue. Reaction counts are directional (several GraphQL searches failed and fell back to REST; Hutan status was read from docs, and the "missing" rows below were re-checked in code by the orchestrator where noted).

## Ranked gaps (missing or partial in Hutan)

| # | Need | Strongest evidence | Hutan today | Plan |
|---|---|---|---|---|
| 1 | One instruction file every agent reads, editable and versioned in the repo | Claude Code #6235 (5189 👍), Cline #5033, Continue #6716, Roo #5966 | each harness reads its own file; Hutan shows none of them | `AGENT-INSTRUCTIONS` |
| 2 | Find in terminal output / scrollback | Ghostty #189 (1221), GhD #3708, #2504 | **missing** (no find in `terminal_view`, checked in code) | `TERM-FIND` |
| 3 | Several accounts per harness | Claude #18435 (1183), #36151 (1065) | one login per harness | `ACCOUNTS-MULTI` (after a design note: ADR 0015 — Hutan never touches tokens; per-account harness homes) |
| 4 | Remote / mobile control of agents; remote/container execution | Zed #11473 (930), Codex #10450 (878), #9224 (544), Warp #6831/#6744, Superset #1513, Orca #3099 | daemon/client split exists; remote transport is M9, blocked on an ADR | `REMOTE-SPIKE` (design, then slices) |
| 5 | Quota / spend visibility (5-hour / weekly limits, cost) | Claude #16157 (726), #38335 (544), Codex #28879 (561), Cursor #126446 | per-session context and cost; no account-level limits | `USAGE-LIMITS` |
| 6 | Event hooks for user automation | Codex #2109 (688) | hooks feed Hutan's own state only | `USER-HOOKS` |
| 7 | Git history and blame | Zed #16827 (261), Helix #3035, Orca #4521, #9948 | diff vs merge base only | `GIT-HISTORY` |
| 8 | Staging granularity, partial commits, conflict resolution | Zed #34813 (169), #45295 (150), Helix #5132 | read-only by decision D2; Finish commits all | **ask the user** (revisit D2) |
| 9 | Memory as files: export, git-versioned, shareable, path-scoped | Claude #25947, #25739, #14467, #90450 | SQLite memory, no export | `MEMORY-FILES` (with MAWAS-FILES: the Home folder can be a repo) |
| 10 | Quick terminal on a global hotkey | Ghostty #2384 (107), cmux #2758 | only Mawas has a global shortcut | `QUICK-TERMINAL` |
| 11 | Paste images into a terminal (for agent CLIs) | cmux #3435, #3639, GhD #10517 | refused (design.md) | `TERM-IMAGE-PASTE` (save to a temp file, paste its path) |
| 12 | Worktree bootstrap (setup script) and retention | vibe #765, claude-squad #181/#260, vibe #1947 | create/remove only | `WORKTREE-SETUP` |
| 13 | Cross-repo changes view | Orca #1099 (78) | Changes per checkout | `CHANGES-MULTI` |
| 14 | Sandboxed / container agent runs | Orca #13665, Superset #2419, #3957 | approvals only | `SANDBOX-SPIKE` |
| 15 | PR / GitLab review request from Finish | vibe #1697, Superset #2714 | push only, by ADR 0020 | **ask the user** (revisit ADR 0020) |
| 16 | Agent skills and user-defined modes visible/configurable | Zed #49057 (192), Cursor #143218 | modes shown, not configured (D4) | later |
| 17 | Voice input | Codex #3000 (300) | macOS dictation works in text fields [INFERENCE] | check, else later |
| — | Linux / Windows | cmux #330 (221) | macOS-first by decision | recorded, not planned |

## Order (Sonnet implementers, three at a time; Opus coordinates)
Wave 1 (now): `TERM-FIND`, `AGENT-INSTRUCTIONS`, `USAGE-LIMITS` (with `MAWAS-FILES` already running).
Wave 2: `MEMORY-FILES`, `GIT-HISTORY`, `QUICK-TERMINAL`, `TERM-IMAGE-PASTE`.
Wave 3: `WORKTREE-SETUP`, `USER-HOOKS`, `CHANGES-MULTI`, `ACCOUNTS-MULTI`.
Design first: `REMOTE-SPIKE`, `SANDBOX-SPIKE`. User decisions: staging (D2), PR creation (ADR 0020).
