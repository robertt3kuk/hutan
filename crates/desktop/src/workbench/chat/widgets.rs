//! Small pieces the chat view shares: buttons, chips, number formatting, and the approval group
//! that every place a permission request is answered draws (the tool card, a request without a
//! tool, the inspector's Requests).

use gpui::{
    AnyElement, FontWeight, Hsla, ParentElement as _, SharedString, Styled as _, div,
    prelude::FluentBuilder as _, px,
};
use hutan_chat_view::ContextUse;
use hutan_core::agent::{PermissionKind, PermissionOption, PermissionOptionKind};

use super::markdown_view::Look;

use crate::icons::Icon;
use crate::workbench::ui;
pub use crate::workbench::ui::{Button, ButtonKind};

/// A small neutral label ("exit 0", "running"); the state's color is on the glyph beside it.
pub fn pill(text: impl Into<SharedString>, look: &Look) -> gpui::Div {
    ui::pill(text, &look.theme, &look.tokens)
}

/// `12 ms`, `1.2 s`, `2 m 3 s`.
pub fn duration(ms: u64) -> String {
    match ms {
        0..=999 => format!("{ms} ms"),
        1000..=59_999 => format!("{:.1} s", ms as f64 / 1000.0),
        _ => format!("{} m {} s", ms / 60_000, (ms % 60_000) / 1000),
    }
}

