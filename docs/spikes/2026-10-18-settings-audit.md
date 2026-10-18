# Settings coverage audit — 2026-10-18

Read-only source audit by the scout agent SettingsAudit on master 7051cac. Feeds ST-1/ST-2/ST-3.

## Summary

**Settings covers desktop presentation/preferences well, but is not yet a complete Hutan management surface.** Its 20 pages include working provider management, harness accounts, start-at-login and keep-running controls. Most daemon behavior/security settings remain file-only; some management operations exist elsewhere in the desktop but are absent from Settings.

The most important concrete findings:

1. **No Settings controls for daemon notification policy, restoration, context permissions, agent-control permissions, executable paths, MCP servers, indexing sources, or task concurrency.**
2. **History indexing can be enabled in the desktop, but there is no desktop disable control in the audited History path.** Disabling requires CLI/API/file.
3. **`sidebar.organize` is registered and functional but omitted from the Settings catalog.**
4. **Provider mutations report success even when SQLite persistence fails:** the daemon logs storage errors rather than returning them.
5. **Settings reads effective layered values but always writes user values.** Workspace/project overrides can therefore make a control appear not to work, including Reset.
6. **Assistant scope overrides and remembered paid-start consent lack a reset/remove-management path.**

Read-only source audit only: no commands, model turns, tests, writes, Keychain access or application interactions were performed. “Live” below describes source behavior, not exercised runtime verification.

## Architecture and locations

- Desktop registry: `crates/desktop/src/settings.rs:1112-1131`.
- Desktop Settings page catalog: `crates/desktop/src/workbench/settings_schema.rs:238-273`.
- Generic controls: `crates/desktop/src/workbench/settings_sheet.rs:618-920`.
- User-facing page/field/value wording: `crates/desktop/src/workbench/settings_labels.rs:50-480`.
- Desktop persistence: `settings.toml`; keyboard overrides: separate `keymap.toml`. Resolution: `crates/settings/src/store.rs:24-62`.
- Layering: defaults < user < workspace < project < runtime. Workspace/project files are `<root>/.hutan/settings.toml`: `crates/settings/src/store.rs:1,258-260,681-710`.
- Settings writes apply immediately to the store, followed by change notifications: `crates/settings/src/store.rs:741-749,784-808`.
- Desktop live-change handler: `crates/desktop/src/workbench/terminals.rs:1090-1123`; macOS surfaces: `crates/desktop/src/workbench/mac.rs:434-456`.
- Daemon configuration is loaded during startup: `crates/daemon/src/daemon.rs:178`; schema/parser: `crates/daemon/src/config.rs:524-632`.
- Normal data directory: `~/Library/Application Support/Hutan`; logs: `~/Library/Logs/Hutan`. `HUTAN_HOME` relocates data and logs; `HUTAN_SOCKET` overrides the socket. Files include `daemon.toml`, `hutan.db`, `index.sqlite`, socket and lock: `crates/daemon/src/paths.rs:11-106`.
- Provider metadata lives in SQLite; provider secrets live in Keychain service `dev.hutan.provider`, account = provider ID: `crates/daemon/src/providers.rs:1-6`; `crates/gateway/src/credentials.rs:4-20`.

## 1. Desktop `settings.toml`: complete namespace/key inventory, page by page

**Control anchors used below:** all ordinary fields use the same generated renderer:
- Boolean: `settings_sheet.rs:618-685`.
- Enum: `settings_sheet.rs:686-737`.
- String/number: `settings_sheet.rs:738-788`.
- String list, add/remove/reorder: `settings_sheet.rs:789-877`.
- Unsupported map/read-only hint: `settings_sheet.rs:878-883`.
- Per-field Reset: `settings_sheet.rs:884-897`.

Paths in these anchors are under `crates/desktop/src/workbench/`.

