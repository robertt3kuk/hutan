//! ⌘P, quick open (E-13a, ADR 0012 amendment 8): one palette with a **Files** section — the
//! files of the active project's folder that match the query (`fs.find`, ranked by the daemon's
//! path-segment scoring plus how recently each was opened here; the recent files with an empty
//! query) — then a **Sessions** section: the jump list of ⌘J (workspaces, projects, sessions).
//!
//! - `path:line[:col]` opens the file at that line and column; Return opens in the focused
//!   pane, ⌘Return in a split to the right.
//! - Every key sends one `fs.find` (the one before is dropped); while it is on its way the file
//!   rows that still match stay, so nothing flickers. Return pressed before the answer of what
//!   was typed waits for it when the top row is chosen.
//! - While ⌘P is open the root is watched: the daemon keeps its index while nothing changes,
//!   and an `fs.generation` of the root (something changed below it) asks again.
//! - A recent file outside the active project's folder is not offered; neither is one that was
//!   gone when it was opened.

use std::path::{Path, PathBuf};

use gpui::{AppContext as _, Context, SharedString, Task};
use hutan_client::Notice;
use hutan_protocol::fs::{FsFind, FsFindMatch};
use tokio::sync::broadcast::error::RecvError;

use super::files::FilePlace;
use super::view::Workbench;
use super::workspaces::{PickerAction, PickerEntry, PickerKind, PickerState};

/// File rows ⌘P shows at most.
pub const FILE_ROWS: usize = 20;
/// What `fs.find` is asked for: more than is shown, so recent files can move up.
const FIND_LIMIT: u32 = 100;
/// Recent files listed when nothing is typed.
pub const RECENT_ROWS: usize = 10;
/// How much the most recent file gains, in the daemon's thousandths of a point (about one
/// matched word start); older ones gain less, down to nothing after [`RECENT_CONSIDERED`].
const RECENT_BONUS: u32 = 1_500;
const RECENT_CONSIDERED: usize = 20;

/// The `fs.find` in flight and the listener of the root's `fs.generation` while ⌘P is open.
#[derive(Default)]
pub(crate) struct QuickOpenTasks {
    find: Option<Task<()>>,
    listener: Option<Task<()>>,
    /// The root watched for ⌘P.
    watched: Option<PathBuf>,
    /// The window ⌘P is open in (a Return that waited for the answer runs in it).
    window: Option<gpui::AnyWindowHandle>,
}

/// A file row: where it is and which bytes of its name and folder the query matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHit {
    /// Absolute.
    pub path: PathBuf,
    pub name_marks: Vec<std::ops::Range<usize>>,
    pub dir_marks: Vec<std::ops::Range<usize>>,
}

/// What ⌘P knows about files.
#[derive(Debug, Clone, Default)]
pub struct FileFinder {
    /// The active project's folder; `None` (Home): no Files section.
    pub root: Option<PathBuf>,
    /// How many of the picker's first entries are files.
    pub rows: usize,
    /// An `fs.find` is on its way.
    pub searching: bool,
    /// The path part of the query the rows answer; `None` before the first answer and after an
    /// `fs.generation`.
    pub answered: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    /// Return was pressed on the top row before the answer: where to open it once it is in.
    pub pending_confirm: Option<FilePlace>,
    pub failed: Option<String>,
    /// `fs.find` answers taken (tests wait on them).
    pub answers: u64,
    /// The root's `fs.generation`s that asked again (tests).
    pub refreshes: u64,
    /// Paths in the daemon's index of the root, as of the last answer.
    pub indexed: u32,
    seq: u64,
}

/// A query split into the path to find and the place in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileQuery {
    pub path: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

/// `src/main.rs:16:5` → `src/main.rs`, line 16, column 5; `main.rs:16` → line 16; a trailing
/// `:` is dropped; `./` in front too.
pub fn parse_query(query: &str) -> FileQuery {
    let mut rest = query.trim();
    let mut numbers: Vec<u32> = Vec::new();
    while numbers.len() < 2 {
        let Some((head, tail)) = rest.rsplit_once(':') else {
            break;
        };
        if tail.is_empty() && numbers.is_empty() {
            rest = head;
            continue;
        }
        match tail.trim().parse::<u32>() {
            Ok(n) if n > 0 => {
                numbers.push(n);
                rest = head;
            }
            _ => break,
        }
    }
    numbers.reverse();
    FileQuery {
        path: rest.trim().trim_start_matches("./").to_owned(),
        line: numbers.first().copied(),
        column: numbers.get(1).copied(),
    }
}

