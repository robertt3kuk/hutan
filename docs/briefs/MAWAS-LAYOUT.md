# MAWAS-LAYOUT — Mawas chat width and the Today rail toggle

Agent: implementer. Branch `slice/MAWAS-LAYOUT`. **Contract ownership:** none; desktop only; **no protocol change**.

**Items (commit each).**
1. `AssistantSettings` (`crates/desktop/src/settings.rs` l. 2075) gains `chat_width_rem: f32` (default 46.0, clamped 30–90 when read) and `rail_open: bool` (default true); both `serde(default)`, so an existing `settings.toml` loads unchanged. Labels in `settings_labels.rs`; Settings → Mawas gets a **Layout** group (*Chat width* with *Reset*, *Show Today*).
2. **Chat width.** Replace the constant `CONVERSATION_WIDTH` (`panels/assistant.rs` l. 193) by `chat_width_rem × rem`. Handles, symmetric drag, live repaint, write on release (rounded 0.5 rem), Esc cancels, double-click resets, none below `FOLD_BELOW`. Palette: *Mawas: Wider Chat*, *Narrower Chat*, *Reset Chat Width* (±4 rem).
3. **Rail toggle.** The `rail_open` field starts from the setting and writes it on toggle; a header icon button above 640 pt (tooltip *Hide Today* / *Show Today*), the ⋯ entry kept, palette *Mawas: Toggle Today*. When hidden the button shows the badge = number of sessions with a request (`Today::requests` grouped by session). A new request never reopens it.
4. **Docs**: `docs/assistant.md` § Desktop (header, Today, width), `docs/design.md` § Mawas (l. 125: "at most 46 rem wide", "300 px"), the settings reference. No `CHANGELOG.md`, `status.md`, `roadmap.md`.

**Acceptance (GPUI tests in `assistant_gpui.rs` / `mawas_gpui.rs`, `common::click_at`, waiting on state).** Dragging the right edge by 40 px widens the column by 80 px and writes `assistant.chat_width_rem` once, on release (not during); Esc during a drag restores and writes nothing; double-click removes the key; clamp at 30 and 90 and at page minus gutter; no handles below 640 pt; a second window follows the written setting; the toggle persists across a new panel and a new window; with the rail hidden the badge equals the number of sessions with a request and the rail stays hidden when one arrives; a hidden Home's `render_count` does not move (the existing rule); the palette commands appear in `ui.actions`. Cua screenshots dark and light (rail shown, rail hidden with badge, wide column) → `~/hutan-scratch/mawaslayout/shots/`; input source ABC, Russian restored. `just check` green.

**Out of scope.** The rail's tabs and width, the Focus chip, any daemon or protocol change, the *Today → side panel* rename.

**Size and conflicts.** Roughly a day. It edits `panels/assistant.rs`, `panels/assistant_rail.rs`, `workbench/assistant.rs`, `settings.rs`, `settings_labels.rs`, the palette actions; TABS-FILES and MD-PREVIEW (file tabs, splits, previews) do not touch these.

## Rules
Read `AGENTS.md`, `docs/process.md`, ADR 0025, `docs/spikes/2026-10-30-mawas-supervisor.md`. Commit after every item (messages end `(MAWAS-LAYOUT)`); GPUI tests use `common::click_at` and wait on state; Cua screenshots dark + light into `~/hutan-scratch/mawaslayout/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; the user's own Hutan runs on this machine — never touch it. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
