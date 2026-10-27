# EX-6 — daemon file operations (create, rename, move, copy, trash, restore)

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/EX-6`

**Contract ownership:** **delegated**: ADR 0012 amendment first, then additive protocol minor **0.54**, capability `fs.ops`, own commit, goldens.

## Context
User 2026-10-27: "there's no file explorer — that's the point of bringing an editor inside". Spike `docs/spikes/2026-10-27-file-explorer.md` §3.5 and §4 EX-6 define this slice (read §3.5, §3.9 and §5 fully). Orchestrator decisions on the spike's §5 questions (2026-10-27, spike picks taken; the user can still overrule): Q1 a fresh layout's right body opens on **Files**; Q2 delete = **system Trash only, always confirmed**; Q3 file operations are **not** MCP tools (CLI `hutan fs …` passes the approval policy); Q4 Open Editors **auto** (≥ 2 file tabs); Q5 Files stays right by default, plus a built-in **Explorer left** preset.

Implement §4 EX-6 items (1)–(7) exactly: ADR 0012 amendment *File operations* (why the daemon, Trash only, confinement reused from `fs.write`, `.git` refusal, permissions per `[control]` with agent-origin approvals, undo tickets); protocol `fs.create|rename|move|copy|trash|restore` with batch partial-failure results; daemon implementation (`clonefile` copy, case-only rename on APFS, cross-root refusal, `fs.changed` origin `self`); trash via a vetted dependency or `objc2-foundation` (`NSFileManager trashItemAtURL`) — record the choice, `cargo deny` clean; client `Connection::fs_*`; CLI `hutan fs mkdir|touch|mv|cp|trash|restore`; docs `protocol.md` § Files, `control.md`.

## Acceptance
Every test listed in §3.9 for `fs.*` (confinement, `..`, link escape, `.git`, exists, case-only rename, cross-root, ticket round-trip, batch partial failure, `fs.changed` origin, agent approval under `ask`/`read_only`) over a real socket; CLI tests; goldens; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(EX-6)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/ex6/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
