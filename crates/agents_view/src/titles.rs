//! What a session's title says beyond its words: the emoji and pictographs an agent puts in front
//! of the title of its terminal (`➖ SUB: …`, `👷 WORKER: …`), which the interface font draws as
//! a flat grey shape. GPUI-free.

/// Whether `c` is part of an emoji or a pictograph: what a terminal title of an agent may begin
/// with (`➖ SUB: …`, `👷 WORKER: …`) and the interface font draws as a flat grey shape.
fn is_pictograph(c: char) -> bool {
    matches!(c,
        '\u{200d}' | '\u{20e3}' | '\u{fe0e}' | '\u{fe0f}'
        | '\u{2190}'..='\u{21ff}'
        | '\u{2300}'..='\u{23ff}'
        | '\u{25a0}'..='\u{27bf}'
        | '\u{2900}'..='\u{297f}'
        | '\u{2b00}'..='\u{2bff}'
        | '\u{3030}' | '\u{303d}' | '\u{3297}' | '\u{3299}'
        | '\u{1f000}'..='\u{1faff}'
        | '\u{e0020}'..='\u{e007f}')
}

/// `title` without the emoji and pictographs it begins with (and the spaces after them): `➖ SUB:
/// Add sub` is `SUB: Add sub`. A title that is nothing else keeps them.
pub fn strip_pictographs(title: &str) -> &str {
    let rest = title
        .trim_start_matches(|c: char| is_pictograph(c) || c.is_whitespace())
        .trim_start();
    if rest.is_empty() { title } else { rest }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_pictographs_and_the_spaces_after_them_go() {
        assert_eq!(strip_pictographs("➖ SUB: Add sub"), "SUB: Add sub");
        assert_eq!(strip_pictographs("⚡️ 🚀 Ship it"), "Ship it");
    }

    #[test]
    fn a_title_without_a_prefix_or_made_only_of_one_is_kept() {
        assert_eq!(strip_pictographs("Fix the build"), "Fix the build");
        assert_eq!(strip_pictographs("🚀"), "🚀");
        assert_eq!(strip_pictographs(""), "");
    }
}
