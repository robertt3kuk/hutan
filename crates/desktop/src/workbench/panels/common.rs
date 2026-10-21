//! Pieces shared by the built-in panels: headers, state dots, time formatting.

use crate::theme::DensityExt as _;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, App, AppContext as _, Hsla, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, SharedString, StatefulInteractiveElement, Styled, div, prelude::*,
    px,
};
use hutan_core::{SessionId, SessionKind};
use hutan_protocol::messages::SessionInfo;

use crate::icons::{Icon, icon};
use crate::sessions::{AgentBadge, AgentRecord, PlacementBadge, Presentation, Tone};
use crate::theme::{Theme, Tokens};
use crate::workbench::actions::{OpenSettingsFor, SessionMenuAt, ShowPlacementMenu};
use crate::workbench::chat::TextInput;
use crate::workbench::chat::vim::Vim;
use crate::workbench::render::probe;
use crate::workbench::style::{TextTooltip, UiStyle};
use crate::workbench::text_field::{EditableText, FieldMut, FieldRef, FieldState, edit_field};
use crate::workbench::ui::{FocusRingExt as _, StyledExt as _};
use crate::workbench::vim_ui::{self, VimConfig};
use crate::workbench::{BadgeSite, Region};

pub fn tone_color(theme: &Theme, tone: Tone) -> Hsla {
    match tone {
        Tone::Warning => theme.warning,
        Tone::Success => theme.success,
        Tone::Accent => theme.accent,
        Tone::Muted => theme.muted,
        Tone::Error => theme.danger,
    }
}

/// The colour of a state word (`Needs you · approval`, `Completed`): the state's own colour for
/// warning, error and success; `muted` for working and idle, because the accent is spent on the
/// working glyph only; `fg` on a selected row, where state colours are not AA in the light pack
/// and only the glyph keeps its colour.
pub fn state_word_color(theme: &Theme, tone: Tone, selected: bool) -> Hsla {
    match tone {
        _ if selected => theme.fg,
        Tone::Warning | Tone::Success | Tone::Error => tone_color(theme, tone),
        Tone::Accent | Tone::Muted => theme.muted,
    }
}

/// The title of an agent record, one component for the Agents page, the Agents panel and Today:
/// medium weight, cut with "…", and `Assistant` beside it for Mawas.
pub fn agent_title_line(record: &AgentRecord, theme: &Theme, tokens: &Tokens) -> gpui::Div {
    use crate::sessions::presentation::Role;
    let mut line = div().flex().items_center().gap(px(8.0)).h(px(16.0)).child(
        div()
            .min_w_0()
            .truncate()
            .font_weight(gpui::FontWeight::MEDIUM)
            .child(SharedString::from(record.title.clone())),
    );
    if record.role == Role::Assistant {
        line = line.child(
            div()
                .flex_none()
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .child(Role::Assistant.label()),
        );
    }
    line
}

/// The one state word of an agent record (`agents_text::state_word`: attention wins over
/// `Working`), at the start of the line under the title, in `state_word_color`.
pub fn agent_state_word(
    record: &AgentRecord,
    selected: bool,
    theme: &Theme,
    tokens: &Tokens,
) -> gpui::Div {
    use crate::sessions::agents_text;
    let (_, tone) = agents_text::lead(record);
    div()
        .flex_none()
        .whitespace_nowrap()
        .text_size(px(tokens.secondary_font_size))
        .text_color(state_word_color(theme, tone, selected))
        .child(SharedString::from(agents_text::state_word(record)))
}

/// The 16 px lane of a row's state glyph: attention first, else the state's own shape, in the
/// state's colour (a glyph is never colour alone).
pub fn agent_lead_glyph(record: &AgentRecord, theme: &Theme) -> gpui::Div {
    let (icon_id, tone) = crate::sessions::agents_text::lead(record);
    div()
        .flex_none()
        .w(px(16.0))
        .text_color(tone_color(theme, tone))
        .child(crate::icons::glyph(icon_id))
}

