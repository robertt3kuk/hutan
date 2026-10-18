# Hutan layout and history index map before the Orca-style relayout

Source for the OL-* slices (2026-10-18). Paths in the Orca sections are relative to a checkout of github.com/stablyai/orca.

## Summary

- **Fresh default:** Projects + History on the left; Inspector + Agents + Tasks on the right; Resources in a hidden bottom dock. **Assistant is enabled, but not initially placed/open**—its registry default destination is right.
- Dock tabs are a shared horizontal renderer, not a dedicated Projects/History component. A vertical left navigation can reuse panel registration, activation and cached views.
- History currently implements **query-driven ranked search**, not a browsable session catalog. Orca-style history needs session-summary metadata and a listing API; moving the existing panel alone will not provide it.

All paths below are relative to the repository. Read-only investigation; no checks run.

## 1. Current layout and mechanics

### Defaults and presets

| Preset | Left | Right | Bottom | Center |
|---|---|---|---|---|
| `default` | `sessions` (“Projects”), `history`; **240 px**, visible | `inspector`, `agents`, `tasks`; **300 px**, visible | `resources`; **200 px**, hidden | Keep current panes |
| `focus` | Same panels/size, hidden | Same panels/size, hidden | Same, hidden | Keep panes; zoom active pane |
| `overview` | Same as default | Same as default | Same, hidden | Show `overview` panel; stash previous layout |

Definitions: `crates/desktop/src/workbench/model/preset.rs:30-89`. Initial active tabs are Projects and Inspector; fresh center is one “Welcome” placeholder with center focus: `workbench/model/mod.rs:105-123`. History test explicitly asserts Projects is initially active: `crates/desktop/tests/history_gpui.rs:170-175`.

**Settings:** `layout.preset = "default"` and empty user `layout.presets` map. Each named override supplies left/right/bottom dock settings and status-bar order; same-name built-ins retain their center behavior, while new presets use `CenterLayout::Keep`: `crates/desktop/src/settings.rs:257-346`.

**Restore matters:** startup loads saved state before considering presets; only a fresh start with a non-`default` selected preset applies that selection in this constructor: `workbench/view.rs:517-542`. Therefore, merely changing built-in defaults does **not** relocate existing users’ panels. There is also a fresh-start edge case: a user override named `default` is not applied by that conditional. Live layout setting changes rebuild definitions and apply a changed selection/current definition: `workbench/terminals.rs:1126-1158`.

### Registration, opening and movement

- `PanelSpec { id, title, icon, default_dock, factory }`; registration replaces duplicate IDs and otherwise preserves registration order: `workbench/registry.rs:31-80`.
- Built-ins:
  - Projects/History → left: `workbench/panels/mod.rs:103-124`.
  - Inspector → right; Overview → left; Agents → right; Resources → bottom: `workbench/panels/mod.rs:125-164`.
  - Tasks/Assistant → right; Notifications → bottom: `workbench/panels/mod.rs:165-197`.
- Registry placement is **only the fallback when opening a closed panel**, not preset membership. Workbench `open_panel` resolves `default_dock`: `workbench/view.rs:1178-1189`. Model opening reveals/focuses an existing location rather than relocating it: `workbench/model/ops.rs:422-449`.
- Factories run lazily; views are cached by panel ID: `workbench/view.rs:1078-1090`.
- Model movement: `move_panel(panel, PanelDest::{Dock, Center})`: `workbench/model/ops.rs:398-420`. Context menu exposes Move to each dock, Open as Tab and Close: `workbench/view.rs:1392-1422`. Dock tabs also support drag/reorder/drop: `workbench/render.rs:951-1030`.
- Presets conserve unlisted open panels by moving them into the first center pane, rather than closing them: `workbench/model/preset.rs:106-175`. **Removing Assistant from a preset is not sufficient to guarantee it stays hidden or left-only.**

### Left Projects/History strip

`render_dock` handles **all three docks**: `workbench/render.rs:933-1180`.

