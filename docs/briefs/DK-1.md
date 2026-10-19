# DK-1 — Recoverable docks: compact left rail, persistent right rail, collapse on demand

Owner: implementer. Crate: `desktop` only (no protocol/IPC change). Design: `docs/spikes/2026-10-20-chrome-v3.md` §4 (4.1–4.4) and `docs/spikes/2026-10-20-chrome-v3-docks.svg` — that is the contract; this brief narrows it.

The user (2026-10-20): "the left and right sections must collapse on demand — we forgot about that entirely."

## Change

1. **Left sidebar states**: Expanded (current, ~280 pt) / Compact (44 pt icon rail: nav icons Search, Tasks, Agents, Mawas, then Projects icon, New session, Settings; tooltips; badges keep counts) / Hidden (0 pt). ⌘B cycles Expanded ↔ Compact (as in §4.1); a separate command "Hide Left Sidebar" and the titlebar button give Hidden; a visible **titlebar toggle button** next to the traffic lights always restores. Drag the left edge below its minimum by 24 pt → release collapses to Compact (Escape cancels).
2. **Right side**: a **vertical 40 pt activity rail outside the right panel body**, always visible (History, Changes, Requests, Agents, Resources, More). Clicking the **active** icon collapses the body to the rail; clicking any icon while collapsed expands that panel; ⌘L toggles body expanded/collapsed; explicit "Hide Right Rail" exists for a full hide with a titlebar recovery button. Body width 240–480 pt (preferred 350); drag-to-collapse like the left.
3. **Bottom**: ⌘⌥J (existing Hutan binding) toggles body + its tab strip together. **Do not change any existing default keybinding** in this slice (the spike's ⌘J/⌘P migration is out of scope); new commands get palette entries and only free chords.
4. **Responsive auto-collapse** (setting `layout.auto_collapse`, default on): right body collapses below 1180 pt window width, left goes Compact below 900 pt, center keeps ≥480 pt, +80 pt hysteresis before auto-restore; automatic collapse is temporary and never overwrites the user's explicit saved state; an explicit user hide is never auto-restored.
5. **Focus**: collapsing a dock that holds focus moves focus to the last active center item; no hidden element keeps focus or hitboxes. Collapse never stops sessions, closes tabs or dismisses approvals.
6. **Persistence**: per layout preset in desktop state (version bump + migration preserving custom panels, widths and the current visible/hidden choice); the Agent Dashboard drawer keeps working as today (its replacement is a later slice, DB-2).
7. Docs: `docs/design.md` (dock section, keyboard table), settings label for `layout.auto_collapse`.

## Tests

- Pure model: transition table for every left/right state × command/active-icon click/drag/auto-collapse, hysteresis, explicit-hide not auto-restored, migration from current layout state.
- GPUI: ⌘B Expanded→Compact→Expanded; compact rail icons open their pages; right active-icon click collapses body and rail stays; any rail icon expands; ⌘L; drag past minimum collapses, Escape cancels; focus moves to center; widths 1280/1180/900/640 behave per §4.3; no session stop request is sent.

## Acceptance

Gate green with `CARGO_BUILD_JOBS=4` and `cargo nextest run --workspace --locked -j 4` (laptop overheats; never more): fmt, clippy `-D warnings`, nextest, cargo-deny. One commit on `slice/DK-1`. Do not edit `core`, `protocol`, `docs/status.md`, `docs/roadmap.md`. Another slice (SK-1, send key) edits chat/launcher/changes-tray and settings labels concurrently — keep edits to `settings_labels.rs`/`settings_schema.rs` minimal and additive. Any `hutan`/`hutand`/`hutan-desktop` run needs `HUTAN_HOME=<temp dir>` inline; never touch `/Applications/Hutan.app`, `~/Library/Application Support/Hutan`, `~/.omp`, `~/.claude`, `~/.codex` or the Keychain.
