# STAGING — stage, unstage and commit selected changes

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/STAGING`

**Contract ownership:** **delegated** — protocol 0.53 as item 1 says.

## Context
User decision 2026-10-26: **yes to partial commits** — this revises decision D2 ("Changes is read-only; no stage, unstage or commit-selected", `docs/orchestration.md` § Staged and unstaged, `docs/design.md` UX-CHANGES-INDEX). Need #8 of `docs/spikes/2026-10-25-user-needs/README.md` (Zed #34813 169, #45295 150, Helix #5132). ADR 0020 stays: the user commits and pushes; agents never push; Finish is user-only.

## Items (in commit order)
1. **Wire** `worktree.stage {checkout, paths? , hunks?: [{path, hunk_id}] , lines?}` / `worktree.unstage {…}` / `worktree.discard {…}` (discard = restore from index/HEAD, destructive) and `worktree.commit {checkout, message, staged_only: true}` (extend the existing commit request; `staged_only` default false keeps today's behaviour) — all **UserOnly** (agent connection → `forbidden`), additive minor, capability `worktree.stage`; hunk ids are stable for a given index/worktree state (digest-checked: a stale hunk id → `conflict`). Own commit; protocol **0.53**.
2. **Daemon**: stage/unstage files and hunks through git plumbing (`git apply --cached` of a generated hunk patch; unstage with `--reverse`), never touching the working tree for stage/unstage; discard takes a checkpoint first (ADR 0019 restore checkpoint) so *Undo* works; refuses while an agent turn runs in that checkout (like restore).
3. **Changes view**: per-file checkbox / *Stage* · *Unstage* on rows, per-hunk *Stage hunk* / *Unstage hunk* / *Discard hunk…* in the diff gutter (keyboard: `s`, `u`, `x` on the focused hunk; Vim-friendly), *Stage all* / *Unstage all*, a commit box (message + *Commit staged* ⌘↩) when anything is staged; *Discard…* asks first and offers Undo. Finish keeps *Commit all*; when something is staged, Finish offers *Commit staged only*.
4. CLI: `hutan wt stage|unstage|discard <checkout> [PATH…] [--hunk ID]`, `hutan wt diff --hunks` prints hunk ids, `hutan finish --staged`.
5. Docs: rewrite D2 lines in `docs/orchestration.md`, `docs/design.md`; record the decision change in ADR 0020 as an amendment (staging is the user's, like Finish).

## Acceptance
Daemon tests on temp repos: stage/unstage file and hunk (incl. new, deleted, renamed, binary refused per hunk, CRLF), stale hunk id → conflict, discard + Undo through the checkpoint, agent-origin → forbidden, commit staged only leaves the rest; GPUI tests for row/hunk actions, keyboard, commit box, discard confirm + Undo; CLI tests; golden fixtures; Cua screenshots dark + light.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(STAGING)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/staging/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
