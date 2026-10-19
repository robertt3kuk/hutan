# MW-1 — Mawas: Home page, tool calls with safeguards ("Ask before acting"), naming

Owner: implementer. Crates: `protocol` (authorized only for the grants contract below), `daemon`, `client`, `cli`, `desktop`, docs. Read ADR `docs/adr/0017-mawas-local-meta-harness.md` (decision 4 is the contract for safeguards) and design `docs/spikes/2026-10-18-meta-harness-assistant.md` §3 (Home UX), §7 (modes/approvals), §8 (MCP surface).

The user (2026-10-19): Mawas must make tool calls; we put safeguards in so it is controlled correctly.

## Change

1. **Naming.** User-facing "Assistant" becomes **Mawas** everywhere (nav item, page title, palette "Mawas: Open", "Mawas: New chat", settings page "Mawas", setup step, labels, statuses, CLI help for `hutan assistant …` (command name unchanged), docs). Internal identifiers and settings keys may stay `assistant.*`.
2. **Home page in the center** (replaces OL-2's left-only page): one closable/reopenable pane item. Header: scope (Global / this workspace) · backend·model chip · **mode chip** (Ask before acting ▾ / Advisor) · New chat · **Stop** (kill switch: interrupts the turn and denies its pending requests). Conversation (existing chat view, ~46rem centered). Collapsible right rail **Today**: *Needs you* (approvals — Mawas's first — questions, blocked agents), *Working* (running agents), *Upcoming* ("No schedules yet"); hide links to services that do not exist yet. Empty state actions: Ask a question, Review what needs you. No polling; hidden page does no work.
3. **Mawas can act — mode "Ask before acting" (default for Mawas):**
   - **Harness launch per mode** (`crates/daemon/src/assistant.rs` ~606-622, `harness.rs` 37-46/207-234): Ask-before-acting → OMP without the `--tools read,grep,glob` restriction, Claude Code in its default permission mode (not `plan`), Codex `sandbox: workspace-write`, `approval_policy: on-request`; every harness permission request (ACP `session/request_permission`, Codex approvals) reaches Hutan's existing approval cards. Advisor → today's read-only launch. Mode change applies to the next turn (restart/resume the harness session as needed; tell the user).
   - **Hutan tools through MCP**: Mawas-origin requests (origin attribution, `ControlService::gate`, `crates/daemon/src/control.rs:187-230`): reads allowed; mutating requests raise a `control` approval regardless of `[control] agents` (never auto-allowed by the global policy); destructive/irreversible ones (session.kill, session.close with stop_running, workspace.delete, worktree.remove/merge, task.delete, daemon.settings.set, assistant.reset, provider credential ops, anything widening control/permissions) **always ask**, grants never cover them. Advisor → all mutating requests `forbidden` with a clear reason.
   - **Grants**: approval answers offer Allow once / Allow for this chat / Always allow this tool in this workspace / Deny. Stored by the daemon (migration: `control_grants(id, tool, workspace_id NULL, chat_session_id NULL, created_ms, created_by)`), consulted before asking; Mawas (any agent origin) can never create, widen or answer grants/approvals (user-only methods).
   - **Limits**: at most N mutating calls per turn (setting `assistant.max_actions_per_turn`, default 10, a daemon setting in the ST-1 table); beyond that the next one always asks with "Mawas has made 10 changes this turn".
   - **Audit**: every Mawas mutating call with its decision (approved by whom / grant id / denied) goes to the existing audit log (`hutan::audit`) and is shown in the chat as a tool card with its approval outcome.
4. **Contract (protocol 0.31, capability `control.grants`)**: `control.grants.list {workspace_id?}` → `{grants:[{id, tool, workspace_id?, chat_session_id?, created_ms}]}`, `control.grants.revoke {id}` → `Empty` (both user-only); approval responses gain the scoped options (additive enum values on the approval decision with capability gating — if the existing decision enum is closed, add a new optional field `grant_scope` instead of new variants). Golden fixtures appended at the END of the lists; `docs/protocol.md`, `docs/control.md`.
5. **Settings → Mawas**: mode default, max actions per turn, grants list with Revoke / Revoke all.
6. Docs: `docs/assistant.md` becomes the Mawas document (Home, modes, safeguards, what is sent to the model, limits of origin attribution), `docs/design.md`, `docs/control.md`, README.

## Tests

- Daemon: Mawas-origin mutating request raises an approval even with `[control] agents = "allow"`; Allow for this chat → the next same tool in that chat runs without asking, other chats still ask; Always allow → same workspace no prompt, other workspace asks; destructive request always asks despite a grant; an agent-origin attempt to list/revoke/answer grants is `forbidden`; Advisor → `forbidden` with reason; per-turn limit forces a prompt; audit lines written; grants survive daemon restart; revoke works.
- Harness launch flags per mode (unit tests on the built ACP/Codex configs; fake harness fixtures, no real model).
- GPUI: nav "Mawas" opens Home (one instance); mode chip switches and shows the next-turn note; Today lists a pending Mawas approval and answering it from Today works; Stop interrupts and denies pending; Settings → Mawas grants list and Revoke; hidden Home does no re-render on store changes; `assistant.enabled=false` hides the nav item.
- CLI: `hutan grants ls|revoke` (or under `hutan control grants`).

## Acceptance

`just check` green with `CARGO_BUILD_JOBS=4` / `nextest -j 4` (the user's laptop overheats). One commit on `slice/MW-1`. Do not edit `core`, `docs/status.md`, `docs/roadmap.md`; `protocol` only for item 4.
