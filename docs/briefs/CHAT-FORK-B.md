# CHAT-FORK-B — fork buttons in the desktop

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/CHAT-FORK-B`

**Contract ownership:** none (desktop; uses 0.56 `chat.fork` and 0.58 `chat.fork.worktree`).

## Context
ADR 0023, spike `docs/spikes/2026-10-28-best-of/README.md` §2 and §5.3 row 4 (mocks). Merged: CHAT-FORK-A (`chat.fork`, `ChatInfo.fork {support, note}`, `SessionInfo.forked_from`), CHAT-FORK-C (`where: new_worktree`, `restore_files`, `archive_parent`, skipped paths). User decisions: fork destination remembered; always ask on a fallback (exact impossible → *latest turn* or *summary*, never silently).

Implement §5.3 row 4 plus the worktree parts of row 5's UI: *Fork…* in each turn's footer and the message hover action bar, the fork popover (cut point with the harness's honest support, *Where: This folder / New worktree* remembered, branch name, *Restore files to this turn* when this folder, *Archive the original*, skipped paths listed), lineage — the fork's row under its parent in the sidebar and a header chip *Forked from <parent> · turn N* that opens the parent, ⌘K *Chat: Fork from Here*; the sidebar card hover ⑂ (SIDEBAR-CARDS reserves it) forks at the latest turn; terminal agents: *Fork session* through their own CLIs (TERM-FORK: `omp --fork`, `claude --resume <id> --fork-session`, `codex fork`) in a new terminal tab when the agent's session id is known.

## Acceptance
GPUI tests over a real daemon with fixture harnesses (footer button, popover per support level and fallback choice, worktree path, lineage row and chip, ⌘K, terminal fork command typed into a new tab); Cua screenshots (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(CHAT-FORK-B)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/chatforkb/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