- Iterates `dock.panels`, resolves glyph/title and compares `dock.active`: lines **951-973**.
- Click sets active panel and dock focus; right-click opens the panel menu: lines **988-1003**.
- Active panel supplies the body through `panel_view`: lines **1034-1050**.
- Left-specific treatment is sidebar background and a traffic-light/titlebar inset: lines **1091-1132**.
- Horizontal tab-strip construction: lines **1133-1150**, followed by the flexible body/drop area.

Thus Projects/History are ordinary registered panels in a dock vector, not hard-coded sidebar tabs.

### Assistant

- `assistant.enabled` defaults **true**; system-wide shortcut defaults empty: `crates/desktop/src/settings.rs:1009-1030`.
- Assistant is **absent from every built-in preset**, despite its right-dock fallback: `workbench/model/preset.rs:44-81`; `workbench/panels/mod.rs:175-184`.
- Explicit `OpenAssistant` reveals/creates its panel, ensures a scoped conversation exists and focuses its composer: `workbench/assistant.rs:44-74`.
- Other reveal paths include opening an existing assistant conversation, attaching context and Send block to Assistant: `workbench/assistant.rs:86-165`; global shortcut routes to `open_assistant`: `workbench/mac.rs:509-510`.
- Turning the setting off closes the panel, evicts its cached view, rebuilds commands, rebinds keys and updates the hotkey: `workbench/assistant.rs:18-40`. This is a **feature-disable switch**, not merely initial visibility.

### Status bar and persistence

Default ordered status IDs: `workbench/registry.rs:211-240`:

- **Left:** connection, workspace, sessions, attention, approvals.
- **Right:** density, theme, palette hint, left-dock toggle, bottom-dock toggle, right-dock toggle.

Rendering uses registered status items and live session/connection/dock data: `workbench/render.rs:1756-1856`; data contract: `workbench/registry.rs:122-153`.

State is stored in `desktop-state.json` under `$HUTAN_HOME`, otherwise macOS Application Support: `workbench/persistence.rs:14-30`. Mutations schedule persistence: `workbench/view.rs:961-1030`; writes use a sibling file and rename: `workbench/persistence.rs:151-160`. Model format is version **1**, normalized on load; unsupported versions are rejected: `workbench/model/persist.rs:5-40`.

## 2. Required layout changes and affected paths

### (a) Vertical left navigation

**Implementation direction:** retain `PanelRegistry` and dock active/body state; replace or augment only the left strip in `workbench/render.rs:933-1180` with vertically arranged navigation entries.

Important distinction:

- Existing tabs enumerate **open panels**.
- A permanent Orca-style navigation list must enumerate **available navigation destinations**, so Assistant/Search remain discoverable even when not open.
- Projects → existing `sessions`; Tasks → existing `tasks`; Agents dashboard must deliberately select existing `agents` tree versus `overview` board.
- Existing `SearchHistory` is history transcript search—not a general code-search page: `workbench/actions.rs:529`, `815`. A genuine separate Search page needs its own registered implementation.

Reuse registry glyph/title/factory and `open_panel`/`set_dock_active`; account for focus, dragging and menus currently attached to tabs. If navigation order is configurable, extend settings/preset serialization rather than inventing a separate hard-coded ordering.

### (b) History as full right-side “Agent Session History”

- Change History registry title/destination: `workbench/panels/mod.rs:115-124`.
- Move `history` from left to right in **all three** built-in presets: `workbench/model/preset.rs:44-81`; choose the intended right-side width/active panel.
- Preserve the ID `"history"` so actions, settings, cached entities and provenance/resume behavior continue to work.
- Handle saved layouts explicitly: startup restoration currently wins. Migration must consider dock placements, center placements and saved/stashed layouts; user `layout.presets` overrides also retain explicit old memberships.
- Replace query-only panel states/list rendering with browsable/grouped sessions: `workbench/panels/history.rs:288-292`, `920-1043`, `1145-1338`.
- Add Workspace / Project / All scope. Today only Workspace and All are mapped; Workspace is a cwd-prefix filter: `workbench/panels/history_model.rs:85-109`.
- Existing footer already has source row/error/index-time data; desired scanned-file count can be derived from status `files`, but total session counts cannot: `workbench/panels/history.rs:1340-1365`; `crates/protocol/src/history.rs:150-177`.

