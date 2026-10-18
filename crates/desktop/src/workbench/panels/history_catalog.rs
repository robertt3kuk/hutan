//! GPUI-free parts of the Agent Session History list: one view-model for a catalog row
//! (`history.sessions`) and for a full-text hit (`history.search`), how rows are grouped, the
//! lines the scan strip and the list bar print, and which actions a row offers.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use hutan_core::{SessionId, WorkspaceId};
use hutan_protocol::history::{
    HistoryHit, HistoryRole, HistoryScanInfo, HistorySessionRow, HistorySessionSort, HistorySource,
    HistoryTurn,
};

use super::history_model::{
    BlockTarget, HistoryResume, HutanTarget, Snippet, parse_snippet, short_path, source_label,
};

/// Quiet time after the last keystroke before the full-text search runs.
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);
/// How often a visible panel asks for the catalog again.
pub const REFRESH_EVERY: Duration = Duration::from_secs(30);
/// How often the catalog is asked again while the daemon is still scanning the transcripts.
pub const SCAN_POLL: Duration = Duration::from_millis(700);
/// Panel widths below this show "History" instead of the full title.
pub const NARROW_WIDTH: f32 = 300.0;
/// Most hits one search asks for.
pub const SEARCH_LIMIT: u32 = 50;

/// How the catalog is cut into groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupBy {
    /// The Hutan project (or workspace) whose folder holds the session's directory.
    #[default]
    Project,
    /// The session's working directory (last two path segments).
    Folder,
    /// The agent that ran it.
    Agent,
}

impl GroupBy {
    pub const ALL: [Self; 3] = [Self::Project, Self::Folder, Self::Agent];

    pub fn label(self) -> &'static str {
        match self {
            Self::Project => "Project",
            Self::Folder => "Folder",
            Self::Agent => "Agent",
        }
    }
}

/// What the sort dropdown calls an order.
pub fn sort_label(sort: HistorySessionSort) -> &'static str {
    match sort {
        HistorySessionSort::Updated => "Last updated",
        HistorySessionSort::Created => "Created",
    }
}

/// What the panel knows about one session, whichever request it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionItem {
    pub source: HistorySource,
    pub key: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub model: Option<String>,
    /// User and assistant messages; unknown for a full-text hit.
    pub messages: Option<u32>,
    pub created_ms: Option<u64>,
    pub updated_ms: Option<u64>,
    pub first_prompt: Option<String>,
    pub latest: Vec<HistoryTurn>,
    pub workspace_id: Option<WorkspaceId>,
    pub resume_argv: Option<Vec<String>>,
    pub path: Option<String>,
    pub session_id: Option<SessionId>,
    /// Set for a full-text hit: the best matching row.
    pub evidence: Option<Evidence>,
    /// A Hutan hit on a terminal block.
    pub block: Option<BlockTarget>,
}

/// The matching row a full-text hit shows instead of the latest turn.
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    pub role: HistoryRole,
    pub snippet: Snippet,
    /// Rows of the session that match.
    pub matches: u32,
}

/// What identifies a session in the panel.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ItemKey {
    pub source: HistorySource,
    pub key: String,
}

impl SessionItem {
    pub fn from_row(row: &HistorySessionRow) -> Self {
        Self {
            source: row.source,
            key: row.session_key.clone(),
            title: row.title.clone(),
            cwd: row.cwd.clone(),
            branch: row.branch.clone(),
            model: row.model.clone(),
            messages: Some(row.messages),
            created_ms: row.created_ms,
            updated_ms: row.updated_ms,
            first_prompt: row.first_prompt.clone(),
            latest: row.latest.clone(),
            workspace_id: row.workspace_id,
            resume_argv: row.resume_argv.clone(),
            path: row.path.clone(),
            session_id: row.session_id,
            evidence: None,
            block: None,
        }
    }

