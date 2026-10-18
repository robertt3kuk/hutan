# AS-1 — idle harness assistants give their memory back

Owner: implementer. Crate: `daemon` (+ its tests, docs). No protocol change.

## Problem

User test 3 (`docs/spikes/2026-10-18-user-test-3.md`, § Performance): each open harness assistant keeps its ACP process alive — an OMP backend is **~460 MB RSS** — even when nobody has used it for hours. Opening the Assistant panel in two scopes left two of them running. The assistant is a helper for occasional questions; it must not cost half a gigabyte per scope while idle.

## Change

1. **Transparent sleep.** A harness-backed assistant chat (`ChatInfo.assistant`, backend omp/claude/codex) whose turn has been idle for `assistant.idle_stop_secs` (new `daemon.toml [assistant]` key, default **600**, `0` = never; minimum 60) with no pending approval or question has its agent process stopped by the daemon. The session stays what it is for clients: state `idle`, same id, same history/journal, same model; it is not shown as stopped or failed, and no recovery chip appears. Log at info ("assistant … asleep, process stopped after N s idle").
2. **Wake on use.** `chat.send` (and anything else that needs the agent: model switch, `chat.models` for that session if it asks the live process) to a sleeping assistant starts the agent again, resuming the conversation the way restart recovery does (RR-1: ACP `session/load` / Codex thread resume), then delivers the message. The client sees the turn start a little later; no extra events beyond what a normal start emits. A wake that cannot resume the conversation (the harness refuses `session/load`) starts a fresh agent session and posts the same notice text restart recovery uses for that case. A wake that fails entirely fails the send with the harness's error, and the session stays usable for a retry.
3. **`assistant.open`** of a sleeping default returns it without waking it (the panel shows the history immediately; the process starts on the first send). Stopped/failed sessions keep today's behaviour (`restart_session`).
4. The idle clock restarts at every turn end, approval answer and question answer. Daemon shutdown with sleeping assistants: nothing to stop; restore after a daemon restart brings them back asleep (do not spawn processes for assistants at daemon start — today's restore respawns chats; for assistants respawn lazily on first use instead).
5. Add `assistant.idle_stop_secs` to `config.rs` and to the daemon settings table (`crates/daemon/src/daemon_settings.rs`, kind Int min 0, applies Live — the sleeper reads the value each check) so the every-field test passes. Desktop labels come later; do not edit desktop.
6. Docs: `docs/assistant.md` (§ Backends / a new § Idle assistants), `daemon.toml` example in `config.rs` docs.

## Tests (deterministic; fake ACP agent fixture as in existing assistant/harness tests; short timeouts via config)

- An idle assistant's agent process exits after `idle_stop_secs` (use a test hook to set seconds, e.g. 1 with the minimum relaxed in tests) while `session.list` still shows it `idle` with its history.
- A send to it wakes the agent, resumes (the fixture sees `session/load` with the old session id) and the turn streams; the history is continuous.
- A pending approval keeps it awake.
- `assistant.open` of a sleeping assistant does not start a process.
- A daemon restart restores a sleeping assistant without starting its process; the first send starts it.
- Agent (non-assistant) harness chats are untouched by the sleeper.

## Acceptance

`just check` green. Do not edit `core`/`protocol`/`desktop`, `docs/status.md`, `docs/roadmap.md`.