### (c) Assistant hidden initially, accessible only as a left page

**Do not set `assistant.enabled=false` just to hide it initially**: that removes the command/shortcut and prevents opening it. Initial non-placement is already implemented.

Required:

1. Change its registry fallback to left: `workbench/panels/mod.rs:175-184`.
2. Provide a left navigation entry that opens/focuses it; keep it out of initial active/default placement.
3. For a strict **left-only** invariant, change existing placement handling: `open_panel` currently honors restored right/center locations; panel menus and drag/drop allow relocation. Enforce the rule consistently in model movement/opening, presets/restoration and menus—not only the registry.
4. Preserve or intentionally redirect non-nav reveal paths: `workbench/assistant.rs:44-165`, `workbench/mac.rs:509-522`.
5. Keep enabled/disabled semantics coherent across commands and keybindings.

### Actions, keymaps, UI automation and tests

| Area | Relevant anchors / consequences |
|---|---|
| Palette | `workbench/actions.rs:420-631`: `SearchHistory`, conditional Assistant Open/Focus, generated `Panel: Open …` entries. Renaming titles updates generated panel commands; permanent navigation may need explicit actions. |
| Keymap | History search is `cmd-shift-f`: `workbench/actions.rs:815`. Disabled Assistant removes user-bound `OpenAssistant`: `workbench/terminals.rs:1018-1023`. |
| Action dispatch | Assistant handler: `workbench/render.rs:498-500`; keep both generic OpenPanel and specialized OpenAssistant behavior consistent. |
| `ui actions` / `ui.run` | Command catalog follows settings; remote generic panel opening constructs `OpenPanel`: `workbench/remote.rs:175-177`, `620-628`. Update catalog/layout publication if navigation introduces new state/actions. |
| Region probes | Current navigation automation uses `Region::DockTab(pos,index)` in `workbench/render.rs:1027`; vertical permanent destinations need stable probes and adjusted positional tests. |
| Layout/model tests | `workbench/model/tests.rs:521-619`, `1041-1168`; `crates/desktop/tests/workbench_gpui.rs:198`, `331-368`, `489`, `735`. Cover placement, movement, presets, restoration and dragging. |
| History tests | `tests/history_gpui.rs:170-195` directly asserts left membership/activation. `tests/history_panel_gpui.rs:142-165` already exercises moving History right. Rewrite query-only expectations and add browse/group/count/sort coverage. |
| Assistant tests | `tests/harness_gpui.rs:304-327` checks feature disabling; `tests/assistant_gpui.rs` covers assistant interactions. Add initial hidden + left-page-only assertions. |
| Remote catalog tests | `tests/control_gpui.rs:547-564`, `614-642`: default enabled action and live enabled-setting behavior. |
| Other layout consumers | `tests/agents_gpui.rs:142` asserts right placement; `tests/keymap_gpui.rs:100-103`, `166` assert dock defaults/status toggles. |

## 3. History index versus Orca-style sessions

### Existing sources, formats and ranking

Authoritative overview: `docs/history-index.md:31-73`.

- **Hutan:** live finished commands/output and agent reports; chat journal messages/tool events also feed source `hutan`.
- **Claude:** JSONL user/assistant/tool content, title/cwd metadata; subagents retain parent resume identity.
- **Codex:** rollout JSONL `session_meta`, `turn_context`, `response_item`; duplicate `event_msg` ignored.
- **OMP:** JSONL session/message/tool content; first-line and changed titles; subagents retain parent resume identity.
- OpenCode deferred; no Cursor/Gemini adapter.
- Title uses source metadata, falling back to first user line, 80 characters. Therefore it is **not guaranteed to be the first prompt**.
- Redacted text is chunked, so indexed rows are **not messages**.

