//! The chrome of the two sidebars (Orca's layout, `docs/design.md` § Layout):
//!
//! - **Left**: a nav list of two rows (Agents, Mawas while `assistant.enabled`; Tasks joins them
//!   once a task exists, and any other panel the user put in the left dock), the active page
//!   below it (Projects by default; a nav item replaces the project list with its page and
//!   shows a back row), and a bottom toolbar (Start, Settings). The pages are the left dock's panels; the active one is
//!   the dock's `active`, so presets, restore and `ui` see nothing new. Agents is no page of the
//!   sidebar: it opens the Agents page (`agents_page.rs`), one tab of the center.
//!   While compact (`docks.rs`) the sidebar is a 44 pt icon rail of the same nav.
//! - **Right**: the panel body (a header with the panel's name, then the panel) next to a 40 pt
//!   activity rail of panel icons with tooltips, outside the body and always there unless hidden;
//!   panels the dock lists in `⋯` ([`Dock::more`]: Requests and Resources in the clean default)
//!   and icons that do not fit are in the More menu ([`rail_split`]). The titlebar's two dock
//!   buttons are here too.

use gpui::{
    AnyElement, ClickEvent, Context, Div, Hsla, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, SharedString, Stateful, StatefulInteractiveElement, Styled,
    Window, div, prelude::*, px,
};

use super::actions;
use super::model::{Dock, DockMode, DockPosition, DropTarget, ItemRef, ItemSource, PanelId};
use super::render::{TITLEBAR_HEIGHT, probe};
use super::style::TextTooltip;
use super::ui;
use super::view::{DragPreview, DraggedItem, Region, Workbench};
use crate::icons::{Icon, icon};
use crate::theme::DensityExt as _;

/// Nav rows are 13 px medium labels with 16 px icons, 8 px × 6 px padding, 2 px apart (Orca):
/// 30 px rows. The label's line is 18 px so the font's own line height does not stretch them.
pub const NAV_LABEL: f32 = 13.0;
pub const NAV_ICON: f32 = 16.0;
const NAV_LINE: f32 = 18.0;

/// What a nav row does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavTarget {
    /// Show Mawas Home, a tab of the center (and put the keyboard in its composer).
    Assistant,
    /// Open (or focus) the Agents page, a tab of the center.
    Agents,
    /// Show this panel as a page of the left sidebar (Memory among them, under Mawas).
    Page(PanelId),
}

/// One row of the left sidebar's nav.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavEntry {
    /// Stable name of the row (`tasks`, `agents`, `assistant`, or the panel id).
    pub key: String,
    pub label: SharedString,
    /// Stable semantic id, never a vendor filename or a text fallback.
    pub icon: Icon,
    /// A count shown as a pill (the agents with a request waiting).
    pub badge: Option<usize>,
    pub target: NavTarget,
    /// The page the row stands for is the one showing.
    pub selected: bool,
}

/// Which icons of a rail show for `panels` panels in `extent` pt of room (the width of the old
/// strip, the height of the right rail), the active one always among them, and which go into
/// the More menu: `(shown, hidden)` as indices into `panels`.
pub fn strip_split(panels: usize, active: Option<usize>, extent: f32) -> (Vec<usize>, Vec<usize>) {
    let slot = RAIL_BUTTON + RAIL_GAP;
    let room = (extent - 2.0 * RAIL_GAP).max(0.0);
    let all = (room / slot).floor() as usize;
    if panels <= all {
        return ((0..panels).collect(), Vec::new());
    }
    // One slot goes to the More button.
    let capacity = all.saturating_sub(1).max(1);
    let mut shown: Vec<usize> = (0..capacity).collect();
    if let Some(active) = active.filter(|a| *a >= capacity)
        && let Some(last) = shown.last_mut()
    {
        *last = active;
    }
    let hidden = (0..panels).filter(|i| !shown.contains(i)).collect();
    (shown, hidden)
}