| Settings page / namespace | Every key | Definition | Reachability, application and wording |
|---|---|---|---|
| Appearance / `appearance` | `density`, `appearance` (legacy alias `chrome`) | `crates/settings/src/appearance.rs:30-40` | Both Settings controls; live chrome/density and terminal restyling. Clear descriptions: `settings_labels.rs:76-91`. |
| General / `app` | `keep_running`, `start_at_login`, `setup_done`, `on_last_window_close`, `menu_bar`, `dock_badge` | `crates/desktop/src/settings.rs:890-917` | All in Settings. Lifecycle/macOS settings apply live; setup flag affects next startup, with **Run setup again** available immediately (`settings_sheet.rs:977-982`). Special login toggle/status/approval link: `settings_sheet.rs:636-680`. Registration is available only inside Hutan.app, explicitly explained. Wording: `settings_labels.rs:94-132`. |
| Keyboard / `keymap` | `base` | `crates/desktop/src/settings.rs:1099-1107` | Settings dropdown, live rebind. Help explicitly says immediate and explains Zed/Hutan bases. Custom bindings remain `keymap.toml` only: `settings_labels.rs:135-143`. |
| Terminal / `terminal` | `font_family`, `font_size`, `line_height`, `theme`, `cursor.style`, `cursor.blink`, `option_as_meta`, `copy_on_select`, `scroll_to_bottom_on_input`, `blocks`, `shell`, `env` | `crates/desktop/src/settings.rs:38-42,69-93` | All except `env` editable in Settings; `env` is a map and read-only/file-only. Style/behavior live; `shell` and `env` apply to **new terminals**, stated in help (`settings_labels.rs:212-226`) and implemented by `settings.rs:128-145`. Option Left/Right/Both all affect both keys, disclosed in help (`settings_labels.rs:181-196`). Theme is free text, not a theme picker. |
| Layout / `layout` | `preset`, `presets.<name>.{left,right,bottom}.{panels,size,visible}`, `presets.<name>.status_bar.{left,right}` | `crates/desktop/src/settings.rs:198-203,235-239,254-269` | `preset` editable, applies at startup/change. `presets` map is read-only/file-only in Settings. Command-palette **Save layout as preset** writes a preset: `settings_sheet.rs:502-551`. Help mentions file/palette alternatives (`settings_labels.rs:228-234`). |
| Projects Sidebar / `panels.sessions` | `group_by`, `needs_you`, `row` | `crates/desktop/src/settings.rs:358-365` | Settings enum/toggle/list; consumed by sidebar, including `group_by` (`panels/sessions.rs:683`). Labels explain row contents. |
| **No Settings page** / `sidebar` | `organize` | `crates/desktop/src/settings.rs:417-424` | Registered but absent from catalog. Live, persistent control exists in sidebar menu: `workspaces.rs:578-581,627-633`. Missing from Settings search and labels catalog. |
| Inspector / `panels.inspector` | `tabs`, `reveal_on_approval` | `crates/desktop/src/settings.rs:430-434` | Settings list/toggle. Tabs consumed on render (`panels/inspector.rs:332-348`); reveal setting checked on approval (`agents.rs:738-742`). |
| Overview / `panels.overview` | `card_size`, `preview`, `sort` | `crates/desktop/src/settings.rs:495-501` | Settings dropdowns. Preview modes and card dimensions are implemented (`panels/overview.rs:110-164`). |
| Workspaces / `workspaces` | `sort`, `hide_empty`, `hide_sleeping`, `hide_cli_created`, `always_show_active` | `crates/desktop/src/settings.rs:558-568` | All Settings controls; live sidebar sorting/filter preferences. Filtering implementation: `crates/desktop/src/sessions/workspaces.rs:48-63`. |
| History / `history` | `sources`, `roles`, `range`, `project`, `show_snippets` | `crates/desktop/src/settings.rs:650-662` | Settings controls for **search defaults/presentation**, not indexing permission or ingestion sources. This namespace duplicates daemon `[history]` naming with a different purpose. |
| Chat / `chat` | `send_on_enter`, `attach_max_bytes` | `crates/desktop/src/settings.rs:684-690` | Settings controls; composer behavior and attachment limit, no restart declared. Attachment limit is read when attaching (`agents.rs:851-852`). |
| Editing / `editor` | `vim_mode`, `vim_use_system_clipboard` | `crates/desktop/src/settings.rs:711-717` | Both in Settings. Vim config is read by text-field event/render paths, e.g. `settings_sheet.rs:177-193`, `changes.rs:632-635`. |
| Agents / `agents` | `show_finished`, `only_blocked`, `scope` | `crates/desktop/src/settings.rs:757-764` | Settings display/filter preferences, not executable paths or account/security configuration. |
| Resources / `resources` | `interval_ms`, `sort` | `crates/desktop/src/settings.rs:819-825` | Settings number/enum. Subscription interval is reread during drawing and clamped (`panels/resources.rs:314-319,397-405`). Help states minimum 1000 ms (`settings_labels.rs:425-429`). |
| Tasks / `tasks` | `show_done`, `scope` | `crates/desktop/src/settings.rs:840-845` | Settings display/filter preferences only; daemon parallelism is separate and inaccessible here. |
| Assistant / `assistant` | `enabled`, `global_shortcut` | `crates/desktop/src/settings.rs:1006-1014` | Settings toggle/text; live panel/commands visibility and system hotkey. Does **not** expose daemon backend/model, context budget or permissions. Disabling preserves conversations, explicitly documented in the schema. |
| Agent Integrations / `integrations` | `auto_install` | `crates/desktop/src/settings.rs:1033-1043` | Settings toggle; turning on can initiate installation immediately (`integrations.rs:114-142`), then on later launches. Turning off does not uninstall. Actual individual hook status/install/uninstall lives in setup, not this page. |
| Start Sheet / `launcher` | `confirm_agent_start` | `crates/desktop/src/settings.rs:1053-1057` | Settings toggle; consulted before prompted immediate chat starts (`launcher.rs:1542-1578`). Separate remembered per-agent/model exemptions can bypass it. Help explains exemptions but provides no way to clear them. |
| Accounts / synthetic `accounts` | Harness installation/detection, login/logout, OMP per-provider logout | `harnesses.rs:368-610` | Custom Settings page, not `settings.toml`. See stored/external state inventory below. |
| Models / synthetic `models` | Provider CRUD, endpoint/default model, credentials, local discovery/model listing | `providers.rs:378-548` | Custom Settings page, not `settings.toml`. See below. |

