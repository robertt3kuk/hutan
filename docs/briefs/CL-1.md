# CL-1 — close sessions from the sidebar, reopen them from History

Owner: implementer. Crates: `protocol` (authorized for exactly the contract below), `storage`, `daemon`, `client`, `cli`, `desktop`, docs.

The user (2026-10-18): close buttons for sessions in the sidebar too; closed sessions can be opened again from History when needed; before closing, if something is running, ask and allow closing it.

Today there is no way to end a session *and remove it*: `session.stop` leaves a stopped row forever, the sidebar keeps every session, and a terminal tab's × only detaches the tab.

## Contract (protocol 0.30, capability `session.close`)

- `session.close {session_id, stop_running?: bool}` → `Empty`.
  - Session not live (exited/stopped/failed/restored-ended) → closed at once.
  - Live and **idle** (terminal at its prompt: no foreground command per the existing OSC 133 block tracking in `registry.rs` ~581/2163, no agent reported `working`/`blocked`; chat: no turn active, no pending approval/question) → stopped (as `session.stop`: SIGHUP, SIGKILL after 3 s / chat agent shut down) and closed.
  - Live and **busy** without `stop_running: true` → error `invalid_state` with `data: {"busy": {"command": "<foreground command line>"?, "agent": "<name>"?, "state": "working|blocked|turn|approval"}}` so the client can ask; with `stop_running: true` → stopped and closed.
  - Closing removes the session from `session.list`, the restore set and the attention/notification state; it **keeps** its journal, blocks and history-index records. Event `session.closed {session_id}` (new event; clients lacking the capability simply see it vanish on the next `session.list`).
- `session.reopen {session_id}` → `SessionResult`: brings a closed session back the way restart recovery does (RR-1 code path): a terminal respawns its saved command in its last known cwd (same id, title, workspace; screen lost, history kept); a chat/harness session resumes its conversation (`session/load` / Codex thread resume; fallback new conversation with the recovery notice); an assistant comes back asleep (AS-1). `not_found` for an unknown id, `invalid_state` if it is not closed.
- Storage: migration adds `sessions.closed_at_ms INTEGER NULL` (closed rows are kept, not deleted; `session.list` and restore skip them). Optional retention later — not now.
- `history.sessions` rows for Hutan sessions: add `closed: bool` (default false, `skip_serializing_if` false) so clients can show **Reopen** instead of **Open**. (Additive field in `HistorySessionRow`.)
- Golden fixtures (`just update-golden`; new fixtures appended at the END of the request/response/event lists to avoid id churn), `docs/protocol.md` rows + "Latest: 0.30" note.

## Daemon / CLI

- Implement the above in `registry.rs` (busy detection reuses existing foreground/agent/turn state), storage migration, router arms (classify `session.close` / `session.reopen` as Mutate in the control gate — agents may close sessions only under the `[control]` policy like `session.kill`).
- MCP: add `session_close` (with `stop_running`) and `session_reopen` tools in `crates/cli/src/mcp/table.rs` (mutating, approval per policy).
- CLI: `hutan close <session> [--force]` (prints what is running and refuses without `--force`), `hutan reopen <session>`.

## Desktop

- **Sidebar**: every session row (Projects tree, unassigned/Home, Needs-you section, Agents board card menu) gets a hover **×** button (also in the row's ⋯/right-click menu as "Close session…", and ⌘⌫ on a selected row). Closing:
  - not busy → closes immediately (no dialog), toast "Closed <title> · Undo" for 5 s (Undo = `session.reopen`);
  - busy (daemon answered `invalid_state` with `busy`) → confirmation: title "Close <title>?", text names what runs ("`npm run dev` is still running" / "Claude Code is working" / "a reply is being written"), buttons **Stop and close** (sends `stop_running: true`) / Cancel. Escape cancels; focus returns per R4-1.
- Closing removes its tabs (reuse `close_session_tabs`) and selects a sensible neighbour.
- Terminal **tab ×** keeps today's meaning (detach the tab only) — add to the tab context menu "Close session…" which runs the same flow. Setting `terminal.tab_close` = `detach` (default) | `close_session` for users who want Orca's behaviour (labels in settings_labels.rs).
- **History panel** (OL-3 files `panels/history*.rs`): Hutan rows of closed sessions show **Reopen** (primary) instead of Open; Reopen calls `session.reopen` and focuses the session in its workspace. Older daemons without the capability: no × buttons, History unchanged.
- Labels, `docs/design.md` (sessions: close/reopen), `docs/history-index.md` (closed flag).

## Tests

- Daemon integration: close idle terminal (gone from list, journal kept, `session.closed` event); busy terminal (`sh -c 'sleep 600'` as foreground block via OSC 133 fixture, or an agent reporting working) → `invalid_state` with busy data, then `stop_running` closes; reopen terminal (same id, cwd, running again) and chat (fake ACP fixture sees `session/load`); closed session not restored after daemon restart; reopen after daemon restart works; MCP tool classification.
- CLI: close refuses busy without `--force`.
- GPUI: hover × closes an idle session without dialog and Undo brings it back; busy session shows the dialog naming the command, Stop and close works, Cancel keeps it; History shows Reopen for a closed Hutan session and reopening focuses it; tab context menu "Close session…".

## Acceptance

`just check` green; one commit on `slice/CL-1`. Do not edit `docs/status.md`/`docs/roadmap.md`.
