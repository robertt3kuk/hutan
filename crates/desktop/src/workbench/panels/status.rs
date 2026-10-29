//! Status bar items: `connection`, `workspace`, `working`, `usage.meters`, `usage`, `sessions`,
//! `attention`, `approvals`, `density`, `theme`, `palette_hint`, the editor's items, and the dock
//! buttons `dock_left`, `dock_bottom`, `dock_right`. The built-in presets show `connection`,
//! `workspace` and `attention` on the left and the editor's items, `usage.meters` and `working`
//! on the right ([`StatusBarLayout`]; `palette_hint`, the palette's key, is off by default); an
//! item with nothing to say draws nothing. `connection` always has
//! something to say: the daemon's state is never only in a menu (FX-STATUSBAR).
//!
//! [`StatusBarLayout`]: crate::workbench::registry::StatusBarLayout

use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AnyView, AppContext as _, Hsla, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, div, px,
};
use hutan_client::ConnectionState;

use crate::icons::{Icon, glyph, icon};
use crate::sessions::usage::{MeterView, Severity};
use crate::theme::DensityExt as _;
use crate::workbench::actions::{
    JumpToNextAttention, OpenJump, ReviewApprovals, ShowAgentsPage, ToggleBottomDock,
    ToggleDensity, ToggleLeftDock, TogglePalette, ToggleRightDock, daemon,
};
use crate::workbench::model::DockPosition;
use crate::workbench::registry::{
    BarWidth, DockState, StatusContext, StatusItemRegistry, StatusItemSpec, StatusSide,
};
use crate::workbench::render::probe;
use crate::workbench::style::TextTooltip;
use crate::workbench::ui::ring;
use crate::workbench::usage_card::UsageCard;
use crate::workbench::view::Region;

use super::harness_icon;

/// Text and tone of the connection state as the popover's headline says it.
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

/// What the `connection` item is: one state of the daemon as the user sees it. The shape of
/// the mark differs per state (a filled dot, a hollow dot, a triangle), so the colour is never
/// the only signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipKind {
    /// Connected: a `success` dot and the daemon's name, muted.
    Up,
    /// Connecting for the first time.
    Connecting,
    /// Starting, stopping or restarting on the user's command.
    Busy,
    /// The connection dropped; the client is trying again.
    Reconnecting,
    /// Stopped (by the user, or the connection is closed): a click starts the daemon.
    Stopped,
    /// The daemon could not start: *Details* are in the popover.
    Failed,
}

impl ChipKind {
    pub fn tone(self) -> Tone {
        match self {
            ChipKind::Up => Tone::Ok,
            ChipKind::Connecting => Tone::Muted,
            ChipKind::Busy | ChipKind::Reconnecting => Tone::Warn,
            ChipKind::Stopped | ChipKind::Failed => Tone::Error,
        }
    }
}

/// The words of the `connection` item and its tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionChip {
    pub kind: ChipKind,
    /// `hutand` while connected, else the state word: `Reconnecting…`, `Stopped · Start`,
    /// `Can't start · Details`.
    pub word: String,
    pub tooltip: String,
}

/// The click's way to the daemon's controls, said in every tooltip.
pub const CHIP_HINT: &str = "Click for the daemon controls";

/// What a click does while the daemon is stopped.
pub const START_HINT: &str = "Click to start it";

/// The chip of a connection that is not otherwise special: connecting, reconnecting, stopped.
/// `Connected` is built by the workbench, which knows the daemon's version, pid and uptime.
pub fn chip_of_connection(state: ConnectionState) -> ConnectionChip {
    let (kind, word, tooltip) = match state {
        ConnectionState::Connecting => (
            ChipKind::Connecting,
            "Connecting…".to_owned(),
            "Connecting to hutand".to_owned(),
        ),
        ConnectionState::Connected => (ChipKind::Up, "hutand".to_owned(), "hutand".to_owned()),
        ConnectionState::Reconnecting { attempt, retry_in } => (
            ChipKind::Reconnecting,
            "Reconnecting…".to_owned(),
            format!(
                "Lost the connection to hutand · attempt {attempt}, {}",
                retry_text(retry_in)
            ),
        ),
        ConnectionState::Stopped => (
            ChipKind::Stopped,
            "Stopped · Start".to_owned(),
            "hutand is not running".to_owned(),
        ),
    };
    let hint = if kind == ChipKind::Stopped {
        START_HINT
    } else {
        CHIP_HINT
    };
    ConnectionChip {
        kind,
        word,
        tooltip: format!("{tooltip} · {hint}"),
    }
}