/// The icons of a side dock's rail in `extent` pt of room and what its More (`⋯`) menu lists:
/// `(shown, more)` as indices into `dock.panels`, in dock order. A panel the dock lists in `⋯`
/// ([`Dock::more`]) is an icon only while it is active or `badged` (Requests while a request
/// waits); the rest are icons as long as they fit, the active one always among them. A dock
/// that lists panels in `⋯` always has the More button.
pub fn rail_split(
    dock: &Dock,
    badged: impl Fn(&PanelId) -> bool,
    extent: f32,
) -> (Vec<usize>, Vec<usize>) {
    let icons: Vec<usize> = dock
        .panels
        .iter()
        .enumerate()
        .filter(|(_, panel)| dock.on_rail(panel, badged(panel)))
        .map(|(index, _)| index)
        .collect();
    let active = dock
        .active
        .as_ref()
        .and_then(|a| icons.iter().position(|i| dock.panels.get(*i) == Some(a)));
    // Panels listed in `⋯` count as one more icon that never shows: it takes the More button's
    // slot when everything else fits, and goes into More with the overflow when it does not.
    let listed = usize::from(icons.len() < dock.panels.len());
    let (fit, _) = strip_split(icons.len() + listed, active, extent);
    let shown: Vec<usize> = fit
        .into_iter()
        .filter_map(|slot| icons.get(slot).copied())
        .collect();
    let more = (0..dock.panels.len())
        .filter(|i| !shown.contains(i))
        .collect();
    (shown, more)
}

impl Workbench {
    // ---- left ------------------------------------------------------------------------------

    /// The rows of the left nav, top to bottom, with the page the dock shows marked (and, for
    /// Agents, the page when it is the tab on screen). Agents is always there and Mawas while
    /// it is enabled; **Tasks** reveals itself with the first task (or while its page is the one
    /// showing, so the row of the page you are on never vanishes), below them, so that it moves
    /// nothing that was already there. Jumping is the filter field
    /// and ⌘P, not a row.
    pub fn nav_entries(&self, cx: &gpui::App) -> Vec<NavEntry> {
        let dock = &self.state.docks.left;
        let page = dock.active.as_ref();
        let selected = |panel: &str| page.is_some_and(|a| a.as_str() == panel);
        let mut entries = Vec::new();
        entries.extend([NavEntry {
            key: "agents".to_owned(),
            label: "Agents".into(),
            icon: Icon::NavAgents,
            // Agent sessions with a request waiting: the page's own count, not every session's.
            badge: Some(self.sessions.read(cx).agent_presentation().requests()).filter(|n| *n > 0),
            target: NavTarget::Agents,
            selected: self.state.active_center_item().as_ref()
                == Some(&ItemRef::panel(super::model::AGENTS_PAGE)),
        }]);
        if self.assistant_enabled() {
            // Home is a tab of the center: the row is selected while that tab is the one shown.
            let home = ItemRef::panel("assistant");
            entries.push(NavEntry {
                key: "assistant".to_owned(),
                label: "Mawas".into(),
                icon: Icon::NavMawas,
                badge: None,
                target: NavTarget::Assistant,
                selected: self.state.active_center_item().as_ref() == Some(&home),
            });
            // Memory is a body of this sidebar: the row is selected while it replaces Projects.
            entries.push(NavEntry {
                key: super::model::MEMORY_PAGE.to_owned(),
                label: "Memory".into(),
                icon: Icon::PanelMawasMemory,
                badge: None,
                target: NavTarget::Page(PanelId::new(super::model::MEMORY_PAGE)),
                selected: selected(super::model::MEMORY_PAGE),
            });
        }
        // Tasks has a row once there is a task (or while its page is open). It goes below the
        // others: appearing with the first terminal, it moves nothing that was already there.
        if selected("tasks") || !self.sessions.read(cx).tasks().is_empty() {
            entries.push(NavEntry {
                key: "tasks".to_owned(),
                label: "Tasks".into(),
                icon: Icon::NavTasks,
                badge: None,
                target: NavTarget::Page(PanelId::new("tasks")),
                selected: selected("tasks"),
            });
        }
        // Whatever else the user keeps in the left dock stays reachable. Files (E-13a) and
        // Search (E-13b) have their keys and commands (⌘⇧E, ⌘P; ⌘⇧F), so their rows show only
        // while their page is the one shown.
        for panel in &dock.panels {
            let named = matches!(
                panel.as_str(),
                "sessions"
                    | "tasks"
                    | "assistant"
                    | super::model::AGENTS_PAGE
                    | super::model::MEMORY_PAGE
            ) || (matches!(panel.as_str(), "files" | "search")
                && !selected(panel.as_str()));
            if named {
                continue;
            }
            let (glyph, title) = self.item_icon_and_title(
                &ItemRef::Panel {
                    panel: panel.clone(),
                },
                cx,
            );
            entries.push(NavEntry {
                key: panel.to_string(),
                label: title,
                icon: glyph,
                badge: None,
                target: NavTarget::Page(panel.clone()),
                selected: selected(panel.as_str()),
            });
        }
        // One selected row: the center page on screen (Agents, Mawas Home) is the selection; a
        // page of the sidebar (Tasks, Files, Search) swaps the list below the nav and marks its
        // own row only while no center page does.
        if entries
            .iter()
            .any(|e| e.selected && matches!(e.target, NavTarget::Agents | NavTarget::Assistant))
        {
            for entry in &mut entries {
                if matches!(entry.target, NavTarget::Page(_)) {
                    entry.selected = false;
                }
            }
        }
        entries
    }