/// The daemon's matches, best first once recency counts: a file opened recently gains up to
/// [`RECENT_BONUS`]. `recent` holds paths relative to the root, most recent first.
pub fn rank(matches: Vec<FsFindMatch>, recent: &[String]) -> Vec<FsFindMatch> {
    let considered = recent.len().min(RECENT_CONSIDERED);
    let bonus = |path: &str| {
        recent
            .iter()
            .take(considered)
            .position(|r| r == path)
            .map_or(0, |i| {
                RECENT_BONUS * (considered - i) as u32 / considered as u32
            })
    };
    let mut ranked: Vec<(u32, FsFindMatch)> = matches
        .into_iter()
        .map(|m| (m.score.saturating_add(bonus(&m.path)), m))
        .collect();
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.path.len().cmp(&b.1.path.len()))
            .then_with(|| a.1.path.cmp(&b.1.path))
    });
    ranked.into_iter().map(|(_, m)| m).collect()
}

/// Whether every word of `query` is a subsequence of `path`, ignoring case (what keeps an old
/// row while the answer to the new query is on its way; the daemon matches the same way).
fn still_matches(query: &str, path: &str) -> bool {
    query.split_whitespace().all(|word| {
        let mut wanted = word.chars().flat_map(char::to_lowercase).peekable();
        for c in path.chars().flat_map(char::to_lowercase) {
            if wanted.peek() == Some(&c) {
                wanted.next();
            }
        }
        wanted.peek().is_none()
    })
}

/// The row of file `relative` below `root`, `matched` being byte ranges of `relative`.
pub(crate) fn file_entry(root: &Path, relative: &str, matched: &[[u32; 2]]) -> PickerEntry {
    let split = relative.rfind('/');
    let (dir, name, name_start) = match split {
        Some(i) => (&relative[..i], &relative[i + 1..], i + 1),
        None => ("", relative, 0),
    };
    let clip = |start: usize, end: usize| -> Vec<std::ops::Range<usize>> {
        matched
            .iter()
            .filter_map(|[a, b]| {
                let (a, b) = ((*a as usize).max(start), (*b as usize).min(end));
                (a < b).then(|| a - start..b - start)
            })
            .collect()
    };
    PickerEntry {
        icon: crate::icons::Icon::ItemFile,
        title: SharedString::from(name.to_owned()),
        detail: SharedString::from(dir.to_owned()),
        action: PickerAction::OpenFile(FileHit {
            path: root.join(relative),
            name_marks: clip(name_start, relative.len()),
            dir_marks: clip(0, dir.len()),
        }),
        // Files are filtered by the daemon, not by the picker.
        fixed: true,
    }
}

/// `path` relative to `root`, as `/`-separated text; `None` outside it.
fn relative_to(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .and_then(Path::to_str)
        .filter(|r| !r.is_empty())
        .map(str::to_owned)
}

/// Whether an `fs.generation` of `changed` concerns `root`.
fn concerns(changed: &Path, root: &Path) -> bool {
    root.starts_with(changed) || changed.starts_with(root)
}

impl FileFinder {
    /// Replaces the file rows (the picker's first entries); a selection among the sessions keeps
    /// its row.
    fn set_rows(picker: &mut PickerState, rows: Vec<PickerEntry>) {
        let before = picker.files.rows;
        let count = rows.len();
        picker.entries.splice(0..before, rows);
        picker.files.rows = count;
        if picker.selected >= before && before > 0 {
            picker.selected = (picker.selected + count).saturating_sub(before);
        }
        picker.refilter();
    }
}

impl Workbench {
    /// ⌘P: the files of the active project, then the jump list.
    pub fn open_quick_open(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let entries = self.jump_entries(cx);
        let root = self.sessions.read(cx).project_files_root();
        let mut state = PickerState::new(PickerKind::QuickOpen, entries);
        state.files.root = root.clone();
        self.show_picker(state, window, cx);
        self.quick_open.window = Some(window.window_handle());
        let Some(root) = root else {
            return;
        };
        self.sessions
            .update(cx, |store, cx| store.watch_dir(&root, cx));
        self.quick_open.watched = Some(root);
        self.quick_open_listen(cx);
        self.quick_open_find(cx);
    }

    /// The recent files below `root`, relative to it, most recent first.
    fn recent_below(&self, root: &Path) -> Vec<String> {
        self.recent_files
            .iter()
            .filter_map(|f| relative_to(root, Path::new(f)))
            .collect()
    }

