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
use crate::sessions::PlacementBadge;
use crate::sessions::{ScopeKind, ScopeLabel};
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
use hutan_agents_view::agents::AgentBadge;
use hutan_agents_view::attention::{Presentation, Tone};
use hutan_agents_view::presentation::AgentRecord;

pub fn tone_color(theme: &Theme, tone: Tone) -> Hsla {
    match tone {
        Tone::Warning => theme.warning,
        Tone::Success => theme.success,
        Tone::Accent => theme.accent,
        Tone::Muted => theme.muted,
        Tone::Error => theme.danger,
    }
}

/// The colour of a state word (`Approval`, `Completed`): the state's own colour for
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
/// the sidebar's name weight, cut with "…", and `Assistant` beside it for Mawas.
pub fn agent_title_line(record: &AgentRecord, theme: &Theme, tokens: &Tokens) -> gpui::Div {
    agent_title_line_with(record, None, theme, tokens)
}

/// [`agent_title_line`] with the name the window gives the session (`SessionsStore::title_parts`):
/// a fork's mark after it, which never gives way to the "…".
pub fn agent_title_line_with(
    record: &AgentRecord,
    parts: Option<crate::sessions::fork::TitleParts>,
    theme: &Theme,
    tokens: &Tokens,
) -> gpui::Div {
    use hutan_agents_view::presentation::Role;
    // The name as the sidebar's card writes it: regular, semibold while something waits for a
    // look, faint once the session ended; an agent's own emoji prefix is not drawn.
    let needs = record.group.attention().is_some();
    let (head, tail) = match parts {
        Some(parts) => (parts.head, parts.tail),
        None => (
            hutan_agents_view::titles::strip_pictographs(&record.title).to_owned(),
            None,
        ),
    };
    let mut line = div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .h(px(16.0))
        .child(
            div()
                .min_w_0()
                .truncate()
                .when(needs, |s| s.font_weight(gpui::FontWeight::SEMIBOLD))
                .when(record.state.is_finished() && !needs, |s| {
                    s.text_color(theme.faint)
                })
                .child(SharedString::from(head)),
        )
        .children(tail.map(|tail| {
            title_mark(
                SharedString::from(format!("agent-mark-{}", record.session)),
                &tail,
                theme.muted,
                tokens.secondary_font_size,
            )
        }));
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

/// The icon of an agent record, as the sidebar's row of the same session draws it: its
/// harness's monogram (an agent chat the same as an agent in a terminal), the bubble of a direct
/// chat, Mawas's sparkle.
pub fn record_icon(record: &AgentRecord) -> Icon {
    use hutan_agents_view::presentation::Role;
    match (record.role, record.agent.as_str()) {
        (Role::Assistant, _) => Icon::PanelAssistant,
        (_, "direct") => Icon::SessionDirect,
        (_, app) => harness_icon(app),
    }
}

/// The lead lane of a row on the Agents page: the harness monogram with the state's badge on
/// it, the same lane as the sidebar's (`ui::row_lane`); `ground` is what the row is filled with.
pub fn agent_lane(record: &AgentRecord, selected: bool, ground: Hsla, theme: &Theme) -> gpui::Div {
    crate::workbench::ui::row_lane(
        SharedString::from(format!("agents-lane-{}", record.session)),
        record_icon(record),
        crate::sessions::agents_text::lane_state(record),
        ground,
        selected,
        theme,
    )
}

/// State shape and tone from the same presentation used by the adjacent label/tooltip.
pub fn state_dot(theme: &Theme, presentation: &Presentation) -> AnyElement {
    icon(
        crate::sessions::agents::presentation_icon(presentation),
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
            .tooltip(move |_, cx| cx.new(|_| TextTooltip::new(tip.clone())).into())
            .children(badge.running_text().map(|text| {
                div()
                    .text_color(theme.accent)
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(crate::icons::glyph(Icon::ActionSubagents))
                    .child(SharedString::from(text))
            }))
            .children(crate::sessions::agents::blocked_icon(badge).map(|id| {
                div()
                    .text_color(theme.warning)
                    .child(crate::icons::glyph(id))
            }))
            .child(probe(regions, Region::AgentBadge(site, session)))
            .into_any_element(),
    )
}

/// The placement badge of a session row's second line: `wt <branch>` (a worktree is news; the
/// main checkout every row would repeat), neutral text, never a tinted pill. A checkout another
/// live agent works in is the project or worktree row's [`shared_checkout_chip`], not a colour here; the
/// tooltip names the checkout.
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
    div()
        .id(SharedString::from(format!("placement-badge-{session}")))
        .relative()
        .min_w_0()
        .flex()
        .items_center()
        .max_w(px(130.0))
        .truncate()
        // Text on the row's own fill, lined up with the title above it.
        .text_size(px(tokens.secondary_font_size))
        .text_color(theme.muted)
        .cursor_pointer()
        .tooltip(move |_, cx| cx.new(|_| TextTooltip::new(tip.clone())).into())
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
        .child(SharedString::from(badge.text.clone()))
        .child(probe(regions, Region::PlacementBadge(site, session)))
        .into_any_element()
}

