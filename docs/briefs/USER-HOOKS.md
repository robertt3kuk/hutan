# USER-HOOKS — user hooks on Hutan events

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/USER-HOOKS`

**Contract ownership:** **delegated**: none expected (hooks are daemon config); if an event needs a new wire field, stop and report.

## Context
Codex #2109 (688 👍) asks for user-defined hooks on agent events (notify, run a script on turn end, block a command); `docs/spikes/2026-10-25-user-needs/agents-cli.md`. Hutan's integration hooks only feed Hutan's own state (`docs/integrations.md`); Hutan has rich events (`docs/protocol.md`: session, turn, approval, attention, task, finish).

## Items (in commit order)
1. **Config**: `[[hooks]]` in `hutan.toml` (user-level only; never from a repo): `on = "turn.finished" | "turn.failed" | "approval.requested" | "attention.changed" | "session.exited" | "task.done" | "finish.done"`, optional filters (`harness`, `project`, `workspace`), `run = ["cmd", "args"]` (argv, no shell unless `shell = true`), `timeout` (default 30 s, max 300), `when_focused = false` (skip while the desktop window is focused).
2. **Runner** in the daemon: event JSON (a documented, redacted, bounded payload: ids, names, titles, state, changed-file count — never transcript text, never secrets) on stdin and as `HUTAN_EVENT_*` env vars; concurrency bound (4), per-hook queue bounded, drops counted; never blocks the event loop; output captured (bounded) into a **Hooks** log.
3. **Visibility**: Settings → Automation lists hooks, last run, exit code, duration, *Run test* (with a sample payload), and the log; `hutan hooks list|test|log`.
4. Hooks are user-only config: agent-originated control requests can't add or change them (the `[control]` policy).
5. Docs: `docs/integrations.md` § User hooks (payload schema per event, examples: notify-send to phone via ntfy, play a sound, append to a journal).

## Acceptance
Daemon tests: each event fires matching hooks with the documented payload (snapshot, redaction checked), filters, timeout kill, concurrency/queue bounds and drop counter, `when_focused`; config from a repo is ignored; agent cannot edit. GPUI test: Automation page lists hooks and runs a test. CLI tests.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(USER-HOOKS)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/userhooks/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
