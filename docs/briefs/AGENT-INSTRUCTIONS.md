# AGENT-INSTRUCTIONS — one instruction file every agent reads, visible and editable in Hutan

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/AGENT-INSTRUCTIONS`

**Contract ownership:** none expected; if the daemon must read/write the files, use the existing `fs.*` calls.

## Context
The single most-reacted request in the whole survey: Claude Code #6235 (5189 👍) and Cline #5033, Continue #6716, Roo #5966 — users want **one** instruction file all their agents read, in the repo, reviewed and versioned like code (`docs/spikes/2026-10-25-user-needs/assistant-memory.md`, `agents-cli.md`). Hutan drives Codex (reads `AGENTS.md`), Claude Code (`CLAUDE.md`, can `@`-import other files), OMP (its own rules; check what it reads: `AGENTS.md`/`.omp/` — verify from OMP's docs or `omp --help`, never edit `~/.omp`). Hutan shows none of these today.

## Items (in commit order)
1. **Discover**: for the active project, list the instruction files each harness will read (project and user level: `AGENTS.md`, nested `AGENTS.md`, `CLAUDE.md`, `.claude/CLAUDE.md`, `CLAUDE.local.md`, OMP's project rules, `~/.codex/AGENTS.md` and `~/.claude/CLAUDE.md` read-only) with which agent reads which; a research note in the commit with the sources for each harness's rules.
2. **Instructions panel** (registered panel `instructions`, right dock, its own settings namespace): the files as rows grouped *This project* / *Your user files* (read-only, "edit in Finder"), each with the agents that read it as small harness glyphs; click opens it in a file tab (normal editor). Empty state: *Create AGENTS.md*.
3. **One file for all**: *Share one file with every agent…* — a confirm sheet that shows exactly what it writes: create `AGENTS.md` if missing (from the content of the existing `CLAUDE.md` when only that exists), and make `CLAUDE.md` a one-line `@AGENTS.md` import (keeping any extra lines below it, never deleting content; a `.bak` is not needed because the change shows in Changes and git). Never touches user-level files. Undo = the git diff (Changes view) and a 10 s Undo toast.
4. **Mawas** knows them: Mawas's context for a workspace includes the project's `AGENTS.md` summary line in its instructions (read-only, bounded, honours the existing context budget and redaction).
5. Docs: `docs/agents.md` § Instruction files, `docs/design.md` (panel), `docs/assistant.md` (Mawas reads AGENTS.md).

## Acceptance
GPUI tests on temp repos: discovery lists the right files per harness (fixture harness configs), the panel opens a file, the share flow creates `AGENTS.md` and the `CLAUDE.md` import exactly as previewed, never touches user-level files, Undo restores; Mawas context includes the summary line (unit test on the context builder).

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(AGENT-INSTRUCTIONS)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/agentinstructions/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
