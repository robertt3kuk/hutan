//! Status bar items: `connection`, `workspace`, `sessions`, `attention`, `approvals`, `density`,
//! `theme`, `palette_hint`, the editor's items, and the dock buttons `dock_left`, `dock_bottom`,
//! `dock_right`. The built-in presets show `attention` and `connection` on the left and the
//! editor's items and `palette_hint` on the right ([`StatusBarLayout`]); an item with nothing to
//! say draws nothing.
//!
//! [`StatusBarLayout`]: crate::workbench::registry::StatusBarLayout

use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AnyView, AppContext as _, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, div, px,
};
use hutan_client::ConnectionState;

use crate::theme::DensityExt as _;
use crate::workbench::actions::{
    JumpToNextAttention, OpenJump, ReviewApprovals, ToggleBottomDock, ToggleDensity,
    ToggleLeftDock, TogglePalette, ToggleRightDock, daemon,
};
use crate::workbench::model::DockPosition;
use crate::workbench::registry::{
    DockState, StatusContext, StatusItemRegistry, StatusItemSpec, StatusSide,
};
use crate::workbench::style::TextTooltip;

/// Text and tone of the connection item.
pub fn connection_label(state: ConnectionState) -> (String, Tone) {
    match state {
        ConnectionState::Connecting => ("connecting to hutand…".to_owned(), Tone::Muted),
        ConnectionState::Connected => ("hutand connected".to_owned(), Tone::Ok),
        ConnectionState::Reconnecting { attempt, retry_in } => (
            format!("reconnecting ({attempt}) · {}", retry_text(retry_in)),
            Tone::Warn,
        ),
        ConnectionState::Stopped => ("hutand stopped".to_owned(), Tone::Error),
    }
}

/// The connection item says something only while the daemon is not simply up (connecting,
/// reconnecting, restarting, stopped); a working connection is the normal state and draws
/// nothing. The daemon's controls stay in the Daemon menu and the palette (*Daemon: Status*).
pub fn connection_shown(tone: Tone) -> bool {
    tone != Tone::Ok
}

fn retry_text(retry_in: Duration) -> String {
    let secs = retry_in.as_secs_f32();
    if secs < 1.0 {
        format!("retry in {} ms", retry_in.as_millis())
    } else {
        format!("retry in {secs:.0} s")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Warn,
    Error,
    Muted,
}

fn clickable(id: &'static str, ctx: &StatusContext<'_>) -> gpui::Stateful<gpui::Div> {
    let hover = ctx.theme.surface_hover;
    div()
        .id(id)
        .px(px(6.0))
        .rounded(px(ctx.density.ui().radius - 2.0))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
}

fn tooltip(text: String) -> impl Fn(&mut gpui::Window, &mut gpui::App) -> AnyView + 'static {
    move |_, cx| {
        cx.new(|_| TextTooltip(SharedString::from(text.clone())))
            .into()
    }
}