/// The mark in front of the connection word: a dot (filled while the daemon is up or on its
/// way, hollow while it is stopped) or, when it cannot start, a triangle.
fn chip_mark(kind: ChipKind, color: gpui::Hsla) -> AnyElement {
    const DOT: f32 = 7.0;
    match kind {
        ChipKind::Stopped => div()
            .flex_none()
            .size(px(DOT))
            .rounded_full()
            .border_1()
            .border_color(color)
            .into_any_element(),
        ChipKind::Failed => icon(Icon::StatusAttention, 13.0, color).into_any_element(),
        _ => div()
            .flex_none()
            .size(px(DOT))
            .rounded_full()
            .bg(color)
            .into_any_element(),
    }
}

/// The words of the `working` item: `1 working`, `3 working`.
pub fn working_text(n: usize) -> String {
    format!("{n} working")
}

/// The words of the `attention` item: `1 request`, `3 requests`, `99+ requests` — the sessions
/// in the sidebar's *Requests* group, never finished work or failures.
pub fn requests_text(n: usize) -> String {
    match n {
        1 => "1 request".to_owned(),
        2..=99 => format!("{n} requests"),
        _ => "99+ requests".to_owned(),
    }
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
        cx.new(|_| TextTooltip::new(SharedString::from(text.clone())))
            .into()
    }
}

