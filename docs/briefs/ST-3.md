# ST-3 — daemon pages and assistant defaults in Settings (desktop)

Owner: implementer. Crate: `desktop` (+ `client` only if a helper is missing). Depends on ST-1 (protocol 0.28 `daemon.settings`, `assistant.reset`) and ST-2 (layer/provenance UI, Data & logs page) being on master.

## Change

1. **Daemon pages** in Settings, rendered from `daemon.settings.get` with the generic controls (bool, int with range, enum, path with "Choose…" file picker and "Use default", string list, string map, read-only table with "Edit daemon.toml"):
   - **Notifications** — `notify.enabled`, `native`, `kinds` (multi-choice with readable names), `min_turn_secs`, `expire_secs`; a "Send test notification" button if the existing notify path offers one (else omit).
   - **Sessions & restore** — `daemon.restore_sessions`, `workspaces.home_folder`, `terminal.env_inherit`, `projects.auto`.
   - **Assistant** (existing page, extended) — default backend/model **per scope**: Global, and the current workspace/project, each with Inherit (shows what it inherits and from where: workspace → parent → global → daemon.toml → Automatic) / OMP / Claude Code / Codex / Provider, and model (Automatic or a model from the backend's catalog). "Inherit" calls `assistant.reset`. Context permissions: `assistant.context.*` (projects), `assistant.global_context.*` (Home), and per-workspace overrides (`assistant.workspaces.<id>.*`) for the current workspace with Inherit; `context_budget_chars`.
   - **Agent control & security** — `control.agents` (ask / allow / read only, with a plain explanation), `control.mcp`, `control.approval_timeout_secs`.
   - **History indexing** (merge with ST-2's History toggle) — sources `history.hutan/claude/codex/omp`, `range_days`, roots (paths).
   - **Agents & tools** — `harness.omp.bin/args/env/cli`, `harness.claude.*`, `codex.bin/args`, `mcp.*` (read-only table), `gateway.max_context_chars`, `tasks.max_parallel`.
2. Each daemon field shows default vs set, Reset (= `daemon.settings.set` with no value), and **Applies**: live fields just apply; restart fields show "Applies after the daemon restarts". When `restart_pending`, a banner at the top of Settings: "Some changes apply after the daemon restarts — Restart now" (reusing the daemon restart flow and its confirmation; sessions are restored).
3. Errors from the daemon (validation, broken daemon.toml with line/column) show inline at the field / as a page banner with "Open daemon.toml".
4. Agent-originated refusal never applies to the desktop; if the daemon lacks capability `daemon.settings` (older daemon), the pages show "Update the daemon to manage these settings here" and the file path.
5. Labels and help in `settings_labels.rs` for every key (plain language; say what each permission lets the assistant/agents read or do). Settings search covers daemon fields.
6. `docs/design.md` § Settings and `docs/assistant.md` (where defaults are set now).

## Tests (GPUI with a real test daemon)

- Notifications page lists daemon values; toggling `notify.enabled` writes daemon.toml and applies live (no banner).
- A restart field (restore_sessions) shows the banner; Restart now restarts the daemon (test daemon) and the banner goes.
- Assistant: set the workspace to Codex, then Inherit → `assistant.list` shows the inherited backend; model "Automatic" stores no model.
- Permission toggle `assistant.global_context.files` writes the file and the next attachment of a file in Home is refused/allowed accordingly (reuse existing context tests).
- Validation error shows inline; broken daemon.toml shows the banner with Open.
- Older daemon (capability absent) shows the update note.

## Acceptance

`just check` green. Do not edit `core`/`protocol`/`daemon`, `docs/status.md`, `docs/roadmap.md`.