/// `842`, `22.2k`, `1.4M`.
pub fn tokens(count: u64) -> String {
    match count {
        0..=999 => count.to_string(),
        1000..=999_999 => format!("{:.1}k", count as f64 / 1000.0),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}

/// `842`, `1.2k`, `122k`, `1M`: token counts as the context meter shows them.
pub fn compact_tokens(count: u64) -> String {
    let trimmed = |value: f64, unit: &str| {
        let text = format!("{value:.1}");
        format!("{}{unit}", text.strip_suffix(".0").unwrap_or(&text))
    };
    match count {
        0..=999 => count.to_string(),
        1_000..=9_999 => trimmed(count as f64 / 1000.0, "k"),
        10_000..=999_499 => format!("{}k", (count + 500) / 1000),
        _ => trimmed(count as f64 / 1_000_000.0, "M"),
    }
}

/// The context chip's text: `context 61 % · 122k / 200k`; without a window size only what was
/// used (`context 122k`), since a percentage of nothing is not known.
pub fn context_text(context: &ContextUse) -> String {
    match (context.window, context.percent()) {
        (Some(window), Some(percent)) => format!(
            "context {} · {} / {}",
            if percent == 0 && context.used > 0 {
                "<1 %".to_owned()
            } else {
                format!("{percent} %")
            },
            compact_tokens(context.used),
            compact_tokens(window)
        ),
        _ => format!("context {}", compact_tokens(context.used)),
    }
}

/// The context chip's tooltip: the exact figures, and the cumulative counts the harness
/// reported (nothing it did not; prices are never computed here).
pub fn context_tooltip(context: &ContextUse) -> String {
    let mut parts = vec![match context.window {
        Some(window) => format!(
            "{} of {} tokens in the context",
            thousands(context.used),
            thousands(window)
        ),
        None => format!(
            "{} tokens in the context; this agent reports no window size",
            thousands(context.used)
        ),
    }];
    let counts = &context.counts;
    for (label, value) in [
        ("input", counts.input_tokens),
        ("cached", counts.cached_input_tokens),
        ("output", counts.output_tokens),
        ("reasoning", counts.reasoning_output_tokens),
    ] {
        if let Some(value) = value {
            parts.push(format!("{label} {}", thousands(value)));
        }
    }
    parts.join(" · ")
}

/// `1,234,567`.
fn thousands(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// The last `max` lines of `text` and how many came before them.
pub fn tail_lines(text: &str, max: usize) -> (&str, usize) {
    let total = text.lines().count();
    if total <= max {
        return (text, 0);
    }
    let skip = total - max;
    let mut at = 0;
    for _ in 0..skip {
        at += text[at..].find('\n').map_or(text.len() - at, |i| i + 1);
    }
    (&text[at..], skip)
}

// ---- the approval group ---------------------------------------------------------------------

/// What an approval asks, by its kind.
pub fn approval_heading(kind: PermissionKind) -> &'static str {
    match kind {
        PermissionKind::Command => "Run this command?",
        PermissionKind::FileChange => "Apply these changes?",
        PermissionKind::Permissions => "Grant extra permissions?",
        PermissionKind::Control => "Let this agent control Hutan?",
        PermissionKind::Other => "Allow this?",
    }
}

/// Longest tool name a sentence about a standing rule quotes.
pub const SCOPE_TITLE: usize = 80;

/// The tool a request is about as the sentences quote it: its title's first line, at most
/// [`SCOPE_TITLE`] characters.
fn quoted_tool(title: &str) -> String {
    let first = title.lines().next().unwrap_or_default();
    let mut tool: String = first.chars().take(SCOPE_TITLE).collect();
    if first.chars().count() > SCOPE_TITLE {
        tool.push('…');
    }
    tool
}

/// The sentence that says what a standing ("Always") option does beyond this request, naming
/// the tool the request is about (`title`) and the harness whose rule it is. `None` for options
/// that only answer this request.
pub fn scope_sentence(
    option: &PermissionOption,
    title: &str,
    agent: Option<&str>,
) -> Option<String> {
    let verb = match option.kind {
        PermissionOptionKind::AllowAlways => "stops asking about",
        PermissionOptionKind::RejectAlways => "refuses, without asking,",
        _ => return None,
    };
    let agent = agent.unwrap_or("the agent");
    let tool = quoted_tool(title);
    Some(format!(
        "beyond this one request, {agent} {verb} what its rule for “{tool}” covers. Hutan cannot show or revoke that rule."
    ))
}

/// The tooltip of option `number` (0-based) when it sets a standing rule: its key and label, then
/// [`scope_sentence`] (`2 Always allow: beyond this one request, OMP stops asking about …`).
pub fn scope_tooltip(
    kind: PermissionKind,
    number: usize,
    option: &PermissionOption,
    title: &str,
    agent: Option<&str>,
) -> Option<String> {
    if kind == PermissionKind::Control {
        return None;
    }
    scope_sentence(option, title, agent)
        .map(|sentence| format!("{} {}: {sentence}", number + 1, option.label))
}

/// The one muted sentence under the buttons while a standing ("Always") option is offered; the
/// whole of what each one does is its button's tooltip.
pub fn always_note(
    kind: PermissionKind,
    options: &[PermissionOption],
    title: &str,
) -> Option<String> {
    // Hutan's own gate (a `control` request) keeps its grants in Settings → Mawas, where the
    // user lists and revokes them: the sentence about rules nobody can see does not apply.
    if kind == PermissionKind::Control {
        return None;
    }
    options
        .iter()
        .any(|o| {
            matches!(
                o.kind,
                PermissionOptionKind::AllowAlways | PermissionOptionKind::RejectAlways
            )
        })
        .then(|| {
            format!(
                "Always sets a standing rule for “{}” that Hutan cannot show or revoke.",
                quoted_tool(title)
            )
        })
}

/// One option of a request: its label and its key (`1`–`4`, the agent's order) inside the button,
/// primary only for *allow once* ([`ui::approval_button_kind`]), and `tooltip` (what a standing
/// option sets) on hover. The caller adds the click and the probe.
pub fn approval_option(
    id: impl Into<gpui::ElementId>,
    number: usize,
    option: &PermissionOption,
    tooltip: Option<String>,
    look: &Look,
) -> Button {
    Button::new(id, option.label.clone(), &look.theme, &look.tokens)
        .kind(ui::approval_button_kind(option.kind))
        .shortcut((number + 1).to_string())
        .when_some(tooltip, |button, tip| button.tooltip(tip))
}

/// A request waiting for an answer, as one group: the question (13 px medium), what it is about
/// (`title`, mono, when the place it sits in does not already say it), where and what it touches
/// (`detail`, mono 12 px `muted`), the option `buttons` on one row (they wrap only when the column
/// is too narrow), and `note` (the Always sentence) muted under them. No fill and no border of
/// its own: it sits in a tool card under a hairline, in a card of its own, or in a panel row.
pub fn approval_group(
    kind: PermissionKind,
    title: Option<&str>,
    detail: Option<&str>,
    buttons: Vec<AnyElement>,
    note: Option<String>,
    suggestion: Option<AnyElement>,
    look: &Look,
) -> gpui::Div {
    let (theme, tokens) = (&look.theme, &look.tokens);
    div()
        .flex()
        .flex_col()
        .gap(px(tokens.spacing * 2.0))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing))
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.fg)
                        .child(approval_heading(kind)),
                )
                .children(title.map(|title| {
                    div()
                        .font_family(look.mono.clone())
                        .text_color(theme.fg)
                        .child(SharedString::from(title.to_owned()))
                }))
                .children(detail.map(|detail| {
                    div()
                        .font_family(look.mono.clone())
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child(SharedString::from(detail.to_owned()))
                })),
        )
        .children(suggestion)
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(tokens.spacing * 2.0))
                .children(buttons),
        )
        .children(note.map(|note| {
            div()
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .child(SharedString::from(note))
        }))
}

