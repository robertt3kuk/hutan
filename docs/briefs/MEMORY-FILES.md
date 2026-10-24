# MEMORY-FILES — Mawas memory as Markdown files: browsable, exportable, versioned in git

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/MEMORY-FILES`

**Contract ownership:** none expected (memory stays daemon-owned; files are a synced mirror written by the daemon); if a wire field is needed, stop and report.

## Context
Users want assistant memory as files they can read, edit, review and version (Claude Code #25947, #25739 export, #14467 team sharing, #90450 path-scoped rules — `docs/spikes/2026-10-25-user-needs/assistant-memory.md`). The user asked (2026-10-25) how Mawas's versions are controlled. Hutan's memory is curated SQLite entries with revisions and approvals (MW-2, `docs/assistant.md` § Memory, ADR 0017); MAWAS-FILES made the Home folder browsable and optionally a git repo.

## Items (in commit order)
1. **Mirror**: the daemon writes the memory as Markdown under the scope's folder — Global → `~/Hutan/.mawas/memory/` (Home folder of the data dir), Workspace → `<project root>/.hutan/memory/` only when the user turns on *Keep memory in the project* (off by default; it is a file in their repo) — one file per category (`profile.md`, `facts.md`, `notes.md`), each entry a section with a stable id comment (`<!-- mawas:id=… rev=… -->`), regenerated atomically after every change; a README line on top says the files are written by Hutan.
2. **Edits from files**: an edit to a mirror file (watched via the daemon's file watcher) becomes a **proposal** in Mawas's memory (the same approval flow; never applied silently), matched by id; a new section without id → new-entry proposal; a deleted section → archive proposal. Invalid edits are reported in the Memory panel.
3. **Export / import**: `hutan memory export [--scope] [--format md|json] PATH`, `hutan memory import PATH` (import creates proposals); the Memory panel's ⋯ has *Export…* / *Import…* / *Show files*.
4. **Versioning**: when the folder is a git repo (MAWAS-FILES *Track changes*), the mirror's changes show in Changes and can be committed like any file; Mawas's turns get checkpoints there.
5. Docs: `docs/assistant.md` § Memory (files, editing, export, versioning), ADR 0017 amendment note in `docs/adr/0017-*.md` (files are a mirror; SQLite stays the source of truth).

## Acceptance
Daemon tests: entries → files byte-stable, regenerated atomically; file edits → proposals (edit, new, delete), never applied without approval; secrets refused as today; export/import round-trip. GPUI test: Memory ⋯ actions, proposal from a file edit appears. CLI tests.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(MEMORY-FILES)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/memoryfiles/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