/// An item that gives way when the window is narrow: below 700 px the built-in bar keeps
/// `connection` and `attention` and nothing else.
fn gives_way(
    render: impl Fn(&StatusContext<'_>) -> AnyElement + 'static,
) -> crate::workbench::registry::StatusRender {
    Rc::new(move |ctx| {
        if ctx.width == BarWidth::Narrow {
            div().into_any_element()
        } else {
            render(ctx)
        }
    })
}

/// The built-in status items.
pub fn builtin_status_items() -> StatusItemRegistry {
    let mut registry = StatusItemRegistry::new();
    registry.register(StatusItemSpec {
        id: "connection",
        default_side: StatusSide::Left,
        render: Rc::new(|ctx: &StatusContext<'_>| {
            let chip = &ctx.data.connection;
            let color = match chip.kind.tone() {
                Tone::Ok => ctx.theme.success,
                Tone::Warn => ctx.theme.warning,
                Tone::Error => ctx.theme.danger,
                Tone::Muted => ctx.theme.muted,
            };
            // A daemon that is up is the normal state: its name stays muted. Any other state
            // is its word in its colour.
            let word_color = if chip.kind == ChipKind::Up {
                ctx.theme.muted
            } else {
                color
            };
            // Below 700 px a daemon that is up is its dot alone; a daemon that is not keeps
            // its word, which is the state.
            let dot_only = ctx.width == BarWidth::Narrow && chip.kind == ChipKind::Up;
            // A stopped daemon's chip says `Start`: the click starts it, with no second click
            // in a popover. Every other state opens the daemon's controls.
            let stopped = chip.kind == ChipKind::Stopped;
            let mut item = clickable("status-connection", ctx)
                .flex()
                .items_center()
                .gap(px(6.0))
                .tooltip(tooltip(chip.tooltip.clone()))
                .on_click(move |_, window, cx| {
                    if stopped {
                        window.dispatch_action(Box::new(daemon::Start), cx)
                    } else {
                        window.dispatch_action(Box::new(daemon::Status), cx)
                    }
                })
                .child(chip_mark(chip.kind, color));
            if !dot_only {
                item = item.child(
                    div()
                        .text_color(word_color)
                        .child(SharedString::from(chip.word.clone())),
                );
            }
            item.into_any_element()
        }),
    });
    registry.register(StatusItemSpec {
        id: "workspace",
        default_side: StatusSide::Left,
        render: gives_way(|ctx: &StatusContext<'_>| {
            let Some(name) = ctx.data.workspace.clone() else {
                return div().id("status-workspace").into_any_element();
            };
            let branch = ctx.data.branch.clone();
            let tip = match &branch {
                Some(branch) => format!("{name} · branch {branch} · click to jump"),
                None => format!("{name} · click to jump"),
            };
            // Below 900 px only the branch (the name when the project has none).
            let only_branch = ctx.width != BarWidth::Wide && branch.is_some();
            let mut item = clickable("status-workspace", ctx)
                .text_color(ctx.theme.muted)
                .tooltip(tooltip(tip))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenJump), cx))
                .flex()
                .items_center()
                .gap(px(4.0));
            if !only_branch {
                item = item.child(
                    div()
                        .min_w_0()
                        .max_w(px(200.0))
                        .truncate()
                        .child(SharedString::from(name)),
                );
            }
            if let Some(branch) = branch {
                if !only_branch {
                    item = item.child("·");
                }
                item = item.child(glyph(Icon::VcsBranch)).child(
                    div()
                        .min_w_0()
                        .max_w(px(160.0))
                        .truncate()
                        .child(SharedString::from(branch)),
                );
            }
            item.into_any_element()
        }),
    });
    registry.register(StatusItemSpec {
        id: "working",
        default_side: StatusSide::Right,
        render: gives_way(|ctx: &StatusContext<'_>| {
            let n = ctx.data.working;
            if n == 0 {
                return div().id("status-working").into_any_element();
            }
            clickable("status-working", ctx)
                .text_color(ctx.theme.muted)
                .tooltip(tooltip(
                    "Agents at work · click for the Agents page".to_owned(),
                ))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ShowAgentsPage), cx))
                .flex()
                .items_center()
                .gap(px(4.0))
                .child(icon(Icon::StateWorking, 14.0, ctx.theme.accent))
                .child(working_text(n))
                .into_any_element()
        }),
    });
    // The tightest limit any harness reported (USAGE-LIMITS): `Codex 72% · resets 3h`. Not in
    // the built-in bar, which has `usage.meters` instead; `status.usage.style = "text"`
    // (Settings → Status bar) swaps it in, and a preset that names it shows it. Nothing
    // reported, nothing drawn.
    registry.register(StatusItemSpec {
        id: "usage",
        default_side: StatusSide::Right,
        render: gives_way(|ctx: &StatusContext<'_>| {
            let Some(usage) = &ctx.data.usage else {
                return div().id("status-usage").into_any_element();
            };
            let color = match usage.severity {
                crate::sessions::usage::Severity::Calm => ctx.theme.muted,
                crate::sessions::usage::Severity::Warning => ctx.theme.warning,
                crate::sessions::usage::Severity::Critical => ctx.theme.danger,
            };
            clickable("status-usage", ctx)
                .text_color(color)
                .tooltip(tooltip(format!(
                    "{}\nClick for the Agents page",
                    usage.tooltip
                )))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ShowAgentsPage), cx))
                .child(SharedString::from(usage.text.clone()))
                .into_any_element()
        }),
    });
    // A ring per harness with a running limit window (STATUS-METERS): the harness's glyph inside,
    // the arc the share of its tightest window used. Amber from 80 %, red from 95 %, and from
    // 80 % the number beside it. Hover: the harness's card with every window; click: the Agents
    // page, whose Usage section has the numbers. In the built-in bar; nothing drawn until a
    // harness reported a limit.
    registry.register(StatusItemSpec {
        id: "usage.meters",
        default_side: StatusSide::Right,
        render: gives_way(|ctx: &StatusContext<'_>| {
            if ctx.data.meters.is_empty() {
                return div().id("status-usage-meters").into_any_element();
            }
            clickable("status-usage-meters", ctx)
                .flex()
                .items_center()
                .gap(px(8.0))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ShowAgentsPage), cx))
                .children(
                    ctx.data
                        .meters
                        .iter()
                        .enumerate()
                        .map(|(index, meter)| usage_meter(index, meter, ctx)),
                )
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
            // No request waits: the bar stays, the words go. Finished work and failures are
            // never counted here (they are in the sidebar's groups).
            let n = ctx.data.requests;
            if n == 0 {
                return div().id("status-attention").into_any_element();
            }
            // A state is a glyph in its color and a word: the glyph is the warning, the words
            // are `fg` (CL-CHROME; the bar is not a place for colored text).
            clickable("status-attention", ctx)
                .text_color(ctx.theme.fg)
                .tooltip(tooltip(format!(
                    "Jump to the next request {}",
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
                .child(requests_text(n))
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
        render: gives_way(|ctx: &StatusContext<'_>| -> AnyElement {
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
        render: gives_way(|ctx: &StatusContext<'_>| -> AnyElement {
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
        id: "editor.wrap",
        default_side: StatusSide::Right,
        render: gives_way(|ctx: &StatusContext<'_>| -> AnyElement {
            match &ctx.data.editor {
                Some(editor) if editor.wrap => clickable("status-editor-wrap", ctx)
                    .text_color(ctx.theme.muted)
                    .tooltip(tooltip(
                        "Long lines wrap at the edge of the view (editor.soft_wrap). Click, or ⌥Z, to turn it off for this tab"
                            .to_owned(),
                    ))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(hutan_code_view::ToggleSoftWrap), cx);
                    })
                    .child("Wrap")
                    .into_any_element(),
                _ => div().id("status-editor-wrap").into_any_element(),
            }
        }),
    });
    registry.register(StatusItemSpec {
        id: "editor.language",
        default_side: StatusSide::Right,
        render: gives_way(|ctx: &StatusContext<'_>| -> AnyElement {
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
        id: "editor.diagnostics",
        default_side: StatusSide::Right,
        render: gives_way(|ctx: &StatusContext<'_>| -> AnyElement {
            match ctx.data.editor.as_ref().and_then(diagnostics_item) {
                Some(item) => render_diagnostics_item(item, ctx),
                None => div().id("status-editor-diagnostics").into_any_element(),
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
        render: gives_way(|ctx: &StatusContext<'_>| -> AnyElement {
            clickable("status-palette-hint", ctx)
                .text_color(ctx.theme.muted)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(TogglePalette), cx))
                .child(ctx.palette_hint.clone())
                .into_any_element()
        }),
    });
    registry.register(dock_item(
        "dock_left",
        DockPosition::Left,
        "left dock",
        || Box::new(ToggleLeftDock),
    ));
    registry.register(dock_item(
        "dock_bottom",
        DockPosition::Bottom,
        "bottom dock",
        || Box::new(ToggleBottomDock),
    ));
    registry.register(dock_item(
        "dock_right",
        DockPosition::Right,
        "right dock",
        || Box::new(ToggleRightDock),
    ));
    registry
}

