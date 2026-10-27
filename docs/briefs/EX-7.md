# EX-7 — explorer file operations

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/EX-7`

**Contract ownership:** none (desktop; uses `fs.ops` from EX-6, protocol 0.54).

## Context
User 2026-10-27: "there's no file explorer — that's the point of bringing an editor inside". The workability report's P0 (`docs/spikes/2026-10-27-workability.md` F-01): the Files panel cannot create, rename, delete or move anything. EX-6 (merged) added `fs.create|rename|move|copy|trash|restore` with undo tickets; EX-1 (merged) polished the tree (`panels/files.rs`, `panels/files_tree.rs`). Spike `docs/spikes/2026-10-27-file-explorer.md` §3.4 (behaviour, keys), §3.9 (tests) and §4 EX-7 define this slice; implement §4 EX-7 items (1)–(7) as written there. Decisions: delete = Trash only, always confirmed (the sheet lists names, unsaved tabs, an agent working there); no permanent delete. Include the header **New File / New Folder** buttons and the row menu entries for these operations (the rest of the row menu is EX-4).

## Acceptance
Real-daemon GPUI tests: new file/folder inline row (validation, stem selection on rename), rename with an open **dirty** tab (tab follows, text kept, LSP reopened), Trash with unsaved text (warned), undo of a batch (one step), a conflicting name, an agent write racing a rename, drag-to-move with ⌥ copy, copy/paste incl. Finder file URLs out of the pasteboard; Cua screenshots on a scratch copy of `~/hutan-scratch/e2e6/repos/ripgrep` (never the original clone) dark + light; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(EX-7)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/ex7/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