    pub fn from_hit(hit: &HistoryHit) -> Self {
        Self {
            source: hit.source,
            key: hit.session_key.clone(),
            title: hit.title.clone(),
            cwd: hit.cwd.clone(),
            branch: None,
            model: None,
            messages: None,
            created_ms: None,
            updated_ms: hit.ts_ms,
            first_prompt: None,
            latest: Vec::new(),
            workspace_id: None,
            resume_argv: hit.resume_argv.clone(),
            path: hit.path.clone(),
            session_id: hit.session_id,
            evidence: Some(Evidence {
                role: hit.role,
                snippet: parse_snippet(&hit.snippet),
                matches: hit.matches,
            }),
            block: hit
                .terminal_id
                .zip(hit.block_id)
                .map(|(terminal_id, block_id)| BlockTarget {
                    terminal_id,
                    block_id,
                }),
        }
    }

    pub fn item_key(&self) -> ItemKey {
        ItemKey {
            source: self.source,
            key: self.key.clone(),
        }
    }

    /// The title, else the first line of the first prompt, else what matched, else the agent's
    /// name.
    pub fn display_title(&self) -> String {
        let named = self
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty());
        if let Some(title) = named {
            return title.to_owned();
        }
        let prompt = self
            .first_prompt
            .as_deref()
            .and_then(|p| p.lines().map(str::trim).find(|l| !l.is_empty()));
        if let Some(line) = prompt {
            return line.chars().take(80).collect();
        }
        if let Some(evidence) = &self.evidence
            && !evidence.snippet.text.is_empty()
        {
            return evidence.snippet.text.chars().take(60).collect();
        }
        format!("{} session", source_label(self.source))
    }

    /// The latest turn as `("Agent", "text")`; `None` when the catalog holds none.
    pub fn latest_turn(&self) -> Option<(&'static str, &str)> {
        self.latest
            .last()
            .map(|turn| (turn_label(turn.role), turn.text.as_str()))
    }

    /// Time of the last message, else of the first.
    pub fn age_ms(&self) -> Option<u64> {
        self.updated_ms.or(self.created_ms)
    }

    /// `OMP · 8 msgs · 3m ago · gpt-5.6-luna` (a hit: `Claude · 3 hits · 1h ago · ~/proj/app`).
    pub fn meta(&self, home: Option<&Path>, now_ms: u64) -> String {
        let mut parts = vec![source_label(self.source).to_owned()];
        match &self.evidence {
            Some(evidence) => parts.push(match evidence.matches {
                1 => "1 hit".to_owned(),
                n => format!("{n} hits"),
            }),
            None => {
                if let Some(count) = self.messages.filter(|m| *m > 0) {
                    parts.push(match count {
                        1 => "1 msg".to_owned(),
                        n => format!("{n} msgs"),
                    });
                }
            }
        }
        if let Some(ms) = self.age_ms() {
            parts.push(age_text(now_ms, ms));
        }
        match &self.evidence {
            Some(_) => {
                if let Some(cwd) = self.cwd.as_deref().filter(|c| !c.is_empty()) {
                    parts.push(short_path(cwd, home));
                }
            }
            None => {
                if let Some(model) = self.model.as_deref().filter(|m| !m.is_empty()) {
                    parts.push(model.to_owned());
                }
            }
        }
        parts.join(" · ")
    }

    /// What Resume runs, for a session that can be resumed.
    pub fn resume(&self) -> Option<HistoryResume> {
        let argv = self.resume_argv.clone().filter(|argv| !argv.is_empty())?;
        Some(HistoryResume {
            argv,
            cwd: self.cwd.clone(),
            title: self.title.clone().filter(|t| !t.trim().is_empty()),
        })
    }

    /// The Hutan session (and block) a Hutan row opens.
    pub fn hutan_target(&self) -> Option<HutanTarget> {
        if self.source != HistorySource::Hutan {
            return None;
        }
        Some(HutanTarget {
            session: self
                .session_id
                .map_or_else(|| self.key.clone(), |id| id.to_string()),
            block: self.block,
        })
    }

    /// The transcript file of another tool's session: absolute, as the index scanned it.
    pub fn transcript(&self) -> Option<&Path> {
        let path = Path::new(self.path.as_deref()?);
        (self.source != HistorySource::Hutan && path.is_absolute()).then_some(path)
    }
}