    /// Shows `panel` as the left sidebar's page, or goes back to Projects when it is showing
    /// already (a nav row toggles its page).
    pub(crate) fn toggle_left_page(&mut self, panel: &PanelId, cx: &mut Context<Self>) {
        let left = &self.state.docks.left;
        let showing = self.dock_mode(DockPosition::Left) == DockMode::Expanded
            && left.active.as_ref() == Some(panel);
        if showing {
            self.open_panel("sessions", cx);
        } else {
            self.open_panel(panel.as_str(), cx);
        }
    }

    /// The inset under the traffic lights, the nav and, on a page, the back row.
    pub(super) fn left_sidebar_top(
        &mut self,
        dock: &Dock,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = self.theme;
        let tokens = self.density.ui();
        let nav_entries = self.nav_entries(cx);
        let hover = theme.sidebar_hover;

        let rows: Vec<AnyElement> = nav_entries
            .into_iter()
            .map(|entry| {
                let key: &'static str = match entry.key.as_str() {
                    "tasks" => "tasks",
                    "agents" => "agents",
                    "assistant" => "assistant",
                    super::model::MEMORY_PAGE => "memory",
                    _ => "page",
                };
                let target = entry.target.clone();
                let dock_index = match &entry.target {
                    NavTarget::Page(panel) => dock.panels.iter().position(|p| p == panel),
                    NavTarget::Agents | NavTarget::Assistant => None,
                };
                let menu_panel = match &entry.target {
                    NavTarget::Page(panel) => Some(panel.clone()),
                    NavTarget::Assistant => Some(PanelId::new("assistant")),
                    NavTarget::Agents => None,
                };
                let selected = entry.selected;
                let color = if selected { theme.fg } else { theme.muted };
                let mut row = div()
                    .id(SharedString::from(format!("nav-{}", entry.key)))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .rounded(px(tokens.radius + 2.0))
                    .cursor_pointer()
                    .text_size(px(NAV_LABEL))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(color)
                    .when(selected, |s| s.bg(theme.sidebar_accent))
                    .when(!selected, move |s| s.hover(move |s| s.bg(hover)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.nav_click(&target, window, cx);
                    }))
                    .child(icon(entry.icon, NAV_ICON, color))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .line_height(px(NAV_LINE))
                            .child(entry.label.clone()),
                    )
                    .children(entry.badge.map(|count| {
                        div()
                            .flex_none()
                            .px(px(6.0))
                            .rounded(px(crate::theme::pill_radius()))
                            .bg(theme.warning.opacity(0.18))
                            .text_color(theme.warning)
                            .text_size(px(tokens.secondary_font_size))
                            .line_height(px(16.0))
                            .child(SharedString::from(count.to_string()))
                    }))
                    .child(probe(&self.regions, Region::Nav(key)));
                if let Some(index) = dock_index {
                    row = row.child(probe(
                        &self.regions,
                        Region::DockTab(DockPosition::Left, index),
                    ));
                }
                if let Some(panel) = menu_panel {
                    row = row.on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            let entries = this.panel_menu(&panel);
                            this.open_menu(event.position, entries, window, cx);
                        }),
                    );
                }
                row.into_any_element()
            })
            .collect();

        let mut out: Vec<AnyElement> = vec![
            // Inset for the traffic lights; also a window drag handle.
            div()
                .h(px(TITLEBAR_HEIGHT))
                .flex_none()
                .w_full()
                .on_mouse_down(MouseButton::Left, |event, window, cx| {
                    if event.click_count == 2 {
                        window.titlebar_double_click();
                    } else {
                        window.start_window_move();
                    }
                    cx.stop_propagation();
                })
                .into_any_element(),
            div()
                .id("left-nav")
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(2.0))
                .p(px(8.0))
                .children(rows)
                .into_any_element(),
        ];

        // A page other than Projects shows its way back.
        if dock
            .active
            .as_ref()
            .is_some_and(|a| a.as_str() != "sessions")
        {
            out.push(
                div()
                    .id("left-back")
                    .relative()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.0))
                    .h(px(32.0))
                    .mx(px(8.0))
                    .px(px(8.0))
                    .rounded(px(tokens.radius + 2.0))
                    .cursor_pointer()
                    .text_size(px(12.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.muted)
                    .hover(move |s| s.bg(hover).text_color(theme.fg))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_panel("sessions", cx);
                        this.focus_root(window, cx);
                    }))
                    .child(icon(Icon::ActionBack, 14.0, theme.muted))
                    .child(div().min_w_0().truncate().child("Projects"))
                    .child(probe(&self.regions, Region::Nav("projects")))
                    .into_any_element(),
            );
        }
        out
    }

    /// The bottom toolbar of the left sidebar: Start (the Start sheet, ⌘⇧T; a plain terminal is
    /// ⌘T) and Settings.
    pub(super) fn left_toolbar(&mut self) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let button = |id: &'static str, glyph: Icon, label: &'static str, region: &'static str| {
            ui::Button::new(id, label, &theme, &tokens)
                .compact()
                .ghost()
                .icon(glyph)
                .child(probe(&self.regions, Region::Toolbar(region)))
        };
        div()
            .id("left-toolbar")
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .px(px(8.0))
            .py(px(4.0))
            .border_t_1()
            .border_color(theme.border)
            // Start and Settings act; they do not take the keyboard to the dock (the dock's own
            // mouse-down does), so the sheet or the launcher hands it back to the editor or the
            // terminal it came from.
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                button("toolbar-start", Icon::ActionAdd, "Start", "start").on_click(
                    |_, window, cx| {
                        window.dispatch_action(
                            Box::new(actions::OpenLauncher(actions::LauncherOpen::default())),
                            cx,
                        );
                    },
                ),
            )
            .child(
                button(
                    "toolbar-settings",
                    Icon::ActionSettings,
                    "Settings",
                    "settings",
                )
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(actions::OpenSettings), cx);
                }),
            )
            .into_any_element()
    }

    // ---- compact left rail ---------------------------------------------------------------------

    /// What a click on a nav row or rail icon does (the Agents row opens the Agents page, every
    /// other one is where the user goes next).
    pub(crate) fn nav_click(
        &mut self,
        target: &NavTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if *target == NavTarget::Agents {
            // The page puts the keyboard in its own list.
            self.open_agents_page(super::panels::AgentsRequest::default(), window, cx);
            return;
        }
        match target {
            NavTarget::Assistant => self.open_assistant(window, cx),
            // Memory takes the keyboard (↑↓ entries, Return edits): no `focus_root` after it.
            NavTarget::Page(panel) if panel.as_str() == super::model::MEMORY_PAGE => {
                self.toggle_memory(window, cx);
                cx.notify();
                return;
            }
            NavTarget::Page(panel) => self.toggle_left_page(panel, cx),
            NavTarget::Agents => {}
        }
        self.focus_root(window, cx);
        cx.notify();
    }

    /// The 44 pt icon rail the left sidebar is down to while compact: the nav rows (Tasks once
    /// a task exists, Agents, Mawas when enabled) and whatever other page the dock holds, then
    /// Projects; Start and Settings at the bottom. Tooltips name every icon; the Agents badge
    /// keeps its count. Agents and Mawas open where they always do, without widening the
    /// sidebar; Tasks and Projects are pages of the sidebar and expand it.
    pub(super) fn left_rail(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let hover = theme.fg.opacity(0.08);
        let mut nav: Vec<AnyElement> = Vec::new();
        for entry in self.nav_entries(cx) {
            let target = entry.target.clone();
            // A page the sidebar is not showing is not selected: only the Agents page and Mawas
            // Home are somewhere the rail leaves visible.
            let selected =
                entry.selected && matches!(target, NavTarget::Agents | NavTarget::Assistant);
            let color = if selected { theme.fg } else { theme.muted };
            let face = icon(entry.icon, NAV_ICON, color).into_any_element();
            let button = rail_button(
                self,
                RailButton {
                    id: SharedString::from(format!("rail-{}", entry.key)),
                    face,
                    label: entry.label.clone(),
                    background: selected.then_some(theme.sidebar_accent),
                    badge: entry.badge,
                    region: Region::Nav(nav_region_key(&entry.key)),
                },
                hover,
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.nav_click(&target, window, cx);
            }));
            nav.push(button.into_any_element());
        }
        nav.push(
            rail_button(
                self,
                RailButton {
                    id: SharedString::from("rail-projects"),
                    face: icon(Icon::NavProjects, NAV_ICON, theme.muted).into_any_element(),
                    label: "Projects".into(),
                    background: None,
                    badge: None,
                    region: Region::Nav("projects"),
                },
                hover,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.nav_click(&NavTarget::Page(PanelId::new("sessions")), window, cx);
            }))
            .into_any_element(),
        );
        let new_session = rail_button(
            self,
            RailButton {
                id: SharedString::from("rail-start"),
                face: icon(Icon::ActionAdd, NAV_ICON, theme.muted).into_any_element(),
                label: "Start".into(),
                background: None,
                badge: None,
                region: Region::Toolbar("start"),
            },
            hover,
        )
        .on_click(|_, window, cx| {
            window.dispatch_action(
                Box::new(actions::OpenLauncher(actions::LauncherOpen::default())),
                cx,
            );
        });
        let settings = rail_button(
            self,
            RailButton {
                id: SharedString::from("rail-settings"),
                face: icon(Icon::ActionSettings, NAV_ICON, theme.muted).into_any_element(),
                label: "Settings".into(),
                background: None,
                badge: None,
                region: Region::Toolbar("settings"),
            },
            hover,
        )
        .on_click(|_, window, cx| {
            window.dispatch_action(Box::new(actions::OpenSettings), cx);
        });

        div()
            .id("left-rail")
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(DockPosition::Left.rail_width()))
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(window_drag_inset())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(RAIL_GAP))
                    .p(px(RAIL_PAD))
                    .children(nav),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(RAIL_GAP))
                    .p(px(RAIL_PAD))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(new_session)
                    .child(settings),
            )
            .child(probe(&self.regions, Region::Rail(DockPosition::Left)))
            .into_any_element()
    }

    // ---- right ---------------------------------------------------------------------------------

    /// The row over the body of the right panel: the name of the panel it shows.
    pub(super) fn right_header(&mut self, dock: &Dock, _cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let title = dock
            .active
            .as_ref()
            .map(|panel| self.panels.title(panel))
            .unwrap_or_default();
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(TITLEBAR_HEIGHT))
            .w_full()
            .px(px(12.0))
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(tokens.font_size))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme.fg)
            .child(div().min_w_0().truncate().child(title))
            .into_any_element()
    }

    /// The 40 pt activity rail of the right side, outside the body of the panel and always there
    /// unless the user hid it: one icon per panel of the dock (Changes, Agents, History in the
    /// clean default), the panels it lists in `⋯` (Requests, Resources) and icons that do not fit
    /// in the More menu, the way back to the body at the bottom. Requests comes onto the rail
    /// with its count while a request waits; it never opens the body by itself. The icon of the
    /// panel the body shows collapses the body to the rail; any other icon opens that panel (also
    /// while the body is collapsed). Icons are `muted`, the active one `fg` (on the selected fill
    /// while the body shows): the rail spends no accent.
    pub(super) fn right_rail(
        &mut self,
        dock: &Dock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let pos = DockPosition::Right;
        let accent = theme.accent;
        let status_h = self.density.ui().status_bar;
        let open = self.dock_mode(pos) == DockMode::Expanded;
        // What is left of the window's height for icons: under the inset, over the bottom
        // button and the status bar.
        let room = f32::from(window.viewport_size().height)
            - TITLEBAR_HEIGHT
            - status_h
            - RAIL_BUTTON
            - 3.0 * RIGHT_RAIL_PAD;
        let approvals = self.sessions.read(cx).approvals().len();
        let badge_of =
            |panel: &PanelId| (panel.as_str() == "inspector" && approvals > 0).then_some(approvals);
        let (shown, hidden) = rail_split(dock, |panel| badge_of(panel).is_some(), room);
        let append_index = dock.panels.len();
        let hover = theme.right_sidebar_accent;

        let buttons: Vec<AnyElement> = shown
            .into_iter()
            .filter_map(|index| dock.panels.get(index).map(|panel| (index, panel.clone())))
            .map(|(index, panel)| {
                let active = dock.active.as_ref() == Some(&panel);
                let pressed = active && open;
                let color = if active { theme.fg } else { theme.muted };
                let (glyph, title) = self.item_icon_and_title(
                    &ItemRef::Panel {
                        panel: panel.clone(),
                    },
                    cx,
                );
                let dragged = DraggedItem {
                    source: ItemSource::Dock {
                        pos,
                        panel: panel.clone(),
                    },
                    item: ItemRef::Panel {
                        panel: panel.clone(),
                    },
                    label: title.clone(),
                };
                let click_panel = panel.clone();
                let menu_panel = panel.clone();
                let face = icon(glyph, 16.0, color).into_any_element();
                rail_button(
                    self,
                    RailButton {
                        id: SharedString::from(format!("strip-{panel}")),
                        face,
                        label: title,
                        background: pressed.then_some(theme.right_sidebar_accent),
                        badge: badge_of(&panel),
                        region: Region::DockTab(pos, index),
                    },
                    hover,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.click_rail_icon(pos, &click_panel, window, cx);
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        let entries = this.panel_menu(&menu_panel);
                        this.open_menu(event.position, entries, window, cx);
                    }),
                )
                .on_drag(dragged, |dragged: &DraggedItem, _, _, cx| {
                    let style = super::style::UiStyle::get(cx);
                    let label = dragged.label.clone();
                    cx.new(|_| DragPreview { label, style })
                })
                .drag_over::<DraggedItem>(move |s, _, _, _| s.border_t_2().border_color(accent))
                .can_drop(|any, _, _| {
                    any.downcast_ref::<DraggedItem>()
                        .is_some_and(|d| d.item.as_panel().is_some())
                })
                .on_drop(cx.listener(move |this, d: &DraggedItem, window, cx| {
                    this.drop_item(d, DropTarget::Dock { pos, index }, window, cx);
                }))
                .into_any_element()
            })
            .collect();

        let more = (!hidden.is_empty()).then(|| {
            let overflow: Vec<PanelId> = hidden
                .into_iter()
                .filter_map(|i| dock.panels.get(i).cloned())
                .collect();
            rail_button(
                self,
                RailButton {
                    id: SharedString::from("strip-more"),
                    face: icon(Icon::ActionMore, 16.0, theme.muted).into_any_element(),
                    label: "More".into(),
                    background: None,
                    badge: None,
                    region: Region::StripMore,
                },
                hover,
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                let entries = overflow
                    .iter()
                    .map(|panel| {
                        let title = this.panels.title(panel);
                        let target = panel.clone();
                        Self::entry(title, move |this, window, cx| {
                            this.click_rail_icon(pos, &target, window, cx);
                        })
                    })
                    .collect();
                this.open_menu(event.position(), entries, window, cx);
            }))
            .into_any_element()
        });

        let collapse_hint = key_hint_of(window, &actions::ToggleRightDock);
        let collapse = rail_button(
            self,
            RailButton {
                id: SharedString::from("rail-collapse"),
                face: icon(
                    Icon::DockRight,
                    16.0,
                    if open { theme.fg } else { theme.muted },
                )
                .into_any_element(),
                label: format!(
                    "{} right panel{collapse_hint}",
                    if open { "Collapse" } else { "Expand" }
                )
                .into(),
                background: None,
                badge: None,
                region: Region::Toolbar("right-collapse"),
            },
            hover,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.toggle_dock_cmd(DockPosition::Right, cx);
        }));

        div()
            .id("right-rail")
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(DockPosition::Right.rail_width()))
            .bg(theme.right_sidebar)
            .border_l_1()
            .border_color(theme.border)
            .child(window_drag_inset())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(RAIL_GAP))
                    .p(px(RIGHT_RAIL_PAD))
                    .children(buttons)
                    .children(more),
            )
            .child(
                div()
                    .id("strip-filler")
                    .flex_1()
                    .w_full()
                    .drag_over::<DraggedItem>(move |s, _, _, _| s.bg(accent.opacity(0.18)))
                    .can_drop(|any, _, _| {
                        any.downcast_ref::<DraggedItem>()
                            .is_some_and(|d| d.item.as_panel().is_some())
                    })
                    .on_drop(cx.listener(move |this, d: &DraggedItem, window, cx| {
                        this.drop_item(
                            d,
                            DropTarget::Dock {
                                pos,
                                index: append_index,
                            },
                            window,
                            cx,
                        );
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .p(px(RIGHT_RAIL_PAD))
                    .child(collapse),
            )
            .child(probe(&self.regions, Region::Rail(pos)))
            .into_any_element()
    }

    // ---- titlebar buttons ----------------------------------------------------------------------

    /// The titlebar button of a side dock, always there: with the left sidebar hidden it is the
    /// way back, next to the traffic lights; with the right rail hidden it is the recovery
    /// button. The left one hides the sidebar (or restores it); the right one does what ⌘L does.
    /// A right click offers every state with the current one checked.
    pub(super) fn dock_toggle_button(
        &mut self,
        pos: DockPosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let mode = self.dock_mode(pos);
        let (glyph, name, states): (Icon, &str, [(&str, DockMode); 3]) = match pos {
            DockPosition::Left => (
                Icon::DockLeft,
                "sidebar",
                [
                    ("Expanded", DockMode::Expanded),
                    ("Compact (icons)", DockMode::Compact),
                    ("Hidden", DockMode::Hidden),
                ],
            ),
            _ => (
                Icon::DockRight,
                "right panel",
                [
                    ("Panel open", DockMode::Expanded),
                    ("Rail only", DockMode::Compact),
                    ("Hidden", DockMode::Hidden),
                ],
            ),
        };
        let hint = match pos {
            DockPosition::Left => key_hint_of(window, &actions::ToggleLeftDock),
            _ => key_hint_of(window, &actions::ToggleRightDock),
        };
        let tip: SharedString = match (pos, mode) {
            (DockPosition::Left, DockMode::Hidden) => format!("Show {name}{hint}"),
            (DockPosition::Left, _) => format!("Hide {name}{hint}"),
            (_, DockMode::Expanded) => format!("Collapse {name}{hint}"),
            (_, DockMode::Compact) => format!("Expand {name}{hint}"),
            (_, DockMode::Hidden) => format!("Show {name}{hint}"),
        }
        .into();
        div()
            .id(SharedString::from(format!("titlebar-dock-{pos:?}")))
            .relative()
            .flex_none()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    let entries = states
                        .iter()
                        .map(|(label, state)| {
                            let state = *state;
                            let mut entry = Self::entry(*label, move |this, _, cx| {
                                this.set_dock_mode_cmd(pos, state, cx);
                            });
                            entry.checked = this.dock_mode(pos) == state;
                            entry
                        })
                        .collect();
                    this.open_menu(event.position, entries, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(
                ui::IconButton::new(
                    SharedString::from(format!("titlebar-dock-button-{pos:?}")),
                    glyph,
                    tip,
                    &theme,
                    &tokens,
                )
                .compact()
                .toggled(mode == DockMode::Expanded)
                .on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| match pos {
                        DockPosition::Left => this.toggle_dock_hidden_cmd(pos, cx),
                        _ => this.toggle_dock_cmd(pos, cx),
                    },
                )),
            )
            .child(probe(&self.regions, Region::DockToggle(pos)))
            .into_any_element()
    }
}