/// State shape and tone from the same presentation used by the adjacent label/tooltip.
pub fn state_dot(theme: &Theme, presentation: &Presentation) -> AnyElement {
    icon(
        presentation.icon(),
        14.0,
        tone_color(theme, presentation.tone),
    )
    .into_any_element()
}

/// The compact agent badge of a session: `↳2` for the subagents running, `▲` when any node is
/// blocked; its tooltip lists them. `None` while the session has nothing to show.
pub fn agent_badge(
    theme: &Theme,
    badge: &AgentBadge,
    site: BadgeSite,
    session: SessionId,
    regions: &crate::workbench::regions::Regions,
    cx: &App,
) -> Option<AnyElement> {
    if badge.is_empty() {
        return None;
    }
    let tokens = UiStyle::get(cx).density.ui();
    let tip = SharedString::from(badge.tooltip());
    let site_name = match site {
        BadgeSite::Row => "row",
        BadgeSite::Tab => "tab",
    };
    Some(
        div()
            .id(SharedString::from(format!(
                "agent-badge-{site_name}-{session}"
            )))
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing))
            .text_size(px(tokens.secondary_font_size))
            .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
            .children(badge.running_text().map(|text| {
                div()
                    .text_color(theme.accent)
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(crate::icons::glyph(Icon::ActionSubagents))
                    .child(SharedString::from(text))
            }))
            .children(badge.blocked_icon().map(|id| {
                div()
                    .text_color(theme.warning)
                    .child(crate::icons::glyph(id))
            }))
            .child(probe(regions, Region::AgentBadge(site, session)))
            .into_any_element(),
    )
}

/// The placement badge of a session row: `main` or `wt <branch>`, framed like the other row
/// badges, with a warning border and `⚠` while another live agent works in the same checkout.
/// Its tooltip names the checkout.
pub fn placement_badge(
    theme: &Theme,
    badge: &PlacementBadge,
    site: BadgeSite,
    session: SessionId,
    regions: &crate::workbench::regions::Regions,
    cx: &App,
) -> AnyElement {
    let tokens = UiStyle::get(cx).density.ui();
    let tip = SharedString::from(badge.tooltip.clone());
    let (color, text) = if badge.shared {
        (theme.warning, badge.text.clone())
    } else {
        (theme.muted, badge.text.clone())
    };
    div()
        .id(SharedString::from(format!("placement-badge-{session}")))
        .relative()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.0))
        .max_w(px(130.0))
        .truncate()
        // A quiet badge is text on the row's own fill and lines up with the title above it; only
        // the warning has a tint to pad.
        .px(px(if badge.shared {
            tokens.spacing * 2.0
        } else {
            0.0
        }))
        .rounded(px(crate::theme::pill_radius()))
        .bg(color.opacity(if badge.shared { 0.14 } else { 0.0 }))
        .text_size(px(tokens.secondary_font_size))
        .text_color(color)
        .cursor_pointer()
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |event, window, cx| {
            window.dispatch_action(
                Box::new(ShowPlacementMenu(SessionMenuAt {
                    session: session.to_string(),
                    position: event.position(),
                })),
                cx,
            );
        })
        .when(badge.shared, |s| {
            s.child(crate::icons::glyph(Icon::VcsSharedWarning))
        })
        .child(SharedString::from(text))
        .child(probe(regions, Region::PlacementBadge(site, session)))
        .into_any_element()
}

