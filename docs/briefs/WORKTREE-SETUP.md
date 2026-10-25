# WORKTREE-SETUP — setup scripts and retention for worktrees

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/WORKTREE-SETUP`

**Contract ownership:** none expected; if `worktree.create` needs a field (e.g. `run_setup: bool`), stop and report.

## Context
Users of vibe-kanban and claude-squad ask for a new worktree to come up ready — dependencies installed, env files copied — and for old worktrees to be cleaned up (vibe #765, claude-squad #181/#260, vibe #1947; `docs/spikes/2026-10-25-user-needs/multi-agent.md`). Hutan creates and removes worktrees (`docs/orchestration.md`, `crates/daemon/src/worktrees/`) but a fresh one has no `node_modules`, `.env`, build cache.

## Items (in commit order)
1. **Project setup config**: `.hutan/worktree.toml` in the repo (committed, shared with the team) and a per-user override in Hutan settings (`[worktrees] setup = …` per project): `copy = [".env", ".env.local"]` (gitignored files copied from the main checkout, globs, bounded size/count), `setup = ["pnpm install"]` (commands run in the new worktree), `teardown = [...]` (before removal). Format documented; unknown keys warned.
2. **Running setup**: after `worktree.create`, the daemon copies files and runs setup in a **visible terminal session** of that worktree (named "Setup", the user sees output, can stop it); the worktree row shows *Setting up…* / *Setup failed* (with *Show output*); agent sessions placed in it wait for setup or start anyway (setting `worktrees.wait_for_setup`, default on). Running repo-provided commands needs trust: the first time a project's `.hutan/worktree.toml` setup runs (or after it changes), the user confirms the exact commands (the LSP trust pattern, `0017_lsp_trust.sql`); agents can never approve it.
3. **Retention**: Settings `worktrees.cleanup` — *Never* (default) / *After merge* / *After N days idle*: merged or idle worktrees with no running session and no uncommitted changes are listed in a *Clean up worktrees…* sheet (never removed silently; teardown runs first); `hutan wt prune [--dry-run]`.
4. CLI: `hutan wt create --no-setup`, `hutan wt setup <wt>` (re-run).
5. Docs: `docs/orchestration.md` § Setup and cleanup.

## Acceptance
Daemon tests on temp repos: copy globs (gitignored only, bounds), setup runs in a session with output, failure state, trust required and remembered by content hash, agent-originated create cannot approve; prune lists only clean merged/idle worktrees and runs teardown. GPUI tests: Setting up…/failed row states, trust sheet, cleanup sheet. CLI tests.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(WORKTREE-SETUP)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/worktreesetup/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