/// Side of a rail button, the gap between two and the padding of a rail around them: 32 pt
/// targets in the 44 pt left rail (and, with 4 pt of padding, in the 40 pt right one).
const RAIL_BUTTON: f32 = 32.0;
const RAIL_GAP: f32 = 4.0;
const RAIL_PAD: f32 = 6.0;
/// The 40 pt right rail pads its 32 pt buttons by 4.
const RIGHT_RAIL_PAD: f32 = 4.0;

/// What one rail button shows.
struct RailButton {
    id: SharedString,
    face: AnyElement,
    /// Its tooltip, and what assistive technology calls it.
    label: SharedString,
    /// Set while the page or panel the button stands for is the one showing.
    background: Option<Hsla>,
    /// A count that stays visible next to the icon (never replaces it).
    badge: Option<usize>,
    region: Region,
}

/// A 32 pt icon button with a tooltip, as in both rails.
fn rail_button(wb: &Workbench, spec: RailButton, hover: Hsla) -> Stateful<Div> {
    let theme = wb.theme;
    let tokens = wb.density.ui();
    let tip = spec.label;
    div()
        .id(spec.id)
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(RAIL_BUTTON))
        .rounded(px(tokens.radius + 2.0))
        .cursor_pointer()
        .when_some(spec.background, |s, background| s.bg(background))
        .when(spec.background.is_none(), move |s| {
            s.hover(move |s| s.bg(hover))
        })
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
        .child(spec.face)
        .children(spec.badge.map(|count| {
            div()
                .absolute()
                .top(px(1.0))
                .right(px(1.0))
                .px(px(4.0))
                .rounded(px(crate::theme::pill_radius()))
                .bg(theme.warning)
                .text_color(theme.bg)
                .text_size(px(10.0))
                .line_height(px(13.0))
                .child(SharedString::from(count.to_string()))
        }))
        .child(probe(&wb.regions, spec.region))
}

