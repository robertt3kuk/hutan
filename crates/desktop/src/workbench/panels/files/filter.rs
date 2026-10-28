//! Filter as you type in the Files panel (EX-3).
//!
//! A field above the tree. Typing a character with the keyboard in the tree, ⌘F, or *Filter in this
//! Folder* (the row menu, the "Showing the first 5 000" row) puts the keyboard in it; what is typed
//! narrows the tree to the files that match and the folders they are in:
//!
//! - **Words.** The query is split on white space and every word must match, each as a fuzzy
//!   subsequence of the file's path below the folder being filtered (the project's root, or the
//!   folder *Filter in this Folder* named), the way the daemon's `fs.find` reads it. A word with an
//!   upper-case letter is exact about case.
//! - **Instant, then the daemon's.** A key shows at once what the tree already knows: the files of
//!   the folders listed so far, plus what the last answer held that still matches. Nothing in a
//!   frame waits on the daemon. After [`DEBOUNCE`] the daemon is asked (`fs.find`, at most
//!   [`FIND_MAX`] files, the previous request dropped, an answer to an older query ignored) and its
//!   answer, which looks in the whole project, replaces the instant one.
//! - **A projection of the tree.** The matches and their folders are the rows ([`Projection`]):
//!   folders are synthesised from the paths, all open, none listed, in the tree's order, folded
//!   like the tree folds them, the matching letters in the accent colour. The tree behind it is
//!   not touched: its open folders, its scroll and its selection come back when the filter ends.
//! - **Keys.** ↓ in the field moves into the results, Return opens the first file, Escape clears
//!   the filter (the tree comes back as it was) and puts the keyboard in the tree; in the tree ↑ on
//!   the first row goes back to the field, a character or Backspace edits the query, Escape clears
//!   it. A folder's row shows that folder in the tree and ends the filter.
//! - **Highlight** (`panels.files.filter_mode`). The tree stays as it is; the loaded rows whose name
//!   matches have their matching letters marked, and ↓ in the field moves to the next one.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, ParentElement, Pixels, Point, SharedString, Styled, Task, Window, div, px,
};
use hutan_protocol::fs::{FIND_MAX, FsFind, FsFound};

use super::super::files_tree::{Row, RowKind, Tree, natural_cmp};
use super::FilesPanel;
use crate::icons::{Icon, icon};
use crate::settings::{FilesPanelSettings, FilterMode};
use crate::theme::DensityExt as _;
use crate::workbench::Region;
use crate::workbench::chat::vim::Outcome;
use crate::workbench::render::probe;
use crate::workbench::style::UiStyle;
use crate::workbench::text_field::FieldRef;
use crate::workbench::ui::{self, FocusRingExt as _};
use crate::workbench::vim_ui::{self, FieldEdit, VimConfig};

/// A key waits this long for the next one before the daemon is asked.
pub const DEBOUNCE: Duration = Duration::from_millis(60);
/// Longest query looked at, in characters (the daemon's limit).
const QUERY_MAX_CHARS: usize = 128;
/// Most files the filter shows.
pub const RESULTS_MAX: usize = FIND_MAX as usize;

// ---- matching ----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    chars: Vec<char>,
    /// The word has an upper-case letter: case counts.
    exact: bool,
}

/// What was typed: words that must all match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    words: Vec<Word>,
    /// The words as typed, separated by one space: what [`crate::workbench::relevance`] ranks by.
    text: String,
}

fn same(a: char, b: char, exact: bool) -> bool {
    if a == b {
        return true;
    }
    if exact {
        return false;
    }
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(&b);
    }
    a.to_lowercase().eq(b.to_lowercase())
}

/// Matches `word` as a subsequence of `text[from..]`, leftmost; the byte ranges of the letters
/// are appended to `out`.
fn subsequence(word: &Word, text: &str, from: usize, out: &mut Vec<Range<usize>>) -> bool {
    let mut found: Vec<Range<usize>> = Vec::with_capacity(word.chars.len());
    for (i, c) in text[from..].char_indices() {
        let Some(&want) = word.chars.get(found.len()) else {
            break;
        };
        if same(c, want, word.exact) {
            let at = from + i;
            found.push(at..at + c.len_utf8());
        }
    }
    if found.len() < word.chars.len() {
        return false;
    }
    out.extend(found);
    true
}

/// `ranges` sorted, with the ones that touch joined.
fn merge(ranges: &mut Vec<Range<usize>>) {
    ranges.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges.drain(..) {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    *ranges = merged;
}

impl Query {
    /// The words of `text`; `None` when there are none.
    pub fn parse(text: &str) -> Option<Self> {
        let text: String = text.chars().take(QUERY_MAX_CHARS).collect();
        let words: Vec<Word> = text
            .split_whitespace()
            .map(|word| Word {
                chars: word.chars().collect(),
                exact: word.chars().any(char::is_uppercase),
            })
            .collect();
        let text = words
            .iter()
            .map(|word| word.chars.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join(" ");
        (!words.is_empty()).then_some(Self { words, text })
    }

    /// How well `path` answers the query, 0 (the file's own name) to 5 (scattered letters): the
    /// class [`crate::workbench::relevance`] gives it, the order the results come in.
    pub fn rank(&self, path: &str) -> u8 {
        crate::workbench::relevance::rank(&self.text, path).class
    }

    /// The byte ranges of `path` the query matched, or `None` when some word does not match. A
    /// word is looked for in the last name first (`rs` in `src/lib.rs` is the extension, not
    /// the `s` of `src`), then in the whole path.
    pub fn matches(&self, path: &str) -> Option<Vec<Range<usize>>> {
        let name = path.rfind('/').map_or(0, |i| i + 1);
        let mut ranges = Vec::new();
        for word in &self.words {
            if !(name > 0 && subsequence(word, path, name, &mut ranges)
                || subsequence(word, path, 0, &mut ranges))
            {
                return None;
            }
        }
        merge(&mut ranges);
        Some(ranges)
    }

    /// Whether the query matches `path` (no ranges kept).
    pub fn is_match(&self, path: &str) -> bool {
        self.matches(path).is_some()
    }
}

// ---- hits and the projection -------------------------------------------------------------------

/// One file the filter found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Below the filtered folder, `/`-separated.
    pub path: String,
    pub kind: RowKind,
    /// Byte ranges of `path` that matched.
    pub ranges: Vec<Range<usize>>,
    /// How well the path answers the query ([`Query::rank`]): the better results come first.
    pub rank: u8,
}

/// The rows of the matches and the folders they are in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Projection {
    rows: Vec<Row>,
    /// Per row, the byte ranges of its name that matched.
    marks: Vec<Vec<Range<usize>>>,
    files: usize,
}