/// The chip a project or worktree row wears while another live agent works in the same
/// checkout: the `warning` glyph on a quiet `warning` fill, 16 pt (the row has no room for a
/// word: the name keeps it). It is said once, on the row of the checkout; the session rows
/// below it do not repeat it. The tooltip says what it is, who and where.
pub fn shared_checkout_chip(
    theme: &Theme,
    badge: &PlacementBadge,
    root: &std::path::Path,
    regions: &crate::workbench::regions::Regions,
) -> AnyElement {
    let tip = SharedString::from(badge.tooltip.clone());
    div()
        .id(SharedString::from(format!(
            "shared-checkout-{}",
            root.display()
        )))
        .relative()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(px(16.0))
        .rounded(px(4.0))
        .bg(theme.warning.opacity(0.14))
        .tooltip(move |_, cx| cx.new(|_| TextTooltip::new(tip.clone())).into())
        .child(icon(Icon::VcsSharedWarning, 12.0, theme.warning))
        .child(probe(regions, Region::shared_checkout(root)))
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
            .tooltip(move |_, cx| cx.new(|_| TextTooltip::new(tip.clone())).into())
            .child(crate::icons::glyph(glyph))
            .child(probe(regions, Region::RecoveryChip(site, session)))
            .into_any_element();
    }
    // A word with its sentence in the tooltip: a neutral pill; a restart that needs a look wears
    // the 16 px warning glyph in front of it, the pill never a warning colour.
    let tip = SharedString::from(chip.detail.clone());
    div()
        .id(SharedString::from(format!("recovery-chip-{session}")))
        .relative()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(tokens.spacing))
        .px(px(tokens.spacing * 2.0))
        .rounded(px(crate::theme::pill_radius()))
        .bg(theme.surface_hover)
        .text_size(px(tokens.secondary_font_size))
        .text_color(theme.fg)
        .tooltip(move |_, cx| cx.new(|_| TextTooltip::new(tip.clone())).into())
        .when(chip.tone == hutan_client::recovery::ChipTone::Warn, |s| {
            s.child(icon(Icon::StateWarning, 14.0, theme.warning))
        })
        .child(chip.label)
        .child(probe(regions, Region::RecoveryChip(site, session)))
        .into_any_element()
}

/// A fork's mark after its name (`⑂ 2`, `⑂ branch`, [`crate::sessions::fork::TitleParts`]): the
/// pack's fork glyph stands for each `⑂` (no text font is promised to have the character), the
/// rest is `size` px text; it never shrinks, the name before it does.
pub fn title_mark(
    id: impl Into<gpui::ElementId>,
    tail: &str,
    color: Hsla,
    size: f32,
) -> gpui::Stateful<gpui::Div> {
    let mut mark = div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.0))
        .text_size(px(size))
        .text_color(color);
    for (n, piece) in tail.split('⑂').enumerate() {
        if n > 0 {
            mark = mark.child(icon(Icon::ActionFork, size, color));
        }
        let piece = piece.trim();
        if !piece.is_empty() {
            mark = mark.child(SharedString::from(piece.to_owned()));
        }
    }
    mark
}

/// The glyph of a coding agent by its program (`omp`, `claude`, `codex`; a hook may say
/// `Claude Code`); another harness wears the generic one.
pub fn harness_icon(app: &str) -> Icon {
    let app = app.trim().to_ascii_lowercase();
    if app.starts_with("omp") {
        Icon::HarnessOmp
    } else if app.starts_with("claude") {
        Icon::HarnessClaude
    } else if app.starts_with("codex") {
        Icon::HarnessCodex
    } else {
        Icon::HarnessUnknown
    }
}

/// The icon of a session on every surface (tab, sidebar row, picker): Mawas's conversations
/// wear the Mawas sparkle whatever backend runs them, every other session its harness's
/// monogram (an agent chat the same as an agent in a terminal), a direct chat the bubble, a
/// plain terminal the prompt.
pub fn session_icon(info: &SessionInfo) -> Icon {
    session_icon_with(info, None)
}

/// [`session_icon`] for a terminal whose agent is no longer reported (the session ended: the
/// daemon forgets what ran in it): `remembered` is the program the window saw in it.
pub fn session_icon_with(info: &SessionInfo, remembered: Option<&str>) -> Icon {
    if info.chat.as_ref().is_some_and(|chat| chat.is_assistant()) {
        return Icon::PanelAssistant;
    }
    match info.kind {
        // A terminal with a coding agent in it is that agent's, not a plain terminal; so is one
        // that started as the agent's program, and one that has ended.
        SessionKind::Terminal => {
            match hutan_agents_view::attention::agent_app(info)
                .or(remembered)
                .or_else(|| started_as(info))
            {
                Some(app) => harness_icon(app),
                None => Icon::SessionTerminal,
            }
        }
        SessionKind::Harness => match info.chat.as_ref().map(|chat| chat.agent.as_str()) {
            Some("direct") => Icon::SessionDirect,
            Some(agent) if harness_icon(agent) != Icon::HarnessUnknown => harness_icon(agent),
            _ => Icon::SessionHarness,
        },
        SessionKind::Direct => Icon::SessionDirect,
    }
}

