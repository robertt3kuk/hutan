//! Small pieces the chat view shares: buttons, chips and number formatting.

use gpui::{Hsla, SharedString};

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
    fn tail_lines_keeps_the_end() {
        let text = "1\n2\n3\n4\n5\n";
        assert_eq!(tail_lines(text, 2), ("4\n5\n", 3));
        assert_eq!(tail_lines(text, 9), (text, 0));
        assert_eq!(tail_lines("", 3), ("", 0));
        assert_eq!(tail_lines("a\nb", 1), ("b", 1));
    }
}