struct Node {
    name: String,
    path: PathBuf,
    kind: RowKind,
    marks: Vec<Range<usize>>,
    dirs: Vec<usize>,
    files: Vec<usize>,
    /// The best [`Hit::rank`] of what is at or below this node.
    best: u8,
}

impl Projection {
    /// The rows of `hits` below `scope`: what holds the better results first ([`Hit::rank`]: a
    /// file called what was typed before a path that only has its letters), and within the same
    /// rank folders first, each group in natural order; the folders synthesised from the paths and
    /// all open; with `compact` a folder that holds only one
    /// folder shares its row with it.
    pub fn build(scope: &Path, hits: &[Hit], compact: bool) -> Self {
        let mut nodes = vec![Node {
            name: String::new(),
            path: scope.to_owned(),
            kind: RowKind::Dir,
            marks: Vec::new(),
            dirs: Vec::new(),
            files: Vec::new(),
            best: u8::MAX,
        }];
        let mut known: HashMap<(usize, &str), usize> = HashMap::new();
        let mut files = 0;
        for hit in hits {
            let mut at = 0;
            let mut start = 0;
            let mut parts = hit.path.split('/').peekable();
            while let Some(part) = parts.next() {
                let span = start..start + part.len();
                start = span.end + 1;
                if part.is_empty() {
                    continue;
                }
                let last = parts.peek().is_none();
                let marks: Vec<Range<usize>> = hit
                    .ranges
                    .iter()
                    .filter(|r| r.start < span.end && r.end > span.start)
                    .map(|r| r.start.max(span.start) - span.start..r.end.min(span.end) - span.start)
                    .collect();
                let node = match known.get(&(at, part)) {
                    Some(&node) => node,
                    None => {
                        let node = nodes.len();
                        let path = nodes[at].path.join(part);
                        nodes.push(Node {
                            name: part.to_owned(),
                            path,
                            kind: if last { hit.kind } else { RowKind::Dir },
                            marks: Vec::new(),
                            dirs: Vec::new(),
                            files: Vec::new(),
                            best: u8::MAX,
                        });
                        if last {
                            nodes[at].files.push(node);
                            files += 1;
                        } else {
                            nodes[at].dirs.push(node);
                        }
                        known.insert((at, part), node);
                        node
                    }
                };
                nodes[node].marks.extend(marks);
                merge(&mut nodes[node].marks);
                nodes[node].best = nodes[node].best.min(hit.rank);
                at = node;
            }
        }
        let mut projection = Self {
            rows: Vec::new(),
            marks: Vec::new(),
            files,
        };
        projection.emit_children(&mut nodes, 0, 0, compact);
        projection
    }

    fn emit_children(&mut self, nodes: &mut [Node], at: usize, depth: usize, compact: bool) {
        let mut dirs = std::mem::take(&mut nodes[at].dirs);
        let mut files = std::mem::take(&mut nodes[at].files);
        dirs.sort_by(|a, b| natural_cmp(&nodes[*a].name, &nodes[*b].name));
        files.sort_by(|a, b| natural_cmp(&nodes[*a].name, &nodes[*b].name));
        // Folders then files in natural order, and what holds the better results first (a stable
        // sort: equal ranks keep that order).
        let mut order: Vec<(usize, bool)> = dirs
            .into_iter()
            .map(|d| (d, true))
            .chain(files.into_iter().map(|f| (f, false)))
            .collect();
        order.sort_by_key(|(node, _)| nodes[*node].best);
        for (child, is_dir) in order {
            if !is_dir {
                let node = &nodes[child];
                self.rows.push(Row {
                    path: node.path.clone(),
                    tail: None,
                    name: node.name.clone().into(),
                    depth,
                    kind: node.kind,
                    ignored: false,
                    expanded: false,
                    loading: false,
                    root: false,
                });
                self.marks.push(node.marks.clone());
                continue;
            }
            let dir = child;
            // A folded chain: this folder, then the one it holds alone, and so on.
            let mut chain = vec![dir];
            let mut last = dir;
            while compact
                && let (true, [only]) = (nodes[last].files.is_empty(), nodes[last].dirs.as_slice())
            {
                last = *only;
                chain.push(last);
            }
            let mut name = String::new();
            let mut marks = Vec::new();
            for &link in &chain {
                if !name.is_empty() {
                    name.push('/');
                }
                let offset = name.len();
                name.push_str(&nodes[link].name);
                marks.extend(
                    nodes[link]
                        .marks
                        .iter()
                        .map(|r| r.start + offset..r.end + offset),
                );
            }
            let tail = *chain.last().unwrap_or(&dir);
            self.rows.push(Row {
                path: nodes[dir].path.clone(),
                tail: (chain.len() > 1).then(|| nodes[tail].path.clone()),
                name: name.into(),
                depth,
                kind: RowKind::Dir,
                ignored: false,
                expanded: true,
                loading: false,
                root: false,
            });
            self.marks.push(marks);
            self.emit_children(nodes, tail, depth + 1, compact);
        }
    }

