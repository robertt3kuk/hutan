# EX-4 — row menu, multi-select, terminal and chat hooks

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/EX-4`

**Contract ownership:** none (desktop; no protocol).

## Context
User 2026-10-27: a real file explorer ("that's the point of bringing an editor inside"). Spike `docs/spikes/2026-10-27-file-explorer.md` §3 and §4 define this slice; EX-1 (tree polish), EX-2 (peek), EX-6 (daemon fs ops) and EX-7 (explorer file operations: name field, Trash, copy/paste, drag, undo; `panels/files/{edit,ops,drag}.rs`) are merged. Decisions: Trash only; file ops are not MCP tools; Open Editors auto (≥ 2 file tabs).

Implement §4 EX-4 items (1)–(4): multi-select (⇧/⌘ click, ⇧↑↓; EX-7's operations already take path lists — widen `acted_paths()`), the full row menu grouped with separators and **key hints** (Open, Open to the Side, Reveal in Finder, Copy Path, Copy Relative Path, Open in Terminal, Attach to Chat, Find in Folder, Show File History, Collapse Folder; plus EX-7's New/Rename/Duplicate/Cut/Copy/Paste/Move to Trash), Select for Compare / Compare with Selected / Compare with HEAD (two-file diff tab on the history diff rows), drag a row onto the chat composer or a terminal → relative path.

## Acceptance
GPUI tests with real key/drag events for each; Cua screenshots (dark + light) on a scratch copy of ripgrep; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(EX-4)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/ex4/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
