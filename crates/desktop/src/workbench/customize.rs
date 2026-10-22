//! Customizing the workbench where the widget is (WIDGET-CONFIG, user 2026-10-22: "each widget
//! configurable"): the panel menu (⋯ in a panel's header, right click on its rail icon or tab),
//! moving, hiding and reordering panels and status items, the panel settings popover, and the
//! one path every such change takes — live in the window, written into the current layout
//! preset of `settings.toml` through the layered settings writer (provenance *You*), and
//! undoable for ten seconds from a toast or with ⌘Z.
//!
//! A built-in preset is never edited in place: the first change made while one is current is
//! written into a new user preset (*Custom*), which becomes the current one, and the toast says
//! so in one line.

use std::time::Duration;

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, Pixels, Point,
    SharedString, StatefulInteractiveElement, Styled, Window, anchored, deferred, div, point,
    prelude::*, px,
};
use hutan_settings::SettingsLayer;

use super::model::{
    AGENTS_PAGE, DockPosition, Docks, LayoutPreset, MEMORY_PAGE, PaneId, PanelDest, PanelId,
    WorkbenchState,
};
use super::registry::StatusBarLayout;
use super::render::probe;
use super::settings_keys::SheetFocus;
use super::settings_labels::namespace_title;
use super::ui::motion::{self, PresentExt as _};
use super::ui::{Button, IconButton, StyledExt as _};
use super::view::{MenuEntry, Region, Workbench, preset_label};
use crate::icons::Icon;
use crate::settings::LayoutSettings;
use crate::theme::DensityExt as _;

/// How long the Undo toast of a layout change stays, and how long ⌘Z undoes it.
pub const LAYOUT_TOAST_TIME: Duration = Duration::from_secs(10);

/// The key context the workbench has while the Undo toast of a layout change shows: ⌘Z is bound
/// in it, so a text field or a code view keeps its own ⌘Z.
pub const LAYOUT_TOAST_CONTEXT: &str = "LayoutToast";

/// The settings page (namespace) that holds the options of panel `panel`, if it has any. Panels
/// with a `panels.<id>` namespace use it; the others show the page they always had (`tasks`,
/// `history`, `resources`, `agents`, `assistant`, `notifications`). `changes` has no options of
/// its own: its menu shows placement only.
pub(crate) fn panel_settings_page(panel: &str) -> Option<&'static str> {
    Some(match panel {
        "sessions" => "panels.sessions",
        "inspector" => "panels.inspector",
        "files" => "panels.files",
        "problems" => "panels.problems",
        "search" => "panels.search",
        "tasks" => "tasks",
        "history" => "history",
        "resources" => "resources",
        "notifications" => "notifications",
        AGENTS_PAGE | "agents" => "agents",
        MEMORY_PAGE | "assistant" => "assistant",
        _ => return None,
    })
}

/// "Moved Files to the left · Undo".
#[derive(Debug, Clone)]
pub struct LayoutToast {
    pub label: SharedString,
    /// The second line: a built-in preset was left alone and its changes went to *Custom*.
    pub note: Option<SharedString>,
    rev: u64,
}

/// What undoes the last customization: the settings keys it wrote, as they were, and the
/// arrangement of the panels it touched.
#[derive(Debug)]
pub(crate) struct LayoutUndo {
    /// The preset that was current.
    preset: String,
    /// The docks' panels as they were (modes and sizes are not undone: the user may have moved
    /// those since).
    docks: Docks,
    /// The docks' modes (`visible`, `collapsed`) right after the change: a dock still in that
    /// mode when undoing goes back to the mode it had (moving a panel into the hidden bottom
    /// dock shows it); one the user changed since keeps theirs.
    after: [(bool, bool); 3],
    /// Where the panels the change named sat in the center before, by pane and tab.
    center: Vec<(PanelId, Option<(PaneId, usize)>)>,
    /// The user-layer values the change replaced, `None` when the key was not set (so undoing
    /// removes it).
    keys: Vec<(String, Option<toml::Value>)>,
    /// The change moved panels (the docks and the center are undone too), not just settings.
    docks_scope: bool,
}

/// The panel settings popover (`Panel settings…`): where it hangs and for which panel.
#[derive(Debug, Clone)]
pub struct PanelPopover {
    pub panel: PanelId,
    pub anchor: Point<Pixels>,
}

