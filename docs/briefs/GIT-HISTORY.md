# GIT-HISTORY — history and blame in the editor and the Changes view

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/GIT-HISTORY`

**Contract ownership:** **delegated**: additive protocol minor for read-only git queries if needed (`git.log {path?, limit, before?}`, `git.blame {path, rev?}`, `git.show {rev, path?}`), capability `git.history`, bounded; golden fixtures appended.

## Context
Read-only git history is a daily need users of Zed, Helix and Orca ask for (Zed #16827 261, Helix #3035 116, Orca #4521, #9948 — `docs/spikes/2026-10-25-user-needs/editors.md`, `ade.md`). Hutan's Changes compares against the merge base only; the editor has no blame. Git access lives in the daemon (`crates/daemon/src/worktrees/`), clients only render.

## Items (in commit order)
1. **Daemon queries** (bounded, cancellable, the existing git runner hygiene): log for a repo or path, blame for a file (porcelain), show a commit's diff (bounded like `worktree.diff`).
2. **Blame in the editor**: *Toggle Blame* (⌘K, file tab menu): a gutter column with author · relative date · short message per hunk (muted, one line, truncated), hover shows the full commit; click opens the commit. The caret line shows inline blame after the end of line when `editor.inline_blame` is on (off by default).
3. **File history**: *Show File History* (file tab menu, Files panel context menu): a list (date, author, message) in a panel tab; selecting one shows its diff for that file (the Changes diff renderer); *Open at this revision* opens a read-only tab.
4. **Repo history** in the Changes view: a *History* segment beside Committed/All/Staged/Unstaged listing recent commits of the checkout with their diffs.
5. CLI `hutan git log|blame|show` thin wrappers over the queries (for agents too); MCP read-only `git_log`, `git_blame`.
6. Docs: `docs/editor.md`, `docs/design.md`, `docs/protocol.md`.

## Acceptance
Daemon tests on temp repos (log paging, blame lines map, renamed file history, bounds); GPUI tests: blame gutter toggles and matches lines after edits (dirty buffer shows *uncommitted* for new lines), file history opens a diff, Changes History segment; CLI tests.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(GIT-HISTORY)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/githistory/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