    /// Asks for the files of the query (after a key, a generation, or on opening).
    pub(crate) fn quick_open_find(&mut self, cx: &mut Context<Self>) {
        let recent = match self.picker.as_ref().and_then(|p| p.files.root.clone()) {
            Some(root) => self.recent_below(&root),
            None => Vec::new(),
        };
        let Some(picker) = &mut self.picker else {
            return;
        };
        if picker.kind != PickerKind::QuickOpen {
            return;
        }
        let Some(root) = picker.files.root.clone() else {
            return;
        };
        if picker.history_query().is_some() {
            self.quick_open.find = None;
            picker.files.searching = false;
            picker.files.pending_confirm = None;
            picker.files.answered = None;
            FileFinder::set_rows(picker, Vec::new());
            return;
        }
        let query = parse_query(&picker.query);
        picker.files.line = query.line;
        picker.files.column = query.column;
        let path = relative_to(&root, Path::new(&query.path)).unwrap_or(query.path);
        if picker.files.answered.as_deref() == Some(path.as_str()) {
            // Only the line or column changed.
            return;
        }
        let connection = self.sessions.read(cx).connection().clone();
        picker.files.seq += 1;
        let seq = picker.files.seq;
        if path.is_empty() {
            // The recent files; one empty `fs.find` builds the daemon's index meanwhile.
            let rows = recent
                .iter()
                .take(RECENT_ROWS)
                .map(|r| file_entry(&root, r, &[]))
                .collect();
            FileFinder::set_rows(picker, rows);
            picker.files.answered = Some(String::new());
            picker.files.searching = false;
            let request = FsFind {
                root: root.clone(),
                query: String::new(),
                limit: Some(1),
            };
            let warm = cx.background_spawn(async move { connection.fs_find(request).await });
            self.quick_open.find = Some(cx.spawn(async move |this, cx| {
                if let Ok(found) = warm.await {
                    this.update(cx, |this, _| {
                        if let Some(picker) = &mut this.picker
                            && picker.kind == PickerKind::QuickOpen
                        {
                            picker.files.indexed = found.indexed;
                        }
                    })
                    .ok();
                }
            }));
            return;
        }
        // The rows that still match stay until the answer.
        let kept: Vec<PickerEntry> = picker.entries[..picker.files.rows]
            .iter()
            .filter(|e| match &e.action {
                PickerAction::OpenFile(hit) => relative_to(&root, &hit.path)
                    .is_some_and(|relative| still_matches(&path, &relative)),
                _ => false,
            })
            .cloned()
            .collect();
        FileFinder::set_rows(picker, kept);
        picker.files.searching = true;
        let request = FsFind {
            root: root.clone(),
            query: path.clone(),
            limit: Some(FIND_LIMIT),
        };
        let found = cx.background_spawn(async move { connection.fs_find(request).await });
        self.quick_open.find = Some(cx.spawn(async move |this, cx| {
            let found = found.await;
            this.update(cx, |this, cx| {
                let Some(picker) = &mut this.picker else {
                    return;
                };
                if picker.kind != PickerKind::QuickOpen || picker.files.seq != seq {
                    return;
                }
                picker.files.searching = false;
                picker.files.answers += 1;
                match found {
                    Ok(found) => {
                        picker.files.failed = None;
                        picker.files.indexed = found.indexed;
                        let rows = rank(found.matches, &recent)
                            .into_iter()
                            .take(FILE_ROWS)
                            .map(|m| file_entry(&root, &m.path, &m.matched))
                            .collect();
                        FileFinder::set_rows(picker, rows);
                        picker.files.answered = Some(path);
                    }
                    Err(error) => {
                        picker.files.failed = Some(super::files::error_text(&error));
                        FileFinder::set_rows(picker, Vec::new());
                    }
                }
                cx.notify();
                if let Some(place) = this
                    .picker
                    .as_mut()
                    .and_then(|p| p.files.pending_confirm.take())
                {
                    this.quick_open_confirm_pending(place, cx);
                }
            })
            .ok();
        }));
    }

