# EX-5 — explorer decorations and sections

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/EX-5`

**Contract ownership:** none (desktop; no protocol).

## Context
User 2026-10-27: a real file explorer ("that's the point of bringing an editor inside"). Spike `docs/spikes/2026-10-27-file-explorer.md` §3 and §4 define this slice; EX-1 (tree polish), EX-2 (peek), EX-6 (daemon fs ops) and EX-7 (explorer file operations: name field, Trash, copy/paste, drag, undo; `panels/files/{edit,ops,drag}.rs`) are merged. Decisions: Trash only; file ops are not MCP tools; Open Editors auto (≥ 2 file tabs).

Implement §4 EX-5 items (1)–(6): diagnostics marks with folder propagation (shared with Problems), agent ◆ marks and a **Changed by agents** section from the E-10 ledger, **Open Editors** section (auto: ≥ 2 file tabs; unsaved dot; close button), section state per root, the Changes rail icon's count badge, a *Working* dot on the scope chip.

## Acceptance
GPUI tests with the fake LSP (`hutan-fake-lsp`) and a scripted agent write; Cua screenshots (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(EX-5)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/ex5/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