/// The size of a usage ring's box and the line it is drawn with: the circle inside the line is
/// 16 px (FX-UI-B; it was 14, thin and grey).
const RING_SIZE: f32 = 20.0;
const RING_STROKE: f32 = 2.0;

/// The colours of a usage ring: its track (the whole circle) and its arc (the share used).
/// Calm is `fg`, not `muted`, so the arc reads against the bar (a graphical object wants 3:1);
/// amber and red from 80 % and 95 %, and then the number beside it, so a colour is never all
/// there is.
pub(crate) fn ring_colors(theme: &crate::theme::Theme, severity: Severity) -> (Hsla, Hsla) {
    let arc = match severity {
        Severity::Calm => theme.fg.opacity(0.9),
        Severity::Warning => theme.warning,
        Severity::Critical => theme.danger,
    };
    (theme.muted.opacity(0.4), arc)
}

/// One ring of `usage.meters`: the harness's glyph inside an arc of the share used, and the
/// harness's card as its hover.
fn usage_meter(index: usize, meter: &MeterView, ctx: &StatusContext<'_>) -> AnyElement {
    let theme = ctx.theme;
    let (track, color) = ring_colors(theme, meter.severity);
    let card = meter.card.clone();
    div()
        .id(("status-usage-meter", index))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(4.0))
        .tooltip(move |_, cx| cx.new(|_| UsageCard::new(card.clone())).into())
        .child(
            div()
                .relative()
                .flex_none()
                .size(px(RING_SIZE))
                .child(ring(RING_SIZE, RING_STROKE, meter.fraction, track, color))
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(harness_icon(&meter.harness), 10.0, color)),
                ),
        )
        .when(meter.says_percent(), |s| {
            s.child(
                div()
                    .text_color(color)
                    .child(SharedString::from(meter.percent.clone())),
            )
        })
        .child(probe(ctx.regions, Region::UsageMeter(index)))
        .into_any_element()
}