impl LayoutToast {
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }
}

/// `(visible, collapsed)` of each dock, left, right, bottom.
fn modes_of(docks: &Docks) -> [(bool, bool); 3] {
    DockPosition::ALL.map(|pos| {
        let dock = docks.get(pos);
        (dock.visible, dock.collapsed)
    })
}

/// Where `panel` sits among the center's tabs, if it does.
fn center_position(state: &WorkbenchState, panel: &PanelId) -> Option<(PaneId, usize)> {
    state.center.panes().into_iter().find_map(|pane| {
        pane.items
            .iter()
            .position(|item| item.as_panel() == Some(panel))
            .map(|index| (pane.id, index))
    })
}

impl Workbench {
    // ---- the panel menu ---------------------------------------------------------------------

    /// The menu of a panel (⋯ in its header, right click on its rail icon, nav row or dock tab):
    /// *Move to ▸ Left · Right · Bottom · Center tab*, *Hide*, and *Panel settings…* for a
    /// panel that has options. `position` is where the next menu opens.
    pub(crate) fn panel_menu(&self, panel: &PanelId, position: Point<Pixels>) -> Vec<MenuEntry> {
        let mut entries = Vec::new();
        // Memory is a body of the left sidebar however it is arranged; it has no placement.
        if panel.as_str() != MEMORY_PAGE {
            let target = panel.clone();
            entries.push(Self::submenu_entry("Move to", move |this, window, cx| {
                let entries = this.placement_entries(&target);
                this.open_menu(position, entries, window, cx);
            }));
        }
        let target = panel.clone();
        entries.push(Self::entry("Hide", move |this, _, cx| {
            this.hide_panel(&target, cx);
        }));
        if panel_settings_page(panel.as_str()).is_some() {
            let target = panel.clone();
            entries.push(Self::entry("Panel settings…", move |this, window, cx| {
                this.open_panel_settings(&target, position, window, cx);
            }));
        }
        entries
    }

    /// *Left · Right · Bottom · Center tab* with the panel's place checked.
    fn placement_entries(&self, panel: &PanelId) -> Vec<MenuEntry> {
        let here = self.state.find_panel(panel);
        let in_dock = |pos: DockPosition| matches!(&here, Some(super::model::ItemSource::Dock { pos: p, .. }) if *p == pos);
        let mut entries: Vec<MenuEntry> = DockPosition::ALL
            .into_iter()
            .map(|pos| {
                let target = panel.clone();
                let mut entry = Self::entry(pos.label(), move |this, _, cx| {
                    this.place_panel(&target, PanelDest::Dock(pos), cx);
                });
                entry.checked = in_dock(pos);
                entry
            })
            .collect();
        let target = panel.clone();
        let mut center = Self::entry("Center tab", move |this, _, cx| {
            this.place_panel(&target, PanelDest::Center, cx);
        });
        center.checked = matches!(here, Some(super::model::ItemSource::Pane { .. }));
        entries.push(center);
        entries
    }

    /// The ⋯ of a panel's header: opens its menu under the button. `site` says where the
    /// panel is (`left`, `right`, `bottom`, `center`), for tests.
    pub(crate) fn panel_menu_button(
        &self,
        panel: PanelId,
        site: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (self.theme, self.density.ui());
        div()
            .relative()
            .flex_none()
            .child(
                IconButton::new(
                    SharedString::from(format!("panel-menu-{site}")),
                    Icon::ActionMore,
                    "Panel menu",
                    &theme,
                    &tokens,
                )
                .compact()
                .on_click(cx.listener(
                    move |this, event: &ClickEvent, window, cx| {
                        let at = event.position();
                        let entries = this.panel_menu(&panel, at);
                        this.open_menu(at, entries, window, cx);
                    },
                )),
            )
            .child(probe(&self.regions, Region::PanelMenu(site)))
            .into_any_element()
    }

    // ---- placement ---------------------------------------------------------------------------