    /// Runs the Return that waited for the answer, in the window.
    fn quick_open_confirm_pending(&mut self, place: FilePlace, cx: &mut Context<Self>) {
        let Some(window) = self.quick_open.window else {
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.update_window(window, |_, window, cx| {
                this.update(cx, |this, cx| {
                    this.picker_confirm_at(None, place, window, cx);
                })
                .ok();
            })
            .ok();
        })
        .detach();
    }

    fn quick_open_listen(&mut self, cx: &mut Context<Self>) {
        let mut notices = self.sessions.read(cx).connection().notices();
        self.quick_open.listener = Some(cx.spawn(async move |this, cx| {
            loop {
                let root = match notices.recv().await {
                    Ok(Notice::FsGeneration { root, .. }) => root,
                    Ok(_) | Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                };
                if this
                    .update(cx, |this, cx| this.quick_open_generation(&root, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    /// Something changed below `changed`: the file rows may be stale, so the query is asked
    /// again (the daemon's index of the root was rebuilt or will be).
    fn quick_open_generation(&mut self, changed: &Path, cx: &mut Context<Self>) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        if picker.kind != PickerKind::QuickOpen
            || !picker
                .files
                .root
                .as_deref()
                .is_some_and(|root| concerns(changed, root))
        {
            return;
        }
        picker.files.refreshes += 1;
        picker.files.answered = None;
        self.quick_open_find(cx);
        cx.notify();
    }

    /// ⌘P closed (or gave way to another picker): nothing is asked or watched any more.
    pub(crate) fn quick_open_closed(&mut self, cx: &mut Context<Self>) {
        self.quick_open.find = None;
        self.quick_open.listener = None;
        if let Some(root) = self.quick_open.watched.take() {
            self.sessions
                .update(cx, |store, cx| store.unwatch_dir(&root, cx));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_names_the_path_then_the_line_and_column() {
        let q = |s: &str| {
            let q = parse_query(s);
            (q.path, q.line, q.column)
        };
        assert_eq!(
            q("src/main.rs:16:5"),
            ("src/main.rs".into(), Some(16), Some(5))
        );
        assert_eq!(q("main.rs:16"), ("main.rs".into(), Some(16), None));
        assert_eq!(q(" main.rs: "), ("main.rs".into(), None, None));
        assert_eq!(q("./src/lib.rs:3:"), ("src/lib.rs".into(), Some(3), None));
        assert_eq!(q("a:b"), ("a:b".into(), None, None));
        assert_eq!(q("x:1:2:3"), ("x:1".into(), Some(2), Some(3)));
        assert_eq!(q("main.rs:0"), ("main.rs:0".into(), None, None));
    }

    fn hit(path: &str, score: u32) -> FsFindMatch {
        FsFindMatch {
            path: path.to_owned(),
            score,
            matched: Vec::new(),
        }
    }

    #[test]
    fn recent_files_move_up_but_a_much_better_match_still_wins() {
        let found = vec![
            hit("src/main.rs", 12_000),
            hit("crates/a/src/main.rs", 11_500),
            hit("tools/maint/run.rs", 6_000),
        ];
        let recent = vec!["crates/a/src/main.rs".to_owned()];
        let ranked: Vec<String> = rank(found.clone(), &recent)
            .into_iter()
            .map(|m| m.path)
            .collect();
        assert_eq!(
            ranked,
            ["crates/a/src/main.rs", "src/main.rs", "tools/maint/run.rs"]
        );
        let recent = vec!["tools/maint/run.rs".to_owned()];
        assert_eq!(rank(found, &recent)[0].path, "src/main.rs");
    }

    #[test]
    fn a_file_row_shows_the_name_and_its_folder_with_the_matched_bytes() {
        let entry = file_entry(Path::new("/repo"), "src/main.rs", &[[0, 1], [4, 8]]);
        assert_eq!(entry.title.as_ref(), "main.rs");
        assert_eq!(entry.detail.as_ref(), "src");
        let PickerAction::OpenFile(hit) = entry.action else {
            panic!("a file row opens a file");
        };
        assert_eq!(hit.path, PathBuf::from("/repo/src/main.rs"));
        let one = |marks: &[std::ops::Range<usize>]| (marks.len() == 1).then(|| marks[0].clone());
        assert_eq!(one(&hit.dir_marks), Some(0..1));
        assert_eq!(one(&hit.name_marks), Some(0..4));
        let top = file_entry(Path::new("/repo"), "Cargo.toml", &[[0, 5]]);
        assert_eq!(top.detail.as_ref(), "");
        let PickerAction::OpenFile(hit) = top.action else {
            panic!("a file row opens a file");
        };
        assert_eq!(one(&hit.name_marks), Some(0..5));
    }

    #[test]
    fn rows_that_still_match_and_generations_that_concern_the_root() {
        assert!(still_matches("mainrs", "src/main.rs"));
        assert!(!still_matches("mainrx", "src/main.rs"));
        // Words are each a subsequence, in any order.
        assert!(still_matches("src main", "src/main.rs"));
        assert!(still_matches("main src", "src/main.rs"));
        assert!(!still_matches("src zzz", "src/main.rs"));
        assert!(concerns(Path::new("/repo"), Path::new("/repo")));
        assert!(concerns(Path::new("/"), Path::new("/repo")));
        assert!(!concerns(Path::new("/other"), Path::new("/repo")));
    }
}
