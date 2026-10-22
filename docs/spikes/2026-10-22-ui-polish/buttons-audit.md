# UX-BUTTONS audit: every button-like element

Generated 2026-10-22 from `rg '\.on_click\('` over `crates/desktop/src`, `crates/chat_view`, `crates/code_view` and `crates/terminal_view` (`chat_view` and `code_view` hold no click handlers: they are model and layout crates; the drawing is in `crates/desktop/src/workbench/chat`). One row per click handler, read from its constructor. **Current look** names the helper that styled it before UX-BUTTONS (`ui::button` with its kind, a per-file `small_button`/`icon_button`/`round_button` wrapper, or a hand-styled `div().id().on_click`); **Target** is the variant it moves to. Rows, tabs, menu items, disclosure headers, fields and checkboxes are selectable surfaces, not buttons: they keep their own hover and selected fills and are listed so the exclusion is explicit. The classification reads each site's constructor and enclosing function; the commit that migrates a surface corrects any row it finds wrong (the table is amended in the closing docs commit). Placement and key rules for every surface: primary trailing, Cancel leading (Esc), Return = the one primary, destructive never default, labels verb-first sentence case.

## Sheets and dialogs (112)

| Site | Element (id / label) | Current look | Target |
|---|---|---|---|
| `chat/rewind.rs:126` | checkpoint_footer · button( / “Rewind…” | `button` (Ghost) | Button ghost |
| `chat/rewind.rs:153` | checkpoint_footer · button( / “Undo” | `button` (Ghost) | Button ghost |
| `closing.rs:228` | render_close_toast · ui::button( / “Undo” | `ui::button` (Ghost) | Button secondary, leading (Esc) |
| `daemon_popover.rs:112` | render_daemon_popover · ui::button(id, label, ui::ButtonKind::Secondary, true, &them / “Log” | `ui::button` (Secondary) | Button secondary |
| `daemon_popover.rs:166` | render_daemon_popover · .id(id) | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `first_run.rs:244` | render_first_screen · ui::button( / “Open a folder  ⌘O” | `ui::button` (Primary) | Button primary (one per surface; trailing; Return) |
| `first_run.rs:259` | render_first_screen · ui::button( / “New terminal” | `ui::button` (Secondary) | Button secondary |
| `first_run.rs:274` | render_first_screen · ui::button( / “Set up Hutan…” | `ui::button` (Ghost) | Button ghost |
| `first_run.rs:334` | first_screen_agent · ui::button( | `ui::button` (Ghost) | Button ghost |
| `first_run.rs:365` | render_start_send_key_offer ·  | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `first_run.rs:386` | render_start_send_key_offer · .id("launch-ask-dismiss") / “Which key sends a message?” | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `first_run.rs:482` | render_notify_offer · button("notify-yes", "Notify me", ButtonKind::Secondary) / “Notify me” | `button` (Secondary) | Button secondary |
| `first_run.rs:485` | render_notify_offer · button("notify-no", "No thanks", ButtonKind::Ghost) / “No thanks” | `button` (Ghost) | Button ghost |
| `first_run.rs:504` | render_notify_offer · button("notify-done", "Done", ButtonKind::Ghost) / “Done” | `button` (Ghost) | Button ghost |
| `first_run.rs:569` | hooks_offer · button( / “Install hooks…” | `button` (Ghost) | Button ghost |
| `first_run.rs:600` | hooks_offer · button( / “Install hooks for {name}” | `button` (Secondary) | Button secondary |
| `first_run.rs:612` | hooks_offer · button( / “No thanks” | `button` (Ghost) | Button ghost |
| `harnesses.rs:384` | render_accounts · let refresh = button( / “Refresh” | `button` (Secondary) | Button secondary |
| `harnesses.rs:459` | render_account_card · let login = button( / “Log in” | `button` (Primary) | Button primary (one per surface; trailing; Return) |
| `harnesses.rs:472` | render_account_card · let logout = button( / “Log out” | `button` (Secondary) | Button secondary |
| `harnesses.rs:521` | render_account_card · button( / “Install…” | `button` (Primary) | Button primary (one per surface; trailing; Return) |
| `harnesses.rs:601` | render_account_row · button( / “Log out” | `button` (Ghost) | Button ghost |
| `launcher_ui.rs:223` | build_launcher · .id(id(format!("launch-project-{at}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `launcher_ui.rs:260` | build_launcher ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `launcher_ui.rs:298` | build_launcher ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `launcher_ui.rs:318` | build_launcher ·  / “Direct · {}” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `launcher_ui.rs:336` | build_launcher · ui::button( / “Direct model · Add a provider…” | `ui::button` (Ghost) | Button ghost |
| `launcher_ui.rs:380` | build_launcher · ui::button( | `ui::button` (Secondary) | Button secondary |
| `launcher_ui.rs:451` | build_launcher · .id("launch-model") | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `launcher_ui.rs:512` | build_launcher · .id(id(format!("launch-model-{at}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `launcher_ui.rs:654` | build_launcher ·  / “What the agent starts with (optional)” | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `launcher_ui.rs:689` | build_launcher ·  / “Main checkout” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `launcher_ui.rs:708` | build_launcher ·  / “New worktree” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `launcher_ui.rs:757` | build_launcher · .id(id(format!("launch-worktree-{at}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `launcher_ui.rs:889` | build_launcher · .id("launch-more") | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `launcher_ui.rs:942` | build_launcher · .id(id(format!("launch-after-{at}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `launcher_ui.rs:991` | build_launcher ·  / “Start now” | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `launcher_ui.rs:1001` | build_launcher ·  / “Queue as task” | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `launcher_ui.rs:1068` | build_launcher · .id("launch-confirm-remember") | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `launcher_ui.rs:1081` | build_launcher · .id("launch-confirm-remember") / “Cancel” | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `launcher_ui.rs:1094` | build_launcher · ui::button( / “Start ↩” | `ui::button` (Primary) | Button primary (one per surface; trailing; Return) |
| `launcher_ui.rs:1120` | build_launcher ·  / “Cancel” | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `launcher_ui.rs:1136` | build_launcher · ui::button( / “Start & new ⌥⌘↩” | `ui::button` (Secondary) | Button secondary |
| `launcher_ui.rs:1154` | build_launcher · ui::button( | `ui::button` (Primary) | Button primary (one per surface; trailing; Return) |
| `launcher_ui.rs:1173` | build_launcher · .id("launch-options") | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `layout_offer.rs:93` | render_layout_offer · button("layout-use-new", "Use new default", ButtonKind::Seco / “Use new default” | `button` (Secondary) | Button secondary |
| `layout_offer.rs:97` | render_layout_offer · button("layout-keep", "Keep mine", ButtonKind::Ghost) / “Keep mine” | `button` (Ghost) | Button ghost |
| `lifecycle.rs:217` | render_daemon_mismatch_banner · ui::button(id, label, style, true, &theme, &tokens) | `ui::button` (?) | Button (kind per surface) |
| `modal.rs:416` | render_modal · .id("modal-checkbox") | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `modal.rs:434` | render_modal · button("modal-cancel", "Cancel".into(), ui::ButtonKind::Ghos / “Cancel” | `button` (Ghost) | Button secondary, leading (Esc) |
| `modal.rs:444` | render_modal · button( / “Cancel” | `button` (Secondary) | Button secondary, leading (Esc) |
| `modal.rs:456` | render_modal · button( | `button` (Primary) | Button primary (one per surface; trailing; Return) |
| `providers.rs:529` | render_providers · button( / “Add compatible” | `button` (Secondary) | Button secondary |
| `providers.rs:546` | render_providers · button( / “Refresh” | `button` (Secondary) | Button secondary |
| `providers.rs:556` | render_providers · button( / “Discover local” | `button` (Secondary) | Button secondary |
| `providers.rs:623` | render_providers · button( / “Edit” | `button` (Secondary) | Button secondary |
| `providers.rs:637` | render_providers · button( / “Set key…” | `button` (Secondary) | Button secondary |
| `providers.rs:649` | render_providers · button( / “Clear key” | `button` (Secondary) | Button destructive (never default) |
| `providers.rs:665` | render_providers · button( / “Models / refresh” | `button` (Secondary) | Button secondary |
| `providers.rs:679` | render_providers · button( / “Remove…” | `button` (Secondary) | Button destructive (never default) |
| `providers.rs:868` | render_provider_failure · .id("provider-failure-details") | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `recovery_summary.rs:143` | render_recovery_summary · ui::button( / “Review” | `ui::button` (Secondary) | Button secondary |
| `recovery_summary.rs:159` | render_recovery_summary · ui::button( / “Dismiss all” | `ui::button` (Ghost) | Button secondary, leading (Esc) |
| `recovery_summary.rs:199` | render_recovery_summary · ui::button( | `ui::button` (Secondary) | Button secondary |
| `recovery_summary.rs:242` | render_recovery_summary · .id(SharedString::from(format!("recovery-open-{index}"))) | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `recovery_summary.rs:285` | render_recovery_summary · ui::button( / “Dismiss” | `ui::button` (Ghost) | Button secondary, leading (Esc) |
| `recovery_summary.rs:352` | render_recovery_summary · ui::button( / “Close” | `ui::button` (Ghost) | Button secondary, leading (Esc) |
| `recovery_summary.rs:378` | render_recovery_summary · ui::button( / “Dismiss all” | `ui::button` (Secondary) | Button secondary, leading (Esc) |
| `reinstall.rs:204` | render_reinstall_card · button("reinstall-run-setup", "Run setup", ButtonKind::Secon / “Run setup” | `button` (Secondary) | Button secondary |
| `reinstall.rs:210` | render_reinstall_card · button("reinstall-not-now", "Not now", ButtonKind::Ghost) / “Not now” | `button` (Ghost) | Button secondary, leading (Esc) |
| `reinstall.rs:230` | render_whats_new · .id("whats-new") | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `rewind_view.rs:240` | render_rewind_sheet · .id(("rewind-file", index)) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `rewind_view.rs:251` | render_rewind_sheet · .id(("rewind-check", index)) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `rewind_view.rs:490` | render_rewind_sheet · button( / “Interrupt it” | `button` (Secondary) | Button secondary |
| `rewind_view.rs:515` | render_rewind_sheet · .id("rewind-note") | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `rewind_view.rs:539` | render_rewind_sheet · button( / “Cancel” | `button` (Ghost) | Button secondary, leading (Esc) |
| `rewind_view.rs:554` | render_rewind_sheet · button( | `button` (Secondary) | Button secondary |
| `rewind_view.rs:570` | render_rewind_sheet · button( | `button` (Primary) | Button primary (one per surface; trailing; Return) |
| `rewind_view.rs:703` | render_checkpoints_list · ui::button( / “Rewind…” | `ui::button` (Secondary) | Button secondary |
| `rewind_view.rs:776` | render_checkpoints_list · ui::button( / “Close” | `ui::button` (Ghost) | Button secondary, leading (Esc) |
| `rewind_view.rs:824` | render_restore_toast · ui::button( / “Undo” | `ui::button` (Ghost) | Button ghost |
| `settings_sheet.rs:1175` | render_settings_sheet · .id(SharedString::from(format!("nav-group-{}", group.title() / “ } else { ” | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_sheet.rs:1203` | render_settings_sheet · .id(SharedString::from(format!("ns-{ns}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_sheet.rs:1247` | render_settings_sheet · .id("sheet-search") | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_sheet.rs:1307` | render_settings_sheet · .id("sheet-footer") | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `setup.rs:940` | render_run_setup_row · ui::button( / “Run Setup Again…” | `ui::button` (Secondary) | Button secondary |
| `setup.rs:987` | render_setup · button("skip", "Skip setup", ButtonKind::Ghost) / “Skip setup” | `button` (Ghost) | Button secondary, leading (Esc) |
| `setup.rs:1001` | render_setup · button("skip-step", "Skip this step", ButtonKind::Ghost) / “Skip this step” | `button` (Ghost) | Button secondary, leading (Esc) |
| `setup.rs:1007` | render_setup · button("back", "Back", ButtonKind::Secondary) / “Back” | `button` (Secondary) | Button secondary, leading (Esc) |
| `setup.rs:1020` | render_setup · button( / “Continue” | `button` (Primary) | Button primary (one per surface; trailing; Return) |
| `setup.rs:1186` | setup_look · .id(id) | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `setup.rs:1237` | setup_look ·  / “Comfortable” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `setup.rs:1313` | setup_agents ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `setup.rs:1320` | setup_agents ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `setup.rs:1351` | setup_agents ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `setup.rs:1415` | setup_editing ·  / “Bring your agents” | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `setup.rs:1455` | setup_send_key_row ·  | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `setup.rs:1507` | setup_harness_row · ui::button( / “Log in…” | `ui::button` (?) | Button (kind per surface) |
| `setup.rs:1513` | setup_harness_row · ui::button( / “Install…” | `ui::button` (?) | Button (kind per surface) |
| `setup.rs:1519` | setup_harness_row · ui::button( / “Install guide” | `ui::button` (?) | Button (kind per surface) |
| `setup.rs:1531` | setup_harness_row ·  / “Uninstall hooks” | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `setup.rs:1609` | setup_hooks · .id(SharedString::from(format!("setup-hook-{id}"))) | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `setup.rs:1694` | setup_history · .id(SharedString::from(format!("setup-history-{id}"))) | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `setup.rs:1756` | setup_background ·  / “Search your agent sessions” | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `setup.rs:1768` | setup_background ·  | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `setup.rs:1809` | setup_background · ui::button( / “Allow notifications” | `ui::button` (Secondary) | Button secondary, leading (Esc) |
| `setup.rs:1825` | setup_background · ui::button( / “ => ” | `ui::button` (Secondary) | Button secondary, leading (Esc) |
| `setup.rs:1890` | setup_command_line ·  / “This app has no hutan to link to. Run:” | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `setup.rs:1928` | setup_command_line · ui::button( / “Copy” | `ui::button` (Secondary) | Button secondary |
| `setup.rs:1998` | setup_done · ui::button( / “Open a folder  ⌘O” | `ui::button` (Primary) | Button primary (one per surface; trailing; Return) |
| `setup.rs:2013` | setup_done · ui::button( / “New terminal” | `ui::button` (Secondary) | Button secondary |
| `start_failure.rs:281` | start_failure_buttons · ui::button(id, label, style, true, &theme, &tokens) | `ui::button` (?) | Button (kind per surface) |

## Settings pages (74)

| Site | Element (id / label) | Current look | Target |
|---|---|---|---|
| `settings_assistant.rs:768` | render_mawas_grants · let revoke = small_button( / “For the chat “{title}”” | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_assistant.rs:792` | render_mawas_grants · small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_assistant.rs:879` | render_assistant_scope · small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_assistant.rs:896` | render_assistant_scope · small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_assistant.rs:939` | render_assistant_scope · small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_assistant.rs:1008` | render_assistant_model · let head = small_button( / “{}: {label}” | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_assistant.rs:1049` | render_assistant_model ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `settings_assistant.rs:1063` | render_assistant_model ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `settings_assistant.rs:1125` | render_workspace_permission · small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_daemon.rs:932` | render_restart_banner · None => small_button("restart-now", words::RESTART_NOW, fals | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_daemon.rs:992` | render_file_banner · small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_daemon.rs:1003` | render_file_banner · small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_daemon.rs:1060` | daemon_gate · small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_daemon.rs:1361` | render_daemon_row ·  | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_daemon.rs:1398` | render_daemon_control ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `settings_daemon.rs:1425` | render_daemon_control · small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_daemon.rs:1470` | render_daemon_dropdown · let head = small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_daemon.rs:1493` | render_daemon_dropdown ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `settings_daemon.rs:1540` | render_daemon_box · .id(SharedString::from(format!("dinput-{key}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_daemon.rs:1598` | render_daemon_list · .id(SharedString::from(format!("ditem-{key}-{index}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_daemon.rs:1604` | render_daemon_list · .id(SharedString::from(format!("ditem-{key}-{index}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_daemon.rs:1616` | render_daemon_list · .id(SharedString::from(format!("dremove-{key}-{index}"))) | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `settings_daemon.rs:1637` | render_daemon_list · let head = small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_daemon.rs:1658` | render_daemon_list · let head = small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_daemon.rs:1692` | render_daemon_list · small_button( / “+ add” | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_daemon.rs:1764` | render_daemon_map · .id(SharedString::from(format!("dvar-value-{key}-{name}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_daemon.rs:1774` | render_daemon_map · .id(SharedString::from(format!("dvar-remove-{key}-{name}"))) | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `settings_daemon.rs:1792` | render_daemon_map · small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_keys.rs:565` | render_no_match · super::settings_rows::small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_mcp.rs:519` | render_mcp_rows · let add = small_button("mcp-add", words::MCP_ADD, false, &th | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_mcp.rs:575` | render_mcp_server · small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_mcp.rs:592` | render_mcp_server · small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_mcp.rs:688` | render_mcp_form · base.id(SharedString::from(format!("mcp-field-{}", field.id( | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_mcp.rs:716` | render_mcp_form · small_button(id, label, on, &theme, &tokens) | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_mcp.rs:752` | render_mcp_form · small_button(id, label, on, &theme, &tokens) | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_mcp.rs:831` | render_mcp_form · small_button("mcp-save", save_label, true, &theme, &tokens) | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_mcp.rs:837` | render_mcp_form · small_button("mcp-cancel", words::MCP_CANCEL, false, &theme, | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_pages.rs:333` | render_history_indexing · let toggle = small_button( / “Off” | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_pages.rs:435` | render_data_page · let reveal = ui::button( | `ui::button` (Secondary) | Button secondary |
| `settings_pages.rs:446` | render_data_page · let open = ui::button( | `ui::button` (Secondary) | Button secondary |
| `settings_pages.rs:521` | render_data_page · let restart = ui::button( | `ui::button` (Secondary) | Button secondary |
| `settings_pages.rs:534` | render_data_page · let stop = ui::button( | `ui::button` (Secondary) | Button destructive (never default) |
| `settings_pages.rs:609` | render_integrations · ui::button( | `ui::button` (Secondary) | Button secondary |
| `settings_pages.rs:806` | render_exemptions · let revoke = ui::button( | `ui::button` (Secondary) | Button destructive (never default) |
| `settings_pages.rs:831` | render_exemptions · let all = ui::button( | `ui::button` (Secondary) | Button secondary |
| `settings_pages.rs:931` | render_keybindings · let open = ui::button( | `ui::button` (Secondary) | Button secondary |
| `settings_pages.rs:942` | render_keybindings · let reload = ui::button( | `ui::button` (Secondary) | Button secondary |
| `settings_pages.rs:1128` | render_map_control · .id(SharedString::from(format!("env-value-{index}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_pages.rs:1162` | render_map_control · .id(SharedString::from(format!("env-remove-{index}"))) / “{name} =” | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `settings_pages.rs:1202` | render_map_control · small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_pages.rs:1414` | render_presets_control · small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_pages.rs:1438` | render_presets_control · small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_pages.rs:1462` | render_presets_control · let save = small_button("preset-save", words::PRESET_SAVE, f | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_rows.rs:505` | render_field_row · let head = small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_rows.rs:528` | render_field_row ·  | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_rows.rs:558` | render_field_row · .id(SharedString::from(format!("input-{key}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_rows.rs:650` | field_reset ·  | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_rows.rs:669` | render_toggle ·  | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_rows.rs:696` | render_toggle · small_button( / “Open Login Items” | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_rows.rs:779` | render_list_control · .id(SharedString::from(format!("item-{key}-{index}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_rows.rs:786` | render_list_control · .id(SharedString::from(format!("item-{key}-{index}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_rows.rs:797` | render_list_control · .id(SharedString::from(format!("remove-{key}-{index}"))) | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `settings_rows.rs:817` | render_list_control · let head = small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_rows.rs:838` | render_list_control · let head = small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_rows.rs:872` | render_list_control · small_button( / “+ add” | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_rows.rs:932` | render_sheet_actions ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `settings_rows.rs:954` | render_sheet_actions · super::ui::button( | `ui::button` (Ghost) | Button ghost |
| `settings_themes.rs:702` | render_pack_row · let head = small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `settings_themes.rs:800` | render_pack_picker · .id(SharedString::from(more_tag.clone())) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_themes.rs:1027` | render_pack_option ·  | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `settings_themes.rs:1126` | render_pack_actions ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `settings_themes.rs:1137` | render_pack_actions ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `settings_themes.rs:1148` | render_pack_actions ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `settings_themes.rs:1159` | render_pack_actions ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |

## Chat (58)

| Site | Element (id / label) | Current look | Target |
|---|---|---|---|
| `chat/attach.rs:672` | render_mention_hint · button( / “Attach ↹” | `button` (Secondary) | Button secondary |
| `chat/attach.rs:691` | render_attach_button · super::widgets::round_button( | `round_button` | IconButton `.round()` (primary send/stop, secondary queue) |
| `chat/attach.rs:758` | stepper · button( | `button` (Secondary) | Button secondary |
| `chat/attach.rs:770` | stepper · button( | `button` (Secondary) | Button secondary |
| `chat/attach.rs:817` | render_attach · .id("chat-attach-close") | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `chat/attach.rs:856` | render_attach_menu · button( | `button` (Secondary) | Button secondary |
| `chat/attach.rs:872` | render_attach_menu · button( / “Attach file or image…” | `button` (Secondary) | Button secondary |
| `chat/attach.rs:995` | render_attach_form · button( / “Recent output” | `button` (Secondary) | Button secondary |
| `chat/attach.rs:1007` | render_attach_form · button( / “Blocks” | `button` (Secondary) | Button secondary |
| `chat/attach.rs:1084` | render_attach_form · button( / “Attach block” | `button` (Secondary) | Button secondary |
| `chat/attach.rs:1139` | render_attach_form · let attach = button( / “Attach” | `button` (Secondary) | Button secondary |
| `chat/attach.rs:1224` | confirm_button · button( | `button` (Primary) | Button primary (one per surface; trailing; Return) |
| `chat/cards.rs:271` | user_card · .id(ids.next("context")) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `chat/cards.rs:360` | reasoning_card · .id(ids.next("reasoning")) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `chat/cards.rs:462` | tool_card · .id(ids.next("tool-header")) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `chat/cards.rs:541` | command_body · .id(ids.next("output-more")) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `chat/cards.rs:568` | command_body · .id(ids.next("output-less")) / “… the output was cut by the daemon” | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `chat/cards.rs:633` | file_change_body · .id(ids.next("file")) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `chat/cards.rs:687` | file_change_body · .id(ids.next("open")) / “Open {}” | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `chat/cards.rs:757` | other_body · .id(ids.next("args")) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `chat/cards.rs:837` | turn_diff_card · .id(ids.next("turn-diff")) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `chat/cards.rs:1008` | approval_card ·  | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `chat/markdown_view.rs:197` | inline_text ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `chat/markdown_view.rs:241` | code_block · .id(copy_id) | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `chat/question_card.rs:477` | question_card · button( / “Submit ⌘↩” | `button` (Primary) | Button primary (one per surface; trailing; Return) |
| `chat/question_card.rs:490` | question_card · button( / “Decline” | `button` (Secondary) | Button secondary |
| `chat/question_card.rs:503` | question_card · button( / “Cancel” | `button` (Secondary) | Button secondary, leading (Esc) |
| `chat/question_card.rs:550` | question_card · button( / “Copy” | `button` (Secondary) | Button secondary |
| `chat/question_card.rs:605` | question_field · .id(SharedString::from(format!("question-field-{id}-{at}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `chat/question_card.rs:622` | question_field ·  / “No” | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `chat/question_card.rs:663` | question_field ·  | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `chat/queue_view.rs:77` | render_queue · button("chat-queue-clear", "Clear", ButtonKind::Ghost, true, / “Clear” | `button` (Ghost) | Button destructive (never default) |
| `chat/queue_view.rs:125` | render_queue · .id(SharedString::from(format!("chat-queue-item-{index}"))) / “Edit” | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `chat/queue_view.rs:132` | render_queue ·  / “Edit” | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `chat/queue_view.rs:144` | render_queue ·  | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `chat/retry.rs:159` | footer_retry · button( / “Retry now” | `button` (Secondary) | Button secondary |
| `chat/rich_view.rs:246` | render_context_row · .id(SharedString::from(format!("chat-context-chip-{index}")) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `chat/rich_view.rs:275` | render_context_row · .id(SharedString::from(format!("chat-context-remove-{index}" / “· refused: {reason}” | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `chat/view.rs:864` | render_header · .id("chat-switch-model") | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `chat/view.rs:961` | render_header · .id("chat-menu") | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `chat/view.rs:1004` | provider_error_action · button( / “Set key…” | `button` (Secondary) | Button secondary |
| `chat/view.rs:1020` | provider_error_action · button( / “Pick another model…” | `button` (Secondary) | Button secondary |
| `chat/view.rs:1071` | render_banner · .id("chat-error-dismiss") | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `chat/view.rs:1147` | render_recovery · button( / “Retry last message” | `button` (Secondary) | Button secondary |
| `chat/view.rs:1159` | render_recovery · button( / “Dismiss” | `button` (Ghost) | Button secondary, leading (Esc) |
| `chat/view.rs:1217` | render_earlier · let load = button( / “Load earlier” | `button` (Secondary) | Button secondary |
| `chat/view.rs:1273` | render_jump · .id("chat-jump-latest") | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `chat/view.rs:1347` | render_send_key_offer ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `chat/view.rs:1375` | render_send_key_offer · .id("chat-send-key-dismiss") / “Which key sends a message?” | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `chat/view.rs:1493` | render_composer ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `chat/view.rs:1528` | render_composer · .id(SharedString::from(format!("chat-chip-remove-{index}"))) | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `chat/view.rs:1556` | render_composer · let send = round_button( | `round_button` | IconButton `.round()` (primary send/stop, secondary queue) |
| `chat/view.rs:1570` | render_composer · let interrupt = round_button( | `round_button` | IconButton `.round()` (primary send/stop, secondary queue) |
| `chat/view.rs:1586` | render_composer · let queue_button = round_button( | `round_button` | IconButton `.round()` (primary send/stop, secondary queue) |
| `chat/view.rs:1599` | render_composer · let send_now = button( | `button` (Ghost) | Button ghost |
| `chat/view.rs:1605` | render_composer · let save = button("chat-edit-save", "Save", ButtonKind::Prim / “Save” | `button` (Primary) | Button primary (one per surface; trailing; Return) |
| `chat/view.rs:1610` | render_composer · let cancel = button("chat-edit-cancel", "Cancel", ButtonKind / “Cancel” | `button` (Ghost) | Button secondary, leading (Esc) |
| `chat/view.rs:1615` | render_composer · let restart = button("chat-restart", "Restart", ButtonKind:: / “Restart” | `button` (Secondary) | Button secondary |

## Editor bars (34)

| Site | Element (id / label) | Current look | Target |
|---|---|---|---|
| `agents.rs:594` | render_agent_picker · button( / “Direct model · {}” | `button` (Secondary) | Button secondary |
| `agents.rs:635` | render_agent_picker · .id(ElementId::from(SharedString::from(format!( | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `agents.rs:794` | render_agent_picker · button( / “Cancel” | `button` (Ghost) | Button secondary, leading (Esc) |
| `agents.rs:813` | render_agent_picker · button( / “Switch ↩” | `button` (Primary) | Button primary (one per surface; trailing; Return) |
| `file_view.rs:416` | render_agent_bar · let reject = ui::button( | `ui::button` (Secondary) | Button destructive (never default) |
| `file_view.rs:440` | render_agent_bar · let accept = ui::button( / “Accept All” | `ui::button` (Ghost) | Button ghost |
| `file_view.rs:462` | render_agent_bar · .id("file-agent-follow") | ad-hoc `div().id().on_click` | Button compact (ghost) |
| `file_view.rs:516` | render_agent_bar · icon_button( / “Previous change (⌘⌥[)” | `icon_button` | IconButton (ghost, 24 compact or 28) |
| `file_view.rs:526` | render_agent_bar · icon_button( / “Next change (⌘⌥])” | `icon_button` | IconButton (ghost, 24 compact or 28) |
| `file_view.rs:1180` | render_find_bar ·  / “Match case” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `file_view.rs:1184` | render_find_bar ·  / “Regular expression” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `file_view.rs:1187` | render_find_bar ·  / “, ” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `file_view.rs:1204` | render_find_bar ·  / “Previous match (⇧Return)” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `file_view.rs:1212` | render_find_bar ·  / “Next match (Return)” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `file_view.rs:1220` | render_find_bar ·  / “Close (Escape)” | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `file_view.rs:1300` | render_replace_bar · ui::button( / “Replace this match (Return)” | `ui::button` (Secondary) | Button secondary |
| `file_view.rs:1319` | render_replace_bar · ui::button( / “Replace All” | `ui::button` (Secondary) | Button secondary |
| `file_view.rs:1368` | render_conflict_bar · ui::button( / “Take the file from disk” | `ui::button` (Secondary) | Button secondary |
| `file_view.rs:1387` | render_conflict_bar · ui::button( / “Keep Mine (Overwrite)” | `ui::button` (Secondary) | Button secondary |
| `file_view.rs:1405` | render_conflict_bar · ui::button( / “Show Diff” | `ui::button` (Ghost) | Button ghost |
| `file_view.rs:1518` | render_state · ui::button("file-open-system", label, kind, true, theme, tok | `ui::button` (?) | Button (kind per surface) |
| `file_view.rs:1551` | render_state · ui::button( / “Open Read-Only” | `ui::button` (Primary) | Button primary (one per surface; trailing; Return) |
| `lsp_bar.rs:121` | render · let trust = ui::button( | `ui::button` (Secondary) | Button secondary |
| `lsp_bar.rs:135` | render · let not_now = ui::button( / “Not now” | `ui::button` (Ghost) | Button secondary, leading (Esc) |
| `lsp_bar.rs:175` | render · let copy = ui::button( / “Copy Command” | `ui::button` (Secondary) | Button secondary |
| `lsp_bar.rs:189` | render · let close = ui::button( / “Dismiss” | `ui::button` (Ghost) | Button secondary, leading (Esc) |
| `panels/agents.rs:402` | render ·  / “All” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `panels/agents.rs:420` | render · .id("agents-more") | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `panels/agents.rs:560` | render_head · .id(SharedString::from(format!("agents-section-{session}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/agents.rs:668` | render_row · .id(SharedString::from(format!("agents-fold-{index}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/agents.rs:766` | render_row · .id(SharedString::from(format!("agents-row-{index}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `terminals.rs:1217` | render_terminal_untracked · let track = ui::button( / “Track as {}” | `ui::button` (Ghost) | Button ghost |
| `terminals.rs:1292` | action_button · ui::button(id, label, kind, true, &style.theme, &style.densi | `ui::button` (?) | Button (kind per surface) |
| `workspaces.rs:1209` | render_picker · .id(ElementId::from(SharedString::from(format!( | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |

## Panels (162)

| Site | Element (id / label) | Current look | Target |
|---|---|---|---|
| `changes.rs:890` | render_index_filter ·  | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `changes.rs:1004` | render_header ·  / “With uncommitted” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `changes.rs:1021` | render_header · ui::button( / “Refresh” | `ui::button` (Ghost) | Button ghost |
| `changes.rs:1035` | render_header · ui::button( / “Finish…” | `ui::button` (Primary) | Button primary (one per surface; trailing; Return) |
| `changes.rs:1136` | render_merged · ui::button( / “Remove worktree and branch” | `ui::button` (?) | Button destructive (never default) |
| `changes.rs:1153` | render_merged · ui::button( / “Keep” | `ui::button` (Primary) | Button primary (one per surface; trailing; Return) |
| `changes.rs:1246` | render_file_strip · .id("changes-files-toggle") | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `changes.rs:1370` | render_files · .id(SharedString::from(format!("changes-file-{index}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `changes.rs:1430` | open_button · .id(id) | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `changes.rs:1524` | render_patch · .id(SharedString::from(format!("changes-hunk-{hunk}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `changes/delivery.rs:486` | render_review_controls · ui::button( / “Accept” | `ui::button` (Secondary) | Button secondary |
| `changes/delivery.rs:501` | render_review_controls · ui::button( / “Request changes…” | `ui::button` (Secondary) | Button secondary |
| `changes/delivery.rs:643` | render_delivery_form · ui::button( / “Cancel” | `ui::button` (Ghost) | Button secondary, leading (Esc) |
| `changes/delivery.rs:656` | render_delivery_form · ui::button( | `ui::button` (Primary) | Button primary (one per surface; trailing; Return) |
| `changes/delivery.rs:801` | render_scope · ui::button( / “Commit all…” | `ui::button` (Secondary) | Button secondary |
| `changes/finish.rs:660` | render_finish_form ·  / “Rebase” | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `changes/finish.rs:710` | render_finish_form ·  / “Push” | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `changes/finish.rs:751` | render_finish_form · ui::button( / “Cancel” | `ui::button` (Ghost) | Button secondary, leading (Esc) |
| `changes/finish.rs:768` | render_finish_form · ui::button( / “Finish” | `ui::button` (Primary) | Button primary (one per surface; trailing; Return) |
| `changes/finish.rs:837` | render_finished · ui::button( / “Done” | `ui::button` (Ghost) | Button ghost |
| `changes/tray.rs:1083` | render_review_tray · .id("changes-review-summary") / “Review” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `changes/tray.rs:1123` | render_review_tray · ui::button( / “Send to agent” | `ui::button` (Primary) | Button primary (one per surface; trailing; Return) |
| `changes/tray.rs:1137` | render_review_tray · ui::button( | `ui::button` (Secondary) | Button secondary |
| `changes/tray.rs:1194` | render_target_menu · .id(SharedString::from(format!("changes-review-target-{index | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `changes/tray.rs:1235` | render_target_menu · .id("changes-review-new-agent") / “No agent works in this checkout” | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `changes/tray.rs:1295` | render_review_list · .id(SharedString::from(format!("changes-review-batch-{n}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `changes/tray.rs:1356` | render_review_row · .id(SharedString::from(format!("changes-review-row-{index}") | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `changes/tray.rs:1503` | review_line · .id("changes-comment-add") | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `changes/tray.rs:1645` | render_comment_box · ui::button( / “Cancel” | `ui::button` (Ghost) | Button secondary, leading (Esc) |
| `changes/tray.rs:1666` | render_comment_box · ui::button( / “Comment  {}” | `ui::button` (Primary) | Button primary (one per surface; trailing; Return) |
| `changes/tray.rs:1767` | render_comment ·  / “Edit” | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `changes/tray.rs:1772` | render_comment ·  / “Delete” | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `changes/tray.rs:1784` | render_comment ·  / “Send now” | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `changes/tray.rs:1795` | render_comment ·  / “Resolve” | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `panels/agents_page.rs:1050` | header · ui::button( / “Start agent…” | `ui::button` (Ghost) | Button ghost |
| `panels/agents_page.rs:1107` | offline_banner · ui::button( / “Daemon controls” | `ui::button` (Ghost) | Button ghost |
| `panels/agents_page.rs:1175` | filter_band · .id("agents-more") | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `panels/agents_page.rs:1205` | filter_band · ui::button( | `ui::button` (Ghost) | Button ghost |
| `panels/agents_page.rs:1231` | filter_band · .id("agents-group-filter") | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/agents_page.rs:1322` | teaching_state · ui::button( / “Start agent…” | `ui::button` (Primary) | Button primary (one per surface; trailing; Return) |
| `panels/agents_page.rs:1340` | teaching_state · ui::button( / “Agent integrations…” | `ui::button` (Secondary) | Button secondary |
| `panels/agents_page.rs:1394` | no_match_state · ui::button( / “Clear filters” | `ui::button` (Secondary) | Button destructive (never default) |
| `panels/agents_page.rs:1412` | no_match_state · ui::button( / “Show all workspaces” | `ui::button` (Secondary) | Button secondary |
| `panels/agents_page.rs:1509` | group_row · .id(SharedString::from(format!("agents-group-{}", group.key( | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/agents_page.rs:1654` | child_row · .id(SharedString::from(format!( | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/agents_page/row.rs:142` | agent_row · .id(SharedString::from(format!("agents-changes-{session}"))) / “· {status}” | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/agents_page/row.rs:183` | agent_row · .id(SharedString::from(format!("agents-children-{session}")) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/agents_page/row.rs:337` | agent_row · ui::button( | `ui::button` (Secondary) | Button secondary |
| `panels/agents_page/row.rs:361` | agent_row · .id(SharedString::from(format!("agents-more-{session}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/agents_page/row.rs:409` | agent_row · .id(SharedString::from(format!("agents-row-{session}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/agents_page/row.rs:580` | details_block · ui::button( / “Interrupt turn” | `ui::button` (Secondary) | Button secondary |
| `panels/agents_page/row.rs:591` | details_block ·  / “Stop session…” | ad-hoc `div().id().on_click` | Button compact (ghost) |
| `panels/agents_page/row.rs:650` | review_block · let button = ui::button( | `ui::button` (?) | Button (kind per surface) |
| `panels/agents_page/row.rs:698` | review_block · ui::button( / “Answer question” | `ui::button` (Secondary) | Button secondary |
| `panels/assistant.rs:1073` | render_header · .id("assistant-scope") | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/assistant.rs:1107` | render_header · .id("assistant-more") | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `panels/assistant.rs:1171` | header_button · .id(id) | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `panels/assistant.rs:1197` | stop_button · .id("assistant-stop") | ad-hoc `div().id().on_click` | Button compact (ghost) |
| `panels/assistant.rs:1230` | mode_chip · .id("assistant-mode") | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/assistant.rs:1335` | backend_chip · .id("assistant-backend") | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/assistant.rs:1402` | render_switcher · .id(SharedString::from(id)) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/assistant.rs:1434` | render_switcher · button( / “Automatic” | `button` (Primary) | Button primary (one per surface; trailing; Return) |
| `panels/assistant.rs:1479` | render_switcher · button( / “✓ {label}” | `button` (Ghost) | Button ghost |
| `panels/assistant.rs:1558` | render_body · let retry = button( / “Retry” | `button` (Secondary) | Button secondary |
| `panels/assistant.rs:1568` | render_body · let models = button( / “Add a provider…” | `button` (Primary) | Button primary (one per surface; trailing; Return) |
| `panels/assistant.rs:1624` | render_body · button( / “Use OMP” | `button` (Primary) | Button primary (one per surface; trailing; Return) |
| `panels/assistant.rs:1664` | render_body · let change = button( / “Change backend…” | `button` (Primary) | Button secondary, leading (Esc) |
| `panels/assistant.rs:1679` | render_body · button( / “Install…” | `button` (Primary) | Button primary (one per surface; trailing; Return) |
| `panels/assistant_rail.rs:359` | render_rail ·  / “Review what needs you” | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `panels/assistant_rail.rs:385` | render_rail · .id(SharedString::from(format!("today-working-{index}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/assistant_rail.rs:435` | render_rail ·  / “State unavailable · {}” | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `panels/assistant_rail.rs:488` | render_rail · .id("assistant-today") / “Memory” | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/assistant_rail.rs:569` | render_needs_row · button( | `button` (?) | Button (kind per surface) |
| `panels/assistant_rail.rs:587` | render_needs_row · button( / “Edit & approve” | `button` (Secondary) | Button secondary |
| `panels/assistant_rail.rs:620` | render_needs_row · button( / “Open” | `button` (Secondary) | Button secondary |
| `panels/changes.rs:388` | row · .id(SharedString::from(format!("changes-row-{click_path}"))) / “ · ” | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/changes.rs:476` | render · .id("changes-refresh") | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `panels/common.rs:202` | placement_badge · .id(SharedString::from(format!("placement-badge-{session}")) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/common.rs:426` | panel_header_with · .id(SharedString::from(format!("gear-{namespace}"))) | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `panels/files.rs:953` | render_rows · .id(("files-row", index)) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/files.rs:1047` | render · .id("files-show-ignored") | ad-hoc `div().id().on_click` | Button compact (ghost) |
| `panels/history_view.rs:229` | render_header · icon_button( / “Group by” | `icon_button` | IconButton (ghost, 24 compact or 28) |
| `panels/history_view.rs:245` | render_header · icon_button( / “Sources” | `icon_button` | IconButton (ghost, 24 compact or 28) |
| `panels/history_view.rs:258` | render_header · icon_button( / “Refresh” | `icon_button` | IconButton (ghost, 24 compact or 28) |
| `panels/history_view.rs:270` | render_header · icon_button( / “History settings” | `icon_button` | IconButton (ghost, 24 compact or 28) |
| `panels/history_view.rs:359` | render_disabled · .id(("history-card-source", index)) / “Claude Code” | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/history_view.rs:451` | render_disabled · button( / “Enable” | `button` (Primary) | Button primary (one per surface; trailing; Return) |
| `panels/history_view.rs:465` | render_disabled · button( / “Not now” | `button` (Ghost) | Button secondary, leading (Esc) |
| `panels/history_view.rs:475` | render_disabled · .id("history-settings-link") | ad-hoc `div().id().on_click` | Button compact (ghost) |
| `panels/history_view.rs:528` | render_short_card · button( / “Set up history…” | `button` (Secondary) | Button secondary |
| `panels/history_view.rs:636` | render_scope_bar · .id(name) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/history_view.rs:730` | render_strip · .id("history-scan") | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `panels/history_view.rs:818` | render_index_details · button( / “Rebuild” | `button` (Secondary) | Button secondary |
| `panels/history_view.rs:830` | render_index_details · button( / “Cancel” | `button` (Secondary) | Button secondary, leading (Esc) |
| `panels/history_view.rs:852` | render_index_details · button( / “Clear…” | `button` (Secondary) | Button destructive (never default) |
| `panels/history_view.rs:866` | render_index_details · button( / “Rebuild…” | `button` (Secondary) | Button secondary |
| `panels/history_view.rs:880` | render_index_details · button( / “Disable” | `button` (Secondary) | Button secondary |
| `panels/history_view.rs:971` | render_list_bar · .id("history-sort") | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `panels/history_view.rs:1099` | render_list_entry · button( / “Show {remaining} more” | `button` (Secondary) | Button secondary |
| `panels/history_view.rs:1137` | render_group · .id(("history-group-row", ix)) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/history_view.rs:1276` | render_row · .id(("history-row", index)) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/history_view.rs:1325` | render_row · .id(("history-more", index)) / “Actions” | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/history_view.rs:1409` | render_details · let mut b = button( | `button` (?) | Button (kind per surface) |
| `panels/history_view.rs:1592` | render_menu · .id(("history-menu-row", i)) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/inspector.rs:202` | session_link · .id(id.into()) / “a session that is gone” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `panels/inspector.rs:283` | approval_row ·  | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/inspector.rs:368` | render · .id(SharedString::from(format!("inspector-tab-{tab}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/inspector.rs:516` | render · .id(SharedString::from(format!("inspector-block-{}", row.id) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/memory.rs:817` | render_header ·  / “This workspace” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `panels/memory.rs:847` | render_header ·  / “Show archived” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `panels/memory.rs:884` | render_header · button( / “Add” | `button` (Primary) | Button primary (one per surface; trailing; Return) |
| `panels/memory.rs:956` | render_proposal · let approve = button( / “Approve” | `button` (Primary) | Button primary (one per surface; trailing; Return) |
| `panels/memory.rs:969` | render_proposal · let edit = button( / “Edit & approve” | `button` (Secondary) | Button secondary |
| `panels/memory.rs:982` | render_proposal · let reject = button( / “Reject” | `button` (Secondary) | Button destructive (never default) |
| `panels/memory.rs:1056` | render_editor · .id("memory-editor") | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/memory.rs:1071` | render_editor ·  | ad-hoc `div().id().on_click` | Button compact (secondary or ghost; destructive for delete/uninstall) |
| `panels/memory.rs:1141` | render_editor · button( / “Save” | `button` (Primary) | Button primary (one per surface; trailing; Return) |
| `panels/memory.rs:1146` | render_editor · button("memory-cancel", "Cancel", ButtonKind::Secondary, tru / “Cancel” | `button` (Secondary) | Button secondary, leading (Esc) |
| `panels/memory.rs:1208` | render_entry · button( | `button` (Ghost) | Button ghost |
| `panels/problems.rs:661` | render_servers · ui::button( / “Start this language server again” | `ui::button` (Ghost) | Button ghost |
| `panels/problems.rs:828` | render_rows · .id(SharedString::from(format!("problem-{index}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/problems.rs:903` | render · .id("problems-severity") | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `panels/problems.rs:920` | render · .id("problems-refresh") / “Read the problems again” | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `panels/resources.rs:667` | header_cell · .id(SharedString::from(format!("resources-header-{region}")) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/resources.rs:808` | render_line · .id(SharedString::from(format!("resources-error-{index}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/resources.rs:867` | render_process · .id(SharedString::from(format!("resources-fold-{index}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/resources.rs:1088` | render_session · .id(SharedString::from(format!("resources-placement-{index}" / “{n} procs” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `panels/search.rs:1366` | render_toggle · .id(SharedString::from(format!("search-toggle-{key}"))) / “{tip}: {}” | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/search.rs:1466` | render_rows · .id(("search-check", index)) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/search.rs:1594` | render_rows · .id(("search-row", index)) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/search.rs:1640` | render_outcome · .id(("search-skipped", n)) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/search.rs:1845` | render · .id("search-toggle-replace") / “Replace (⌘⇧H)” | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `panels/search.rs:1921` | render · let mut button = ui::button( / “Replace every ticked match (asks first)” | `ui::button` (Secondary) | Button secondary |
| `panels/search.rs:2001` | render · let yes = ui::button("search-confirm", "Replace", ButtonKind / “Replace” | `ui::button` (Primary) | Button primary (one per surface; trailing; Return) |
| `panels/search.rs:2005` | render · let no = ui::button("search-cancel", "Cancel", ButtonKind::S / “Cancel” | `ui::button` (Secondary) | Button secondary, leading (Esc) |
| `panels/sessions.rs:1047` | open_folder_empty · ui::button( / “Open folder…” | `ui::button` (Ghost) | Button ghost |
| `panels/sessions.rs:1142` | render · .id("sessions-more") | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `panels/sessions.rs:1159` | render · .id("sessions-plus") | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `panels/sessions.rs:1442` | render_container · .id(SharedString::from(format!("workspace-{id_text}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/sessions.rs:1549` | render_container · .id(SharedString::from(format!("workspace-fold-{id_text}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/sessions.rs:1607` | render_container · .id(SharedString::from(format!("workspace-start-{id_text}")) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/sessions.rs:1709` | render_row · .id(SharedString::from(format!("session-review-{id}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/sessions.rs:1837` | render_row · .id(SharedString::from(format!("session-close-{id}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/sessions.rs:1904` | render_row · .id(SharedString::from(format!("session-row-{id}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/status.rs:111` | builtin_status_items · return div().id("status-connection").into_any_element(); / “{epoch} · click for the daemon controls” | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/status.rs:139` | builtin_status_items ·  / “Active workspace · click to jump” | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `panels/status.rs:177` | builtin_status_items · return div().id("status-attention").into_any_element(); | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/status.rs:205` | builtin_status_items · return div().id("status-approvals").into_any_element(); | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/status.rs:277` | builtin_status_items · None => div().id("status-editor-diagnostics").into_any_eleme | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/status.rs:289` | builtin_status_items · None => div().id("status-editor-diagnostics").into_any_eleme / “Terminal theme (change in settings)” | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/status.rs:307` | builtin_status_items ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `panels/status.rs:383` | dock_button · .id(id) | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `panels/status.rs:482` | render_diagnostics_item ·  | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `panels/tasks.rs:370` | render · let new_button = small_button( / “+ New task” | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `panels/tasks.rs:392` | render ·  / “Done” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `panels/tasks.rs:413` | render ·  / “All workspaces” | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `panels/tasks.rs:457` | render · small_button( / “New task…” | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `panels/tasks.rs:530` | render_cascade · small_button( / “Delete them too” | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `panels/tasks.rs:545` | render_cascade · small_button( / “Keep” | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `panels/tasks.rs:626` | render_row · .id(SharedString::from(format!("tasks-row-{index}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `panels/tasks.rs:696` | render_row · small_button( | `small_button` | Button compact (secondary; `selected` for a toggled one) |
| `panels/tasks.rs:717` | render_row · small_button( / “Changes” | `small_button` | Button compact (secondary; `selected` for a toggled one) |

## Sidebars, title bar, tabs, menus (18)

| Site | Element (id / label) | Current look | Target |
|---|---|---|---|
| `render.rs:1265` | render_dock · .id(id(format!("dock-tab-{pos:?}-{panel}"))) / “dock-tab-{pos:?}-{panel}” | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `render.rs:1986` | render_tab · .id(id(format!("tab-{pane_id}-{index}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `render.rs:2067` | render_tab · .id(id(format!("tab-close-{pane_id}-{index}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `render.rs:2160` | empty_start_button · ui::button( / “Start…” | `ui::button` (Primary) | Button primary (one per surface; trailing; Return) |
| `render.rs:2331` | render_palette · .id(id(format!("palette-row-{row}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `render.rs:2435` | render_context_menu · .id(id(format!("menu-row-{i}"))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `sidebars.rs:258` | left_sidebar_top · .id(SharedString::from(format!("nav-{}", entry.key))) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |
| `sidebars.rs:350` | left_sidebar_top · .id("left-back") | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `sidebars.rs:403` | left_toolbar · button("toolbar-start", Icon::ActionAdd, "Start", "start").o / “Start” | `button` (?) | Button (kind per surface) |
| `sidebars.rs:419` | left_toolbar · button( / “Settings” | `button` (?) | Button (kind per surface) |
| `sidebars.rs:479` | left_rail ·  | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `sidebars.rs:497` | left_rail ·  / “Projects” | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `sidebars.rs:514` | left_rail ·  / “Start” | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `sidebars.rs:532` | left_rail ·  / “Settings” | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `sidebars.rs:671` | right_rail ·  | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `sidebars.rs:715` | right_rail ·  / “More” | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `sidebars.rs:753` | right_rail ·  / “Expand” | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `sidebars.rs:879` | dock_toggle_button · .id(SharedString::from(format!("titlebar-dock-{pos:?}"))) / “titlebar-dock-{pos:?}” | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |

## Terminal view (3)

| Site | Element (id / label) | Current look | Target |
|---|---|---|---|
| `terminal_view/src/view.rs:2243` | actions_bar · .id(key) | ad-hoc `div().id().on_click` | Button (kind chosen at the surface migration commit) |
| `terminal_view/src/view.rs:2379` | jump_pill · .id("hutan-terminal-jump-latest") | ad-hoc `div().id().on_click` | IconButton (ghost, tooltip) |
| `terminal_view/src/view.rs:2480` | paste_card · .id(id) | ad-hoc `div().id().on_click` | not a button: row, tab, menu item, disclosure, field or chip; keeps its own hover and selected fills |

## Totals by target

- not a button: row, tab, menu item, disclosure, field or chip: 110
- Button secondary: 74
- Button compact: 69
- Button: 61
- IconButton: 47
- Button primary: 31
- Button secondary, leading: 29
- Button ghost: 26
- Button destructive: 10
- IconButton `.round()`: 4