/// The eight built-in status items.
pub fn builtin_status_items() -> StatusItemRegistry {
    let mut registry = StatusItemRegistry::new();
    registry.register(StatusItemSpec {
        id: "connection",
        default_side: StatusSide::Left,
        render: Rc::new(|ctx: &StatusContext<'_>| {
            let (text, tone) = ctx.data.connection_line.clone();
            if !connection_shown(tone) {
                return div().id("status-connection").into_any_element();
            }
            let color = match tone {
                Tone::Ok => ctx.theme.success,
                Tone::Warn => ctx.theme.warning,
                Tone::Error => ctx.theme.danger,
                Tone::Muted => ctx.theme.muted,
            };
            let epoch = ctx.data.epoch.clone().map_or_else(
                || "no daemon epoch yet".to_owned(),
                |e| format!("daemon epoch {e}"),
            );
            clickable("status-connection", ctx)
                .flex()
                .items_center()
                .gap(px(5.0))
                .text_color(ctx.theme.muted)
                .tooltip(tooltip(format!("{epoch} · click for the daemon controls")))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(daemon::Status), cx))
                .child(crate::icons::icon(
                    match ctx.data.connection {
                        ConnectionState::Connected => crate::icons::Icon::StatusConnected,
                        ConnectionState::Connecting | ConnectionState::Reconnecting { .. } => {
                            crate::icons::Icon::StatusConnecting
                        }
                        ConnectionState::Stopped => crate::icons::Icon::StatusDisconnected,
                    },
                    14.0,
                    color,
                ))
                .child(SharedString::from(text))
                .into_any_element()
        }),
    });
    registry.register(StatusItemSpec {
        id: "workspace",
        default_side: StatusSide::Left,
        render: Rc::new(|ctx: &StatusContext<'_>| {
            let name = ctx
                .data
                .workspace
                .clone()
                .unwrap_or_else(|| "no workspace".to_owned());
            clickable("status-workspace", ctx)
                .text_color(ctx.theme.fg)
                .tooltip(tooltip("Active workspace · click to jump".to_owned()))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenJump), cx))
                .flex()
                .items_center()
                .gap(px(4.0))
                .child(crate::icons::glyph(crate::icons::Icon::StatusWorkspace))
                .child(SharedString::from(name))
                .into_any_element()
        }),
    });
    registry.register(StatusItemSpec {
        id: "sessions",
        default_side: StatusSide::Left,
        render: Rc::new(|ctx: &StatusContext<'_>| {
            let n = ctx.data.sessions;
            div()
                .id("status-sessions")
                .text_color(ctx.theme.muted)
                .child(format!("{n} session{}", if n == 1 { "" } else { "s" }))
                .into_any_element()
        }),
    });
    registry.register(StatusItemSpec {
        id: "attention",
        default_side: StatusSide::Left,
        render: Rc::new(|ctx: &StatusContext<'_>| {
            // Nothing needs you: the bar stays, the words go.
            let n = ctx.data.needs_you;
            if n == 0 {
                return div().id("status-attention").into_any_element();
            }
            // A state is a glyph in its color and a word: the glyph is the warning, the words
            // are `fg` (CL-CHROME; the bar is not a place for colored text).
            clickable("status-attention", ctx)
                .text_color(ctx.theme.fg)
                .tooltip(tooltip(format!(
                    "Jump to the next session that needs you {}",
                    ctx.attention_hint
                )))
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(JumpToNextAttention), cx);
                })
                .flex()
                .items_center()
                .gap(px(4.0))
                .child(
                    div()
                        .text_color(ctx.theme.warning)
                        .child(crate::icons::glyph(crate::icons::Icon::StatusAttention)),
                )
                .child(format!("{n} need{} you", if n == 1 { "s" } else { "" }))
                .into_any_element()
        }),
    });
    registry.register(StatusItemSpec {
        id: "approvals",
        default_side: StatusSide::Left,
        render: Rc::new(|ctx: &StatusContext<'_>| {
            let n = ctx.data.approvals;
            if n == 0 {
                return div().id("status-approvals").into_any_element();
            }
            clickable("status-approvals", ctx)
                .text_color(ctx.theme.warning)
                .tooltip(tooltip(
                    "Review the permission requests of your agents".to_owned(),
                ))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ReviewApprovals), cx))
                .flex()
                .items_center()
                .gap(px(4.0))
                .child(crate::icons::glyph(crate::icons::Icon::StatusApprovals))
                .child(format!("{n} approval{}", if n == 1 { "" } else { "s" }))
                .into_any_element()
        }),
    });
    registry.register(StatusItemSpec {
        id: "editor.mode",
        default_side: StatusSide::Right,
        render: Rc::new(|ctx: &StatusContext<'_>| -> AnyElement {
            match &ctx.data.editor {
                Some(editor) if editor.vim => div()
                    .id("status-editor-mode")
                    .tooltip(tooltip(
                        "Vim mode, and the keys of a command in progress (editor.vim_mode)"
                            .to_owned(),
                    ))
                    .child(ctx.editor_items.mode(ctx.height))
                    .into_any_element(),
                _ => div().into_any_element(),
            }
        }),
    });
    registry.register(StatusItemSpec {
        id: "editor.position",
        default_side: StatusSide::Right,
        render: Rc::new(|ctx: &StatusContext<'_>| -> AnyElement {
            match &ctx.data.editor {
                Some(_) => div()
                    .id("status-editor-position")
                    .tooltip(tooltip(
                        "Line and column of the selection (or of the top line)".to_owned(),
                    ))
                    .child(ctx.editor_items.position(ctx.height))
                    .into_any_element(),
                None => div().into_any_element(),
            }
        }),
    });
    registry.register(StatusItemSpec {
        id: "editor.language",
        default_side: StatusSide::Right,
        render: Rc::new(|ctx: &StatusContext<'_>| -> AnyElement {
            match &ctx.data.editor {
                Some(editor) => div()
                    .id("status-editor-language")
                    .text_color(ctx.theme.muted)
                    .child(SharedString::from(editor.language.clone()))
                    .into_any_element(),
                None => div().into_any_element(),
            }
        }),
    });
    registry.register(StatusItemSpec {
        id: "density",
        default_side: StatusSide::Right,
        render: Rc::new(|ctx: &StatusContext<'_>| {
            clickable("status-density", ctx)
                .text_color(ctx.theme.muted)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleDensity), cx))
                .child(ctx.density.label())
                .into_any_element()
        }),
    });
    registry.register(StatusItemSpec {
        id: "theme",
        default_side: StatusSide::Right,
        render: Rc::new(|ctx: &StatusContext<'_>| {
            clickable("status-theme", ctx)
                .text_color(ctx.theme.muted)
                .tooltip(tooltip("Terminal theme (change in settings)".to_owned()))
                .on_click(|_, window, cx| {
                    window.dispatch_action(
                        Box::new(crate::workbench::actions::OpenSettingsFor(
                            "terminal".to_owned(),
                        )),
                        cx,
                    );
                })
                .child(SharedString::from(ctx.data.terminal_theme.clone()))
                .into_any_element()
        }),
    });
    registry.register(StatusItemSpec {
        id: "palette_hint",
        default_side: StatusSide::Right,
        render: Rc::new(|ctx: &StatusContext<'_>| -> AnyElement {
            clickable("status-palette-hint", ctx)
                .text_color(ctx.theme.muted)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(TogglePalette), cx))
                .child(ctx.palette_hint.clone())
                .into_any_element()
        }),
    });
    registry.register(dock_button(
        "dock_left",
        DockPosition::Left,
        "left dock",
        || Box::new(ToggleLeftDock),
    ));
    registry.register(dock_button(
        "dock_bottom",
        DockPosition::Bottom,
        "bottom dock",
        || Box::new(ToggleBottomDock),
    ));
    registry.register(dock_button(
        "dock_right",
        DockPosition::Right,
        "right dock",
        || Box::new(ToggleRightDock),
    ));
    registry
}

