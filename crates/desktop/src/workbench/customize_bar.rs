//! The status bar, customized where it is (WIDGET-CONFIG item 3): right click the bar for a
//! checklist of its items (the left and the right group, and what is not shown), right click an
//! item for its own options (`workspace`: show the branch; `connection`: show the version),
//! its place and *Hide*, and drag an item to reorder it or to move it to the other side. Every
//! change goes through `customize.rs`: live, written into the current preset's `status_bar`
//! (a built-in preset is never edited in place), undoable.

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, Pixels, Point, SharedString, StatefulInteractiveElement, Styled, div,
    prelude::*,
};

use super::registry::{StatusBarLayout, StatusSide};
use super::render::probe;
use super::style::UiStyle;
use super::view::{DragPreview, MenuEntry, Region, Workbench, preset_label};

/// One option of a status item (`status.<item>.<key>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusOption {
    pub label: &'static str,
    /// The settings key, `status.workspace.show_branch`.
    pub key: &'static str,
    /// What it shows, for the toast: `the branch`.
    pub what: &'static str,
    pub on: bool,
}

/// What the checklist and the toasts call a status item.
pub fn status_item_title(id: &str) -> &str {
    match id {
        "connection" => "Daemon state",
        "workspace" => "Project and branch",
        "working" => "Agents at work",
        "usage" => "Usage limit",
        "sessions" => "Session count",
        "attention" => "Requests waiting",
        "approvals" => "Approvals",
        "editor.mode" => "Vim mode",
        "editor.position" => "Line and column",
        "editor.wrap" => "Soft wrap",
        "editor.language" => "Language",
        "editor.diagnostics" => "Diagnostics",
        "density" => "Density",
        "theme" => "Theme",
        "palette_hint" => "Command palette key",
        "dock_left" => "Left sidebar button",
        "dock_bottom" => "Bottom dock button",
        "dock_right" => "Right panel button",
        other => other,
    }
}

/// A status item being dragged to another place in the bar.
#[derive(Clone, Debug)]
pub struct DraggedStatusItem {
    pub id: &'static str,
    pub label: SharedString,
}

fn side_name(side: StatusSide) -> &'static str {
    match side {
        StatusSide::Left => "left",
        StatusSide::Right => "right",
    }
}

fn side_of(bar: &StatusBarLayout, id: &str) -> Option<StatusSide> {
    if bar.left.iter().any(|i| i == id) {
        Some(StatusSide::Left)
    } else if bar.right.iter().any(|i| i == id) {
        Some(StatusSide::Right)
    } else {
        None
    }
}

fn list_mut(bar: &mut StatusBarLayout, side: StatusSide) -> &mut Vec<String> {
    match side {
        StatusSide::Left => &mut bar.left,
        StatusSide::Right => &mut bar.right,
    }
}

/// Takes `id` out of the bar wherever it is, and puts it on `side`: before `before` when that
/// item is there, else at the end.
fn place(bar: &mut StatusBarLayout, id: &str, side: StatusSide, before: Option<&str>) {
    bar.left.retain(|i| i != id);
    bar.right.retain(|i| i != id);
    let list = list_mut(bar, side);
    let at = before
        .and_then(|b| list.iter().position(|i| i == b))
        .unwrap_or(list.len());
    list.insert(at, id.to_owned());
}