### Shared Settings UX

- **Search exists**, including human-friendly field titles/help/choice labels: `settings_schema.rs:281-300`; search input: `settings_sheet.rs:987-1027`.
- **Per-field Reset exists** for editable schema fields. It writes the default value rather than removing an override: `settings_sheet.rs:298-315`.
- Maps have neither editor nor Reset: `settings_sheet.rs:878-897`.
- Settings diagnostics display file/key/error information: `settings_sheet.rs:937-972`.
- Generic Settings has **no user/workspace/project layer selector**, despite saying “Can be set per workspace or project”: `settings_sheet.rs:1103-1111`.
- No generic restart-required indication exists in the renderer. Existing desktop fields mostly apply live; new-terminal and next-start semantics are communicated individually in help/schema.
- Labels are generally understandable and significantly better than raw TOML keys. Remaining jargon includes “Meta”, free-text model/theme IDs, and the underlying daemon term; descriptions usually explain it.

## 2. Daemon `daemon.toml`: complete key inventory and coverage

Unless called out otherwise, these are **file-only configuration**, loaded at daemon startup and requiring daemon restart to apply. The basis is startup loading (`crates/daemon/src/daemon.rs:178`) and the complete Settings catalog (`settings_schema.rs:238-273`), which contains no daemon-configuration page. Settings consequently provides neither controls nor restart messaging for them.

