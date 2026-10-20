//! Small pieces the chat view shares: buttons, chips and number formatting.

use gpui::{Hsla, SharedString};
use hutan_chat_view::ContextUse;

use super::markdown_view::Look;

use crate::workbench::ui;
pub use crate::workbench::ui::ButtonKind;

/// A text button (see [`ui::button`]) styled by the chat's [`Look`]. A disabled one is drawn dim
/// and ignores the mouse (the caller adds the click handler only when enabled).
pub fn button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    enabled: bool,
    look: &Look,
) -> gpui::Stateful<gpui::Div> {
    ui::button(id, label, kind, enabled, &look.theme, &look.tokens)
}

/// A round icon button (see [`ui::round_button`]).
pub fn round_button(
    id: impl Into<gpui::ElementId>,
    glyph: crate::icons::Icon,
    primary: bool,
    enabled: bool,
    size: f32,
    look: &Look,
) -> gpui::Stateful<gpui::Div> {
    ui::round_button(id, glyph, primary, enabled, size, &look.theme)
}

/// A small label in a tone's color ("exit 0", "running").
pub fn pill(text: impl Into<SharedString>, color: Hsla, look: &Look) -> gpui::Div {
    ui::pill(text, color, &look.tokens)
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

#[cfg(test)]
mod tests {
    use super::*;

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