impl Workbench {
    /// The checklist of the status bar's items: the left group, the right group, and the
    /// items that are not shown (each checked while shown), and *Reset status bar*. A choice
    /// keeps the list open at `position`.
    pub(crate) fn status_checklist(&self, position: Point<Pixels>) -> Vec<MenuEntry> {
        let bar = self.status_bar();
        let registered: Vec<(&'static str, StatusSide)> = self
            .status_items
            .iter()
            .map(|spec| (spec.id, spec.default_side))
            .collect();
        let known = |id: &String| registered.iter().find(|(r, _)| *r == id.as_str()).copied();
        let mut entries = Vec::new();
        let mut section = |title: &'static str, ids: Vec<(&'static str, bool)>| {
            if ids.is_empty() {
                return;
            }
            entries.push(Self::disabled_entry(title));
            for (id, shown) in ids {
                let mut entry = Self::entry(status_item_title(id), move |this, window, cx| {
                    this.toggle_status_item(id, cx);
                    let entries = this.status_checklist(position);
                    this.open_menu(position, entries, window, cx);
                });
                entry.checked = shown;
                entries.push(entry);
            }
        };
        let shown_on = |side: StatusSide| -> Vec<(&'static str, bool)> {
            let list = match side {
                StatusSide::Left => &bar.left,
                StatusSide::Right => &bar.right,
            };
            list.iter()
                .filter_map(|id| known(id).map(|(id, _)| (id, true)))
                .collect()
        };
        section("Status bar, left", shown_on(StatusSide::Left));
        section("Status bar, right", shown_on(StatusSide::Right));
        let hidden: Vec<(&'static str, bool)> = registered
            .iter()
            .filter(|(id, _)| side_of(&bar, id).is_none())
            .map(|(id, _)| (*id, false))
            .collect();
        section("Not shown", hidden);
        entries.push(Self::entry("Reset status bar", |this, _, cx| {
            this.reset_status_bar(cx)
        }));
        entries
    }

    /// The options of status item `id` and their state (`workspace`: show the branch;
    /// `connection`: show the daemon's version).
    pub fn status_item_options(&self, id: &str) -> Vec<StatusOption> {
        let settings = self.settings.get::<crate::settings::StatusSettings>();
        match id {
            "workspace" => vec![StatusOption {
                label: "Show branch",
                key: "status.workspace.show_branch",
                what: "the branch",
                on: settings.workspace.show_branch,
            }],
            "connection" => vec![StatusOption {
                label: "Show version",
                key: "status.connection.show_version",
                what: "the daemon's version",
                on: settings.connection.show_version,
            }],
            _ => Vec::new(),
        }
    }

    /// Flips the option whose settings key is `key`; kept with an Undo.
    pub fn toggle_status_option(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(option) = ["workspace", "connection"]
            .into_iter()
            .flat_map(|id| self.status_item_options(id))
            .find(|option| option.key == key)
        else {
            return;
        };
        let verb = if option.on { "Hid" } else { "Showed" };
        self.customize_keys(
            format!("{verb} {} in the status bar", option.what),
            vec![(
                option.key.to_owned(),
                Some(toml::Value::Boolean(!option.on)),
            )],
            cx,
        );
    }

    /// The menu of one item of the bar: its own options, where it sits, *Hide*, and the
    /// checklist.
    pub(crate) fn status_item_menu(
        &self,
        id: &'static str,
        position: Point<Pixels>,
    ) -> Vec<MenuEntry> {
        let mut entries = Vec::new();
        for option in self.status_item_options(id) {
            let key = option.key;
            let mut entry = Self::entry(option.label, move |this, _, cx| {
                this.toggle_status_option(key, cx);
            });
            entry.checked = option.on;
            entries.push(entry);
        }
        let bar = self.status_bar();
        let side = side_of(&bar, id).unwrap_or(StatusSide::Right);
        let other = match side {
            StatusSide::Left => StatusSide::Right,
            StatusSide::Right => StatusSide::Left,
        };
        entries.push(Self::entry(
            format!("Move to the {} side", side_name(other)),
            move |this, _, cx| this.move_status_item(id, other, None, cx),
        ));
        let list = match side {
            StatusSide::Left => &bar.left,
            StatusSide::Right => &bar.right,
        };
        let index = list.iter().position(|i| i == id);
        match index.filter(|i| *i > 0) {
            Some(i) => {
                let before = list[i - 1].clone();
                entries.push(Self::entry("Move earlier", move |this, _, cx| {
                    this.move_status_item(id, side, Some(&before), cx);
                }));
            }
            None => entries.push(Self::disabled_entry("Move earlier")),
        }
        match index.filter(|i| i + 1 < list.len()) {
            Some(i) => {
                // After the next item: before the one after it, else at the end.
                let after = list.get(i + 2).cloned();
                entries.push(Self::entry("Move later", move |this, _, cx| {
                    this.move_status_item(id, side, after.as_deref(), cx);
                }));
            }
            None => entries.push(Self::disabled_entry("Move later")),
        }
        entries.push(Self::entry("Hide", move |this, _, cx| {
            this.toggle_status_item(id, cx);
        }));
        entries.push(Self::submenu_entry(
            "Status bar items",
            move |this, window, cx| {
                let entries = this.status_checklist(position);
                this.open_menu(position, entries, window, cx);
            },
        ));
        entries
    }

    /// Hides `id` while it shows (left or right), shows it on its default side otherwise.
    pub fn toggle_status_item(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(default_side) = self.status_items.get(id).map(|spec| spec.default_side) else {
            return;
        };
        let title = status_item_title(id).to_lowercase();
        let shown = side_of(&self.status_bar(), id).is_some();
        let label = if shown {
            format!("Hid {title} from the status bar")
        } else {
            format!("Showed {title} in the status bar")
        };
        let id = id.to_owned();
        self.customize_status(label, cx, move |bar| {
            if shown {
                bar.left.retain(|i| *i != id);
                bar.right.retain(|i| *i != id);
            } else {
                place(bar, &id, default_side, None);
            }
            true
        });
    }

    /// Puts `id` on `side` of the bar, before `before` when given: a drag, *Move earlier*,
    /// *Move to the other side*. An item that is not shown is shown.
    pub fn move_status_item(
        &mut self,
        id: &str,
        side: StatusSide,
        before: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if self.status_items.get(id).is_none() || before == Some(id) {
            return;
        }
        let current = self.status_bar();
        let mut moved = current.clone();
        place(&mut moved, id, side, before);
        if moved == current {
            return;
        }
        let title = status_item_title(id).to_lowercase();
        let label = if side_of(&current, id) == Some(side) {
            "Reordered the status bar".to_owned()
        } else {
            format!("Moved {title} to the {} of the status bar", side_name(side))
        };
        let (id, before) = (id.to_owned(), before.map(str::to_owned));
        self.customize_status(label, cx, move |bar| {
            place(bar, &id, side, before.as_deref());
            true
        });
    }

    /// *Reset status bar*: the current preset follows the built-in bar again.
    pub fn reset_status_bar(&mut self, cx: &mut Context<Self>) {
        if self.status_bar() == StatusBarLayout::default() {
            self.set_notice("The status bar is the built-in one", cx);
            return;
        }
        let name = preset_label(&self.state.preset);
        self.customize_status(
            format!("Status bar of {name} reset to the built-in one"),
            cx,
            |bar| {
                *bar = StatusBarLayout::default();
                true
            },
        );
    }

    /// An item of the bar as it is drawn: draggable to reorder, with its own right-click menu.
    /// `side` is the group it is in.
    pub(crate) fn status_item_slot(
        &self,
        id: &'static str,
        side: StatusSide,
        body: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = self.theme.accent.opacity(0.18);
        let label: SharedString = status_item_title(id).into();
        div()
            .id(SharedString::from(format!("status-slot-{id}")))
            .relative()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    let entries = this.status_item_menu(id, event.position);
                    this.open_menu(event.position, entries, window, cx);
                    cx.stop_propagation();
                }),
            )
            .on_drag(
                DraggedStatusItem { id, label },
                |dragged: &DraggedStatusItem, _, _, cx| {
                    let style = UiStyle::get(cx);
                    let label = dragged.label.clone();
                    cx.new(|_| DragPreview { label, style })
                },
            )
            .drag_over::<DraggedStatusItem>(move |s, _, _, _| s.bg(hover))
            .on_drop(
                cx.listener(move |this, dragged: &DraggedStatusItem, _, cx| {
                    this.move_status_item(dragged.id, side, Some(id), cx);
                }),
            )
            .child(body)
            .child(probe(&self.regions, Region::StatusItem(id)))
            .into_any_element()
    }

    /// The group of items on one `side` of the bar: where an item dropped on room between or
    /// after the items goes to the end.
    pub(crate) fn status_group(
        &self,
        side: StatusSide,
        group: gpui::Stateful<gpui::Div>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = self.theme.accent.opacity(0.10);
        group
            .drag_over::<DraggedStatusItem>(move |s, _, _, _| s.bg(hover))
            .on_drop(
                cx.listener(move |this, dragged: &DraggedStatusItem, _, cx| {
                    this.move_status_item(dragged.id, side, None, cx);
                }),
            )
            .into_any_element()
    }
}

/// A shorthand for a group's id.
pub(crate) fn group_id(side: StatusSide) -> &'static str {
    match side {
        StatusSide::Left => "status-group-left",
        StatusSide::Right => "status-group-right",
    }
}