/// The small chip a session wears after a daemon restart until it is acknowledged:
/// `respawned`, `resumed`, `interrupted`, `not restarted`. Its tooltip says what happened. The
/// session's other chips are the caller's; this one only says what the restart did.
pub fn recovery_chip(
    theme: &Theme,
    chip: &hutan_client::recovery::Chip,
    site: BadgeSite,
    session: SessionId,
    compact: bool,
    regions: &crate::workbench::regions::Regions,
    cx: &App,
) -> AnyElement {
    let tokens = UiStyle::get(cx).density.ui();
    if compact {
        // Quiet: a restart that went as planned is a muted glyph; only a session that needs a
        // look is a warning.
        let color = match chip.tone {
            hutan_client::recovery::ChipTone::Info => theme.muted,
            hutan_client::recovery::ChipTone::Warn => theme.warning,
        };
        // A glyph instead of a word, so a sidebar full of restarted sessions keeps their titles
        // readable; the tooltip has the word and the sentence.
        let tip = SharedString::from(format!("{}: {}", chip.label, chip.detail));
        let glyph = match chip.tone {
            hutan_client::recovery::ChipTone::Info => Icon::ActionRecover,
            hutan_client::recovery::ChipTone::Warn => Icon::StateWarning,
        };
        return div()
            .id(SharedString::from(format!("recovery-chip-{session}")))
            .relative()
            .flex_none()
            .text_size(px(tokens.secondary_font_size))
            .text_color(color)
            .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
            .child(crate::icons::glyph(glyph))
            .child(probe(regions, Region::RecoveryChip(site, session)))
            .into_any_element();
    }
    let color = match chip.tone {
        hutan_client::recovery::ChipTone::Info => theme.accent,
        hutan_client::recovery::ChipTone::Warn => theme.warning,
    };
    let tip = SharedString::from(chip.detail.clone());
    div()
        .id(SharedString::from(format!("recovery-chip-{session}")))
        .relative()
        .flex_none()
        .px(px(tokens.spacing * 2.0))
        .rounded(px(crate::theme::pill_radius()))
        .bg(color.opacity(0.14))
        .text_size(px(tokens.secondary_font_size))
        .text_color(color)
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
        .child(chip.label)
        .child(probe(regions, Region::RecoveryChip(site, session)))
        .into_any_element()
}

pub fn kind_icon(kind: SessionKind) -> Icon {
    match kind {
        SessionKind::Terminal => Icon::SessionTerminal,
        SessionKind::Harness => Icon::SessionHarness,
        SessionKind::Direct => Icon::SessionDirect,
    }
}

/// Last path component of the session's working directory (`~` for the home directory).
pub fn placement(info: &SessionInfo) -> String {
    match &info.cwd {
        Some(cwd) => {
            if std::env::home_dir().is_some_and(|home| *cwd == home) {
                return "~".to_owned();
            }
            cwd.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| cwd.display().to_string())
        }
        None => String::new(),
    }
}

/// Applies a key press to a text field (see [`edit_keys`] for the keys). With `editor.vim_mode`
/// on (`config`), Escape (Insert, Visual to Normal) and the Normal and Visual commands are Vim's;
/// in Normal and Visual mode a key Vim does not use changes nothing (⌘ shortcuts still apply).
/// Typed text is not a key: it reaches the field through the platform's text input
/// ([`field_box`]'s element). `vim` is the field's modal state. `true` when the text or the
/// caret changed or the key was used.
pub fn edit_text(
    input: &mut TextInput,
    vim: &mut Vim,
    config: VimConfig,
    event: &KeyDownEvent,
    multiline: bool,
    cx: &mut App,
) -> bool {
    edit_field(
        &mut FieldMut {
            input,
            vim: config.enabled.then_some(vim),
            multiline,
        },
        config,
        event,
        cx,
    )
}