    /// *Move to*: puts `panel` in a dock or among the tabs of the center — a panel that is not
    /// open anywhere (hidden) is opened there — and keeps the arrangement in the current
    /// preset.
    pub fn place_panel(&mut self, panel: &PanelId, dest: PanelDest, cx: &mut Context<Self>) {
        let title = self.panels.title(panel);
        let label = match dest {
            PanelDest::Dock(pos) => {
                format!("Moved {title} to the {}", pos.label().to_lowercase())
            }
            PanelDest::Center => format!("Moved {title} to a center tab"),
        };
        let target = panel.clone();
        self.customize_layout(label, std::slice::from_ref(panel), cx, move |state, _| {
            if state.find_panel(&target).is_none() {
                match dest {
                    PanelDest::Dock(pos) => {
                        state.open_panel_in_dock(&target, pos);
                    }
                    PanelDest::Center => state.open_panel_in_center(&target),
                }
                return true;
            }
            state.move_panel(&target, dest).is_ok()
        });
    }

    /// *Hide*: closes `panel` wherever it is open. It stays reachable from the palette (*Panel:
    /// Open …*) and from the rail's checklist.
    pub fn hide_panel(&mut self, panel: &PanelId, cx: &mut Context<Self>) {
        let title = self.panels.title(panel);
        let target = panel.clone();
        self.customize_layout(
            format!("Hid {title}"),
            std::slice::from_ref(panel),
            cx,
            move |state, _| state.close_panel(&target).is_ok(),
        );
    }

    // ---- rails and docks ---------------------------------------------------------------------

    /// The panels a dock's checklist offers: every registered panel but the pages that live in
    /// the center (Mawas Home, the Agents page), Memory (a body of the left sidebar, shown from
    /// its nav row) and the placeholder `notifications`.
    fn checklist_panels(&self) -> Vec<(PanelId, SharedString)> {
        self.panels
            .iter()
            .filter(|spec| {
                !matches!(
                    spec.id.as_str(),
                    "assistant" | AGENTS_PAGE | MEMORY_PAGE | "notifications"
                )
            })
            .map(|spec| (spec.id.clone(), spec.title.clone()))
            .collect()
    }

    /// The checklist of the panels of dock `pos` (right click on the right rail, the bottom
    /// dock's tab strip or the left sidebar's nav): every available panel with a check while
    /// it is shown there, and *Reset layout to preset*. A choice keeps the list open at
    /// `position`, so several panels can be switched in a row.
    pub(crate) fn dock_checklist(
        &self,
        pos: DockPosition,
        position: Point<Pixels>,
    ) -> Vec<MenuEntry> {
        let mut entries = vec![Self::disabled_entry(match pos {
            DockPosition::Left => "Left sidebar pages",
            DockPosition::Right => "Right rail panels",
            DockPosition::Bottom => "Bottom panels",
        })];
        for (panel, title) in self.checklist_panels() {
            let shown = self.state.docks.find(&panel) == Some(pos);
            let mut entry = Self::entry(title, move |this, window, cx| {
                this.toggle_dock_panel(&panel, pos, cx);
                let entries = this.dock_checklist(pos, position);
                this.open_menu(position, entries, window, cx);
            });
            entry.checked = shown;
            entries.push(entry);
        }
        entries.push(self.reset_entry());
        entries
    }

    /// *Reset layout to preset*: the docks go back to what the current preset says.
    fn reset_entry(&self) -> MenuEntry {
        Self::entry(
            format!("Reset layout to {}", preset_label(&self.state.preset)),
            |this, _, cx| this.reset_to_preset(cx),
        )
    }

    /// The current preset applied again: its panels, modes and sizes, whatever the window drifted
    /// to. The center and every open session stay.
    pub fn reset_to_preset(&mut self, cx: &mut Context<Self>) {
        let name = self.state.preset.clone();
        self.apply_preset_named(&name, cx);
        // The sidebar's sections go back to theirs too.
        for key in ["nav_rows", "show_projects"] {
            if let Err(error) = self.settings.remove(SettingsLayer::User, "sidebar", key) {
                tracing::warn!("cannot reset sidebar.{key}: {error}");
            }
        }
        self.set_notice(format!("Layout reset to {}", preset_label(&name)), cx);
    }

    /// A checked panel of a checklist is hidden, an unchecked one shown in the dock.
    fn toggle_dock_panel(&mut self, panel: &PanelId, pos: DockPosition, cx: &mut Context<Self>) {
        if self.state.docks.find(panel) == Some(pos) {
            self.hide_panel(panel, cx);
        } else {
            self.show_panel(panel, pos, cx);
        }
    }