/// The coding agent a terminal session was started as (`omp`, `claude`, `codex` as its program),
/// whether or not it reports now.
pub fn started_as(info: &SessionInfo) -> Option<&'static str> {
    let program = info.command.first()?;
    let name = std::path::Path::new(program).file_name()?.to_str()?;
    ["omp", "claude", "codex"]
        .into_iter()
        .find(|app| name.eq_ignore_ascii_case(app))
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
    move |_, cx| cx.new(|_| TextTooltip::new(text.clone())).into()
}

/// The ⚙ of a panel's header: opens Settings at the panel's namespace.
pub fn settings_gear(namespace: &'static str, cx: &App) -> AnyElement {
    let style = UiStyle::get(cx);
    let tokens = style.density.ui();
    crate::workbench::ui::IconButton::new(
        SharedString::from(format!("gear-{namespace}")),
        Icon::ActionSettings,
        "Panel settings",
        &style.theme,
        &tokens,
    )
    .compact()
    .on_click(move |_, window, cx| {
        window.dispatch_action(Box::new(OpenSettingsFor(namespace.to_owned())), cx);
    })
    .into_any_element()
}

/// A titled panel header with `extra` controls between the title and the ⚙.
pub fn panel_header_with(
    title: &str,
    namespace: &'static str,
    extra: Vec<AnyElement>,
    cx: &App,
) -> AnyElement {
    let gear = settings_gear(namespace, cx);
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
        .child(crate::workbench::ui::group_label(
            title,
            &style.theme,
            &tokens,
        ))
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

/// The toolbar of a panel whose dock tab (or page) already names it (CL-POLISH-4): no title,
/// the `lead` controls on the left (the scope chip), the `trail` ones and the ⚙ of `namespace`
/// on the right. 32 px like a titled header.
pub fn panel_toolbar(
    namespace: &'static str,
    lead: Vec<AnyElement>,
    trail: Vec<AnyElement>,
    cx: &App,
) -> AnyElement {
    let style = UiStyle::get(cx);
    let tokens = style.density.ui();
    let gear = settings_gear(namespace, cx);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .gap(px(tokens.spacing))
        .h(px(32.0))
        .px(px(tokens.spacing * 3.0))
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(tokens.spacing))
                .children(lead),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(tokens.spacing))
                .children(trail)
                .child(gear),
        )
        .into_any_element()
}

/// What a scope chip says: the container's name, and a worktree's branch after it.
pub fn scope_text(scope: &ScopeLabel) -> String {
    match (&scope.kind, &scope.branch) {
        (ScopeKind::Worktree, Some(branch)) => format!("{} · {branch}", scope.name),
        _ => scope.name.clone(),
    }
}

/// The small chip a scoped panel (Files, Changes, Search, Problems, the Agents rail) shows in
/// its header: the glyph of the container's kind and its name, so a panel always says whose
/// files, changes or problems it lists. `panel` names it for tests (`Region::Scope`).
pub fn scope_chip(
    scope: Option<&ScopeLabel>,
    panel: &'static str,
    regions: &crate::workbench::regions::Regions,
    cx: &App,
) -> Option<AnyElement> {
    let scope = scope?;
    let style = UiStyle::get(cx);
    let (theme, tokens) = (style.theme, style.density.ui());
    let glyph = match scope.kind {
        ScopeKind::Home => Icon::NavHome,
        ScopeKind::Workspace => Icon::NavWorkspace,
        ScopeKind::Repository => Icon::NavProjectRepository,
        ScopeKind::Folder => Icon::NavProjectFolder,
        ScopeKind::Worktree => Icon::VcsWorktree,
        ScopeKind::Mawas => Icon::NavMawas,
    };
    let text = SharedString::from(scope_text(scope));
    // The tooltip says who works there too: the dot beside the name is never the only cue.
    let tip = match &scope.working {
        Some(working) => SharedString::from(format!("{text} · {working}")),
        None => text.clone(),
    };
    let working = scope.working.is_some();
    Some(
        div()
            .id(SharedString::from(format!("scope-chip-{panel}")))
            .relative()
            .flex()
            .items_center()
            .gap(px(4.0))
            .min_w_0()
            .max_w(px(168.0))
            .h(px(20.0))
            .px(px(6.0))
            .rounded(px(tokens.radius))
            .text_size(px(11.0))
            .text_color(theme.muted)
            .tooltip(move |_, cx| cx.new(|_| TextTooltip::new(tip.clone())).into())
            .child(icon(glyph, 12.0, theme.muted))
            .child(div().min_w_0().truncate().child(text))
            .when(working, |chip| {
                chip.child(
                    div()
                        .relative()
                        .flex_none()
                        .size(px(6.0))
                        .rounded_full()
                        .bg(theme.muted)
                        .child(probe(regions, Region::ScopeWorking(panel))),
                )
            })
            .child(probe(regions, Region::Scope(panel)))
            .into_any_element(),
    )
}