/// A text field's box: the text with a caret while `focused`, or the dim `placeholder` while it
/// is empty. The caller adds the id's click, focus tracking and key handler. `field` makes it
/// editable by the mouse and the platform's text input. With `vim` (the field's modal state,
/// given under Vim mode) the caret is a block in Normal and Visual mode and the mode pill sits
/// at the right while `focused`.
#[allow(clippy::too_many_arguments)] // the field's look and its state, one call per form field
pub fn field_box(
    id: impl Into<SharedString>,
    input: &TextInput,
    vim: Option<&Vim>,
    placeholder: &str,
    focused: bool,
    multiline: bool,
    field: Option<FieldRef>,
    theme: &Theme,
    tokens: &Tokens,
) -> gpui::Stateful<gpui::Div> {
    let id: SharedString = id.into();
    let text = EditableText::new(
        SharedString::from(format!("{id}-text")),
        FieldState::of(input, vim),
        theme,
    )
    .placeholder(placeholder.to_owned())
    .multiline(multiline)
    .focused(focused)
    .field(field);
    let field = div()
        .id(id)
        .flex()
        .items_center()
        .min_h(px(tokens.sidebar_row))
        .when(multiline, |s| {
            s.min_h(px(tokens.sidebar_row * 2.5)).items_start()
        })
        .px(px(tokens.spacing * 2.0))
        .py(px(tokens.spacing / 2.0))
        .outlined(theme, tokens)
        .focus_ring(focused, theme, tokens)
        .text_color(theme.fg)
        .cursor_text();
    match vim {
        None => field.child(div().flex_1().min_w_0().child(text)),
        Some(vim) => field
            .gap(px(tokens.spacing))
            .child(div().flex_1().min_w_0().child(text))
            .when(focused, |s| {
                s.child(vim_ui::mode_pill(vim.mode(), theme, tokens))
            }),
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// `now`, `3m`, `2h`, `4d` since `created_at_ms`.
pub fn age_label(created_at_ms: u64) -> String {
    let secs = now_ms().saturating_sub(created_at_ms) / 1000;
    match secs {
        0..=59 => "now".to_owned(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// The tooltip of a time shown relative (`3m`, `3 min ago`): the absolute local time with its
/// zone (`docs/design.md` § Time).
pub fn time_tooltip(ms: u64) -> impl Fn(&mut gpui::Window, &mut App) -> gpui::AnyView + 'static {
    let text = SharedString::from(hutan_client::time::local_time_seconds_text(ms));
    move |_, cx| cx.new(|_| TextTooltip(text.clone())).into()
}

/// Panel header (Orca's section header: 32 px, 12 px semibold, muted): the label on the left, the ⚙ that opens the panel's settings on the
/// right.
pub fn panel_header(title: &str, namespace: &'static str, cx: &App) -> AnyElement {
    panel_header_with(title, namespace, Vec::new(), cx)
}

/// [`panel_header`] with `extra` controls between the title and the ⚙.
pub fn panel_header_with(
    title: &str,
    namespace: &'static str,
    extra: Vec<AnyElement>,
    cx: &App,
) -> AnyElement {
    let style = UiStyle::get(cx);
    let tokens = style.density.ui();
    let hover = style.theme.surface_hover;
    let gear = div()
        .id(SharedString::from(format!("gear-{namespace}")))
        .flex_none()
        .px(px(tokens.spacing * 1.5))
        .rounded(px(tokens.radius))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| {
            window.dispatch_action(Box::new(OpenSettingsFor(namespace.to_owned())), cx);
        })
        .child(icon(Icon::ActionSettings, 14.0, style.theme.muted))
        .into_any_element();
    panel_header_controls(title, extra.into_iter().chain([gear]).collect(), cx)
}

/// A panel header with exactly the `controls` on its right (the Projects header's `＋ ⋯`: its
/// settings are an entry of the `⋯` menu, not a ⚙ of their own).
pub fn panel_header_controls(title: &str, controls: Vec<AnyElement>, cx: &App) -> AnyElement {
    let style = UiStyle::get(cx);
    let tokens = style.density.ui();
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .h(px(32.0))
        .px(px(tokens.spacing * 3.0))
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_size(px(12.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(style.theme.muted)
                .child(SharedString::from(title.to_owned())),
        )
        .child(
            div()
                .flex()
                .min_w_0()
                .items_center()
                .gap(px(tokens.spacing))
                .children(controls),
        )
        .into_any_element()
}
