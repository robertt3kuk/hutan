//! GPUI-free parts of the History panel and the ⌘J `?` search: the filters and the request they
//! build, the `«…»` snippet markers, labels, and what a hit can do.

use std::ops::Range;
use std::path::Path;
use std::time::Duration;

use hutan_core::TerminalId;
use hutan_protocol::history::{HistoryHit, HistoryRole, HistorySearch, HistorySource};

use crate::settings::{HistoryRange, HistoryScope, HistorySettings};

/// Quiet time after the last keystroke before a search runs.
pub const DEBOUNCE: Duration = Duration::from_millis(150);
/// Sessions the ⌘J `?` search asks for.
pub const PICKER_LIMIT: u32 = 8;

/// Every source, in the order the chips show them.
pub const SOURCES: [HistorySource; 4] = [
    HistorySource::Hutan,
    HistorySource::Claude,
    HistorySource::Codex,
    HistorySource::Omp,
];

/// Every role, in the order the chips show them.
pub const ROLES: [HistoryRole; 5] = [
    HistoryRole::User,
    HistoryRole::Assistant,
    HistoryRole::Tool,
    HistoryRole::Command,
    HistoryRole::Output,
];

/// The panel's filter chips. An empty `sources` or `roles` means all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filters {
    pub sources: Vec<HistorySource>,
    pub roles: Vec<HistoryRole>,
    pub range: HistoryRange,
    pub scope: HistoryScope,
}

impl Filters {
    /// The defaults the `history` settings namespace asks for; unknown names are ignored.
    pub fn from_settings(settings: &HistorySettings) -> Self {
        Self {
            sources: SOURCES
                .into_iter()
                .filter(|s| settings.sources.iter().any(|name| name == s.as_str()))
                .collect(),
            roles: ROLES
                .into_iter()
                .filter(|r| settings.roles.iter().any(|name| name == r.as_str()))
                .collect(),
            range: settings.range,
            scope: settings.project,
        }
    }

    /// Adds `source` to the chosen ones, or removes it when it is chosen already.
    pub fn toggle_source(&mut self, source: HistorySource) {
        toggle(&mut self.sources, source);
    }

    /// Adds `role` to the chosen ones, or removes it when it is chosen already.
    pub fn toggle_role(&mut self, role: HistoryRole) {
        toggle(&mut self.roles, role);
    }

    /// True when any chip narrows the search.
    pub fn narrows(&self) -> bool {
        !self.sources.is_empty()
            || !self.roles.is_empty()
            || self.range != HistoryRange::All
            || self.scope != HistoryScope::All
    }

    /// The `history.search` for `query`; `project` is the folder of the active workspace, used
    /// when the scope says so.
    pub fn request(
        &self,
        query: &str,
        limit: u32,
        now_ms: u64,
        project: Option<&Path>,
    ) -> HistorySearch {
        HistorySearch {
            query: query.trim().to_owned(),
            limit,
            sources: self.sources.clone(),
            roles: self.roles.clone(),
            project: match self.scope {
                HistoryScope::Workspace => project.map(|p| p.display().to_string()),
                HistoryScope::All => None,
            },
            since_ms: self
                .range
                .window_ms()
                .map(|window| now_ms.saturating_sub(window)),
            until_ms: None,
        }
    }
}

fn toggle<T: PartialEq + Copy>(chosen: &mut Vec<T>, item: T) {
    match chosen.iter().position(|c| *c == item) {
        Some(at) => {
            chosen.remove(at);
        }
        None => chosen.push(item),
    }
}

/// A snippet with the index's `«…»` markers taken out: the text to show and the byte ranges of
/// the matched words. Never contains a marker, whatever the input looks like.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snippet {
    pub text: String,
    pub marks: Vec<Range<usize>>,
}

/// Parses `raw`: `«` opens and `»` closes a match, an unmatched closer is dropped, an unclosed
/// match ends with the text, and every run of whitespace (snippets span lines) becomes one space.
/// Marks never start or end on a space.
pub fn parse_snippet(raw: &str) -> Snippet {
    let mut text = String::with_capacity(raw.len());
    let mut marks = Vec::new();
    let mut open: Option<usize> = None;
    for c in raw.chars() {
        match c {
            '«' => {
                open.get_or_insert(text.len());
            }
            '»' => {
                if let Some(mark) = open.take().and_then(|start| mark_of(&text, start)) {
                    marks.push(mark);
                }
            }
            c if c.is_whitespace() => {
                if !text.is_empty() && !text.ends_with(' ') {
                    text.push(' ');
                }
            }
            c => text.push(c),
        }
    }
    if let Some(mark) = open.and_then(|start| mark_of(&text, start)) {
        marks.push(mark);
    }
    if text.ends_with(' ') {
        text.pop();
    }
    Snippet { text, marks }
}

/// The part of `text` from `start` on, without the spaces at either end; `None` when nothing
/// is left.
fn mark_of(text: &str, start: usize) -> Option<Range<usize>> {
    let marked = &text[start..];
    let first = start + (marked.len() - marked.trim_start().len());
    let last = start + marked.trim_end().len();
    (first < last).then_some(first..last)
}