/// What Mawas suggests for a request that waits (protocol 0.64, `ask.suggest`): the answer she
/// would give, why, and *Use* / *Dismiss* (`buttons`). A tinted box with her name on it, so it
/// is never mistaken for the agent's own words or for the user's options; it answers nothing
/// until the user clicks *Use*, which is the user's own answer.
pub fn suggestion_box(
    answer: &str,
    reason: &str,
    buttons: Vec<AnyElement>,
    look: &Look,
) -> gpui::Div {
    let (theme, tokens) = (&look.theme, &look.tokens);
    div()
        .relative()
        .flex()
        .flex_col()
        .gap(px(tokens.spacing))
        .p(px(tokens.spacing * 2.0))
        .rounded(px(tokens.radius - 2.0))
        .bg(theme.accent_soft)
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(tokens.spacing))
                .text_size(px(tokens.secondary_font_size))
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.accent)
                        .child("Mawas suggests:"),
                )
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.fg)
                        .child(SharedString::from(answer.to_owned())),
                ),
        )
        .child(
            div()
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .child(SharedString::from(reason.to_owned())),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(tokens.spacing * 2.0))
                .children(buttons),
        )
}

/// *Use* and *Dismiss* of a suggestion of Mawas: plain buttons (the accent is spent on *Allow
/// once*, never on advice). `key` tells the card apart; the caller adds the handlers and probes.
pub fn suggestion_buttons(key: &str, look: &Look) -> (Button, Button) {
    (
        Button::new(
            SharedString::from(format!("suggest-use-{key}")),
            "Use",
            &look.theme,
            &look.tokens,
        ),
        Button::new(
            SharedString::from(format!("suggest-dismiss-{key}")),
            "Dismiss",
            &look.theme,
            &look.tokens,
        )
        .ghost(),
    )
}

/// The glyph and color of a decision: allowed in `success`, denied in `danger`, anything else
/// (cancelled, the agent's own kind, no answer) `muted`.
pub fn decision_mark(kind: Option<PermissionOptionKind>, look: &Look) -> (Icon, Hsla) {
    let theme = &look.theme;
    match kind {
        Some(PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways) => {
            (Icon::StateCompleted, theme.success)
        }
        Some(PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways) => {
            (Icon::StateFailed, theme.danger)
        }
        _ => (Icon::StateIdle, theme.muted),
    }
}