/// The status item that toggles one dock: a small window glyph with the dock's side filled,
/// pressed (an `accent_soft` fill, the glyph `fg`) while the dock shows, dim and inert while it
/// is hidden and holds no panel. Its tooltip carries the key of the active keymap. Not in the
/// built-in presets' status bar; a preset that names it shows it.
fn dock_button(
    id: &'static str,
    pos: DockPosition,
    name: &'static str,
    toggle: fn() -> Box<dyn gpui::Action>,
) -> StatusItemSpec {
    StatusItemSpec {
        id,
        default_side: StatusSide::Right,
        render: Rc::new(move |ctx: &StatusContext<'_>| -> AnyElement {
            let state = *ctx.data.docks.get(pos);
            let enabled = state.visible || state.has_panels;
            let hint = ctx.dock_hints.get(pos).clone();
            let color = if state.visible {
                ctx.theme.fg
            } else {
                ctx.theme.muted
            };
            let glyph = crate::icons::icon(
                match pos {
                    DockPosition::Left => crate::icons::Icon::DockLeft,
                    DockPosition::Right => crate::icons::Icon::DockRight,
                    DockPosition::Bottom => crate::icons::Icon::DockBottom,
                },
                14.0,
                color,
            );
            let tip = dock_tooltip(name, state, &hint);
            let button = div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .w(px(22.0))
                .h(px(18.0))
                .rounded(px(ctx.density.ui().radius - 2.0))
                .child(glyph);
            if !enabled {
                return button.opacity(0.4).tooltip(tooltip(tip)).into_any_element();
            }
            let hover = ctx.theme.surface_hover;
            button
                .cursor_pointer()
                .when(state.visible, |s| s.bg(ctx.theme.accent_soft))
                .hover(move |s| s.bg(hover))
                .tooltip(tooltip(tip))
                .on_click(move |_, window, cx| window.dispatch_action(toggle(), cx))
                .into_any_element()
        }),
    }
}