/// What the chip and the row call a source.
pub fn source_label(source: HistorySource) -> &'static str {
    match source {
        HistorySource::Hutan => "Hutan",
        HistorySource::Claude => "Claude",
        HistorySource::Codex => "Codex",
        HistorySource::Omp => "OMP",
    }
}

/// A single glyph per source (no icon assets are bundled).
pub fn source_icon(source: HistorySource) -> &'static str {
    match source {
        HistorySource::Hutan => "❯",
        HistorySource::Claude => "✳",
        HistorySource::Codex => "◈",
        HistorySource::Omp => "π",
    }
}

/// `~/proj/app` for paths under `home`, else the path as written.
pub fn short_path(path: &str, home: Option<&Path>) -> String {
    let path = Path::new(path);
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// `argv` as one line a shell reads back as the same words.
pub fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|word| shell_word(word))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// What resuming a hit runs and where (the payload of `ResumeHistoryHit`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryResume {
    pub argv: Vec<String>,
    /// The session's recorded working directory, which may be gone by now.
    pub cwd: Option<String>,
    pub title: Option<String>,
}

impl HistoryResume {
    /// The command of a hit that can be resumed.
    pub fn of(hit: &HistoryHit) -> Option<Self> {
        let argv = hit.resume_argv.clone().filter(|argv| !argv.is_empty())?;
        Some(Self {
            argv,
            cwd: hit.cwd.clone(),
            title: hit.title.clone().filter(|t| !t.trim().is_empty()),
        })
    }
}

/// `812 B`, `3.4 KB`, `12.0 MB`, `1.2 GB`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// What opening a Hutan hit shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HutanTarget {
    /// The Hutan session id.
    pub session: String,
    /// The block the hit is about, when it is a block's command or output (and the index knows
    /// which).
    pub block: Option<BlockTarget>,
}

/// A terminal block, as the index recorded it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockTarget {
    /// The terminal the block was in. Terminal ids belong to one daemon run: a session whose
    /// terminal is another one has been restarted since, and the block is gone with it.
    pub terminal_id: TerminalId,
    pub block_id: u64,
}

/// The Hutan session (and block) a hit of the `hutan` source points at.
pub fn hutan_target(hit: &HistoryHit) -> Option<HutanTarget> {
    if hit.source != HistorySource::Hutan {
        return None;
    }
    Some(HutanTarget {
        session: hit.session_key.clone(),
        block: hit
            .terminal_id
            .zip(hit.block_id)
            .map(|(terminal_id, block_id)| BlockTarget {
                terminal_id,
                block_id,
            }),
    })
}

/// The transcript file a hit of another tool came from, for "Reveal File": absolute, as the
/// index scanned it.
pub fn transcript_path(hit: &HistoryHit) -> Option<&Path> {
    let path = Path::new(hit.path.as_deref()?);
    (hit.source != HistorySource::Hutan && path.is_absolute()).then_some(path)
}