    /// The rows of a workspace's tree: each root that has hits is a row of its own (open), its
    /// [`Self::build`] one level in.
    pub fn build_roots(found: &[(PathBuf, Vec<Hit>)], compact: bool) -> Self {
        let mut projection = Self::default();
        for (root, hits) in found.iter().filter(|(_, hits)| !hits.is_empty()) {
            projection.rows.push(Row {
                path: root.clone(),
                tail: None,
                name: root
                    .file_name()
                    .map_or_else(
                        || root.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    )
                    .into(),
                depth: 0,
                kind: RowKind::Dir,
                ignored: false,
                expanded: true,
                loading: false,
                root: true,
            });
            projection.marks.push(Vec::new());
            let inner = Self::build(root, hits, compact);
            projection.files += inner.files;
            projection.marks.extend(inner.marks);
            projection
                .rows
                .extend(inner.rows.into_iter().map(|mut row| {
                    row.depth += 1;
                    row
                }));
        }
        projection
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The matched byte ranges of the name of row `index`.
    pub fn marks(&self, index: usize) -> &[Range<usize>] {
        self.marks.get(index).map_or(&[], Vec::as_slice)
    }

    /// How many files the rows hold.
    pub fn files(&self) -> usize {
        self.files
    }

    /// Where `path` is: its own row, or for a folder inside a folded chain the chain's.
    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.path.as_os_str() == path.as_os_str())
            .or_else(|| {
                self.rows.iter().position(|row| {
                    row.tail
                        .as_deref()
                        .is_some_and(|tail| path.starts_with(&row.path) && tail.starts_with(path))
                })
            })
    }

    /// The parent row of the row at `index`.
    pub fn parent_index(&self, index: usize) -> Option<usize> {
        let depth = self.rows.get(index)?.depth;
        (0..index).rev().find(|i| self.rows[*i].depth < depth)
    }
}

// ---- the panel's state -------------------------------------------------------------------------

/// The files found below each folder looked in (several in a workspace's tree).
type Found = Vec<(PathBuf, Vec<Hit>)>;

/// What the daemon answered for a query.
struct Answer {
    scopes: Vec<PathBuf>,
    found: Found,
}

/// What the tree comes back to when the filter ends.
struct Restore {
    selected: Option<PathBuf>,
    /// The rows of a selection of several, and where its range started.
    picked: HashSet<PathBuf>,
    anchor: Option<PathBuf>,
    offset: Point<Pixels>,
    /// A file was revealed meanwhile (the tab changed): the tree scrolls to it.
    revealed: bool,
}

/// What the line under the field says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterStatus {
    /// No filter.
    Idle,
    /// Highlight mode: `count` loaded rows match.
    Marked(usize),
    /// The instant matches are shown; the daemon's answer is on its way.
    Searching,
    /// The daemon answered: `shown` files; `capped` when there are more than it gives.
    Found {
        shown: usize,
        capped: bool,
        index_truncated: bool,
    },
    /// The daemon could not answer.
    Failed(String),
}

pub(super) struct Filter {
    pub text: String,
    pub edit: FieldEdit,
    pub focus: FocusHandle,
    /// The folder being filtered; `None` is the root.
    pub scope: Option<PathBuf>,
    query: Option<Query>,
    projection: Option<Projection>,
    answer: Option<Answer>,
    status: FilterStatus,
    seq: u64,
    task: Option<Task<()>>,
    restore: Option<Restore>,
    requests: usize,
    answers: usize,
}

impl Filter {
    pub fn new(cx: &mut gpui::App) -> Self {
        Self {
            text: String::new(),
            edit: FieldEdit::new(),
            focus: cx.focus_handle(),
            scope: None,
            query: None,
            projection: None,
            answer: None,
            status: FilterStatus::Idle,
            seq: 0,
            task: None,
            restore: None,
            requests: 0,
            answers: 0,
        }
    }

    /// The rows the filter shows instead of the tree's, while it filters.
    pub fn projection(&self) -> Option<&Projection> {
        self.projection.as_ref()
    }

    /// Forgets the filter without restoring anything (the root changed).
    pub fn reset(&mut self) {
        self.text.clear();
        self.edit.reset();
        self.scope = None;
        self.query = None;
        self.projection = None;
        self.answer = None;
        self.status = FilterStatus::Idle;
        self.seq += 1;
        self.task = None;
        self.restore = None;
    }

    /// Something is typed or a folder is chosen: the filter is "on".
    pub fn engaged(&self) -> bool {
        !self.text.is_empty() || self.scope.is_some()
    }
}

/// Hits of the daemon's answer.
fn hits_of(found: FsFound, query: &Query) -> Vec<Hit> {
    found
        .matches
        .into_iter()
        .map(|m| Hit {
            ranges: m
                .matched
                .iter()
                .map(|r| r[0] as usize..r[1] as usize)
                .collect(),
            rank: query.rank(&m.path),
            path: m.path,
            kind: RowKind::File,
        })
        .collect()
}

/// A hidden name (a dot first) anywhere in `path`.
fn is_hidden(path: &str) -> bool {
    path.split('/').any(|part| part.starts_with('.'))
}

impl FilesPanel {
    // ---- read access (also for tests) ---------------------------------------------------------

    /// What is typed in the filter field.
    pub fn filter_text(&self) -> &str {
        &self.filter.text
    }

    /// The folder the filter looks in when it is not the root.
    pub fn filter_scope(&self) -> Option<&Path> {
        self.filter.scope.as_deref()
    }

    /// Whether a query narrows the tree (a projection) or marks it (highlight mode).
    pub fn filtering(&self) -> bool {
        self.filter.query.is_some()
    }

    /// Whether the rows are a projection of the matches rather than the tree.
    pub fn projecting(&self) -> bool {
        self.filter.projection.is_some()
    }

    pub fn filter_status(&self) -> &FilterStatus {
        &self.filter.status
    }

    /// `fs.find` requests sent so far.
    pub fn filter_requests(&self) -> usize {
        self.filter.requests
    }

    /// `fs.find` answers taken so far (an answer to an older query is not).
    pub fn filter_answers(&self) -> usize {
        self.filter.answers
    }