    /// Shows `panel` in dock `pos` without taking the keyboard or opening a dock the user
    /// closed: moved there when it is open elsewhere, else added at the end.
    pub fn show_panel(&mut self, panel: &PanelId, pos: DockPosition, cx: &mut Context<Self>) {
        let title = self.panels.title(panel);
        let open = self.state.find_panel(panel).is_some();
        let place = pos.label().to_lowercase();
        let label = if open {
            format!("Moved {title} to the {place}")
        } else {
            format!("Showed {title} on the {place}")
        };
        let target = panel.clone();
        self.customize_layout(label, std::slice::from_ref(panel), cx, move |state, _| {
            if open {
                return state.move_panel(&target, PanelDest::Dock(pos)).is_ok();
            }
            let dock = state.docks.get_mut(pos);
            if dock.panels.is_empty() {
                dock.visible = true;
            }
            dock.panels.push(target.clone());
            state.normalize();
            true
        });
    }

    /// A panel dragged to another place in a dock (the rail's icons, a tab of the bottom
    /// dock), to another dock, or out of one: kept like every other placement.
    pub(crate) fn drop_panel(
        &mut self,
        panel: &PanelId,
        source: super::model::ItemSource,
        target: super::model::DropTarget,
        cx: &mut Context<Self>,
    ) {
        use super::model::{DropTarget, ItemSource};
        let title = self.panels.title(panel);
        let label = match (&source, &target) {
            (ItemSource::Dock { pos: from, .. }, DropTarget::Dock { pos, .. }) if from == pos => {
                format!("Reordered the {} panels", pos.label().to_lowercase())
            }
            (_, DropTarget::Dock { pos, .. }) => {
                format!("Moved {title} to the {}", pos.label().to_lowercase())
            }
            _ => format!("Moved {title} to a center tab"),
        };
        self.customize_layout(label, std::slice::from_ref(panel), cx, move |state, _| {
            state.move_item(source, target).is_ok()
        });
    }

    // ---- the one path of a customization -----------------------------------------------------

