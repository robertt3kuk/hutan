# 0023. Fork a chat from any turn; extra folders per session

- Status: accepted (2026-10-28; the user asked for "fork the session from any point" and "open another directory and combine it into the session"; the spike's recommended defaults were taken)
- Date: 2026-10-28
- Amends: ADR 0019 § Decision 9 (conversation rewind "is not promised"; CK-7) — a fork is the non-destructive conversation rewind
- Evidence: `docs/spikes/2026-10-28-best-of/README.md` §2 (fork: per-harness support, checkpoint worktree proof), §3 (folders: per-harness support), §5.3 (slices)

## Context
Hutan has no conversation fork: only `chat.retry`, queueing and files-only checkpoint restores (ADR 0019). A session has exactly one folder (`cwd`), while workspaces already hold several projects. The harnesses differ: Codex forks at any turn and into another folder (`thread/fork {lastTurnId, cwd}`); OMP forks at the head over ACP (`session/fork`), refuses a different `cwd` but accepts `/move` afterwards; Claude Code's ACP adapter forks at a message only through a non-standard `_meta` field; direct chats are Hutan's own journal. Extra folders: Claude `--add-dir` / ACP `additionalDirectories` (resent on load), Codex `--add-dir` / per-turn `writableRoots`, OMP `--add-dir` or the local `/add-dir` command (it ignores `session/new.additionalDirectories` in 18.12.3).

## Decision
1. **Fork = a new chat that starts with the parent's history up to turn N.** The parent is never changed. The journal prefix is **copied** (not linked) into the new session, so either can be deleted independently. `SessionInfo.forked_from {session, turn}` records the lineage.
2. **Cut point honesty.** Each harness reports what it can do: *exact* (cut at turn N: direct, Codex, Claude when its adapter advertises the extension), *head* (OMP over ACP), or *summary* (a new chat seeded with a generated summary of turns ≤ N). Hutan never silently substitutes: when the exact cut is impossible the user chooses *latest turn* or *summary*.
3. **Where the fork works**: *this folder* (default, the choice is remembered) or **a new worktree whose files are the checkpoint of turn N** (ADR 0019 refs; `git worktree add` + `read-tree --reset -u <checkpoint>` through the private index; skipped paths are listed like Rewind does). Codex gets the new `cwd`; OMP forks then `/move`s; Claude takes the summary path.
4. **Session folders are user-only.** `extra_dirs` on chat/terminal creation and `chat.add_dir` / `chat.remove_dir` are `UserOnly` (an agent-originated connection gets `forbidden`, like `checkpoint.restore`), listed in `ChatInfo.dirs` and shown in the chat header. Hutan applies them per harness (ACP `additionalDirectories` resent on every load/resume; Codex `writableRoots`; OMP `/add-dir`; terminals' launch arguments); for a **running terminal agent** Hutan only *pastes* `/add-dir <path>` for the user to confirm. The daemon's file-service roots and Contain's write roots include them (`applied: restart` when a restart is needed, never pretended).
5. **Protocol**: additive minors with capabilities `chat.fork` and `session.dirs` (ADR 0002), numbers taken at merge.

## Consequences
- Turn rows of the Context panel and the turn footer gain *Fork…*; terminal agents get *Fork session* through their own CLIs (`omp --fork`, `claude --resume --fork-session`, `codex fork`, head only).
- Long journals make forks heavier: the copy is one bounded SQLite transaction, measured in the first slice.
- Claude's exact fork depends on an adapter extension that may change: feature-detected per adapter version, fixtures per version, summary fallback.
- Revisit when OMP exposes `fork {entryId}` over ACP (then OMP becomes *exact*), or ACP standardises mid-session folder changes.