/// Tooltip of a dock button: what a click does and the key of the active keymap.
fn dock_tooltip(name: &str, state: DockState, hint: &str) -> String {
    let action = match (state.visible, state.has_panels) {
        (true, _) => "Hide",
        (false, true) => "Show",
        (false, false) => return format!("The {name} has no panels"),
    };
    if hint.is_empty() {
        format!("{action} the {name}")
    } else {
        format!("{action} the {name} ({hint})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_status_bar_is_attention_and_connection_then_the_palette_key() {
        use crate::workbench::registry::StatusBarLayout;
        let bar = StatusBarLayout::default();
        assert_eq!(bar.left, ["attention", "connection"]);
        assert_eq!(
            bar.right,
            [
                "editor.mode",
                "editor.position",
                "editor.language",
                "palette_hint"
            ]
        );
        // Every item the built-in bar names is registered, and so is every item it no longer
        // shows: a preset that lists them still gets them.
        let registry = builtin_status_items();
        let registered: Vec<&str> = registry.iter().map(|spec| spec.id).collect();
        for id in bar.left.iter().chain(&bar.right) {
            assert!(registered.contains(&id.as_str()), "{id} is registered");
        }
        for id in [
            "workspace",
            "sessions",
            "approvals",
            "density",
            "theme",
            "dock_left",
            "dock_bottom",
            "dock_right",
        ] {
            assert!(registered.contains(&id), "{id} stays registered");
            assert!(
                !bar.left.iter().chain(&bar.right).any(|shown| shown == id),
                "{id} is not in the default bar"
            );
        }
    }

    #[test]
    fn the_connection_item_speaks_only_while_the_daemon_is_not_up() {
        let shown = |state| connection_shown(connection_label(state).1);
        assert!(!shown(ConnectionState::Connected));
        assert!(shown(ConnectionState::Connecting));
        assert!(shown(ConnectionState::Stopped));
        assert!(shown(ConnectionState::Reconnecting {
            attempt: 1,
            retry_in: Duration::from_secs(2),
        }));
    }

    #[test]
    fn connection_states_have_distinct_labels() {
        assert_eq!(connection_label(ConnectionState::Connected).1, Tone::Ok);
        let (text, tone) = connection_label(ConnectionState::Reconnecting {
            attempt: 3,
            retry_in: Duration::from_millis(400),
        });
        assert_eq!(tone, Tone::Warn);
        assert!(text.contains("reconnecting (3)"), "{text}");
        assert_eq!(connection_label(ConnectionState::Stopped).1, Tone::Error);
    }

    #[test]
    fn dock_tooltips_name_the_action_and_the_key() {
        let shown = DockState {
            visible: true,
            has_panels: true,
        };
        let hidden = DockState {
            visible: false,
            has_panels: true,
        };
        let empty = DockState {
            visible: false,
            has_panels: false,
        };
        assert_eq!(
            dock_tooltip("left dock", shown, "⌘B"),
            "Hide the left dock (⌘B)"
        );
        assert_eq!(
            dock_tooltip("left dock", hidden, "⌘B"),
            "Show the left dock (⌘B)"
        );
        assert_eq!(dock_tooltip("left dock", hidden, ""), "Show the left dock");
        assert_eq!(
            dock_tooltip("bottom dock", empty, "⌘J"),
            "The bottom dock has no panels"
        );
        // A visible dock without panels can still be hidden.
        let open_empty = DockState {
            visible: true,
            has_panels: false,
        };
        assert_eq!(
            dock_tooltip("right dock", open_empty, ""),
            "Hide the right dock"
        );
    }
}