/// `You`, `Agent`, `Tool` (a Hutan terminal: `Command`, `Output`).
pub fn turn_label(role: HistoryRole) -> &'static str {
    match role {
        HistoryRole::User => "You",
        HistoryRole::Assistant => "Agent",
        HistoryRole::Tool => "Tool",
        HistoryRole::Command => "Command",
        HistoryRole::Output => "Output",
    }
}

/// `just now`, `3m ago`, `2h ago`, `4d ago`.
pub fn age_text(now_ms: u64, then_ms: u64) -> String {
    let secs = now_ms.saturating_sub(then_ms) / 1000;
    match secs {
        0..=59 => "just now".to_owned(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The scan strip: `✓ Scanned 986 files · Just now`, or `Scanning 12 / 986…` while it runs.
pub fn scan_line(scan: &HistoryScanInfo, now_ms: u64) -> String {
    if scan.scanning {
        return format!("Scanning {} / {}…", scan.done, scan.files);
    }
    match scan.finished_ms {
        Some(finished) => format!(
            "✓ Scanned {} files · {}",
            scan.files,
            capitalised(&hutan_client::time::relative_text(now_ms, finished))
        ),
        None => format!("Scanned {} files", scan.files),
    }
}

/// The list bar of the catalog: `2 of 250 sessions` (shown of loaded). More matches than rows
/// asked for read `250 of 1203 shown · 4000 sessions`.
pub fn list_bar(shown: usize, matched: u64, loaded: u64) -> String {
    let noun = if loaded == 1 { "session" } else { "sessions" };
    if matched <= shown as u64 {
        format!("{matched} of {loaded} {noun}")
    } else {
        format!("{shown} of {matched} shown · {loaded} {noun}")
    }
}

/// The list bar of a full-text search: `3 results` (`50+ results` when the limit cut it).
pub fn results_bar(count: usize, limit: u32) -> String {
    let plus = if count >= limit as usize { "+" } else { "" };
    match count {
        1 => "1 result".to_owned(),
        n => format!("{n}{plus} results"),
    }
}

/// True when `query` names a `repo:` or `path:` qualifier, which only the catalog filter
/// understands (the full-text index has no such field).
pub fn has_qualifier(query: &str) -> bool {
    query.split_whitespace().any(|word| {
        let word = word.trim_start_matches('"').to_ascii_lowercase();
        word.starts_with("repo:") || word.starts_with("path:")
    })
}

// ---- grouping ---------------------------------------------------------------------------------

/// A group of the grouped list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub key: String,
    pub label: String,
    /// Indexes into the item list, in the order the daemon sorted them.
    pub items: Vec<usize>,
}

/// The last two segments of `cwd` (`proj/hutan`); `~` for the home folder itself.
pub fn folder_label(cwd: &str, home: Option<&Path>) -> String {
    let path = Path::new(cwd);
    if home.is_some_and(|home| path == home) {
        return "~".to_owned();
    }
    let names: Vec<_> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    match names.len() {
        0 => "/".to_owned(),
        1 => names[0].clone(),
        n => format!("{}/{}", names[n - 2], names[n - 1]),
    }
}

/// Cuts `items` into groups, in order of first appearance (the daemon's sort decides the group
/// order too). `project_name` names a workspace or project the daemon attributed a row to.
pub fn group_items(
    items: &[SessionItem],
    by: GroupBy,
    home: Option<&Path>,
    project_name: impl Fn(WorkspaceId) -> Option<String>,
) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let (key, label) = match by {
            GroupBy::Agent => (
                item.source.as_str().to_owned(),
                source_label(item.source).to_owned(),
            ),
            GroupBy::Folder => folder_group(item, home),
            GroupBy::Project => match item
                .workspace_id
                .and_then(|id| project_name(id).map(|name| (id, name)))
            {
                Some((id, name)) => (format!("project:{id}"), name),
                None => folder_group(item, home),
            },
        };
        match groups.iter_mut().find(|g| g.key == key) {
            Some(group) => group.items.push(index),
            None => groups.push(Group {
                key,
                label,
                items: vec![index],
            }),
        }
    }
    groups
}