    /// Runs `change` on the live layout and the status bar of the current preset, then writes the
    /// result into that preset (a built-in one is left alone: a new *Custom* preset takes the
    /// change), shows the Undo toast and remembers how to undo. `change` says whether it
    /// changed anything; `affected` are the panels whose place in the center ⌘Z must restore.
    /// Returns whether anything changed.
    pub(crate) fn customize_layout(
        &mut self,
        label: String,
        affected: &[PanelId],
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut WorkbenchState, &mut StatusBarLayout) -> bool,
    ) -> bool {
        self.customize(label, affected, true, cx, change)
    }

    /// [`Self::customize_layout`] for a change of the status bar alone: the preset's docks are
    /// written as they were, whatever the window drifted to, and undoing leaves the docks alone.
    pub(crate) fn customize_status(
        &mut self,
        label: String,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut StatusBarLayout) -> bool,
    ) -> bool {
        self.customize(label, &[], false, cx, move |_, status| change(status))
    }

    fn customize(
        &mut self,
        label: String,
        affected: &[PanelId],
        dock_scope: bool,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut WorkbenchState, &mut StatusBarLayout) -> bool,
    ) -> bool {
        let docks = self.state.docks.clone();
        let center: Vec<_> = affected
            .iter()
            .map(|panel| (panel.clone(), center_position(&self.state, panel)))
            .collect();
        let preset = self.state.preset.clone();
        let mut status = self.status_bar();
        if !change(&mut self.state, &mut status) {
            return false;
        }
        self.after_change(cx);

        let (target, created) = self.customizable_preset();
        let base = self
            .preset_defs
            .iter()
            .find(|def| def.layout.name == preset)
            .map(|def| def.layout.docks.clone())
            .unwrap_or_else(|| LayoutPreset::default_preset().docks);
        let mut written = base;
        for pos in DockPosition::ALL.into_iter().filter(|_| dock_scope) {
            let live = self.state.docks.get(pos);
            let dock = written.get_mut(pos);
            let was_empty = dock.panels.is_empty();
            dock.panels = live.panels.clone();
            dock.more = live.more.clone();
            // A dock the preset left empty and the change filled shows the way it shows now.
            if was_empty && !live.panels.is_empty() {
                dock.visible = live.visible;
                dock.collapsed = live.collapsed;
                dock.size = live.size;
            }
        }
        let mut keys = vec![format!("layout.presets.{target}")];
        if created {
            keys.push("layout.preset".to_owned());
        }
        let before: Vec<(String, Option<toml::Value>)> = keys
            .iter()
            .map(|key| {
                (
                    key.clone(),
                    self.settings.layer_value(SettingsLayer::User, key),
                )
            })
            .collect();
        let saved = self.write_preset(&target, &written, &status, created);
        if let Err(error) = saved {
            // The window shows the change anyway; it is just not kept.
            tracing::warn!("cannot save the layout change: {error}");
            self.set_notice(format!("Cannot save the layout change: {error}"), cx);
            return true;
        }
        if created {
            self.state.preset = target.clone();
        }
        self.refresh_layout_defs(cx);
        let note = created.then(|| {
            SharedString::from(format!(
                "{} is built in: saved in your {} preset.",
                preset_label(&preset),
                preset_label(&target)
            ))
        });
        self.toast_rev += 1;
        let rev = self.toast_rev;
        self.layout_undo = Some(LayoutUndo {
            preset,
            docks,
            after: modes_of(&self.state.docks),
            center,
            keys: before,
            docks_scope: dock_scope,
        });
        self.show_layout_toast(label, note, rev, cx);
        true
    }

    /// The preset a change is written to, and whether it is new: the current one when it is the
    /// user's own, else a fresh *Custom* (`custom`, `custom-2`, …) — a built-in preset is never
    /// edited in place.
    fn customizable_preset(&self) -> (String, bool) {
        let current = &self.state.preset;
        let built_in = LayoutPreset::builtin().iter().any(|p| p.name == *current);
        if !built_in || self.layout_seen.presets.contains_key(current) {
            return (current.clone(), false);
        }
        let taken = |name: &str| self.preset_defs.iter().any(|d| d.layout.name == name);
        let mut name = "custom".to_owned();
        let mut n = 2;
        while taken(&name) {
            name = format!("custom-{n}");
            n += 1;
        }
        (name, true)
    }

    /// Writes preset `name` (`layout.presets.<name>`), and makes it the current one
    /// (`layout.preset`) when it was just created. The bar is written only when it is not the
    /// built-in one, so a preset that never chose its bar keeps following the built-in bar.
    fn write_preset(
        &self,
        name: &str,
        docks: &Docks,
        status: &StatusBarLayout,
        created: bool,
    ) -> Result<(), String> {
        let setting = LayoutSettings::preset_from(docks, status);
        let mut value = toml::Value::try_from(setting).map_err(|e| e.to_string())?;
        if *status == StatusBarLayout::default()
            && let Some(table) = value.as_table_mut()
        {
            table.remove("status_bar");
        }
        self.settings
            .write_user(&format!("layout.presets.{name}"), value)
            .map_err(|e| e.to_string())?;
        if created {
            self.settings
                .write_user("layout.preset", toml::Value::String(name.to_owned()))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// A change of settings keys alone (an item's option, the sidebar's sections): writes each
    /// `(key, value)` into the user's `settings.toml` (`None` removes the key), shows the Undo
    /// toast and remembers the values the keys had. Returns whether it was written.
    pub(crate) fn customize_keys(
        &mut self,
        label: String,
        writes: Vec<(String, Option<toml::Value>)>,
        cx: &mut Context<Self>,
    ) -> bool {
        let before: Vec<(String, Option<toml::Value>)> = writes
            .iter()
            .map(|(key, _)| {
                (
                    key.clone(),
                    self.settings.layer_value(SettingsLayer::User, key),
                )
            })
            .collect();
        for (key, value) in &writes {
            let result = match value {
                Some(value) => self.settings.write_user(key, value.clone()),
                None => {
                    let (namespace, rest) = key.split_once('.').unwrap_or((key.as_str(), ""));
                    self.settings
                        .remove(SettingsLayer::User, namespace, rest)
                        .map(|_| ())
                }
            };
            if let Err(error) = result {
                tracing::warn!("cannot save {key}: {error}");
                self.set_notice(format!("Cannot save the change: {error}"), cx);
                return false;
            }
        }
        self.toast_rev += 1;
        let rev = self.toast_rev;
        self.layout_undo = Some(LayoutUndo {
            preset: self.state.preset.clone(),
            docks: self.state.docks.clone(),
            after: modes_of(&self.state.docks),
            center: Vec::new(),
            keys: before,
            docks_scope: false,
        });
        self.show_layout_toast(label, None, rev, cx);
        true
    }

    // ---- the Undo toast ----------------------------------------------------------------------

    fn show_layout_toast(
        &mut self,
        label: String,
        note: Option<SharedString>,
        rev: u64,
        cx: &mut Context<Self>,
    ) {
        self.layout_toast = Some(LayoutToast {
            label: label.into(),
            note,
            rev,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LAYOUT_TOAST_TIME).await;
            this.update(cx, |this, cx| {
                if this.layout_toast.as_ref().is_some_and(|t| t.rev == rev) {
                    this.layout_toast = None;
                    this.layout_undo = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// The Undo toast of the last layout change, while it shows.
    pub fn layout_toast(&self) -> Option<&LayoutToast> {
        self.layout_toast.as_ref()
    }

    /// Undo of the toast and of ⌘Z: the settings keys the change wrote go back to what they
    /// were, and so do the panels it moved.
    pub fn undo_layout_change(&mut self, cx: &mut Context<Self>) {
        let Some(undo) = self.layout_undo.take() else {
            return;
        };
        self.layout_toast = None;
        for (key, previous) in undo.keys.iter().rev() {
            let result = match previous {
                Some(value) => self.settings.write_user(key, value.clone()),
                None => {
                    let (namespace, rest) = key.split_once('.').unwrap_or((key.as_str(), ""));
                    self.settings
                        .remove(SettingsLayer::User, namespace, rest)
                        .map(|_| ())
                }
            };
            if let Err(error) = result {
                tracing::warn!("cannot undo {key}: {error}");
            }
        }
        self.refresh_layout_defs(cx);
        self.state.preset = undo.preset;
        if undo.docks_scope {
            for (index, pos) in DockPosition::ALL.into_iter().enumerate() {
                let before = undo.docks.get(pos);
                let dock = self.state.docks.get_mut(pos);
                dock.panels = before.panels.clone();
                dock.more = before.more.clone();
                dock.active = before.active.clone();
                if (dock.visible, dock.collapsed) == undo.after[index] {
                    dock.visible = before.visible;
                    dock.collapsed = before.collapsed;
                }
            }
            // Panels that sat among the center's tabs go back to their tab; the ones that did
            // not leave it.
            for (panel, position) in &undo.center {
                for pane in self.state.center.panes_mut() {
                    pane.items.retain(|item| item.as_panel() != Some(panel));
                    pane.active = pane.active.min(pane.items.len().saturating_sub(1));
                }
                if let Some((pane, index)) = position {
                    let fallback = self.state.active_pane_id();
                    let at = if self.state.pane(*pane).is_some() {
                        *pane
                    } else {
                        fallback
                    };
                    if let Some(pane) = self.state.center.pane_mut(at) {
                        let index = (*index).min(pane.items.len());
                        pane.items.insert(
                            index,
                            super::model::ItemRef::Panel {
                                panel: panel.clone(),
                            },
                        );
                    }
                }
            }
            self.state.normalize();
        }
        self.after_change(cx);
    }

    pub(crate) fn render_layout_toast(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let toast =
            self.layout_toast_exit
                .follow(self.layout_toast.as_ref(), motion::Kind::Toast, cx)?;
        let (theme, tokens) = (self.theme, self.density.ui());
        let (wanted, toast) = (toast.wanted, toast.value);
        Some(
            motion::present(
                "layout-toast-motion",
                motion::Kind::Toast,
                wanted,
                motion::Slide::Rise,
                div()
                    .id("layout-toast")
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .toast(&theme, &tokens)
                    .px(px(tokens.spacing * 3.0))
                    .py(px(tokens.spacing * 2.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(tokens.spacing * 3.0))
                            .child(toast.label.clone())
                            .child(
                                Button::new("layout-undo", "Undo", &theme, &tokens)
                                    .link()
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.undo_layout_change(cx)),
                                    )
                                    .child(probe(&self.regions, Region::LayoutToast("undo"))),
                            ),
                    )
                    .children(toast.note.clone().map(|note| {
                        div()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child(note)
                    }))
                    .child(probe(&self.regions, Region::LayoutToast("toast"))),
            )
            .into_any_element(),
        )
    }

    // ---- the panel settings popover ----------------------------------------------------------

    /// *Panel settings…*: the options of `panel` (its `panels.<id>` namespace, or the page it
    /// has) in a popover under `anchor`, drawn by the same rows as the Settings sheet.
    pub fn open_panel_settings(
        &mut self,
        panel: &PanelId,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(page) = panel_settings_page(panel.as_str()) else {
            return;
        };
        self.open_settings(Some(page), window, cx);
        if let Some(sheet) = &mut self.sheet {
            sheet.popover = Some(PanelPopover {
                panel: panel.clone(),
                anchor,
            });
            sheet.zone = SheetFocus::Controls;
        }
        cx.notify();
    }

    /// The popover is open for this panel.
    pub fn panel_settings_open(&self) -> Option<&PanelId> {
        self.sheet
            .as_ref()
            .and_then(|s| s.popover.as_ref())
            .map(|p| &p.panel)
    }

    /// *Open in Settings*: the popover becomes the full sheet on the same page.
    pub fn open_panel_settings_in_sheet(&mut self, cx: &mut Context<Self>) {
        if let Some(sheet) = &mut self.sheet
            && sheet.popover.take().is_some()
        {
            sheet.zone = SheetFocus::Search;
            cx.notify();
        }
    }

    pub(super) fn build_panel_popover(
        &mut self,
        wanted: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let sheet = self.sheet.as_ref()?;
        let popover = sheet.popover.clone()?;
        let fields = sheet.fields().to_vec();
        let scroll = sheet.scroll.clone();
        let message = sheet.message.clone();
        let page = sheet.namespace;
        let (theme, tokens) = (self.theme, self.density.ui());
        let rows: Vec<AnyElement> = fields
            .iter()
            .map(|field| self.render_field_row(field, cx))
            .collect();
        let title = SharedString::from(format!("{} settings", self.panels.title(&popover.panel)));
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap(px(tokens.spacing * 2.0))
            .h(px(tokens.sidebar_row + 8.0))
            .px(px(tokens.spacing * 3.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(title),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .child(
                        Button::new("panel-settings-open", "Open in Settings", &theme, &tokens)
                            .link()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_panel_settings_in_sheet(cx);
                            })),
                    )
                    .child(probe(&self.regions, Region::PanelSettings("open"))),
            );
        let card = div()
            .id("panel-settings-popover")
            .key_context(super::vim_ui::vim_key_context(
                "SettingsSheet",
                self.overlay_vim_mode(super::vim_ui::OverlayField::SheetValue),
            ))
            .when(wanted, |s| s.track_focus(&self.sheet_focus))
            .occlude()
            .relative()
            .w(px(420.0))
            .max_h(px(460.0))
            .flex()
            .flex_col()
            .popover(&theme, &tokens)
            .overflow_hidden()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.sheet_focus, cx);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_settings(window, cx)))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                this.sheet_key(event, window, cx);
            }))
            .on_action(
                cx.listener(|this, _: &super::actions::sheet::Dismiss, window, cx| {
                    this.sheet_dismiss(window, cx);
                }),
            )
            .child(header)
            .child(
                div()
                    .id("panel-settings-fields")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .px(px(tokens.spacing * 3.0))
                    .when(rows.is_empty(), |s| {
                        s.child(
                            div()
                                .py(px(tokens.spacing * 3.0))
                                .text_color(theme.muted)
                                .child(SharedString::from(format!(
                                    "No options here; see {} in Settings.",
                                    namespace_title(page)
                                ))),
                        )
                    })
                    .children(rows),
            )
            .when_some(message, |s, message| {
                s.child(
                    div()
                        .px(px(tokens.spacing * 3.0))
                        .py(px(tokens.spacing))
                        .text_color(theme.danger)
                        .child(SharedString::from(message)),
                )
            })
            .child(probe(&self.regions, Region::PanelSettings("popover")));
        let anchor = popover.anchor;
        Some(
            deferred(
                anchored()
                    .position(point(anchor.x, anchor.y + px(4.0)))
                    .snap_to_window_with_margin(px(8.0))
                    .child(
                        card.presented(
                            "panel-settings-motion",
                            motion::Kind::Popover,
                            wanted,
                            motion::Slide::Drop,
                        )
                        .inert(!wanted),
                    ),
            )
            .priority(10)
            .into_any_element(),
        )
    }
}
