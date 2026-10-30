# MAWAS-GLOBAL — Mawas is global first; a workspace is a Focus

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/MAWAS-GLOBAL`

**Contract ownership:** **delegated**: additive protocol minor (next free at merge — 0.61 today), `assistant.focus` (UserOnly) + `ChatInfo.focus`, capability `assistant.focus`; own commit, goldens.

## Context
User 2026-10-30: "Mawas should be global as an assistant across all projects… global, connecting to the agents". ADR 0025 (accepted) and `docs/spikes/2026-10-30-mawas-supervisor.md` §1 (Global first; no storage migration; old workspace chats and memory stay reachable) and §4.1 row 3 define this slice; D1 taken: Home opens Global for everyone, the old behaviour behind `assistant.home_scope = "follow_workspace"`. MAWAS-LAYOUT (chat width, Today toggle) is merged.

## Items (in commit order)
1. Protocol `assistant.focus` (UserOnly) + `ChatInfo.focus`, capability `assistant.focus` (0.61).
2. Daemon: a Global chat can carry a Focus (a workspace id or none); the Focus is a filter and the source of memory notes and project instructions delivered to Mawas (deltas when it changes, per spike §1); existing workspace-scoped chats/memory untouched.
3. Desktop: `assistant.home_scope = global | follow_workspace` (default `global`); Mawas Home opens the Global chat; a **Focus chip** in the header (*All workspaces* / a workspace) with a picker; a **Chats ▾** menu listing Global chats and the old per-workspace chats (opening one works as today); the sidebar Mawas row and ⌘K follow.
4. Docs `docs/assistant.md`, `docs/design.md`, `docs/protocol.md`.

## Acceptance
Daemon tests (focus set/cleared, memory/instructions deltas, UserOnly), GPUI tests (Home opens Global, Focus chip and picker, Chats menu reaches an old workspace chat, `follow_workspace` keeps the old behaviour); Cua screenshots (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(MAWAS-GLOBAL)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/mawasglobal/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