fn folder_group(item: &SessionItem, home: Option<&Path>) -> (String, String) {
    match item.cwd.as_deref().filter(|c| !c.is_empty()) {
        Some(cwd) => (
            format!("dir:{}", cwd.trim_end_matches('/')),
            folder_label(cwd, home),
        ),
        None => ("unknown".to_owned(), "Unknown location".to_owned()),
    }
}

/// A line of the flat list the virtualized list draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Header {
        key: String,
        label: String,
        count: usize,
        collapsed: bool,
    },
    /// Index into the item list.
    Row(usize),
    /// "Show more": rows the daemon matched beyond the ones asked for.
    More { remaining: u64 },
}

/// The list lines of `groups`: a header and its rows each, folded groups without rows.
pub fn entries(groups: &[Group], collapsed: &HashSet<String>, remaining: u64) -> Vec<Entry> {
    let mut out = Vec::new();
    for group in groups {
        let folded = collapsed.contains(&group.key);
        out.push(Entry::Header {
            key: group.key.clone(),
            label: group.label.clone(),
            count: group.items.len(),
            collapsed: folded,
        });
        if !folded {
            out.extend(group.items.iter().copied().map(Entry::Row));
        }
    }
    if remaining > 0 {
        out.push(Entry::More { remaining });
    }
    out
}

/// A search result list is flat: one row per hit, best first.
pub fn flat_entries(count: usize) -> Vec<Entry> {
    (0..count).map(Entry::Row).collect()
}

// ---- row actions ------------------------------------------------------------------------------

/// What the row's buttons and its `⋯` / right-click menu do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowAction {
    /// Run the resume command in a new terminal tab in the session's project.
    Resume,
    /// Show a Hutan session.
    OpenSession,
    CopyResumeCommand,
    OpenLog,
    RevealLog,
    OpenDirectory,
    CopySessionId,
    CopyLogPath,
}

/// One entry of the row menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionEntry {
    pub action: RowAction,
    pub label: &'static str,
    pub enabled: bool,
}

/// Why `item` cannot be resumed or opened, or `None` when it can. `session_exists` says
/// whether the daemon still has the Hutan session of a Hutan row.
pub fn blocker(item: &SessionItem, session_exists: bool) -> Option<&'static str> {
    if item.source == HistorySource::Hutan {
        return (!session_exists).then_some("That terminal session no longer exists.");
    }
    item.resume()
        .is_none()
        .then_some("Cannot resume: the index holds no resume command for this session.")
}

/// The entries of the row menu, in order. Resume (or Open Session for a Hutan row) comes first
/// and is the primary action.
pub fn actions(item: &SessionItem, session_exists: bool) -> Vec<ActionEntry> {
    let hutan = item.source == HistorySource::Hutan;
    let entry = |action, label, enabled| ActionEntry {
        action,
        label,
        enabled,
    };
    let mut out = Vec::new();
    if hutan {
        out.push(entry(
            RowAction::OpenSession,
            "Open Session",
            session_exists && item.hutan_target().is_some(),
        ));
    } else {
        out.push(entry(RowAction::Resume, "Resume", item.resume().is_some()));
        out.push(entry(
            RowAction::CopyResumeCommand,
            "Copy Resume Command",
            item.resume().is_some(),
        ));
        out.push(entry(
            RowAction::OpenLog,
            "Open Log",
            item.transcript().is_some(),
        ));
        out.push(entry(
            RowAction::RevealLog,
            "Reveal Log",
            item.transcript().is_some(),
        ));
    }
    out.push(entry(
        RowAction::OpenDirectory,
        "Open Working Directory",
        item.cwd.as_deref().is_some_and(|c| !c.is_empty()),
    ));
    out.push(entry(RowAction::CopySessionId, "Copy Session ID", true));
    if !hutan {
        out.push(entry(
            RowAction::CopyLogPath,
            "Copy Log Path",
            item.transcript().is_some(),
        ));
    }
    out
}