| TOML section / complete keys | Definition | Desktop/CLI reachability and application |
|---|---|---|
| `[daemon] restore_sessions` (`respawn` / `stopped`) | `config.rs:95-104,567-571` | File-only; next daemon startup/restoration. |
| `[workspaces] home_folder` | `config.rs:562-565,651-666` | File-only Home-folder configuration; startup. Separate from desktop workspace ordering. |
| `[history] enabled`, `range_days`, `hutan`, `claude`, `codex`, `omp`; `[history.roots] claude`, `codex`, `omp` | `config.rs:230-258` | `enabled` reachable from History panel **enable only**, and CLI `index enable/disable`. Sources/range/root paths file-only. `history.set_enabled` reloads history config and applies live, persisting enabled: `history.rs:276-301`; thus toggling/re-enabling through API can apply edited history options without restarting. |
| `[codex] bin`, `args` | `config.rs:174-196` | File-only executable/app-server arguments. |
| `[harness.omp]` and `[harness.claude]`: `bin`, `args`, `env`, `cli` | `config.rs:199-222` | File-only; accounts can report missing harness but Settings cannot correct executable/CLI paths or custom arguments/environment. |
| `[mcp.<name>] command`, `args`, `url`, `transport` | `config.rs:224-236` | File-only harness-session server definitions; command/URL alternatives validated in `config.rs:635-648`. No MCP management Settings page. |
| `[gateway] max_context_chars` | `config.rs:152-170` | File-only direct-chat conversation truncation budget. |
| `[assistant] backend`, `provider`, `model` | `config.rs:340-351` | File defaults; overridden by live daemon-stored `assistant.configure` scope choices from Assistant panel/setup/API. **Not on Assistant Settings page.** |
| `[assistant] context_budget_chars` | `config.rs:352-354` | File-only context attachment budget. |
| `[assistant.context] shell`, `agents`, `files`, `search` | `config.rs:277-298` | File-only project-assistant access permissions; defaults all true. |
| `[assistant.global_context] shell`, `agents`, `files`, `search` | `config.rs:300-307,355-358` | File-only Home-assistant permissions; files false by default. |
| `[assistant.workspaces.<id-or-name>] shell`, `agents`, `files`, `search` | `config.rs:329-337,359-361,379-393` | File-only optional per-workspace permission overrides; ID applied then name, so name wins. No Settings permission editor or inheritance display. |
| `[notify] enabled`, `min_turn_secs`, `kinds`, `expire_secs`, `native` | `config.rs:403-438` | File-only intent/native-notification policy. macOS permission is separately OS-managed; no notification Settings page. |
| `[terminal] env_inherit` (`login` / `daemon`) | `config.rs:143-149` | File-only daemon environment source. Desktop Terminal page exposes additional env/shell but not this source selection. |
| `[tasks] max_parallel` | `config.rs:445-458` | File-only auto-start concurrency; desktop Tasks page only contains display filters. Manual task starts ignore this limit, documented in schema. |
| `[projects] auto` | `config.rs:127-141` | File-only automatic project assignment behavior. |
| `[control] agents` (`ask` / `allow` / `read_only`), `mcp`, `approval_timeout_secs` | `config.rs:462-519` | File-only agent mutation security policy, automatic Hutan MCP injection and approval timeout. |

**No `[worktree]` daemon configuration exists in this schema.** Searches for worktree settings/configure and the actual worktree protocol/service show operation-specific parameters instead; treating “worktree settings” as an existing missing TOML section would be incorrect.

Paths in this table abbreviate `crates/daemon/src/`.

## 3. Daemon-stored and external state changed by requests