/// An answered (or expired) request, collapsed to one line: the decision's glyph in its color,
/// the words (`Allowed once · from this window`) `muted`.
pub fn approval_outcome(
    mark: (Icon, Hsla),
    text: impl Into<SharedString>,
    look: &Look,
) -> gpui::Div {
    let (theme, tokens) = (&look.theme, &look.tokens);
    div()
        .flex()
        .items_center()
        .gap(px(tokens.spacing * 1.5))
        .min_w_0()
        .text_size(px(tokens.secondary_font_size))
        .text_color(theme.muted)
        .child(
            div()
                .flex_none()
                .text_color(mark.1)
                .child(crate::icons::glyph(mark.0)),
        )
        .child(div().min_w_0().truncate().child(text.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(kind: PermissionOptionKind, label: &str) -> PermissionOption {
        PermissionOption {
            id: label.to_owned(),
            label: label.to_owned(),
            kind,
            provider: None,
        }
    }

    #[test]
    fn only_allow_once_is_primary() {
        use PermissionOptionKind::*;
        let primary: Vec<bool> = [
            AllowOnce,
            AllowAlways,
            RejectOnce,
            RejectAlways,
            Cancel,
            Other,
        ]
        .map(|kind| ui::approval_button_kind(kind) == ButtonKind::Primary)
        .to_vec();
        assert_eq!(primary, [true, false, false, false, false, false]);
    }

    #[test]
    fn standing_options_say_whose_rule_they_set_and_for_what() {
        let title = "cargo test --workspace\nsecond line";
        let always = option(PermissionOptionKind::AllowAlways, "Always allow `cargo`");
        let sentence = scope_sentence(&always, title, Some("Claude Code")).unwrap();
        assert!(sentence.starts_with(
            "beyond this one request, Claude Code stops asking about what its rule for “cargo test --workspace” covers."
        ));
        let never = option(PermissionOptionKind::RejectAlways, "Never");
        assert!(
            scope_sentence(&never, title, None)
                .unwrap()
                .starts_with("beyond this one request, the agent refuses, without asking, what")
        );
        for kind in [
            PermissionOptionKind::AllowOnce,
            PermissionOptionKind::RejectOnce,
            PermissionOptionKind::Cancel,
            PermissionOptionKind::Other,
        ] {
            assert_eq!(scope_sentence(&option(kind, "x"), title, None), None);
            assert_eq!(
                scope_tooltip(PermissionKind::Command, 0, &option(kind, "x"), title, None),
                None
            );
        }
        // The tooltip is the line the card used to print under its buttons.
        assert_eq!(
            scope_tooltip(PermissionKind::Command, 1, &always, title, Some("OMP")).unwrap(),
            format!(
                "2 Always allow `cargo`: {}",
                scope_sentence(&always, title, Some("OMP")).unwrap()
            )
        );
        // A long tool name is cut, not left to flood the card.
        let long = "x".repeat(200);
        let sentence = scope_sentence(&always, &long, None).unwrap();
        assert!(sentence.contains(&format!("“{}…”", "x".repeat(SCOPE_TITLE))));
    }

    #[test]
    fn one_sentence_stands_for_every_always_option() {
        let once = [
            option(PermissionOptionKind::AllowOnce, "Allow once"),
            option(PermissionOptionKind::RejectOnce, "Reject"),
        ];
        assert_eq!(
            always_note(PermissionKind::Command, &once, "edit: src/lib.rs"),
            None
        );
        // Hutan's own gate lists and revokes what it stores (Settings → Mawas): no such sentence.
        let control = [
            option(PermissionOptionKind::AllowOnce, "Allow once"),
            option(PermissionOptionKind::AllowAlways, "Allow for this chat"),
        ];
        assert_eq!(
            always_note(PermissionKind::Control, &control, "rename a session"),
            None
        );
        assert_eq!(
            scope_tooltip(
                PermissionKind::Control,
                1,
                &control[1],
                "rename a session",
                None
            ),
            None
        );
        let all = [
            option(PermissionOptionKind::AllowOnce, "Allow once"),
            option(PermissionOptionKind::AllowAlways, "Always allow"),
            option(PermissionOptionKind::RejectOnce, "Reject"),
            option(PermissionOptionKind::RejectAlways, "Always reject"),
        ];
        assert_eq!(
            always_note(PermissionKind::Command, &all, "edit: src/lib.rs\nmore").as_deref(),
            Some(
                "Always sets a standing rule for “edit: src/lib.rs” that Hutan cannot show or revoke."
            )
        );
    }

    #[test]
    fn durations_and_counts_read_naturally() {
        assert_eq!(duration(12), "12 ms");
        assert_eq!(duration(1234), "1.2 s");
        assert_eq!(duration(125_000), "2 m 5 s");
        assert_eq!(tokens(842), "842");
        assert_eq!(tokens(22_178), "22.2k");
        assert_eq!(tokens(1_400_000), "1.4M");
    }

    #[test]
    fn the_context_chip_reads_naturally() {
        use hutan_core::agent::TokenCounts;
        let used = |used: u64, window: Option<u64>| ContextUse {
            used,
            window,
            counts: TokenCounts::default(),
        };
        assert_eq!(
            context_text(&used(122_000, Some(200_000))),
            "context 61 % · 122k / 200k"
        );
        assert_eq!(
            context_text(&used(1_234, Some(200_000))),
            "context <1 % · 1.2k / 200k"
        );
        assert_eq!(
            context_text(&used(0, Some(200_000))),
            "context 0 % · 0 / 200k"
        );
        assert_eq!(
            context_text(&used(1_900_000, Some(2_000_000))),
            "context 95 % · 1.9M / 2M"
        );
        assert_eq!(context_text(&used(122_000, None)), "context 122k");
        assert_eq!(compact_tokens(999_600), "1M");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(thousands(12), "12");
        let counts = TokenCounts {
            input_tokens: Some(1_200),
            output_tokens: Some(34),
            ..TokenCounts::default()
        };
        assert_eq!(
            context_tooltip(&ContextUse {
                used: 1_234,
                window: Some(200_000),
                counts,
            }),
            "1,234 of 200,000 tokens in the context · input 1,200 · output 34"
        );
        assert!(context_tooltip(&used(5, None)).contains("reports no window size"));
    }

    #[test]
    fn tail_lines_keeps_the_end() {
        let text = "1\n2\n3\n4\n5\n";
        assert_eq!(tail_lines(text, 2), ("4\n5\n", 3));
        assert_eq!(tail_lines(text, 9), (text, 0));
        assert_eq!(tail_lines("", 3), ("", 0));
        assert_eq!(tail_lines("a\nb", 1), ("b", 1));
    }
}