/// The inset under the traffic lights and over a rail's icons; also a window drag handle.
fn window_drag_inset() -> AnyElement {
    div()
        .h(px(TITLEBAR_HEIGHT))
        .flex_none()
        .w_full()
        .on_mouse_down(MouseButton::Left, |event, window, cx| {
            if event.click_count == 2 {
                window.titlebar_double_click();
            } else {
                window.start_window_move();
            }
            cx.stop_propagation();
        })
        .into_any_element()
}

/// The static name a nav entry's region carries (`Region::Nav` takes a `&'static str`).
fn nav_region_key(key: &str) -> &'static str {
    match key {
        "tasks" => "tasks",
        "agents" => "agents",
        "assistant" => "assistant",
        super::model::MEMORY_PAGE => "memory",
        _ => "page",
    }
}

/// " (⌘L)"-style suffix for a tooltip: the key of `action` in the active keymap, or nothing.
fn key_hint_of(window: &Window, action: &dyn gpui::Action) -> String {
    window
        .highest_precedence_binding_for_action(action)
        .map(|binding| {
            let keys: Vec<String> = binding.keystrokes().iter().map(|k| k.to_string()).collect();
            format!(" ({})", keys.join(" "))
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_icons_fit_a_default_sidebar() {
        let (shown, hidden) = strip_split(5, Some(0), 350.0);
        assert_eq!(shown, vec![0, 1, 2, 3, 4]);
        assert!(hidden.is_empty());
    }

    #[test]
    fn a_narrow_sidebar_moves_icons_into_more_and_keeps_the_active_one_visible() {
        // 130 px: room for 3 slots; one is More, two icons show.
        let (shown, hidden) = strip_split(5, Some(0), 130.0);
        assert_eq!(shown, vec![0, 1]);
        assert_eq!(hidden, vec![2, 3, 4]);
        // The active tab is swapped into the last visible slot.
        let (shown, hidden) = strip_split(5, Some(4), 130.0);
        assert_eq!(shown, vec![0, 4]);
        assert_eq!(hidden, vec![1, 2, 3]);
        // Never fewer than one icon, whatever the width.
        let (shown, _) = strip_split(5, None, 20.0);
        assert_eq!(shown.len(), 1);
    }

    #[test]
    fn the_clean_rail_shows_three_icons_and_lists_requests_and_resources_in_more() {
        use crate::workbench::model::LayoutPreset;
        let dock = LayoutPreset::default_preset().docks.right;
        let none = |_: &PanelId| false;
        let names = |indices: &[usize]| -> Vec<String> {
            indices
                .iter()
                .map(|i| dock.panels[*i].to_string())
                .collect()
        };
        // Changes, Agents, History as icons; Requests and Resources behind ⋯.
        let (shown, more) = rail_split(&dock, none, 600.0);
        assert_eq!(names(&shown), ["changes", "agents", "history"]);
        assert_eq!(names(&more), ["inspector", "resources"]);
        // A waiting request brings Requests onto the rail with its count; Resources stays.
        let requests = |p: &PanelId| p.as_str() == "inspector";
        let (shown, more) = rail_split(&dock, requests, 600.0);
        assert_eq!(names(&shown), ["changes", "agents", "history", "inspector"]);
        assert_eq!(names(&more), ["resources"]);
        // The panel the body shows is always an icon.
        let mut active = dock.clone();
        active.active = Some(PanelId::new("resources"));
        let (shown, more) = rail_split(&active, none, 600.0);
        assert_eq!(names(&shown), ["changes", "agents", "history", "resources"]);
        assert_eq!(names(&more), ["inspector"]);
        // Short of room, icons join ⋯ and the More button keeps its slot (130 pt: three slots).
        let (shown, more) = rail_split(&dock, none, 130.0);
        assert_eq!(names(&shown), ["changes", "agents"]);
        assert_eq!(names(&more), ["history", "inspector", "resources"]);
        // A layout without `more` (saved before the clean default) keeps every icon.
        let mut old = dock.clone();
        old.more.clear();
        let (shown, more) = rail_split(&old, none, 600.0);
        assert_eq!(shown.len(), 5);
        assert!(more.is_empty());
    }
}
