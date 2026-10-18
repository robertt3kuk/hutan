# ST-2 — Settings page correctness and management (desktop, existing protocol)

Owner: implementer. Crates: `desktop`, `settings`. No protocol or daemon changes (ST-1 adds daemon settings; ST-3 renders them later — leave a clean place for daemon pages in the catalog but do not add them).

Source: settings audit 2026-10-18 (`docs/spikes/2026-10-18-settings-audit.md`, read §1, §3, §5). Goal: the user can manage Hutan from Settings; every control shows the truth and can be undone.

## Change

1. **Layers and provenance** (`settings_sheet.rs:234-315,1103-1111`, `crates/settings/src/store.rs`): each field shows where its effective value comes from (Default / You / Workspace `name` / Project `name`, with the file path on hover). When a workspace or project layer overrides the user value, the control edits **that** layer by default and offers a layer switch (You · Workspace · Project, only the layers that exist for the current container). **Reset removes the key from the edited layer** (add `SettingsStore::remove(layer, ns, key)` in `crates/settings`), it does not write the default. A "Reset page" button removes every key of the page from the edited layer, with a confirmation listing them.
2. **Missing namespace:** `sidebar.organize` gets a Settings entry (Projects Sidebar page) with labels.
3. **Validated choices** instead of free text where the set is known: `terminal.theme` (the installed themes), `layout.preset` (existing presets), `panels.sessions.row` (known row part ids), `panels.inspector.tabs` (known tab ids). Numbers show their effective range and clamp at input (`terminal.font_size`, `terminal.line_height`, `resources.interval_ms`): never save a value the app silently ignores.
4. **Maps** (`terminal.env`, `layout.presets`): a small key/value editor for `terminal.env` (add/edit/remove rows, values masked? no — env is not secret by policy, but warn "do not put secrets here; they stay in settings.toml"); `layout.presets` stays read-only with a Rename/Delete per preset and "Save current layout as…" (fix `write_layout_preset` `settings_sheet.rs:516-551` so it also writes `layout.preset`, as its doc says).
5. **History indexing on/off** in Settings → History (a real toggle using the existing `history.set_enabled` with true/false, showing the indexed sources and size from `history.status`), and a Disable action in the History panel next to Enable/Clear/Rebuild (`panels/history.rs:390-417`).
6. **Agent Integrations page**: per agent (Claude Code, Codex, OMP) show hook status (installed / not / outdated) with Install, Repair, Uninstall buttons using the existing integration requests (`integrations.rs`), each with a confirmation naming the file it changes. Keep `auto_install`, and say plainly that turning it off does not uninstall.
7. **Start Sheet page**: list the remembered "Don't ask again" agent/model exemptions (`launcher.rs:1562-1601`) with Revoke per row and Revoke all; turning `confirm_agent_start` on asks whether to also clear them.
8. **Models page**: Clear key asks for confirmation; add the Models and Accounts pages' controls to Settings search (keywords: API key, endpoint, base URL, model, login, logout, account, provider).
9. **Data & logs page** (new, General group): data folder, `settings.toml`, `keymap.toml`, daemon.toml, logs folder — each with Reveal in Finder / Open; sizes of the database and the history index (existing `history.status`/`daemon.status` data only); "Restart daemon…" and "Stop daemon…" buttons reusing the Daemon menu's existing flows (`workbench/daemon.rs:323-396`) with their confirmations.
10. **Keyboard page**: list the effective custom bindings from `keymap.toml` (read-only list: key, action, context) with "Open keymap.toml" and "Reload"; search filters them. (A full binding editor is out of scope.)
11. Labels for everything new in `settings_labels.rs`; `docs/design.md` § Settings updated.

## Tests (GPUI, deterministic; follow existing `settings_gpui`/`*_gpui.rs` patterns)

- A project-layer override: the field shows "Project", editing writes the project file, Reset removes the key from it and the user value shows again.
- Reset removes the key from settings.toml (file no longer contains it), Reset page with confirmation.
- `sidebar.organize` present and changes the sidebar.
- Theme/preset choices are drop-downs; font size clamps at the bound.
- `terminal.env` editor writes the map; new terminal gets the variable (existing terminal-env test style).
- History toggle off → `history.status` disabled; on again → enabled.
- Exemption list shows a remembered exemption; Revoke removes it and the next agent start asks again.
- Clear key confirmation; Settings search "api key" finds the Models page.
- Data page buttons exist and Reveal calls the reveal seam (do not open Finder in tests).

## Acceptance

`just check` green; every item above visible and working in a GPUI test or, for Finder/OS seams, through the existing fakes. Do not edit `core`/`protocol`/`daemon`, `docs/status.md`, `docs/roadmap.md`.