/// The hit's title, or the first words of what matched when the session has none.
pub fn hit_title(hit: &HistoryHit) -> String {
    match hit.title.as_deref().map(str::trim) {
        Some(title) if !title.is_empty() => title.to_owned(),
        _ => {
            let text = parse_snippet(&hit.snippet).text;
            if text.is_empty() {
                format!("{} session", source_label(hit.source))
            } else {
                text.chars().take(60).collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(source: HistorySource, role: HistoryRole) -> HistoryHit {
        HistoryHit {
            source,
            session_key: "k".into(),
            title: None,
            cwd: None,
            role,
            ts_ms: Some(42),
            snippet: "a «b» c".into(),
            score: 1.0,
            matches: 1,
            resume_argv: None,
            session_id: None,
            path: None,
            terminal_id: None,
            block_id: None,
        }
    }

    #[test]
    fn snippets_lose_their_markers_and_keep_the_matches() {
        let s = parse_snippet("run «cargo» «test -p»\n   daemon");
        assert_eq!(s.text, "run cargo test -p daemon");
        let marked: Vec<&str> = s.marks.iter().map(|m| &s.text[m.clone()]).collect();
        assert_eq!(marked, ["cargo", "test -p"]);
    }

    #[test]
    fn odd_markers_never_reach_the_text() {
        for raw in [
            "»x«",
            "«un closed",
            "«« nested »» x",
            "«»",
            " «a»  ",
            "«»»«",
        ] {
            let s = parse_snippet(raw);
            assert!(
                !s.text.contains(['«', '»']),
                "{raw:?} kept a marker: {:?}",
                s.text
            );
            for mark in &s.marks {
                assert!(
                    s.text.is_char_boundary(mark.start) && s.text.is_char_boundary(mark.end),
                    "{raw:?}"
                );
                assert!(mark.end <= s.text.len() && mark.start < mark.end, "{raw:?}");
            }
        }
        let open = parse_snippet("«un closed");
        assert_eq!(open.marks.len(), 1);
        assert_eq!((open.marks[0].start, open.marks[0].end), (0, 9));
        assert_eq!(parse_snippet("»x«").text, "x");
        assert_eq!(parse_snippet(" «a»  ").text, "a");
        assert_eq!(parse_snippet("…«ü»…").marks.len(), 1);
    }

    #[test]
    fn filters_follow_the_settings_and_build_the_request() {
        let settings = HistorySettings {
            sources: vec!["omp".into(), "bogus".into(), "claude".into()],
            roles: vec!["user".into()],
            range: HistoryRange::Week,
            project: HistoryScope::Workspace,
            show_snippets: true,
        };
        let filters = Filters::from_settings(&settings);
        assert_eq!(filters.sources, [HistorySource::Claude, HistorySource::Omp]);
        assert!(filters.narrows());
        let now = 10_000_000_000;
        let request = filters.request(" quokka ", 8, now, Some(Path::new("/work/app")));
        assert_eq!(request.query, "quokka");
        assert_eq!(request.limit, 8);
        assert_eq!(request.roles, [HistoryRole::User]);
        assert_eq!(request.project.as_deref(), Some("/work/app"));
        assert_eq!(request.since_ms, Some(now - 7 * 24 * 3_600_000));
        let all = Filters::from_settings(&HistorySettings::default());
        assert!(!all.narrows());
        let request = all.request("x", 5, now, Some(Path::new("/work/app")));
        assert!(request.project.is_none() && request.since_ms.is_none());
        assert!(request.sources.is_empty() && request.roles.is_empty());
    }

    #[test]
    fn chips_toggle() {
        let mut filters = Filters::from_settings(&HistorySettings::default());
        filters.toggle_source(HistorySource::Codex);
        filters.toggle_source(HistorySource::Omp);
        filters.toggle_source(HistorySource::Codex);
        assert_eq!(filters.sources, [HistorySource::Omp]);
        filters.toggle_role(HistoryRole::Tool);
        assert_eq!(filters.roles, [HistoryRole::Tool]);
    }

    #[test]
    fn resume_commands_are_quoted_for_a_shell() {
        let argv = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            shell_join(&argv(&["claude", "--resume", "5f3e-1c"])),
            "claude --resume 5f3e-1c"
        );
        assert_eq!(
            shell_join(&argv(&["omp", "--resume=0f4c7d52"])),
            "omp --resume=0f4c7d52"
        );
        assert_eq!(
            shell_join(&argv(&["a b", "", "it's"])),
            r"'a b' '' 'it'\''s'"
        );
    }

    #[test]
    fn hutan_blocks_are_found_by_their_id_not_by_time() {
        let mut output = hit(HistorySource::Hutan, HistoryRole::Output);
        output.terminal_id = Some(TerminalId(7));
        output.block_id = Some(12);
        // Whatever the hit's time says (here 42) names no block.
        let target = hutan_target(&output).unwrap();
        assert_eq!(
            target.block,
            Some(BlockTarget {
                terminal_id: TerminalId(7),
                block_id: 12
            })
        );
        // A row the index wrote before it kept block ids, and a report, name none.
        let old = hutan_target(&hit(HistorySource::Hutan, HistoryRole::Output)).unwrap();
        assert_eq!(old.block, None);
        let report = hutan_target(&hit(HistorySource::Hutan, HistoryRole::Tool)).unwrap();
        assert_eq!(report.block, None);
        assert!(hutan_target(&hit(HistorySource::Claude, HistoryRole::User)).is_none());
    }

    #[test]
    fn only_hits_of_other_tools_with_an_absolute_file_can_be_revealed() {
        let mut claude = hit(HistorySource::Claude, HistoryRole::User);
        assert_eq!(transcript_path(&claude), None);
        claude.path = Some("/Users/me/.claude/projects/-w/s.jsonl".into());
        assert_eq!(
            transcript_path(&claude),
            Some(Path::new("/Users/me/.claude/projects/-w/s.jsonl"))
        );
        claude.path = Some("relative/s.jsonl".into());
        assert_eq!(transcript_path(&claude), None);
        let mut hutan = hit(HistorySource::Hutan, HistoryRole::Command);
        hutan.path = Some("/somewhere".into());
        assert_eq!(transcript_path(&hutan), None);
    }

    #[test]
    fn untitled_hits_are_named_after_what_matched() {
        assert_eq!(
            hit_title(&hit(HistorySource::Codex, HistoryRole::User)),
            "a b c"
        );
        let mut titled = hit(HistorySource::Codex, HistoryRole::User);
        titled.title = Some(" Scheduler stall ".into());
        assert_eq!(hit_title(&titled), "Scheduler stall");
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(12 * 1024 * 1024), "12.0 MB");
        assert!(format_bytes(u64::MAX).ends_with("TB"));
    }

    #[test]
    fn paths_shorten_under_home() {
        let home = Path::new("/Users/me");
        assert_eq!(short_path("/Users/me/proj/x", Some(home)), "~/proj/x");
        assert_eq!(short_path("/Users/me", Some(home)), "~");
        assert_eq!(short_path("/work/app", Some(home)), "/work/app");
    }
}