| State / management operation | Where it lives and mutation path | Settings coverage / apply semantics / reversibility |
|---|---|---|
| Provider `kind`, `name`, `base_url`, `default_model`, generated ID | SQLite provider records; `crates/daemon/src/providers.rs:261-355,371-381` | **Models** page add/edit/remove (`providers.rs:378-548`); sequential prompts (`providers.rs:237-329`). Live registry updates, no restart. Kind immutable during edit; create a new provider instead, explained in code, not prominently in the page. |
| Provider API key | macOS Keychain, service/account above; `crates/daemon/src/providers.rs:198-252` | **Models** page Set key / Clear key / Remove. Secure write-only modal: `providers.rs:155-186`; removal confirms and removes key first (`providers.rs:200-218`; daemon `providers.rs:359-368`). Clear key has no confirmation. Keys cannot be recovered through Hutan, intentionally. |
| Provider local discovery / model refresh | Runtime discovery/cache; `providers.rs:333-375,413-442,503-518` | **Models** page Discover local / Models-refresh / Refresh. No model turn is required by these source paths. |
| Assistant default backend/provider/model per global or workspace/project scope | SQLite `app_meta`, `assistant.configure`; `crates/daemon/src/assistant.rs:427-531` | **Assistant panel**, not Settings: `panels/assistant.rs:555-591,850-947`. Global initial choice also setup (`setup.rs:439-453`). Live for assistant selection/opening; daemon documentation says choices govern newly created assistants. Inheritance: project/workspace → parent → global → daemon.toml (`assistant.rs:448-483`). **No deletion/reset-to-inherit operation in configure.** “Automatic” clears model choice but still saves an explicit backend. |
| Harness accounts/login/logout | Harness-owned authentication state, not Hutan provider Keychain records | **Accounts** Settings page refresh/login/logout (`harnesses.rs:368-610`); flow opens a Hutan terminal (`harnesses.rs:330-365`). OMP logged-in rows have per-provider logout. Intro clearly explains Hutan delegates authentication. Harness install button available when installer exists (`harnesses.rs:519-540`). |
| Hook integration install/uninstall/status | Agent-owned config files; Hutan-marked entries/backups | Not individual controls on Agent Integrations Settings page. Available in **Run setup again → Agents** and CLI. `integrations.rs:1-11,218-243`; setup reachability from General: `settings_sheet.rs:977-982`. Auto-install toggle is not an uninstall toggle. |
| Worktree create branch/base/repo/path; remove force/delete-branch | Git is source of truth; SQLite tracks Hutan-created paths. `crates/protocol/src/worktree.rs:20-35,84-96`; daemon `worktrees.rs:1-8,171-181` | Start sheet selects main/existing/new worktree (`launcher.rs:88-95,1442-1482`); Changes offers cleanup (`changes.rs:574-628`). No Settings page/default path policy. Explicit custom create path available CLI/API (`crates/cli/src/args.rs:381-393`). Changes are operation-specific/live, not restart settings. |
| Workspace/project names, roots, pinning, placement/organization | Daemon workspace/project state plus desktop preferences | Sidebar management, not Settings; panel description enumerates create/reparent/rename (`panels/sessions.rs:1-5`). Sidebar organization persists `sidebar.organize` (`workspaces.rs:578-581`). |
| History index enable/clear/rebuild | `daemon.toml` enabled flag plus `index.sqlite` | **History panel**, not Settings. Enable `panels/history.rs:390-417`; clear/rebuild `:429-495`; confirmations `:1327-1347`. Clear warns Hutan-owned records cannot be rebuilt. Disable is CLI/API/file only. |
| Daemon lifecycle | Running process / connection / launch agent | Start/stop/restart/log reveal in **Daemon menu/popover**, not Settings: `daemon.rs:323-396`; `daemon_popover.rs:110-126`. General has keep-running/login switches, duplicated intentionally in daemon surfaces. Restart explicitly warns sessions stop and new daemon starts empty (`daemon.rs:363-370`). |
| Per-agent/model “Don’t ask again” paid-start exemptions | Desktop launcher memory, persisted by workbench state save | Set in Start confirmation; not Settings and no reset list: `launcher.rs:1562-1601`. Turning `launcher.confirm_agent_start` on does not override already remembered exemptions. |
| Last launch agent/model/place and related desktop layout/session selection | Desktop saved state | Remembered outside Settings (`launcher.rs:1620-1638`; desktop persistence wired at `crates/desktop/src/app.rs:350`). No central data/state reset surface observed in the Settings renderer. |

## 4. CLI/API/file-only management inventory

Not every CLI command is a “setting”; the following separates management gaps from automation/session operations.

