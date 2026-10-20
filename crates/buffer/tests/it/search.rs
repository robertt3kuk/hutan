use hutan_buffer::{SearchQuery, Text};
use proptest::prelude::*;

use crate::common::{config, text};

proptest! {
    #![proptest_config(config())]

    /// Literal search over a multi-chunk rope equals `str::match_indices`.
    #[test]
    fn literal_search_equals_std(hay in text(1500), needle in text(3)) {
        let needle = if needle.is_empty() { "a".to_string() } else { needle };
        let t = Text::from(hay.as_str());
        let expected: Vec<std::ops::Range<usize>> = hay.match_indices(needle.as_str()).map(|(i, m)| i..i + m.len()).collect();
        let found = t.search_all(&SearchQuery::literal(needle.as_str()), 1_000_000).unwrap();
        prop_assert_eq!(found.matches, expected);
        prop_assert!(!found.truncated);
    }

    /// A limit cuts the result list and says so; a sub-range only returns matches inside it.
    #[test]
    fn limits_and_ranges(hay in text(400), needle in text(2), limit in 0usize..6, a in any::<usize>(), b in any::<usize>()) {
        let needle = if needle.is_empty() { "a".to_string() } else { needle };
        let t = Text::from(hay.as_str());
        let all = hay.match_indices(needle.as_str()).count();
        let found = t.search_all(&SearchQuery::literal(needle.as_str()), limit).unwrap();
        prop_assert_eq!(found.matches.len(), all.min(limit));
        prop_assert_eq!(found.truncated, all > limit);

        let bounds = crate::common::boundaries(&hay);
        let (mut lo, mut hi) = (bounds[a % bounds.len()], bounds[b % bounds.len()]);
        if lo > hi { std::mem::swap(&mut lo, &mut hi); }
        let inside = t.search(&SearchQuery::literal(needle.as_str()), lo..hi, 1000).unwrap();
        let expected: Vec<_> = hay[lo..hi].match_indices(needle.as_str()).map(|(i, m)| lo + i..lo + i + m.len()).collect();
        prop_assert_eq!(inside.matches, expected);
    }
}

#[test]
fn regex_search_runs_over_a_large_rope() {
    let mut s = String::new();
    for i in 0..20_000 {
        s.push_str(&format!("fn item_{i}() {{ /* é */ }}\n"));
    }
    let t = Text::from(s.as_str());
    let found = t
        .search_all(&SearchQuery::regex(r"item_\d*7\b"), 100_000)
        .unwrap();
    assert_eq!(found.matches.len(), 2000);
    assert!(!found.truncated);
    for m in found.matches.iter().take(20) {
        assert!(t.slice(m.clone()).unwrap().starts_with("item_"));
    }
}