    /// Whether the keyboard is in the filter field.
    pub fn filter_focused(&self, window: &Window) -> bool {
        self.filter.focus.is_focused(window)
    }

    /// The matched byte ranges of the name of row `index`, in either mode.
    pub fn filter_marks(&self, index: usize) -> Vec<Range<usize>> {
        if let Some(projection) = &self.filter.projection {
            return projection.marks(index).to_vec();
        }
        match (&self.filter.query, self.rows().get(index)) {
            (Some(query), Some(row))
                if self.filter_mode() == FilterMode::Highlight
                    && matches!(row.kind, RowKind::Dir | RowKind::File | RowKind::Link) =>
            {
                query.matches(&row.name).unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    /// What typing does: a folder's own filter always narrows (the daemon looks in it); in the
    /// root it is what `panels.files.filter_mode` says.
    pub(super) fn filter_mode(&self) -> FilterMode {
        if self.filter.scope.is_some() {
            return FilterMode::Filter;
        }
        self.env.settings.get::<FilesPanelSettings>().filter_mode
    }

    /// The folder *Filter in this Folder* means: the selected folder, the folder of the selected
    /// file, else the root.
    pub(super) fn selected_folder(&self) -> Option<PathBuf> {
        let root = self.root()?.to_owned();
        let Some(selected) = self.selected.clone() else {
            return Some(root);
        };
        match self.index_of(&selected).map(|i| self.rows()[i].clone()) {
            Some(row) if row.kind == RowKind::Dir => Some(row.folder().to_owned()),
            Some(_) => selected.parent().map(Path::to_owned),
            None => Some(root),
        }
    }

    /// The folders the filter looks in: the one it was narrowed to, else every root of the tree
    /// (one, or a workspace's projects).
    fn filter_folders(&self) -> Vec<PathBuf> {
        match (&self.filter.scope, &self.tree) {
            (Some(scope), _) => vec![scope.clone()],
            (None, Some(tree)) => tree.roots().to_vec(),
            _ => Vec::new(),
        }
    }

    /// Whether the results are grouped under a row per root: a workspace's tree not narrowed to
    /// a folder.
    fn filter_grouped(&self) -> bool {
        self.filter.scope.is_none() && self.tree.as_ref().is_some_and(Tree::is_grouped)
    }

    // ---- starting, changing and ending the filter ---------------------------------------------

    /// The keyboard goes to the filter field: `typed` is added to what is there (the character
    /// that started it), else what is there is selected so typing replaces it.
    pub fn start_filter(
        &mut self,
        typed: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tree.is_none() {
            return;
        }
        self.drop_edit();
        match typed {
            Some(typed) => {
                if self.filter.text.is_empty() {
                    self.filter.edit.reset();
                }
                self.filter.text.push_str(typed);
            }
            None => {
                let text = self.filter.text.clone();
                self.filter.edit.select_all(&text);
            }
        }
        window.focus(&self.filter.focus, cx);
        self.refilter(cx);
        cx.notify();
    }

    /// *Filter in this Folder*: the filter looks in `dir` only, with nothing typed yet.
    pub fn filter_in_folder(&mut self, dir: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tree) = &self.tree else {
            return;
        };
        let Some(root) = tree.root_of(dir) else {
            return;
        };
        // A single tree's root is the whole filter; a root of a workspace's tree is one project.
        let scope = (dir != root || tree.is_grouped()).then(|| dir.to_owned());
        self.drop_edit();
        self.filter.text.clear();
        self.filter.edit.reset();
        self.filter.scope = scope;
        window.focus(&self.filter.focus, cx);
        self.refilter(cx);
        cx.notify();
    }

    /// Replaces what is typed (tests; the field's own typing arrives through [`FieldRef`]).
    pub fn set_filter_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.filter.text != text {
            self.filter.text = text.to_owned();
            self.refilter(cx);
            cx.notify();
        }
    }

    /// Escape and the field's ✕: the text and the folder go, the tree comes back as it was and
    /// has the keyboard.
    pub fn clear_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.text.clear();
        self.filter.edit.reset();
        self.filter.scope = None;
        self.refilter(cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Ends the filter before an operation that needs the tree itself (a new file, a paste).
    pub(super) fn leave_filter(&mut self, cx: &mut Context<Self>) {
        if self.filter.engaged() {
            self.filter.text.clear();
            self.filter.edit.reset();
            self.filter.scope = None;
            self.refilter(cx);
        }
    }

    /// A row of a folder was chosen in the results: the filter ends and the tree shows the folder
    /// open and selected.
    pub(super) fn leave_filter_at(
        &mut self,
        folder: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.filter.text.clear();
        self.filter.edit.reset();
        self.filter.scope = None;
        self.refilter(cx);
        // The folders above it open, then it.
        for dir in self
            .tree
            .as_ref()
            .and_then(|t| t.ancestors_of(folder))
            .unwrap_or_default()
        {
            if !self.tree.as_ref().is_some_and(|t| t.is_expanded(&dir)) {
                self.open_dir(&dir, cx);
            }
        }
        self.open_dir(folder, cx);
        // Its row may not exist yet (the folders above are being listed): it is selected when
        // it does.
        self.follow = Some((folder.to_owned(), false));
        self.apply_follow();
        if self.follow.is_none()
            && let Some(index) = self.tree.as_ref().and_then(|t| t.index_of(folder))
        {
            self.scroll
                .scroll_to_item(index, gpui::ScrollStrategy::Center);
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// A file of the tab was revealed: it is what the tree selects when the filter ends. `true`
    /// while the rows are the filter's, which do not show it.
    pub(super) fn filter_revealed(&mut self, file: &Path) -> bool {
        if let Some(restore) = &mut self.filter.restore {
            restore.selected = Some(file.to_owned());
            restore.picked.clear();
            restore.anchor = Some(file.to_owned());
            restore.revealed = true;
        }
        self.filter.projection.is_some()
    }

    /// The keyboard goes to the filter field.
    pub(super) fn focus_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.filter.focus, cx);
        cx.notify();
    }

    /// The text or the folder changed: what the rows show follows it.
    fn refilter(&mut self, cx: &mut Context<Self>) {
        self.filter.query = Query::parse(&self.filter.text);
        self.filter.seq += 1;
        self.filter.task = None;
        let Some(query) = self.filter.query.clone() else {
            return self.end_filter();
        };
        if self.filter.restore.is_none() {
            self.filter.restore = Some(Restore {
                selected: self.selected.clone(),
                picked: self.picked.clone(),
                anchor: self.anchor.clone(),
                offset: self.scroll.0.borrow().base_handle.offset(),
                revealed: false,
            });
        }
        match self.filter_mode() {
            FilterMode::Highlight => {
                self.filter.projection = None;
                self.filter.answer = None;
                self.count_marked();
            }
            FilterMode::Filter => {
                let hits = self.instant_hits(&query);
                self.show_hits(&hits);
                self.filter.status = FilterStatus::Searching;
                self.find(cx);
                self.scroll
                    .0
                    .borrow()
                    .base_handle
                    .set_offset(Point::default());
            }
        }
        self.forget_hidden_selection();
    }

    /// The query went: the tree is back, with its selection and its scroll.
    fn end_filter(&mut self) {
        self.filter.projection = None;
        self.filter.answer = None;
        self.filter.status = FilterStatus::Idle;
        let Some(restore) = self.filter.restore.take() else {
            return;
        };
        self.selected = restore.selected;
        self.picked = restore.picked;
        self.anchor = restore.anchor;
        self.keep_selection();
        self.scroll
            .0
            .borrow()
            .base_handle
            .set_offset(restore.offset);
        if restore.revealed
            && let Some(index) = self
                .selected
                .as_deref()
                .and_then(|s| self.tree.as_ref().and_then(|t| t.index_of(s)))
        {
            self.scroll
                .scroll_to_item(index, gpui::ScrollStrategy::Center);
        }
    }

    /// Highlight mode: counts the loaded rows that match.
    pub(super) fn count_marked(&mut self) {
        let Some(query) = &self.filter.query else {
            return;
        };
        let count = self
            .rows()
            .iter()
            .filter(|r| matches!(r.kind, RowKind::Dir | RowKind::File | RowKind::Link))
            .filter(|r| query.is_match(&r.name))
            .count();
        self.filter.status = FilterStatus::Marked(count);
    }

    /// A selection that is not among the rows shown is dropped (a key must never act on a row that
    /// is not on screen); the tree's own comes back with the tree.
    fn forget_hidden_selection(&mut self) {
        if self.filter.query.is_some() {
            self.keep_selection();
        }
    }

    // ---- what is found ------------------------------------------------------------------------

    /// What the panel knows without asking: the files of the folders listed so far, and what the
    /// last answer held that still matches (so the rows of the key before do not vanish while the
    /// next answer comes).
    fn instant_hits(&self, query: &Query) -> Found {
        let scopes = self.filter_folders();
        let hidden = !self.env.settings.get::<FilesPanelSettings>().show_hidden;
        let mut out: Found = Vec::with_capacity(scopes.len());
        for scope in &scopes {
            let mut hits: Vec<Hit> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            if let Some(answer) = &self.filter.answer
                && answer.scopes == scopes
                && let Some((_, before)) = answer.found.iter().find(|(s, _)| s == scope)
            {
                for hit in before {
                    if hits.len() >= RESULTS_MAX {
                        break;
                    }
                    if let Some(ranges) = query.matches(&hit.path) {
                        seen.insert(hit.path.clone());
                        hits.push(Hit {
                            path: hit.path.clone(),
                            kind: hit.kind,
                            ranges,
                            rank: query.rank(&hit.path),
                        });
                    }
                }
            }
            if let Some(tree) = &self.tree {
                tree.for_each_listed_file(scope, &mut |entry| {
                    if hits.len() >= RESULTS_MAX {
                        return false;
                    }
                    let Some(relative) = entry.path.strip_prefix(scope).ok().and_then(Path::to_str)
                    else {
                        return true;
                    };
                    if seen.contains(relative) {
                        return true;
                    }
                    if let Some(ranges) = query.matches(relative) {
                        hits.push(Hit {
                            path: relative.to_owned(),
                            kind: match entry.kind {
                                hutan_protocol::fs::FsKind::Symlink => RowKind::Link,
                                _ => RowKind::File,
                            },
                            ranges,
                            rank: query.rank(relative),
                        });
                    }
                    true
                });
            }
            if hidden {
                hits.retain(|h| !is_hidden(&h.path));
            }
            out.push((scope.clone(), hits));
        }
        out
    }

    fn show_hits(&mut self, found: &Found) {
        let compact = self
            .env
            .settings
            .get::<FilesPanelSettings>()
            .compact_folders;
        self.filter.projection = if self.filter_grouped() {
            Some(Projection::build_roots(found, compact))
        } else {
            found
                .first()
                .map(|(scope, hits)| Projection::build(scope, hits, compact))
        };
    }

    /// Asks the daemon for the files of the folders that match, [`DEBOUNCE`] after the last key.
    fn find(&mut self, cx: &mut Context<Self>) {
        let scopes = self.filter_folders();
        if scopes.is_empty() || self.filter.query.is_none() {
            return;
        }
        let text = self.filter.text.trim().to_owned();
        let seq = self.filter.seq;
        let connection = self.env.sessions.read(cx).connection().clone();
        let requests: Vec<FsFind> = scopes
            .iter()
            .map(|scope| FsFind {
                root: scope.clone(),
                query: text.clone(),
                limit: Some(FIND_MAX),
            })
            .collect();
        self.filter.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            this.update(cx, |this, _| this.filter.requests += 1).ok();
            let answer = cx
                .background_executor()
                .spawn(async move {
                    let mut out = Vec::with_capacity(requests.len());
                    for request in requests {
                        let scope = request.root.clone();
                        out.push((scope, connection.fs_find(request).await));
                    }
                    out
                })
                .await;
            this.update(cx, |this, cx| this.found(seq, scopes, answer, cx))
                .ok();
        }));
    }

    fn found(
        &mut self,
        seq: u64,
        scopes: Vec<PathBuf>,
        answer: Vec<(PathBuf, Result<FsFound, hutan_client::ClientError>)>,
        cx: &mut Context<Self>,
    ) {
        // A newer key made this answer stale.
        if seq != self.filter.seq || self.filter.query.is_none() {
            return;
        }
        self.filter.task = None;
        self.filter.answers += 1;
        let hide = !self.env.settings.get::<FilesPanelSettings>().show_hidden;
        let Some(query) = self.filter.query.clone() else {
            return;
        };
        let mut found: Found = Vec::new();
        let mut failure: Option<hutan_client::ClientError> = None;
        let (mut shown, mut capped, mut index_truncated) = (0, false, false);
        for (scope, result) in answer {
            match result {
                Ok(reply) => {
                    capped |= reply.matches.len() >= RESULTS_MAX;
                    index_truncated |= reply.index_truncated;
                    let mut hits = hits_of(reply, &query);
                    if hide {
                        hits.retain(|h| !is_hidden(&h.path));
                    }
                    shown += hits.len();
                    found.push((scope, hits));
                }
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        match failure {
            // Every folder failed: nothing to show. One that failed beside others that answered
            // is left out.
            Some(error) if found.is_empty() => {
                self.filter.status =
                    FilterStatus::Failed(crate::workbench::files::error_text(&error));
            }
            _ => {
                self.show_hits(&found);
                self.filter.status = FilterStatus::Found {
                    shown,
                    capped,
                    index_truncated,
                };
                self.filter.answer = Some(Answer { scopes, found });
            }
        }
        self.forget_hidden_selection();
        cx.notify();
    }

    /// The tree or a notice from the daemon changed what a query would find: asks again.
    pub(super) fn refind(&mut self, cx: &mut Context<Self>) {
        if self.filter.query.is_some() && self.filter_mode() == FilterMode::Filter {
            self.filter.seq += 1;
            self.find(cx);
        }
    }

    // ---- the keyboard -------------------------------------------------------------------------

    /// A key of the tree while the filter may want it: a character starts or extends the filter,
    /// Backspace takes one back, Escape clears it. `true` when the key was used.
    pub(super) fn filter_tree_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if modifiers.platform || modifiers.control || modifiers.alt || modifiers.function {
            return false;
        }
        match keystroke.key.as_str() {
            "escape" if self.filter.engaged() => {
                self.clear_filter(window, cx);
                return true;
            }
            "backspace" if !modifiers.shift && !self.filter.text.is_empty() => {
                self.filter.text.pop();
                window.focus(&self.filter.focus, cx);
                self.refilter(cx);
                cx.notify();
                return true;
            }
            _ => {}
        }
        match keystroke.key_char.as_deref() {
            Some(typed)
                if !typed.is_empty()
                    && typed != " "
                    && !typed.chars().any(char::is_control)
                    && keystroke.key != "space" =>
            {
                self.start_filter(Some(typed), window, cx);
                true
            }
            _ => false,
        }
    }

    /// A key of the filter field itself.
    fn on_filter_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if modifiers.platform
            && !(modifiers.control || modifiers.alt || modifiers.shift)
            && keystroke.key == "f"
        {
            let text = self.filter.text.clone();
            self.filter.edit.select_all(&text);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let vim = VimConfig::of(&self.env.settings);
        let mut text = self.filter.text.clone();
        if self.filter.edit.key(&mut text, event, vim, true, cx) == Outcome::Handled {
            if text != self.filter.text {
                self.filter.text = text;
                self.refilter(cx);
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if modifiers.platform || modifiers.control || modifiers.alt || modifiers.function {
            return;
        }
        match keystroke.key.as_str() {
            "escape" => self.clear_filter(window, cx),
            "down" => self.filter_into_results(window, cx),
            "enter" => self.filter_open_first(window, cx),
            "up" | "tab" => {}
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// ↓ in the field: the keyboard moves to the first result (in highlight mode, the next match).
    fn filter_into_results(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = match &self.filter.projection {
            Some(projection) if !projection.rows().is_empty() => Some(0),
            Some(_) => None,
            None => self.next_match(),
        };
        if let Some(index) = target {
            self.select_index(index);
            window.focus(&self.focus, cx);
        }
    }

    /// Return in the field: the first file of the results opens and has the keyboard.
    fn filter_open_first(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.filter.projection {
            Some(projection) => {
                let first = projection
                    .rows()
                    .iter()
                    .position(|r| matches!(r.kind, RowKind::File | RowKind::Link));
                if let Some(index) = first {
                    self.activate(index, true, window, cx);
                }
            }
            None => self.filter_into_results(window, cx),
        }
    }

    /// In highlight mode: the first row after the selection (wrapping) whose name matches.
    fn next_match(&self) -> Option<usize> {
        let query = self.filter.query.as_ref()?;
        let rows = self.rows();
        let from = self.selected_index().map_or(0, |i| i + 1);
        (from..rows.len()).chain(0..from.min(rows.len())).find(|i| {
            matches!(rows[*i].kind, RowKind::Dir | RowKind::File | RowKind::Link)
                && query.is_match(&rows[*i].name)
        })
    }

    // ---- drawing ------------------------------------------------------------------------------

    /// How the field's element reaches its text: the platform's text input and the mouse.
    fn filter_field(&self, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.filter.focus.clone(),
            |this: &mut FilesPanel, cx, f| {
                let vim_on = VimConfig::of(&this.env.settings).enabled;
                let mut text = this.filter.text.clone();
                this.filter.edit.with(&mut text, vim_on, false, f);
                if text != this.filter.text {
                    this.filter.text = text;
                    this.refilter(cx);
                }
            },
        )
    }

    /// What the line under the field says, when there is something to say.
    fn filter_line(&self) -> Option<String> {
        self.filter.query.as_ref()?;
        let rows = self.filter.projection.as_ref();
        let mut line = match (&self.filter.status, rows) {
            (FilterStatus::Failed(message), _) => message.clone(),
            (FilterStatus::Found { capped: true, .. }, _) => {
                format!("The first {RESULTS_MAX} files — type more to narrow")
            }
            (
                FilterStatus::Found {
                    index_truncated: true,
                    ..
                },
                _,
            ) => "This project has more files than are searched".to_owned(),
            (FilterStatus::Found { shown: 0, .. }, _) => String::new(),
            (FilterStatus::Found { shown, .. }, _) => {
                format!("{shown} {}", if *shown == 1 { "file" } else { "files" })
            }
            // The empty list says it: no line under the field for a search that found nothing.
            (FilterStatus::Searching, Some(p)) if p.files() == 0 => String::new(),
            (FilterStatus::Searching, Some(p)) => {
                format!("{} found so far… searching", p.files())
            }
            (FilterStatus::Searching, None) => "Searching…".to_owned(),
            (FilterStatus::Marked(count), _) => {
                format!("{count} {}", if *count == 1 { "match" } else { "matches" })
            }
            (FilterStatus::Idle, _) => String::new(),
        };
        if !line.is_empty()
            && self.env.settings.get::<FilesPanelSettings>().show_ignored
            && self.filter_mode() == FilterMode::Filter
        {
            line.push_str(" · Ignored files are not searched; use Search");
        }
        (!line.is_empty()).then_some(line)
    }

    /// The field above the tree and the line under it.
    pub(super) fn filter_row(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let focused = self.filter.focus.is_focused(window);
        let text = self.filter.text.clone();
        let vim_on = VimConfig::of(&self.env.settings).enabled;
        let scope: Option<SharedString> = self.filter.scope.as_ref().and_then(|scope| {
            let root = self.tree.as_ref()?.root_of(scope)?;
            let inside = scope.strip_prefix(root).unwrap_or(scope);
            // A root of a workspace's tree is named; a folder below the root by its path.
            let shown = if inside.as_os_str().is_empty() {
                root.file_name().map_or_else(
                    || root.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                )
            } else {
                inside.display().to_string()
            };
            Some(SharedString::from(format!("in {shown}")))
        });
        let clear = self.filter.engaged().then(|| {
            ui::IconButton::new(
                "files-filter-clear",
                Icon::ActionClose,
                "Clear the filter (Esc)",
                &theme,
                &tokens,
            )
            .compact()
            .on_click(cx.listener(|this, _, window, cx| this.clear_filter(window, cx)))
            .into_any_element()
        });
        let field = div()
            .id("files-filter")
            .key_context("FilesFilter")
            .track_focus(&self.filter.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_filter_key(event, window, cx);
            }))
            .relative()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.0))
            .mx(px(tokens.spacing * 2.0))
            .mb(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .h(px(tokens.sidebar_row))
            .rounded(px(tokens.radius))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .focus_ring(focused, &theme, &tokens)
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, window, cx| {
                    // The dock re-focuses the workbench on this same mouse down; take the
                    // keyboard after the event has been handled.
                    cx.defer_in(window, |this, window, cx| {
                        window.focus(&this.filter.focus, cx);
                    });
                }),
            )
            .child(
                div()
                    .flex_none()
                    .child(icon(Icon::ActionSearch, 12.0, theme.muted)),
            )
            .children(scope.map(|scope| {
                div()
                    .flex_none()
                    .max_w_1_2()
                    .truncate()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(scope)
            }))
            .children(vim_ui::string_field(
                "files-filter-text",
                &self.filter.edit,
                &text,
                vim_on,
                "Filter files",
                focused,
                false,
                self.filter_field(cx),
                &theme,
                &tokens,
            ))
            .children(clear)
            .when(
                text.is_empty() && !focused && self.filter.scope.is_none(),
                |s| {
                    s.child(
                        div()
                            .flex_none()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.faint)
                            .child("⌘F"),
                    )
                },
            )
            .child(probe(&self.env.regions, Region::Files("filter")));
        let line = self.filter_line().map(|line| {
            div()
                .id("files-filter-line")
                .flex_none()
                .px(px(tokens.spacing * 3.0))
                .pb(px(tokens.spacing))
                .text_size(px(tokens.secondary_font_size))
                .text_color(if matches!(self.filter.status, FilterStatus::Failed(_)) {
                    theme.danger
                } else {
                    theme.muted
                })
                .truncate()
                .child(SharedString::from(line))
        });
        div()
            .flex_none()
            .flex()
            .flex_col()
            .child(field)
            .children(line)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(path: &str, query: &str) -> Hit {
        let ranges = Query::parse(query)
            .and_then(|q| q.matches(path))
            .unwrap_or_default();
        Hit {
            path: path.to_owned(),
            kind: RowKind::File,
            ranges,
            rank: Query::parse(query).map_or(u8::MAX, |q| q.rank(path)),
        }
    }

    fn shape(p: &Projection) -> Vec<String> {
        p.rows()
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.name))
            .collect()
    }

    #[test]
    fn every_word_must_match_in_any_order() {
        let q = Query::parse("file09 mod5").unwrap();
        assert!(q.is_match("mod5/file09.txt"));
        assert!(!q.is_match("mod5/file08.txt"));
        assert!(Query::parse("   ").is_none());
    }

    #[test]
    fn a_word_is_a_subsequence_and_smart_case() {
        assert!(Query::parse("rtr").unwrap().is_match("src/router.rs"));
        assert!(Query::parse("Router").unwrap().is_match("src/Router.rs"));
        assert!(!Query::parse("Router").unwrap().is_match("src/router.rs"));
    }

    /// A range written so that a one-element array of it is not a lint.
    fn span(start: usize, end: usize) -> Range<usize> {
        start..end
    }

    #[test]
    fn the_last_name_is_looked_in_first_and_ranges_are_joined() {
        let found = Query::parse("rs").unwrap().matches("src/lib.rs").unwrap();
        // The `rs` of the extension, not the `s` of `src`.
        assert_eq!(found, vec![span(8, 10)]);
        let found = Query::parse("lib").unwrap().matches("src/lib.rs").unwrap();
        assert_eq!(found, vec![span(4, 7)]);
        let found = Query::parse("src lib")
            .unwrap()
            .matches("src/lib.rs")
            .unwrap();
        assert_eq!(found, vec![0..3, 4..7]);
    }

    #[test]
    fn matches_come_with_the_folders_they_are_in() {
        let hits = [
            hit("src/b.rs", "rs"),
            hit("docs/a.rs", "rs"),
            hit("src/inner/c.rs", "rs"),
            hit("top.rs", "rs"),
        ];
        let p = Projection::build(Path::new("/r"), &hits, false);
        assert_eq!(
            shape(&p),
            [
                "docs", "  a.rs", "src", "  inner", "    c.rs", "  b.rs", "top.rs"
            ]
        );
        assert_eq!(p.files(), 4);
        assert_eq!(p.rows()[0].path, Path::new("/r/docs"));
        assert_eq!(p.rows()[4].path, Path::new("/r/src/inner/c.rs"));
    }

    #[test]
    fn the_file_called_what_was_typed_comes_before_paths_that_only_have_its_letters() {
        // N-02: filter `glue`: the exact file first, in its folder, ahead of `.github/…`.
        let hits = [
            hit(".github/ISSUE_TEMPLATE/bug_report.yml", "glue"),
            hit("crates/ignore/src/glue.rs", "glue"),
            hit("UNLICENSE", "glue"),
            hit("glue.rs", "glue"),
        ];
        let p = Projection::build(Path::new("/r"), &hits, false);
        let rows = shape(&p);
        // `glue.rs` at the root (class 0) and `crates` (class 0 below) come before `.github`
        // (scattered letters), which used to sort first.
        let at = |name: &str| rows.iter().position(|r| r.trim() == name).unwrap();
        assert!(at("crates") < at(".github"), "{rows:?}");
        assert!(at("glue.rs") < at(".github"), "{rows:?}");
        assert!(at("glue.rs") < at("UNLICENSE"), "{rows:?}");
        // Equal ranks keep the tree's order: folders first, natural.
        let hits = [hit("b.rs", "rs"), hit("a/x.rs", "rs"), hit("c.rs", "rs")];
        let p = Projection::build(Path::new("/r"), &hits, false);
        assert_eq!(shape(&p), ["a", "  x.rs", "b.rs", "c.rs"]);
    }

    #[test]
    fn folders_are_in_natural_order_before_files() {
        let hits = [
            hit("f10/x", "x"),
            hit("f9/x", "x"),
            hit("a2.txt", "x"),
            hit("a10.txt", "x"),
        ];
        let p = Projection::build(Path::new("/r"), &hits, false);
        assert_eq!(shape(&p), ["f9", "  x", "f10", "  x", "a2.txt", "a10.txt"]);
    }

    #[test]
    fn a_folder_that_holds_one_folder_folds_with_it() {
        let hits = [hit("a/b/c/x.rs", "x"), hit("a/b/c/y.rs", "")];
        let p = Projection::build(Path::new("/r"), &hits, true);
        assert_eq!(shape(&p), ["a/b/c", "  x.rs", "  y.rs"]);
        let chain = &p.rows()[0];
        assert_eq!(chain.path, Path::new("/r/a"));
        assert_eq!(chain.tail.as_deref(), Some(Path::new("/r/a/b/c")));
        // A folder inside the chain is found at the chain's row.
        assert_eq!(p.index_of(Path::new("/r/a/b")), Some(0));
        let hits = [hit("a/b/x.rs", "x"), hit("a/c.rs", "c")];
        let p = Projection::build(Path::new("/r"), &hits, true);
        assert_eq!(shape(&p), ["a", "  b", "    x.rs", "  c.rs"]);
    }

    #[test]
    fn the_matched_letters_are_marked_in_each_name() {
        let hits = [hit("crates/desktop/src/lib.rs", "des lib")];
        let p = Projection::build(Path::new("/r"), &hits, false);
        assert_eq!(
            shape(&p),
            ["crates", "  desktop", "    src", "      lib.rs"]
        );
        assert_eq!(p.marks(0), &[] as &[Range<usize>]);
        assert_eq!(p.marks(1), &[span(0, 3)]);
        assert_eq!(p.marks(3), &[span(0, 3)]);
        // Folded, the marks move with the names.
        let p = Projection::build(Path::new("/r"), &hits, true);
        assert_eq!(shape(&p), ["crates/desktop/src", "  lib.rs"]);
        assert_eq!(p.marks(0), &[span(7, 10)]);
        assert_eq!(p.marks(1), &[span(0, 3)]);
    }

    #[test]
    fn the_parent_of_a_row_is_the_nearest_shallower_one() {
        let hits = [hit("a/b/x", "x"), hit("a/y", "y"), hit("z", "z")];
        let p = Projection::build(Path::new("/r"), &hits, false);
        assert_eq!(shape(&p), ["a", "  b", "    x", "  y", "z"]);
        assert_eq!(p.parent_index(2), Some(1));
        assert_eq!(p.parent_index(3), Some(0));
        assert_eq!(p.parent_index(0), None);
        assert_eq!(p.parent_index(4), None);
    }

    #[test]
    fn no_hit_is_no_row() {
        let p = Projection::build(Path::new("/r"), &[], true);
        assert!(p.rows().is_empty());
        assert_eq!(p.files(), 0);
    }

    #[test]
    fn hidden_names_are_told_by_any_part() {
        assert!(is_hidden(".github/ci.yml"));
        assert!(is_hidden("src/.env"));
        assert!(!is_hidden("src/main.rs"));
    }
}