| Operation | Available outside Settings |
|---|---|
| Disable history ingestion | `hutan index disable`; CLI grammar `crates/cli/src/args.rs:1309-1318`. Desktop only sends `history_set_enabled(true)` (`panels/history.rs:401`). |
| Custom keybinding overrides | Edit `keymap.toml`; Settings only selects base (`settings_labels.rs:135-143`). |
| Workspace/project settings layers | `hutan ui set … --layer workspace/project` (`crates/cli/src/args.rs:181-207`) or edit `.hutan/settings.toml`. Settings itself always writes user layer. |
| Daemon policies listed in §2 | Edit `daemon.toml`; no equivalent dedicated CLI setters observed apart from index enable/disable. Raw API does not add missing server-side setters. |
| Custom worktree create path/base/repository | CLI/API operation parameters (`args.rs:381-393`; protocol `worktree.rs:20-35`). |
| Integration install/uninstall/status | CLI `integration …` (`args.rs:1246-1253`), also setup; not strictly CLI-only. |
| Daemon start/status/stop and recovery | CLI plus desktop Daemon/recovery surfaces; not strictly CLI-only (`args.rs:757` onward; `workbench/daemon.rs:323-396`). |
| Providers/accounts | CLI plus Settings; **not gaps in basic Settings reachability**. |
| Protocol/event tooling, raw API, terminal attach/send, agent hook reporting, MCP stdio server | CLI automation/diagnostic operations rather than missing user preferences: `args.rs:31-130`. |
| Storage/log relocation and custom socket | Environment (`HUTAN_HOME`, `HUTAN_SOCKET`) / CLI socket argument (`args.rs:18-21`), not Settings. Restart/relaunch under new environment required; no Settings migration affordance. |

## 5. Ranked gaps and obvious functional bugs

### Blocker

No unconditional product-wide blocker established from source alone. The following persistence bug can become a blocker under storage failure.

### Major

1. **Provider changes can claim success without persisting.**  
   Add/update mutate the in-memory vector and call `persist`; `persist` returns no result and uses `log_write` for SQLite errors. Remove also logs database-delete failure and returns success. A user can add/edit/remove successfully in Models, then see state revert after daemon restart if storage failed.  
   **Evidence:** `crates/daemon/src/providers.rs:303-315,345-355,359-381`.

2. **Security/privacy policy has no Settings UI.**  
   Assistant file/shell/transcript/search permissions, per-workspace permissions and agent-control mutation policy require TOML editing/restart. Users cannot inspect effective inherited permissions in Settings.  
   **Evidence:** `crates/daemon/src/config.rs:277-307,329-393,462-519`; exhaustive UI catalog `settings_schema.rs:238-273`.

3. **No notification policy management in Settings.**  
   Master/native toggles, intent kinds, turn threshold and expiration are all file-only. This is normal-user behavior, not merely an advanced executable override.  
   **Evidence:** `crates/daemon/src/config.rs:403-438`.

4. **History enable is effectively one-way in desktop UI.**  
   History panel provides Enable, Clear and Rebuild, but its enable request is hard-coded true; no disable control appears in that code path. Settings History only controls search defaults.  
   **Evidence:** `panels/history.rs:390-417,401,1327-1347`; `crates/desktop/src/settings.rs:650-662`; CLI disable `crates/cli/src/args.rs:1314-1315`.

5. **Settings silently edits a different layer from the one it displays.**  
   Values come from `entry.current(&self.settings)`—effective layered values—while `sheet_write` always calls `write_user`. A terminal preference overridden in workspace/project config can remain visually unchanged after editing/Reset. Header only says it “Can be set per workspace or project,” without showing provenance or selecting layer.  
   **Evidence:** `settings_sheet.rs:234-242,252-260,298-315,1103-1111`; store layering `crates/settings/src/store.rs:1`.

6. **Daemon behavior configuration is broadly absent.**  
   Restore mode, Home folder, history sources/roots/range, task parallelism, project auto-assignment, executable/CLI paths, MCP servers, terminal environment inheritance and gateway/context budgets have no Settings controls. Missing restart instructions compound the gap.  
   **Evidence:** full daemon inventory above; `config.rs:107-124,524-560`; UI catalog `settings_schema.rs:238-273`.

7. **Assistant backend/model management is split away from Assistant Settings; overrides cannot be removed.**  
   The panel can configure scopes, but Settings only exposes enabled/shortcut. Configure always writes a concrete config; no reset-to-inherit branch exists. Selecting “Automatic” changes only model selection, not inheritance of backend/global config.  
   **Evidence:** `panels/assistant.rs:555-591,875-895`; daemon `assistant.rs:486-531`.