Implementation: `crates/index/src/adapters/{claude,codex,omp}.rs`, scanner/store/indexer under `crates/index/src`; daemon search bridge: `crates/daemon/src/history.rs:190-227`.

Ranking is BM25 with body/identifier weights **1/2**, multiplied by role weights command **1.4**, user **1.3**, assistant **1**, tool **0.7**, output **0.6**. SQL picks the best matching chunk per indexed session, counts matching chunks and orders by relevance, then matching-chunk time: `crates/index/src/search.rs:9-16`, `113-143`; `docs/history-index.md:62-64`.

### Exact current fields

`HistoryHit`: `crates/protocol/src/history.rs:91-134`:

```text
source, session_key, title?, cwd?, role, ts_ms?,
snippet, score, matches,
resume_argv?, session_id?, path?, terminal_id?, block_id?
```

Internal `Hit`: `crates/index/src/types.rs:165-184`; daemon adds validated resume argv/Hutan session identity: `crates/daemon/src/history.rs:434-459`.

Schema v2: `crates/index/src/store.rs:33-74`:

```text
sources:  path, source, size, mtime_ns, offset, tail_hash,
          format_version, state, rows, errors, last_indexed_ms
sessions: id, source, key, source_id, title, cwd
entries:  id, session_id, role, ts_ms, ext_key, terminal_id, block_id
fts:      body, idents
meta:     key, value
```

**Identity caveat:** external `sessions.source_id` is unique: effectively one indexed session per transcript file, not necessarily one logical agent conversation. Parent/subagent files can share a source session key; current search partitions by internal `session_id`: `store.rs:52-60`; `search.rs:127-130`.

### Orca field compatibility

| Desired list feature | Available today? |
|---|---|
| One row per session | **Partially:** one best hit per indexed session/file, only for matching text; logical parent/subagent deduplication is not guaranteed. |
| Title = first user prompt | **Partial:** optional session title with first-user fallback; explicit titles may replace it. |
| “Agent: last message” | **No:** snippet comes from best FTS match, not last assistant message. |
| Agent icon/name | **Partial:** source permits generic Claude/Codex/OMP/Hutan branding; no structured per-session agent/display name. |
| Number of messages | **No:** `matches` counts matching chunks; source `rows` counts indexed chunks. Neither is total messages. |
| Age / last updated sort | **Partial:** `ts_ms` is the winning chunk’s timestamp, not session last activity. File mtime exists internally but is not returned per hit. |
| Model | **No structured field** in session schema or hit. |
| Project grouping | **Partial:** `cwd` allows client grouping; no canonical project identity or project/session counts. |
| Workspace / Project / All | **Partial:** current Workspace/All cwd-prefix filtering only; no indexed workspace ID. |
| “Scanned N files · Just now” | **Mostly:** source status exposes file counts and last-indexed times, not a scan-progress/completion contract. |
| “2 of 250 sessions” | **No:** response is only `{ hits }`, no total matching/global count, offset or cursor. |
| Expand row | Existing ordered session-text retrieval exists internally: `store.rs:552+`, daemon `history.rs:232-240`; not a ready-made paginated history-row expansion API. |
| Row menu | Existing resume/copy/open-block/reveal-file behavior can be retained: `workbench/panels/history.rs:1145-1264`. |

**Essential backend work:** session-level summary storage/aggregation (first prompt, last assistant message, true message count, last activity, model/agent identity), logical session identity across files, and a paginated **list/browse** contract with scopes, totals and ordering. Empty search is currently rejected at the daemon (`history.rs:190-195`) and FTS layer (`index/src/search.rs:49-54`); the panel deliberately stays idle for an empty query (`panels/history.rs:288-292`). This is the principal functional gap—not a styling issue.