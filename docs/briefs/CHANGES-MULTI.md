# CHANGES-MULTI — one Changes view across a workspace's repositories

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/CHANGES-MULTI`

**Contract ownership:** none expected.

## Context
Orca #1099: a workspace with several repos wants one Changes view (`docs/spikes/2026-10-25-user-needs/ade.md`). Hutan workspaces can hold several projects (`docs/orchestration.md`); Changes shows one checkout at a time.

## Items (in commit order)
1. **All checkouts** scope in the Changes panel when the workspace holds 2+ projects or the active project has worktrees with changes: files grouped by checkout (repo name · branch header, counts, collapse), the same diff view on select; the scope chip switches *This checkout* / *All in workspace*.
2. **Badges**: the sidebar's project rows show a changed-file count when not the active one (cheap: from the existing watcher-driven status, no extra git runs per frame).
3. **Finish per checkout** from the group header (opens the existing Finish sheet for that checkout).
4. Settings `changes.default_scope`.
5. Docs: `docs/orchestration.md`, `docs/design.md`.

## Acceptance
GPUI tests with two temp repos in one workspace (and a worktree): grouping, counts, select diff, Finish from header, badges; perf: no git process per render (assert via the daemon test hooks).

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(CHANGES-MULTI)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/changesmulti/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