/// The status item that toggles one dock: a small window glyph with the dock's side filled,
/// pressed (an `accent_soft` fill, the glyph `fg`) while the dock shows, dim and inert while it
/// is hidden and holds no panel. Its tooltip carries the key of the active keymap. Not in the
/// built-in presets' status bar; a preset that names it shows it.
fn dock_item(
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
            let tip = dock_tooltip(name, state, &hint);
            let style = ctx.density.ui();
            crate::workbench::ui::IconButton::new(
                id,
                match pos {
                    DockPosition::Left => crate::icons::Icon::DockLeft,
                    DockPosition::Right => crate::icons::Icon::DockRight,
                    DockPosition::Bottom => crate::icons::Icon::DockBottom,
                },
                tip,
                ctx.theme,
                &style,
            )
            .compact()
            .toggled(state.visible)
            .disabled(!enabled)
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

/// What `editor.diagnostics` shows: the counts by severity (each with its glyph, a shape per
/// severity) and the server's state while it is not simply ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsItem {
    /// (glyph, count, severity) for errors, warnings, and information with hints.
    pub counts: Vec<(&'static str, usize, hutan_code_view::Severity)>,
    /// `indexing`, `starting`, `failed`, … (nothing while ready).
    pub state: Option<String>,
    pub tooltip: String,
    /// The server waits for its folder's trust: a click brings the trust bar back (*Trust* /
    /// *Not now*) instead of opening Problems.
    pub untrusted: bool,
}

/// `None` when the focused file has no language server.
pub fn diagnostics_item(
    editor: &crate::workbench::registry::EditorStatus,
) -> Option<DiagnosticsItem> {
    use hutan_code_view::Severity;
    use hutan_protocol::lsp::LspServerState;
    let server = editor.diagnostics.server.as_ref()?;
    let counts = editor.diagnostics.counts.unwrap_or_default();
    let shown = vec![
        (Severity::Error.symbol(), counts.errors, Severity::Error),
        (
            Severity::Warning.symbol(),
            counts.warnings,
            Severity::Warning,
        ),
        (
            Severity::Info.symbol(),
            counts.infos + counts.hints,
            Severity::Info,
        ),
    ];
    let state = match server.state {
        LspServerState::Ready => None,
        LspServerState::NotInstalled => Some("not installed".to_owned()),
        other => Some(other.as_str().to_owned()),
    };
    let plural = |n: usize, one: &str| {
        if n == 1 {
            format!("{n} {one}")
        } else {
            format!("{n} {one}s")
        }
    };
    let mut tooltip = format!(
        "{}: {} · {}, {}",
        server.name,
        server.state.as_str().replace('_', " "),
        plural(counts.errors, "error"),
        plural(counts.warnings, "warning"),
    );
    if let Some(reason) = &server.reason {
        tooltip.push_str(&format!(" · {reason}"));
    }
    let untrusted = server.state == LspServerState::Untrusted;
    tooltip.push_str(if untrusted {
        " · click to choose whether to trust this folder"
    } else {
        " · click for Problems"
    });
    Some(DiagnosticsItem {
        counts: shown
            .into_iter()
            .filter(|(_, n, s)| *n > 0 || *s != Severity::Info)
            .collect(),
        state,
        tooltip,
        untrusted,
    })
}

fn render_diagnostics_item(item: DiagnosticsItem, ctx: &StatusContext<'_>) -> AnyElement {
    use hutan_code_view::Severity;
    let color = |severity: Severity, n: usize| match severity {
        _ if n == 0 => ctx.theme.muted,
        Severity::Error => ctx.theme.danger,
        Severity::Warning => ctx.theme.warning,
        Severity::Info | Severity::Hint => ctx.theme.accent,
    };
    let mut row = clickable("status-editor-diagnostics", ctx)
        .flex()
        .items_center()
        .gap(px(8.0))
        .text_color(ctx.theme.muted)
        .tooltip(tooltip(item.tooltip))
        .on_click(move |_, window, cx| {
            if item.untrusted {
                window.dispatch_action(Box::new(crate::workbench::actions::ReviewLspTrust), cx);
            } else {
                window.dispatch_action(Box::new(crate::workbench::actions::ShowProblems), cx);
            }
        });
    for (glyph, n, severity) in item.counts {
        row = row.child(
            div()
                .flex()
                .items_center()
                .gap(px(3.0))
                .child(div().text_color(color(severity, n)).child(glyph))
                .child(div().text_color(ctx.theme.fg).child(n.to_string())),
        );
    }
    if let Some(state) = item.state {
        row = row.child(SharedString::from(state));
    }
    row.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_calm_usage_ring_reads_against_the_bar_in_both_built_in_themes() {
        use crate::theme::Theme;
        use crate::workbench::ui::contrast;
        use gpui::Rgba;
        // The colour as it lands on the bar: alpha composited over it.
        let flat = |color: Hsla, bar: Hsla| -> Hsla {
            let (c, b) = (Rgba::from(color), Rgba::from(bar));
            let mix = |top: f32, below: f32| below + (top - below) * c.a;
            Rgba {
                r: mix(c.r, b.r),
                g: mix(c.g, b.g),
                b: mix(c.b, b.b),
                a: 1.0,
            }
            .into()
        };
        assert!((RING_SIZE - 2.0 * RING_STROKE - 16.0).abs() < f32::EPSILON);
        for theme in [Theme::dark(), Theme::light()] {
            let (track, arc) = ring_colors(&theme, Severity::Calm);
            for bar in [theme.bg, theme.sidebar] {
                assert!(contrast(flat(arc, bar), bar) >= 3.0, "the arc");
                assert!(contrast(flat(track, bar), bar) >= 1.3, "the track");
            }
        }
    }

    #[test]
    fn an_untrusted_servers_item_brings_the_trust_choice_back() {
        use crate::workbench::registry::{EditorDiagnostics, EditorServer, EditorStatus};
        use hutan_protocol::lsp::LspServerState;
        let status = |state| EditorStatus {
            language: "Rust".to_owned(),
            vim: false,
            wrap: false,
            diagnostics: EditorDiagnostics {
                counts: None,
                server: Some(EditorServer {
                    name: "rust-analyzer".to_owned(),
                    state,
                    reason: None,
                }),
            },
        };
        let item = diagnostics_item(&status(LspServerState::Untrusted)).unwrap();
        assert!(item.untrusted);
        assert_eq!(item.state.as_deref(), Some("untrusted"));
        assert!(
            item.tooltip
                .ends_with("click to choose whether to trust this folder"),
            "{}",
            item.tooltip
        );
        let ready = diagnostics_item(&status(LspServerState::Ready)).unwrap();
        assert!(!ready.untrusted);
        assert!(ready.tooltip.ends_with("click for Problems"));
    }

    #[test]
    fn the_default_status_bar_is_connection_workspace_attention_then_working() {
        use crate::workbench::registry::StatusBarLayout;
        let bar = StatusBarLayout::default();
        assert_eq!(bar.left, ["connection", "workspace", "attention"]);
        assert_eq!(
            bar.right,
            [
                "editor.diagnostics",
                "editor.mode",
                "editor.position",
                "editor.wrap",
                "editor.language",
                "usage.meters",
                "working"
            ]
        );
        // Every item the built-in bar names is registered, and so is every item it does not
        // show: a preset that lists them still gets them.
        let registry = builtin_status_items();
        let registered: Vec<&str> = registry.iter().map(|spec| spec.id).collect();
        for id in bar.left.iter().chain(&bar.right) {
            assert!(registered.contains(&id.as_str()), "{id} is registered");
        }
        for id in [
            "sessions",
            "usage",
            "approvals",
            "density",
            "theme",
            "palette_hint",
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
    fn the_usage_style_decides_which_usage_item_the_bar_draws() {
        use crate::settings::UsageStyle;
        use crate::workbench::registry::StatusBarLayout;
        let built_in = StatusBarLayout::default();
        let styled = |bar: &StatusBarLayout, style| {
            let mut bar = bar.clone();
            bar.with_usage_style(style);
            bar
        };
        assert_eq!(
            styled(&built_in, UsageStyle::Meters),
            built_in,
            "rings are the default"
        );
        // Words stand where the rings stood (same place, same neighbours).
        let words = styled(&built_in, UsageStyle::Text);
        let at = built_in
            .right
            .iter()
            .position(|i| i == "usage.meters")
            .unwrap();
        assert_eq!(words.right[at], "usage");
        assert_eq!(words.right.len(), built_in.right.len());
        // Off takes both out, wherever they were put.
        let mut both = built_in.clone();
        both.left.push("usage".to_owned());
        let off = styled(&both, UsageStyle::Off);
        assert!(
            !off.left
                .iter()
                .chain(&off.right)
                .any(|i| i.starts_with("usage")),
            "{off:?}"
        );
        // A bar that names the words already keeps one item, not two.
        let words_too = styled(&both, UsageStyle::Text);
        let count = words_too
            .left
            .iter()
            .chain(&words_too.right)
            .filter(|i| i.starts_with("usage"))
            .count();
        assert_eq!(count, 1, "{words_too:?}");
        // A bar the user took the item out of is the same in every style.
        let mut none = built_in.clone();
        none.right.retain(|i| i != "usage.meters");
        for style in UsageStyle::ALL {
            assert_eq!(styled(&none, style), none);
        }
    }

    #[test]
    fn every_connection_state_has_its_word_and_a_distinct_mark() {
        let reconnecting = chip_of_connection(ConnectionState::Reconnecting {
            attempt: 3,
            retry_in: Duration::from_millis(400),
        });
        assert_eq!(
            (reconnecting.kind, reconnecting.word.as_str()),
            (ChipKind::Reconnecting, "Reconnecting…")
        );
        assert!(
            reconnecting.tooltip.contains("attempt 3, retry in 400 ms"),
            "{}",
            reconnecting.tooltip
        );
        let stopped = chip_of_connection(ConnectionState::Stopped);
        assert_eq!(
            (stopped.kind, stopped.word.as_str()),
            (ChipKind::Stopped, "Stopped · Start")
        );
        let connecting = chip_of_connection(ConnectionState::Connecting);
        assert_eq!(connecting.word, "Connecting…");
        let up = chip_of_connection(ConnectionState::Connected);
        assert_eq!((up.kind, up.word.as_str()), (ChipKind::Up, "hutand"));
        // The tone is the state's colour; the daemon being up is the only `Ok`.
        assert_eq!(ChipKind::Up.tone(), Tone::Ok);
        assert_eq!(ChipKind::Reconnecting.tone(), Tone::Warn);
        assert_eq!(ChipKind::Busy.tone(), Tone::Warn);
        assert_eq!(ChipKind::Stopped.tone(), Tone::Error);
        assert_eq!(ChipKind::Failed.tone(), Tone::Error);
        assert_eq!(working_text(2), "2 working");
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