/// The text `Copy Session ID` puts on the clipboard.
pub fn session_id_text(item: &SessionItem) -> String {
    match (item.source, item.session_id) {
        (HistorySource::Hutan, Some(id)) => id.to_string(),
        _ => item.key.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(source: HistorySource, key: &str) -> HistorySessionRow {
        HistorySessionRow {
            source,
            session_key: key.into(),
            title: None,
            cwd: None,
            branch: None,
            model: None,
            messages: 0,
            created_ms: None,
            updated_ms: None,
            first_prompt: None,
            latest: Vec::new(),
            workspace_id: None,
            resume_argv: None,
            path: None,
            session_id: None,
        }
    }

    fn item(source: HistorySource, key: &str, cwd: Option<&str>) -> SessionItem {
        let mut r = row(source, key);
        r.cwd = cwd.map(str::to_owned);
        SessionItem::from_row(&r)
    }

    #[test]
    fn rows_are_named_after_their_title_then_the_first_prompt() {
        let mut r = row(HistorySource::Omp, "k");
        assert_eq!(SessionItem::from_row(&r).display_title(), "OMP session");
        r.first_prompt = Some("\n  fix the scheduler\nthen more".into());
        assert_eq!(
            SessionItem::from_row(&r).display_title(),
            "fix the scheduler"
        );
        r.title = Some(" Scheduler stall ".into());
        assert_eq!(SessionItem::from_row(&r).display_title(), "Scheduler stall");
    }

    #[test]
    fn the_latest_turn_names_its_role() {
        let mut r = row(HistorySource::Claude, "k");
        assert!(SessionItem::from_row(&r).latest_turn().is_none());
        r.latest = vec![
            HistoryTurn {
                role: HistoryRole::User,
                text: "why".into(),
                ts_ms: None,
            },
            HistoryTurn {
                role: HistoryRole::Assistant,
                text: "because".into(),
                ts_ms: None,
            },
        ];
        assert_eq!(
            SessionItem::from_row(&r).latest_turn(),
            Some(("Agent", "because"))
        );
        r.latest.push(HistoryTurn {
            role: HistoryRole::Tool,
            text: "ran".into(),
            ts_ms: None,
        });
        assert_eq!(
            SessionItem::from_row(&r).latest_turn(),
            Some(("Tool", "ran"))
        );
    }

    #[test]
    fn meta_lists_agent_messages_age_and_model() {
        let mut r = row(HistorySource::Omp, "k");
        r.messages = 8;
        r.updated_ms = Some(1_000_000);
        r.model = Some("gpt-5.6-luna".into());
        let item = SessionItem::from_row(&r);
        assert_eq!(
            item.meta(None, 1_000_000 + 3 * 60_000),
            "OMP · 8 msgs · 3m ago · gpt-5.6-luna"
        );
        r.messages = 1;
        r.model = None;
        r.updated_ms = None;
        r.created_ms = Some(0);
        assert_eq!(
            SessionItem::from_row(&r).meta(None, 2 * 3_600_000),
            "OMP · 1 msg · 2h ago"
        );
        r.messages = 0;
        assert_eq!(
            SessionItem::from_row(&r).meta(None, 10_000),
            "OMP · just now"
        );
    }

    #[test]
    fn a_hit_shows_its_evidence_and_hit_count() {
        let hit = HistoryHit {
            source: HistorySource::Claude,
            session_key: "k".into(),
            title: None,
            cwd: Some("/Users/me/proj/app".into()),
            role: HistoryRole::User,
            ts_ms: Some(0),
            snippet: "the «quokka» stalls".into(),
            score: 1.0,
            matches: 3,
            resume_argv: Some(vec!["claude".into(), "--resume".into(), "k".into()]),
            session_id: None,
            path: None,
            terminal_id: None,
            block_id: None,
        };
        let item = SessionItem::from_hit(&hit);
        assert_eq!(item.display_title(), "the quokka stalls");
        assert_eq!(
            item.meta(Some(Path::new("/Users/me")), 60_000),
            "Claude · 3 hits · 1m ago · ~/proj/app"
        );
        assert_eq!(item.evidence.as_ref().unwrap().snippet.marks.len(), 1);
        assert!(item.resume().is_some());
    }

    #[test]
    fn groups_follow_the_chosen_key_in_order_of_appearance() {
        let id = WorkspaceId::new();
        let mut a = item(HistorySource::Claude, "1", Some("/w/app"));
        a.workspace_id = Some(id);
        let b = item(HistorySource::Codex, "2", Some("/w/other/lib"));
        let c = item(HistorySource::Claude, "3", Some("/w/app/"));
        let d = item(HistorySource::Omp, "4", None);
        let items = vec![a, b, c, d];
        let name = |w| (w == id).then(|| "app".to_owned());

        let by_project = group_items(&items, GroupBy::Project, None, name);
        let labels: Vec<_> = by_project
            .iter()
            .map(|g| (g.label.as_str(), g.items.clone()))
            .collect();
        // `c` has no attributed project, so it falls back to its folder.
        assert_eq!(
            labels,
            [
                ("app", vec![0]),
                ("other/lib", vec![1]),
                ("w/app", vec![2]),
                ("Unknown location", vec![3])
            ]
        );

        let by_folder = group_items(&items, GroupBy::Folder, None, name);
        assert_eq!(
            by_folder[0].items,
            [0, 2],
            "a trailing slash is the same folder"
        );
        assert_eq!(by_folder[0].label, "w/app");

        let by_agent = group_items(&items, GroupBy::Agent, None, name);
        let agents: Vec<_> = by_agent
            .iter()
            .map(|g| (g.label.as_str(), g.items.len()))
            .collect();
        assert_eq!(agents, [("Claude", 2), ("Codex", 1), ("OMP", 1)]);
    }

    #[test]
    fn folders_read_as_their_last_two_segments() {
        assert_eq!(folder_label("/Users/me/proj/hutan", None), "proj/hutan");
        assert_eq!(folder_label("/work", None), "work");
        assert_eq!(folder_label("/", None), "/");
        assert_eq!(folder_label("/Users/me", Some(Path::new("/Users/me"))), "~");
    }

    #[test]
    fn folded_groups_lose_their_rows_and_more_is_last() {
        let items = vec![
            item(HistorySource::Claude, "1", Some("/w/a")),
            item(HistorySource::Claude, "2", Some("/w/b")),
            item(HistorySource::Claude, "3", Some("/w/a")),
        ];
        let groups = group_items(&items, GroupBy::Folder, None, |_| None);
        let mut folded = HashSet::new();
        let open = entries(&groups, &folded, 0);
        assert_eq!(open.len(), 2 + 3);
        folded.insert("dir:/w/a".to_owned());
        let some = entries(&groups, &folded, 7);
        assert_eq!(
            some,
            [
                Entry::Header {
                    key: "dir:/w/a".into(),
                    label: "w/a".into(),
                    count: 2,
                    collapsed: true
                },
                Entry::Header {
                    key: "dir:/w/b".into(),
                    label: "w/b".into(),
                    count: 1,
                    collapsed: false
                },
                Entry::Row(1),
                Entry::More { remaining: 7 },
            ]
        );
    }

    #[test]
    fn the_scan_strip_and_the_bars_read_like_orcas() {
        let mut scan = HistoryScanInfo {
            files: 986,
            done: 12,
            scanning: true,
            finished_ms: None,
            per_source: Vec::new(),
        };
        assert_eq!(scan_line(&scan, 0), "Scanning 12 / 986…");
        scan.scanning = false;
        scan.finished_ms = Some(1_000);
        assert_eq!(scan_line(&scan, 5_000), "✓ Scanned 986 files · Just now");
        assert_eq!(
            scan_line(&scan, 1_000 + 5 * 60_000),
            "✓ Scanned 986 files · 5 min ago"
        );
        scan.finished_ms = None;
        assert_eq!(scan_line(&scan, 0), "Scanned 986 files");

        assert_eq!(list_bar(2, 2, 250), "2 of 250 sessions");
        assert_eq!(list_bar(1, 1, 1), "1 of 1 session");
        assert_eq!(
            list_bar(250, 1203, 4000),
            "250 of 1203 shown · 4000 sessions"
        );
        assert_eq!(results_bar(1, 50), "1 result");
        assert_eq!(results_bar(3, 50), "3 results");
        assert_eq!(results_bar(50, 50), "50+ results");
    }

    #[test]
    fn only_repo_and_path_words_need_the_catalog_filter() {
        assert!(has_qualifier("scheduler repo:hutan"));
        assert!(has_qualifier("PATH:src"));
        assert!(has_qualifier("\"repo:my app\""));
        assert!(!has_qualifier("scheduler stall"));
        assert!(
            has_qualifier("the repo: is"),
            "a lone `repo:` word is a qualifier"
        );
    }

    #[test]
    fn rows_offer_the_actions_they_can_do() {
        let mut claude = item(HistorySource::Claude, "k", Some("/w/app"));
        let by = |entries: &[ActionEntry], action| {
            entries
                .iter()
                .find(|e| e.action == action)
                .map(|e| e.enabled)
        };
        let entries = actions(&claude, true);
        assert_eq!(by(&entries, RowAction::Resume), Some(false));
        assert_eq!(by(&entries, RowAction::OpenDirectory), Some(true));
        assert_eq!(by(&entries, RowAction::OpenLog), Some(false));
        assert_eq!(by(&entries, RowAction::OpenSession), None);
        assert!(
            blocker(&claude, true)
                .unwrap()
                .contains("no resume command")
        );

        claude.resume_argv = Some(vec!["claude".into(), "--resume".into(), "k".into()]);
        claude.path = Some("/home/me/.claude/projects/-w/k.jsonl".into());
        let entries = actions(&claude, true);
        for action in [
            RowAction::Resume,
            RowAction::CopyResumeCommand,
            RowAction::OpenLog,
            RowAction::RevealLog,
            RowAction::CopyLogPath,
            RowAction::CopySessionId,
        ] {
            assert_eq!(by(&entries, action), Some(true), "{action:?}");
        }
        assert!(blocker(&claude, true).is_none());

        // A relative path is not a transcript the app may open.
        claude.path = Some("k.jsonl".into());
        assert_eq!(by(&actions(&claude, true), RowAction::OpenLog), Some(false));

        let hutan = item(HistorySource::Hutan, "01J", None);
        let entries = actions(&hutan, false);
        assert_eq!(by(&entries, RowAction::OpenSession), Some(false));
        assert_eq!(by(&entries, RowAction::Resume), None);
        assert_eq!(by(&entries, RowAction::OpenDirectory), Some(false));
        assert!(blocker(&hutan, false).unwrap().contains("no longer exists"));
        assert!(blocker(&hutan, true).is_none());
        assert_eq!(
            by(&actions(&hutan, true), RowAction::OpenSession),
            Some(true)
        );
    }

    #[test]
    fn a_hutan_row_opens_its_session_by_id() {
        let id = SessionId::new();
        let mut r = row(HistorySource::Hutan, "key-ignored");
        r.session_id = Some(id);
        let item = SessionItem::from_row(&r);
        assert_eq!(item.hutan_target().unwrap().session, id.to_string());
        assert_eq!(session_id_text(&item), id.to_string());
        let other = SessionItem::from_row(&row(HistorySource::Codex, "abc"));
        assert!(other.hutan_target().is_none());
        assert_eq!(session_id_text(&other), "abc");
    }

    #[test]
    fn sort_and_group_names_are_the_dropdowns_words() {
        assert_eq!(sort_label(HistorySessionSort::Updated), "Last updated");
        assert_eq!(sort_label(HistorySessionSort::Created), "Created");
        let names: Vec<_> = GroupBy::ALL.iter().map(|g| g.label()).collect();
        assert_eq!(names, ["Project", "Folder", "Agent"]);
    }
}
