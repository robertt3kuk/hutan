//! The chrome of the two sidebars (Orca's layout, `docs/design.md` § Layout):
//!
//! - **Left**: a nav list (Search, Tasks, Agents, Assistant when `assistant.enabled`, then any
//!   other panel the user put in the left dock), the active page below it (Projects by default;
//!   a nav item replaces the project list with its page and shows a back row), and a bottom
//!   toolbar (New session, Settings). The pages are the left dock's panels; the active one is
//!   the dock's `active`, so presets, restore and `ui` see nothing new. Agents is no page: it
//!   opens the Agent Dashboard drawer (`agents_drawer.rs`) over the center.
//! - **Right**: a 36 px strip of panel icons with tooltips; icons that do not fit move into a
//!   More menu. The active panel's body fills the rest.

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};

use super::actions;
use super::model::{Dock, DockPosition, DropTarget, ItemRef, ItemSource, PanelId};
use super::render::{TITLEBAR_HEIGHT, probe};
use super::style::TextTooltip;
use super::view::{DragPreview, DraggedItem, Region, Workbench};
use crate::icons::{Icon, icon};
use crate::theme::DensityExt as _;

/// Nav rows are 13 px medium labels with 16 px icons, 8 px × 6 px padding, 2 px apart (Orca):
/// 30 px rows. The label's line is 18 px so the font's own line height does not stretch them.
pub const NAV_LABEL: f32 = 13.0;
pub const NAV_ICON: f32 = 16.0;
const NAV_LINE: f32 = 18.0;
/// A strip icon button and the gap between two.
const STRIP_BUTTON: f32 = 28.0;
const STRIP_GAP: f32 = 4.0;

/// What a nav row does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavTarget {
    /// Open the jump palette (⌘P / ⌘J): workspaces, sessions, history.
    Search,
    /// Show the Assistant page (and put the keyboard in its composer).
    Assistant,
    /// Open or close the Agent Dashboard drawer.
    Agents,
    /// Show this panel as a page of the left sidebar.
    Page(PanelId),
}

/// One row of the left sidebar's nav.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavEntry {
    /// Stable name of the row (`search`, `tasks`, `agents`, `assistant`, or the panel id).
    pub key: String,
    pub label: SharedString,
    /// The row's icon of the set; a panel the set has no icon for shows `glyph`.
    pub icon: Option<Icon>,
    pub glyph: SharedString,
    /// A count shown as a pill (the agents that need you).
    pub badge: Option<usize>,
    pub target: NavTarget,
    /// The page (or the drawer) the row stands for is the one showing.
    pub selected: bool,
}

