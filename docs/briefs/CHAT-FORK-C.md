# CHAT-FORK-C — fork into a new worktree at the turn's files

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/CHAT-FORK-C`

**Contract ownership:** **delegated**: additive protocol minor (next free at merge — 0.58 today), extends `chat.fork` with `where: new_worktree`, `restore_files`, `archive_parent`; own commit, goldens.

## Context
ADR 0023 §3, spike `docs/spikes/2026-10-28-best-of/README.md` §2.5 (proof: `git worktree add -b fork/… <base>` then `read-tree --reset -u <checkpoint>` and `reset --mixed <base>` through CK-1's private index) and §5.3 row 5. CHAT-FORK-A (0.56) merged: `chat.fork`, journal copy, per-harness forks. User decisions (2026-10-28): fork destination remembered, always ask on a fallback; session folders user-only; running terminal agents get a pasted `/add-dir` to confirm. ADR 0023 accepted.

Implement §5.3 row 5: `where: new_worktree` creates a worktree (WORKTREE-SETUP rules apply: copy/setup/trust) whose files are the checkpoint of turn N (skipped paths listed like Rewind), the fork runs there: Codex `thread/fork` with the new `cwd`; OMP fork then `/move <path>`; Claude summary path (its transcript can't move); `restore_files` (files to turn N in the *current* folder via the checkpoint restore, with Undo) and `archive_parent`. CLI `hutan chat fork --worktree`.

## Acceptance
Daemon tests on temp repos: the new worktree's tree equals the checkpoint (modified, deleted, untracked), source repo untouched, skipped paths reported, per-harness fork in the new cwd with mocks/fixtures, `restore_files` + Undo, refusals; CLI test; goldens; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(CHAT-FORK-C)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/chatforkc/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
