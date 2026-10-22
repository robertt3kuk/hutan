# FX-STATUSBAR — a status bar that tells you something: daemon status always, project and branch, work in progress

Agent: implementer (Claude Sonnet 5.5)   Depends on: UX-ATTN-3, UX-BUTTONS merged   Branch: `slice/FX-STATUSBAR`

**Contract ownership:** none.

## Context
The user (2026-10-22): "where is the daemon status — and why is the bottom (status bar) empty?". CL-CHROME made the built-in status bar show only `attention` (`▲ N requests`, nothing while none wait) and `palette_hint` (`docs/design.md` § Workbench model, the CL-CHROME removal table at "status items `workspace`, `sessions`, `approvals`, … `connection` while connected"). With no requests and no file tab focused the bar is empty, and the daemon's state is only in the Daemon menu. The items still exist (registered status items; a preset's `status_bar` can name them; the `connection` item opens the daemon popover — § Daemon controls).

## Items (in commit order)
1. **Built-in status bar** (all built-in presets): left — `connection` **always** (connected: a small `state.ok` dot + `hutand` muted, tooltip with version, pid, uptime, sessions, clients; reconnecting / starting / stopped by you / start failed: the state word in its state colour, e.g. `● Reconnecting…`, `○ Stopped · Start`, `▲ Can't start · Details`; click opens the daemon popover), then `workspace` (active project name · branch, `⎇ main`; click opens the jump palette), then `attention`; right — the editor's items while a file tab has focus (unchanged), a quiet `working` count when agents are running (`◌ 2 working`, click → Agents page; hidden at 0), `palette_hint`. Same quiet styling (muted text, one accent at most, 24 px bar, items separated by space not dividers).
2. **Layouts saved with the CL-CHROME default** get the new default status bar on load (they did not choose it); a status bar the user customised (`layout.presets.<name>.status_bar` in their settings, or edited via *Save Layout as Preset*) is left alone. One-time, logged.
3. **Narrow window**: below 900 px the `workspace` item shows only the branch, below 700 px only the `connection` dot and `attention` stay.
4. Docs: `docs/design.md` (Workbench model status bar sentence, CL-CHROME table row updated: these items are back by user decision 2026-10-22).

## Acceptance
GPUI tests: the built-in bar shows the connection item in each daemon state (connected, reconnecting, stopped by you, start failed) with the right word and click → popover; workspace item follows the active project/branch; working count appears/disappears; a customised preset keeps its own bar; narrow tiers. Cua screenshots dark + light, wide and narrow, connected and stopped → `~/hutan-scratch/fxstatusbar/shots/`. `just check` green; commit per item, messages end `(FX-STATUSBAR)`. Fresh `HUTAN_HOME`; no `hutand` left.

## Out of scope
`docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