/// Which icons of a right strip show for `panels` in a dock `width` px wide, the active one
/// always among them, and which go into the More menu: `(shown, hidden)` as indices into
/// `panels`.
pub fn strip_split(panels: usize, active: Option<usize>, width: f32) -> (Vec<usize>, Vec<usize>) {
    let slot = STRIP_BUTTON + STRIP_GAP;
    let room = (width - 2.0 * STRIP_GAP).max(0.0);
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

impl Workbench {
    // ---- left ------------------------------------------------------------------------------

    /// The rows of the left nav, top to bottom, with the page the dock shows marked (and, for
    /// Agents, the drawer when it is open).
    pub fn nav_entries(&self, cx: &gpui::App) -> Vec<NavEntry> {
        let dock = &self.state.docks.left;
        let page = dock.active.as_ref();
        let selected = |panel: &str| page.is_some_and(|a| a.as_str() == panel);
        let mut entries = vec![
            NavEntry {
                key: "search".to_owned(),
                label: "Search".into(),
                icon: Some(Icon::Search),
                glyph: "⌕".into(),
                badge: None,
                target: NavTarget::Search,
                selected: false,
            },
            NavEntry {
                key: "tasks".to_owned(),
                label: "Tasks".into(),
                icon: Some(Icon::ListChecks),
                glyph: "☑".into(),
                badge: None,
                target: NavTarget::Page(PanelId::new("tasks")),
                selected: selected("tasks"),
            },
            NavEntry {
                key: "agents".to_owned(),
                label: "Agents".into(),
                icon: Some(Icon::LayoutDashboard),
                glyph: "⊞".into(),
                badge: Some(self.sessions.read(cx).needs_you().len()).filter(|n| *n > 0),
                target: NavTarget::Agents,
                selected: self.agents_drawer,
            },
        ];
        if self.assistant_enabled() {
            entries.push(NavEntry {
                key: "assistant".to_owned(),
                label: "Assistant".into(),
                icon: Some(Icon::Sparkles),
                glyph: "✦".into(),
                badge: None,
                target: NavTarget::Assistant,
                selected: selected("assistant"),
            });
        }
        // Whatever else the user keeps in the left dock stays reachable.
        for panel in &dock.panels {
            let named = matches!(
                panel.as_str(),
                "sessions" | "tasks" | "overview" | "assistant"
            );
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
                icon: Icon::for_panel(panel.as_str()),
                glyph,
                badge: None,
                target: NavTarget::Page(panel.clone()),
                selected: selected(panel.as_str()),
            });
        }
        entries
    }

    /// Shows `panel` as the left sidebar's page, or goes back to Projects when it is showing
    /// already (a nav row toggles its page).
    pub(crate) fn toggle_left_page(&mut self, panel: &PanelId, cx: &mut Context<Self>) {
        let showing =
            self.state.docks.left.visible && self.state.docks.left.active.as_ref() == Some(panel);
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
        let hover = theme.fg.opacity(0.08);

        let rows: Vec<AnyElement> = nav_entries
            .into_iter()
            .map(|entry| {
                let key: &'static str = match entry.key.as_str() {
                    "search" => "search",
                    "tasks" => "tasks",
                    "agents" => "agents",
                    "assistant" => "assistant",
                    _ => "page",
                };
                let target = entry.target.clone();
                let dock_index = match &entry.target {
                    NavTarget::Page(panel) => dock.panels.iter().position(|p| p == panel),
                    NavTarget::Assistant => {
                        dock.panels.iter().position(|p| p.as_str() == "assistant")
                    }
                    NavTarget::Search | NavTarget::Agents => None,
                };
                let menu_panel = match &entry.target {
                    NavTarget::Page(panel) => Some(panel.clone()),
                    NavTarget::Assistant => Some(PanelId::new("assistant")),
                    NavTarget::Search | NavTarget::Agents => None,
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
                        if target == NavTarget::Agents {
                            // The drawer holds the keyboard itself (Escape closes it).
                            this.toggle_agents_drawer(window, cx);
                            return;
                        }
                        // Any other row is where the user goes next.
                        this.agents_drawer = false;
                        match &target {
                            NavTarget::Search => {
                                window.dispatch_action(Box::new(actions::OpenJump), cx);
                            }
                            NavTarget::Assistant => {
                                if this.state.docks.left.visible
                                    && this.state.docks.left.active.as_ref().map(PanelId::as_str)
                                        == Some("assistant")
                                {
                                    this.open_panel("sessions", cx);
                                } else {
                                    this.open_assistant(window, cx);
                                }
                            }
                            NavTarget::Page(panel) => this.toggle_left_page(panel, cx),
                            NavTarget::Agents => {}
                        }
                        this.focus_root(window, cx);
                        cx.notify();
                    }))
                    .child(match entry.icon {
                        Some(glyph) => icon(glyph, NAV_ICON, color).into_any_element(),
                        None => div()
                            .flex_none()
                            .w(px(NAV_ICON))
                            .flex()
                            .justify_center()
                            .text_size(px(NAV_ICON - 2.0))
                            .text_color(color)
                            .child(entry.glyph.clone())
                            .into_any_element(),
                    })
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
                            .rounded(px(crate::theme::PILL_RADIUS))
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
                    .child(icon(Icon::ChevronLeft, 14.0, theme.muted))
                    .child(div().min_w_0().truncate().child("Projects"))
                    .child(probe(&self.regions, Region::Nav("projects")))
                    .into_any_element(),
            );
        }
        out
    }

    /// The bottom toolbar of the left sidebar: New session and Settings.
    pub(super) fn left_toolbar(&mut self) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let hover = theme.fg.opacity(0.08);
        let button = |id: &'static str, glyph: Icon, label: &'static str, region: &'static str| {
            div()
                .id(id)
                .relative()
                .flex()
                .items_center()
                .min_w_0()
                .gap(px(6.0))
                .px(px(8.0))
                .h(px(28.0))
                .rounded(px(tokens.radius))
                .cursor_pointer()
                .text_size(px(12.0))
                .text_color(theme.muted)
                .hover(move |s| s.bg(hover).text_color(theme.fg))
                .child(icon(glyph, 14.0, theme.muted))
                .child(div().min_w_0().truncate().child(label))
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
            .child(
                button("toolbar-new", Icon::Plus, "New session", "new").on_click(
                    |_, window, cx| {
                        window.dispatch_action(Box::new(actions::NewTerminal), cx);
                    },
                ),
            )
            .child(
                button("toolbar-settings", Icon::Settings, "Settings", "settings").on_click(
                    |_, window, cx| {
                        window.dispatch_action(Box::new(actions::OpenSettings), cx);
                    },
                ),
            )
            .into_any_element()
    }

    // ---- right -----------------------------------------------------------------------------

    /// The 36 px icon strip of the right sidebar.
    pub(super) fn right_strip(
        &mut self,
        dock: &Dock,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let pos = DockPosition::Right;
        let accent = theme.accent;
        let active_index = dock
            .active
            .as_ref()
            .and_then(|a| dock.panels.iter().position(|p| p == a));
        let (shown, hidden) = strip_split(dock.panels.len(), active_index, dock.size);
        let append_index = dock.panels.len();

        let buttons: Vec<AnyElement> = shown
            .into_iter()
            .filter_map(|index| dock.panels.get(index).map(|panel| (index, panel.clone())))
            .map(|(index, panel)| {
                let active = dock.active.as_ref() == Some(&panel);
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
                let tip = title.clone();
                let hover = theme.right_sidebar_accent;
                div()
                    .id(SharedString::from(format!("strip-{panel}")))
                    .relative()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(STRIP_BUTTON))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .text_size(px(15.0))
                    .text_color(if active { accent } else { theme.muted })
                    .when(active, |s| s.bg(theme.right_sidebar_accent))
                    .when(!active, move |s| {
                        s.hover(move |s| s.bg(hover).text_color(theme.fg))
                    })
                    .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let panel = click_panel.clone();
                        this.mutate(cx, |s| {
                            let _ = s.set_dock_active(pos, &panel);
                            s.focus_dock(pos);
                        });
                        this.focus_root(window, cx);
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
                    .drag_over::<DraggedItem>(move |s, _, _, _| s.border_l_2().border_color(accent))
                    .can_drop(|any, _, _| {
                        any.downcast_ref::<DraggedItem>()
                            .is_some_and(|d| d.item.as_panel().is_some())
                    })
                    .on_drop(cx.listener(move |this, d: &DraggedItem, window, cx| {
                        this.drop_item(d, DropTarget::Dock { pos, index }, window, cx);
                    }))
                    .child(match Icon::for_panel(panel.as_str()) {
                        Some(set_icon) => {
                            icon(set_icon, 16.0, if active { accent } else { theme.muted })
                                .into_any_element()
                        }
                        None => glyph.into_any_element(),
                    })
                    .child(probe(&self.regions, Region::DockTab(pos, index)))
                    .into_any_element()
            })
            .collect();

        let more = (!hidden.is_empty()).then(|| {
            let hover = theme.right_sidebar_accent;
            let overflow: Vec<PanelId> = hidden
                .into_iter()
                .filter_map(|i| dock.panels.get(i).cloned())
                .collect();
            div()
                .id("strip-more")
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(STRIP_BUTTON))
                .rounded(px(tokens.radius))
                .cursor_pointer()
                .text_color(theme.muted)
                .hover(move |s| s.bg(hover).text_color(theme.fg))
                .tooltip(|_, cx| cx.new(|_| TextTooltip("More".into())).into())
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    let entries = overflow
                        .iter()
                        .map(|panel| {
                            let title = this.panels.title(panel);
                            let target = panel.clone();
                            Self::entry(title, move |this, _, cx| {
                                let target = target.clone();
                                this.mutate(cx, |s| {
                                    let _ = s.set_dock_active(pos, &target);
                                    s.focus_dock(pos);
                                });
                            })
                        })
                        .collect();
                    this.open_menu(event.position(), entries, window, cx);
                }))
                .child(icon(Icon::Ellipsis, 16.0, theme.muted))
                .child(probe(&self.regions, Region::StripMore))
                .into_any_element()
        });

        div()
            .id("right-strip")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(STRIP_GAP))
            .h(px(TITLEBAR_HEIGHT))
            .w_full()
            .px(px(STRIP_GAP))
            .border_b_1()
            .border_color(theme.border)
            .children(buttons)
            .children(more)
            .child(
                div()
                    .id("strip-filler")
                    .flex_1()
                    .h_full()
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
            .into_any_element()
    }
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
}
