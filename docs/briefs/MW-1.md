# MW-1 — Mawas Home page, Advisor enforcement, naming

Owner: implementer. Crates: `daemon` (Advisor gate), `desktop` (Home page, naming), docs. Protocol: **no new messages** unless strictly needed for the Advisor refusal reason (report to the integrator first). Read ADR `docs/adr/0017-mawas-local-meta-harness.md` and design `docs/spikes/2026-10-18-meta-harness-assistant.md` §3 (Home UX), §7 (Advisor/Operator).

## Change

1. **Naming.** User-facing "Assistant" becomes **Mawas** everywhere (nav item, page title, palette commands "Mawas: Open", "Mawas: New chat", settings page "Mawas", setup step, labels, status texts, CLI help text `hutan assistant …` keeps its command name but describes Mawas; docs). Internal identifiers may stay `assistant`. Settings keys stay (`assistant.enabled` …) — labels change only.
2. **Home page in the center.** The left nav item opens a **center page** (a pane item like Overview/Changes, one instance, reopenable, closable) instead of the left-sidebar page (replace OL-2's left-only rule; keep `assistant.enabled = false` default). Layout per the design §3.2 wireframe, using the existing assistant chat model/view:
   - header row: scope (Global / this workspace) · backend·model chip (Automatic …) · mode chip **Advisor** (disabled "Operator — coming soon" entry with a tooltip; no fake control) · New chat;
   - conversation (existing chat view, max content width ~46rem centered, Orca-like calm canvas);
   - collapsible right rail **Today**: *Needs you* (approvals/questions/blocked from the sessions store), *Working* (agents running, from the store / Agents board data), *Upcoming* (empty state "No schedules yet"), and links Memory · Skills · Schedules · Traces that open a placeholder **only if** they say clearly "Coming in a later release" — better: hide links whose service does not exist yet (preferred);
   - empty state with three real actions: Ask a question (focus composer), Review what needs you (focus Today), Learn from a session (disabled until MW-3; omit if not available).
   - Today rail and page do no polling: they render from store state; hidden page does no work.
3. **Advisor enforcement in the daemon.** Calls made by a Mawas (assistant) session's agent to **mutating** Hutan control/MCP requests are refused with `forbidden` and a clear reason ("Mawas is in Advisor mode: it can read and suggest, not change Hutan") regardless of `[control] agents`, using the existing origin attribution (`ClientInfo.origin`, `ControlService::gate`, `crates/daemon/src/control.rs:187-230`). Read requests still allowed. Document the limit honestly: origin attribution is cooperative (ADR 0016), stronger isolation comes with MW-10.
4. Keep the ⌥-hotkey/`OpenAssistant` action working (opens Home); `send to assistant` flows target the Home page.
5. Docs: `docs/assistant.md` → Mawas (Home page, Advisor, what is sent to the model), `docs/design.md` Home section, README mention.

## Tests

- GPUI: nav "Mawas" opens the Home page in the center (one instance; reopen focuses it); header chips present; Today shows a pending approval and a working agent from a test daemon and updates on events; empty state actions; page hidden → no re-render on store changes (render counter as in perf_gpui); narrow window (900 px) layout usable; `assistant.enabled=false` hides the nav item.
- Daemon: an assistant-origin connection calling a mutating request (e.g. `session.create`, `workspace.rename`) gets `forbidden` with the Advisor reason even with `[control] agents = "allow"`; read requests succeed; non-assistant agents unaffected.
- Wording tests updated (labels say Mawas).

## Acceptance

`just check` green with `CARGO_BUILD_JOBS=4` / `nextest -j 4` (the user's laptop overheats). One commit on `slice/MW-1`. Do not edit `core`/`protocol`, `docs/status.md`, `docs/roadmap.md`.