8. **Integration opt-out is not uninstall, and Settings has no per-agent repair/status/uninstall.**  
   Disabling auto-install leaves hooks installed. Users must rediscover setup or CLI to undo installation. The implementation explicitly never uninstalls automatically.  
   **Evidence:** `integrations.rs:1-11,114-142,218-243`; Settings page schema `settings.rs:1033-1043`.

9. **Remembered paid-start consent cannot be revoked from Settings.**  
   Exemptions are checked after the global confirmation preference, and remembering inserts a key into persisted memory. Resetting `confirm_agent_start` to true does not remove exemptions.  
   **Evidence:** `launcher.rs:1542-1601`.

### Minor

10. **Registered `sidebar.organize` omitted from Settings entirely.**  
    Sidebar menu works; page catalog/search/labels omit the namespace.  
    **Evidence:** `settings.rs:417-424,1125`; `settings_schema.rs:238-273`; `workspaces.rs:627-633`.

11. **Environment variables and custom layout maps are visible but not editable/resettable.**  
    Generic unsupported renderer exposes JSON-like value plus “edit settings.toml”; there is no structured map editor.  
    **Evidence:** `settings_schema.rs:145-159`; `settings_sheet.rs:878-897`.

12. **Reset writes explicit defaults, not removal of customizations.**  
    This prevents returning to inherited/default-by-absence behavior. No per-page reset-all is present in the renderer.  
    **Evidence:** `settings_sheet.rs:298-315`.

13. **Free-text/list fields allow confusing ineffective values.**  
    Theme, layout preset, sidebar row IDs and inspector tab IDs are arbitrary text/list inputs rather than validated choices. Numeric fields also accept values outside effective clamped ranges: terminal font size/line height and resource interval may display saved values different from actual behavior.  
    **Evidence:** generic editor `settings_sheet.rs:418-430,738-877`; terminal clamp `settings.rs:123-126,155-159`; resource clamp `panels/resources.rs:314-319`.  
    **[INFERENCE]** More domain-aware validation/choices would avoid “saved but no visible effect” reports.

14. **Custom provider/account page contents are not indexed by Settings search.**  
    Search covers catalog names and schema fields. Synthetic Accounts/Models entries have null values/no schema fields, so searching “API key”, “endpoint” or “login” does not search their custom controls.  
    **Evidence:** `settings_schema.rs:258-273,281-300`; custom render dispatch `settings_sheet.rs:973-976`.

15. **Layout save function’s documented activation is not implemented in that function.**  
    `write_layout_preset` says it writes the preset “and makes it the current preset,” but only writes `layout.presets.<name>` and updates `self.state.preset`; it does not write `layout.preset`.  
    **Evidence:** `settings_sheet.rs:516-551`.  
    **[INFERENCE]** A subsequent startup may choose the old configured preset; persistence interaction should be traced before classifying this as a confirmed user-visible restart regression.

16. **Data management/discovery incomplete in Settings.**  
    No data-folder reveal, settings-file reveal, index/database sizes overview, desktop-log reveal, relocation or backup/reset surface appears in Settings. Daemon log reveal and lifecycle controls exist elsewhere, so these should not be reported as globally absent.  
    **Evidence:** Settings renderer `settings_sheet.rs:973-1140`; daemon log reveal `daemon.rs:388-396`; paths `crates/daemon/src/paths.rs:11-106`.

## Conclusions for fix slicing

1. **First:** make provider persistence errors reach the UI; add history disable.
2. **Then:** add daemon Settings sections for security/context permissions, notifications and restore, with restart semantics.
3. **Then:** unify scope/layer/provenance management: assistant defaults/inheritance, workspace/project settings, sidebar organization.
4. **Then:** expose integration repair/uninstall and consent-reset management; add data/log/lifecycle entry points.
5. Preserve what already exists: search, per-field Reset, human-readable labels, provider CRUD/key management, account flows, login registration/status and live desktop preference handling.

[You have received this identical output 3 times. Re-reading 'agent://SettingsAudit' will not change it — use a narrower selector (path:A-B), or proceed with the edit.]