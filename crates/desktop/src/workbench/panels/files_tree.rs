//! The Files panel's tree model: the listings of the folders seen, which folders are open, and
//! the flat rows of what shows, drawn by the panel with `uniform_list`.
//!
//! - **Natural order.** A folder's entries are sorted here, folders first, each group in a
//!   case-insensitive, numeric-aware order (`file2` before `file10`).
//! - **Spliced, not rebuilt.** The rows are one flat `Vec`. A listing that arrives for a folder
//!   replaces only the rows of that folder's subtree (and none at all when its entries look the
//!   same), so a listing costs what the folder shows, not what the whole tree shows; the bench
//!   (`files_tree_bench`, B-EX) holds that to its budget at 100 000 open rows.
//! - **Folded folders.** With `compact`, an open folder whose only visible entry is a folder
//!   shares a row with it (`src/utils/helpers`), the chain opened as one: the row's `path` is the
//!   first folder, `tail` the last, and the children are the last one's. Opening a folder that
//!   holds one folder opens that one too (listed through [`Tree::take_opened`]), so a chain never
//!   shows half-folded.
//! - **Filters.** `.gitignore` / `.ignore` matches and dotfiles hide unless asked for; both are
//!   [`TreeOptions`], applied when the rows are built.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::SharedString;
use hutan_protocol::fs::{FsEntry, FsKind, FsListing};

use crate::icons::Icon;

/// What a row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Dir,
    File,
    /// A symbolic link: what it points at is asked when it is opened.
    Link,
    /// The folder has more entries than `fs.list` gives.
    More,
    /// The folder could not be listed.
    Failed,
}

/// One drawn row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The folder or file; for a folded chain its first folder.
    pub path: PathBuf,
    /// The last folder of a folded chain (`None` for any other row): its children follow the row.
    pub tail: Option<PathBuf>,
    /// The entry's name; a folded chain's names joined with `/`.
    pub name: SharedString,
    pub depth: usize,
    pub kind: RowKind,
    pub ignored: bool,
    pub expanded: bool,
    /// An open folder whose listing is on its way.
    pub loading: bool,
}

impl Row {
    /// The folder whose entries follow this row: the last one of a folded chain, else the row's
    /// own path.
    pub fn folder(&self) -> &Path {
        self.tail.as_deref().unwrap_or(&self.path)
    }
}

/// What the tree leaves out and how it folds; the panel reads them from `panels.files.*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeOptions {
    /// List what `.gitignore` / `.ignore` rules match (dimmed).
    pub show_ignored: bool,
    /// List dotfiles.
    pub show_hidden: bool,
    /// Fold single-folder chains into one row.
    pub compact: bool,
}

impl Default for TreeOptions {
    fn default() -> Self {
        Self {
            show_ignored: false,
            show_hidden: true,
            compact: true,
        }
    }
}

/// What is known of a folder.
#[derive(Debug, Clone)]
enum Listing {
    Loading,
    Listed {
        entries: Vec<FsEntry>,
        truncated: bool,
    },
    Failed(String),
}

/// Why *expand all* did nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpandRefusal {
    /// It would show more than [`EXPAND_ALL_ROWS`] rows.
    TooManyRows,
    /// It would have to list more than [`EXPAND_ALL_UNLISTED`] folders first.
    TooManyUnlisted,
}

/// What the row after a truncated listing says (`fs.list` gives at most [`LIST_MAX`]
/// entries); *Filter in this folder* is wired in EX-3.
///
/// [`LIST_MAX`]: hutan_protocol::fs::LIST_MAX
pub const MORE_ROW: &str = "Showing the first 5 000 — filter in this folder";

/// *Expand all* refuses to show more rows than this.
pub const EXPAND_ALL_ROWS: usize = 2_000;
/// *Expand all* refuses to list more folders than this first.
pub const EXPAND_ALL_UNLISTED: usize = 10;

/// The tree of one root.
#[derive(Debug, Clone)]
pub struct Tree {
    root: PathBuf,
    listings: HashMap<PathBuf, Listing>,
    expanded: HashSet<PathBuf>,
    rows: Vec<Row>,
    options: TreeOptions,
    /// Folders opened by folding that the panel has not been told of yet.
    opened: Vec<PathBuf>,
}

/// Case-insensitive, numeric-aware order of two names: runs of digits compare by value, so
/// `file2` comes before `file10`; ties fall back to the plain order so it is total.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut left, mut right) = (a.chars().peekable(), b.chars().peekable());
    // The first difference in the length of equal numbers (`1` before `01`).
    let mut zeros = Ordering::Equal;
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (run_a, run_b) = (digits(&mut left), digits(&mut right));
                let (value_a, value_b) =
                    (run_a.trim_start_matches('0'), run_b.trim_start_matches('0'));
                match value_a
                    .len()
                    .cmp(&value_b.len())
                    .then_with(|| value_a.cmp(value_b))
                {
                    Ordering::Equal => {
                        if zeros == Ordering::Equal {
                            zeros = run_a.len().cmp(&run_b.len());
                        }
                    }
                    other => return other,
                }
            }
            (Some(x), Some(y)) => {
                left.next();
                right.next();
                match x.to_lowercase().cmp(y.to_lowercase()) {
                    Ordering::Equal => {}
                    other => return other,
                }
            }
        }
    }
    zeros.then_with(|| a.cmp(b))
}

/// The run of digits at the front of `chars`.
fn digits(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut run = String::new();
    while let Some(c) = chars.next_if(char::is_ascii_digit) {
        run.push(c);
    }
    run
}

/// The icon of a file, by its extension (or its whole name for the well-known ones); every file
/// without a kind of its own has the generic one (`item.file`). The artwork is the icon pack's:
/// each kind is a semantic id (`item.file.rust`, …, `docs/themes.md`).
pub fn file_icon(name: &str) -> Icon {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "poetry.lock"
        | "gemfile.lock" | "flake.lock" => return Icon::ItemFileLock,
        "dockerfile" | "makefile" | "justfile" | ".gitignore" | ".gitattributes" | ".ignore"
        | ".editorconfig" | ".env" => return Icon::ItemFileConfig,
        "license" | "licence" | "copying" | "notice" => return Icon::ItemFileText,
        _ => {}
    }
    let Some((_, extension)) = lower.rsplit_once('.') else {
        return Icon::ItemFile;
    };
    match extension {
        "rs" => Icon::ItemFileRust,
        "js" | "mjs" | "cjs" | "jsx" => Icon::ItemFileJavascript,
        "ts" | "mts" | "cts" | "tsx" => Icon::ItemFileTypescript,
        "py" | "pyi" | "pyw" => Icon::ItemFilePython,
        "md" | "markdown" | "mdx" => Icon::ItemFileMarkdown,
        "txt" | "log" | "rst" | "text" => Icon::ItemFileText,
        "html" | "htm" | "xhtml" | "vue" | "svelte" => Icon::ItemFileHtml,
        "css" | "scss" | "sass" | "less" => Icon::ItemFileStyle,
        "json" | "jsonc" | "json5" | "ndjson" | "jsonl" | "csv" | "tsv" | "xml" => {
            Icon::ItemFileData
        }
        "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "plist" | "env" => Icon::ItemFileConfig,
        "sh" | "bash" | "zsh" | "fish" | "command" => Icon::ItemFileShell,
        "sql" => Icon::ItemFileSql,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "bmp" | "tif" | "tiff"
        | "heic" | "avif" => Icon::ItemFileImage,
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "zst" => Icon::ItemFileArchive,
        "pdf" => Icon::ItemFilePdf,
        "lock" => Icon::ItemFileLock,
        _ => Icon::ItemFile,
    }
}

/// Folders first, then [`natural_cmp`] by name.
fn sort_entries(entries: &mut [FsEntry]) {
    entries.sort_by(|a, b| {
        (a.kind != FsKind::Dir)
            .cmp(&(b.kind != FsKind::Dir))
            .then_with(|| natural_cmp(&a.name, &b.name))
    });
}

/// Whether two listings draw the same rows (names, kinds, ignore marks); size and time do not.
fn same_shape(old: &[FsEntry], new: &[FsEntry]) -> bool {
    old.len() == new.len()
        && old
            .iter()
            .zip(new)
            .all(|(a, b)| a.name == b.name && a.kind == b.kind && a.ignored == b.ignored)
}

impl Tree {
    /// The root, open and not listed yet.
    pub fn new(root: PathBuf, options: TreeOptions) -> Self {
        let mut tree = Self {
            listings: HashMap::new(),
            expanded: HashSet::from([root.clone()]),
            root,
            rows: Vec::new(),
            options,
            opened: Vec::new(),
        };
        tree.listings.insert(tree.root.clone(), Listing::Loading);
        tree
    }

    /// The root with `dirs` open as well (the ones remembered from the last run): each is
    /// marked as being listed, the panel lists them.
    pub fn with_expanded(
        root: PathBuf,
        options: TreeOptions,
        dirs: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        let mut tree = Self::new(root, options);
        for dir in dirs {
            if dir.starts_with(&tree.root) && dir != tree.root {
                tree.listings.entry(dir.clone()).or_insert(Listing::Loading);
                tree.expanded.insert(dir);
            }
        }
        tree
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn options(&self) -> TreeOptions {
        self.options
    }

    /// Whether `dir` is open (the root always is).
    pub fn is_expanded(&self, dir: &Path) -> bool {
        self.expanded.contains(dir)
    }

    /// The open folders, the root first.
    pub fn expanded_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = self.expanded.iter().cloned().collect();
        dirs.sort_by(|a, b| {
            a.components()
                .count()
                .cmp(&b.components().count())
                .then_with(|| a.cmp(b))
        });
        dirs
    }

    /// Whether `dir` was listed (or is being listed).
    pub fn knows(&self, dir: &Path) -> bool {
        self.listings.contains_key(dir)
    }

    /// Whether `dir` has its entries (a listing arrived and it is not a failure).
    pub fn is_listed(&self, dir: &Path) -> bool {
        matches!(self.listings.get(dir), Some(Listing::Listed { .. }))
    }

    /// The names a folder lists, in the order of its rows, ignoring what the filters hide; for
    /// tests and the panel's *Showing the first 5 000* row.
    pub fn is_truncated(&self, dir: &Path) -> bool {
        matches!(
            self.listings.get(dir),
            Some(Listing::Listed {
                truncated: true,
                ..
            })
        )
    }

    /// The names `dir` lists, whatever the filters hide (a name typed for a new entry collides
    /// with a hidden one as well); `None` while the folder has no listing.
    pub fn entry_names(&self, dir: &Path) -> Option<Vec<&str>> {
        match self.listings.get(dir) {
            Some(Listing::Listed { entries, .. }) => {
                Some(entries.iter().map(|e| e.name.as_str()).collect())
            }
            _ => None,
        }
    }

    /// What the listing of `path`'s folder says `path` is (folder, file, link); `None` when the
    /// folder is not listed or has no such entry.
    pub fn kind_of(&self, path: &Path) -> Option<FsKind> {
        let Some(Listing::Listed { entries, .. }) = self.listings.get(path.parent()?) else {
            return None;
        };
        entries.iter().find(|e| e.path == path).map(|e| e.kind)
    }

    /// Applies new options; the rows are built again when they changed. Folders the new options
    /// make foldable are opened (see [`Self::take_opened`]).
    pub fn set_options(&mut self, options: TreeOptions) {
        if self.options == options {
            return;
        }
        self.options = options;
        self.settle_all();
        self.rebuild();
    }

    /// `dir` is being listed (again): a folder seen for the first time shows a loading row.
    pub fn set_loading(&mut self, dir: &Path) {
        self.listings
            .entry(dir.to_owned())
            .or_insert(Listing::Loading);
    }

    /// The daemon's listing of `dir`, sorted here. Canonical paths are the daemon's business: the
    /// entries are filed under the folder the tree asked for. Returns the open folders that went
    /// with entries that are gone (for unwatching).
    pub fn set_listing(&mut self, dir: &Path, listing: FsListing) -> Vec<PathBuf> {
        let mut entries: Vec<FsEntry> = listing
            .entries
            .into_iter()
            .map(|mut entry| {
                entry.path = dir.join(&entry.name);
                entry
            })
            .collect();
        sort_entries(&mut entries);
        let (unchanged, gone) = match self.listings.get(dir) {
            Some(Listing::Listed {
                entries: old,
                truncated,
            }) => {
                let now: HashSet<&str> = entries.iter().map(|n| n.name.as_str()).collect();
                let gone: Vec<PathBuf> = old
                    .iter()
                    .filter(|o| o.kind == FsKind::Dir && !now.contains(o.name.as_str()))
                    .map(|o| o.path.clone())
                    .collect();
                (
                    *truncated == listing.truncated && same_shape(old, &entries),
                    gone,
                )
            }
            _ => (false, Vec::new()),
        };
        let was_listed = self.is_listed(dir);
        self.listings.insert(
            dir.to_owned(),
            Listing::Listed {
                entries,
                truncated: listing.truncated,
            },
        );
        let mut forgotten = Vec::new();
        for path in gone {
            forgotten.extend(self.forget_below(&path));
        }
        if unchanged && was_listed && forgotten.is_empty() {
            return forgotten;
        }
        self.settle(dir);
        self.refresh(dir);
        forgotten
    }

    pub fn set_failed(&mut self, dir: &Path, message: String) {
        self.listings
            .insert(dir.to_owned(), Listing::Failed(message));
        self.refresh(dir);
    }

    /// Opens `dir`; true when it has to be listed first (the panel lists it again either way,
    /// so a folder closed for a while is fresh).
    pub fn expand(&mut self, dir: &Path) -> bool {
        self.expanded.insert(dir.to_owned());
        let needs = !self.listings.contains_key(dir);
        if needs {
            self.listings.insert(dir.to_owned(), Listing::Loading);
        }
        self.settle(dir);
        self.refresh(dir);
        needs
    }

    /// Closes `dir` and every folder inside it; returns the folders closed (for unwatching).
    pub fn collapse(&mut self, dir: &Path) -> Vec<PathBuf> {
        if dir == self.root {
            return Vec::new();
        }
        let closed: Vec<PathBuf> = self
            .expanded
            .iter()
            .filter(|d| d.starts_with(dir))
            .cloned()
            .collect();
        for d in &closed {
            self.expanded.remove(d);
        }
        self.refresh(dir);
        closed
    }

    /// Closes every folder but the root; returns the folders closed (for unwatching).
    pub fn collapse_all(&mut self) -> Vec<PathBuf> {
        let closed: Vec<PathBuf> = self
            .expanded
            .iter()
            .filter(|d| **d != self.root)
            .cloned()
            .collect();
        self.expanded.retain(|d| *d == self.root);
        self.opened.clear();
        self.rebuild();
        closed
    }

    /// Opens every folder the tree can see: each folder of a listed, open folder, and the
    /// folders inside those that are already listed. Refuses (changing nothing) when that would
    /// show more than [`EXPAND_ALL_ROWS`] rows or first list more than [`EXPAND_ALL_UNLISTED`]
    /// folders. Returns the open folders that are not listed yet (the panel lists them).
    pub fn expand_all(&mut self) -> Result<Vec<PathBuf>, ExpandRefusal> {
        let mut open = Vec::new();
        let mut unlisted = Vec::new();
        let mut rows = 0usize;
        let mut queue = vec![self.root.clone()];
        while let Some(dir) = queue.pop() {
            let Some(Listing::Listed { entries, .. }) = self.listings.get(&dir) else {
                continue;
            };
            for entry in entries.iter().filter(|e| self.visible(e)) {
                rows += 1;
                if entry.kind != FsKind::Dir {
                    continue;
                }
                if !self.expanded.contains(&entry.path) {
                    open.push(entry.path.clone());
                }
                if matches!(self.listings.get(&entry.path), Some(Listing::Listed { .. })) {
                    queue.push(entry.path.clone());
                } else {
                    unlisted.push(entry.path.clone());
                }
            }
        }
        if rows > EXPAND_ALL_ROWS {
            return Err(ExpandRefusal::TooManyRows);
        }
        if unlisted.len() > EXPAND_ALL_UNLISTED {
            return Err(ExpandRefusal::TooManyUnlisted);
        }
        for dir in open {
            self.listings.entry(dir.clone()).or_insert(Listing::Loading);
            self.expanded.insert(dir);
        }
        self.rebuild();
        Ok(unlisted)
    }

    /// The folders opened by folding since the last call: the panel lists and watches them.
    pub fn take_opened(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.opened)
    }

    /// Forgets `dir` and everything below it (it is gone): closed, listing dropped, rows
    /// refreshed from its parent. Returns the open folders forgotten.
    pub fn forget(&mut self, dir: &Path) -> Vec<PathBuf> {
        let forgotten = self.forget_below(dir);
        if let Some(parent) = dir.parent() {
            self.refresh(parent);
        }
        forgotten
    }

    fn forget_below(&mut self, dir: &Path) -> Vec<PathBuf> {
        let open: Vec<PathBuf> = self
            .expanded
            .iter()
            .filter(|d| d.starts_with(dir))
            .cloned()
            .collect();
        for d in &open {
            self.expanded.remove(d);
        }
        self.listings.retain(|d, _| !d.starts_with(dir));
        self.opened.retain(|d| !d.starts_with(dir));
        open
    }

    /// The folders between the root (included) and `path` (excluded), outermost first; `None`
    /// when `path` is not below the root.
    pub fn ancestors_of(&self, path: &Path) -> Option<Vec<PathBuf>> {
        let relative = path.strip_prefix(&self.root).ok()?;
        let mut dirs = vec![self.root.clone()];
        let mut at = self.root.clone();
        let components: Vec<_> = relative.components().collect();
        for component in components.iter().take(components.len().saturating_sub(1)) {
            at.push(component);
            dirs.push(at.clone());
        }
        Some(dirs)
    }

    /// Where `path` is among the rows: its own row, or for a folder inside a folded chain the
    /// chain's.
    pub fn index_of(&self, path: &Path) -> Option<usize> {
        let own = |row: &Row| row.path.as_os_str() == path.as_os_str();
        self.rows.iter().position(own).or_else(|| {
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

    // ---- building rows ----------------------------------------------------------------------

    fn visible(&self, entry: &FsEntry) -> bool {
        (self.options.show_ignored || !entry.ignored)
            && (self.options.show_hidden || !entry.name.starts_with('.'))
    }

    /// The one visible entry of `dir` when it is a folder and nothing else is listed.
    fn sole_dir(&self, dir: &Path) -> Option<&FsEntry> {
        let Some(Listing::Listed {
            entries,
            truncated: false,
        }) = self.listings.get(dir)
        else {
            return None;
        };
        let mut visible = entries.iter().filter(|e| self.visible(e));
        let only = visible.next()?;
        (only.kind == FsKind::Dir && visible.next().is_none()).then_some(only)
    }

    /// Opens the folder `dir` holds alone (and the one that holds, and so on) so the chain
    /// shows folded; the folders opened without a listing are noted in `opened`.
    fn settle(&mut self, dir: &Path) {
        if !self.options.compact {
            return;
        }
        let mut at = dir.to_owned();
        while at != self.root && self.expanded.contains(&at) {
            let Some(next) = self.sole_dir(&at).map(|e| e.path.clone()) else {
                break;
            };
            if !self.expanded.contains(&next) {
                self.expanded.insert(next.clone());
                self.listings
                    .entry(next.clone())
                    .or_insert(Listing::Loading);
                self.opened.push(next.clone());
            }
            at = next;
        }
    }

    fn settle_all(&mut self) {
        for dir in self.expanded_dirs() {
            self.settle(&dir);
        }
    }

    /// Builds every row again.
    fn rebuild(&mut self) {
        let mut rows = Vec::with_capacity(self.rows.len());
        self.push_rows(&self.root, 0, &mut rows);
        self.rows = rows;
    }

    /// Builds again the rows that `dir`'s change can have moved: those of the row `dir` is on
    /// (its own, or the first folder of the chain it is folded in) and everything below it.
    fn refresh(&mut self, dir: &Path) {
        if dir == self.root {
            return self.rebuild();
        }
        // A change of `dir` can fold or unfold the chain it is in, so the rows start at the
        // chain's first folder: the nearest open parent that holds nothing else.
        let mut dir = dir.to_owned();
        while self.options.compact
            && let Some(parent) = dir.parent()
            && parent != self.root
            && self.expanded.contains(parent)
            && self.sole_dir(parent).is_some_and(|e| e.path == dir)
        {
            dir = parent.to_owned();
        }
        let dir = dir.as_path();
        let start = self
            .rows
            .iter()
            .position(|row| row.path.as_os_str() == dir.as_os_str())
            .or_else(|| {
                dir.ancestors()
                    .skip(1)
                    .take_while(|a| *a != self.root)
                    .find_map(|a| {
                        self.rows
                            .iter()
                            .position(|row| row.path.as_os_str() == a.as_os_str())
                    })
            });
        let Some(start) = start else {
            // No row shows it (a closed ancestor, a filtered folder): nothing on screen moves.
            return;
        };
        let (head, depth) = (self.rows[start].path.clone(), self.rows[start].depth);
        let entry = head
            .parent()
            .and_then(|parent| match self.listings.get(parent) {
                Some(Listing::Listed { entries, .. }) => entries.iter().find(|e| e.path == head),
                _ => None,
            });
        let Some(entry) = entry else {
            return self.rebuild();
        };
        let end = self.rows[start + 1..]
            .iter()
            .position(|row| row.depth <= depth)
            .map_or(self.rows.len(), |n| start + 1 + n);
        let mut fresh = Vec::new();
        self.push_entry(entry, depth, &mut fresh);
        self.rows.splice(start..end, fresh);
    }

    fn push_rows(&self, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
        match self.listings.get(dir) {
            None | Some(Listing::Loading) => {}
            Some(Listing::Failed(message)) => rows.push(Row {
                path: dir.join("\u{0}failed"),
                tail: None,
                name: message.clone().into(),
                depth,
                kind: RowKind::Failed,
                ignored: false,
                expanded: false,
                loading: false,
            }),
            Some(Listing::Listed { entries, truncated }) => {
                for entry in entries.iter().filter(|e| self.visible(e)) {
                    self.push_entry(entry, depth, rows);
                }
                if *truncated {
                    rows.push(Row {
                        path: dir.join("\u{0}more"),
                        tail: None,
                        name: MORE_ROW.into(),
                        depth,
                        kind: RowKind::More,
                        ignored: false,
                        expanded: false,
                        loading: false,
                    });
                }
            }
        }
    }

    /// The row of `entry` and, when it is an open folder, the rows below it.
    fn push_entry(&self, entry: &FsEntry, depth: usize, rows: &mut Vec<Row>) {
        let kind = match entry.kind {
            FsKind::Dir => RowKind::Dir,
            FsKind::Symlink => RowKind::Link,
            FsKind::File | FsKind::Other => RowKind::File,
        };
        let expanded = kind == RowKind::Dir && self.expanded.contains(&entry.path);
        let mut tail: Option<&FsEntry> = None;
        let mut name = entry.name.clone();
        if expanded && self.options.compact {
            let mut at = entry;
            while let Some(next) = self.sole_dir(&at.path)
                && self.expanded.contains(&next.path)
                && self.is_listed(&next.path)
            {
                name.push('/');
                name.push_str(&next.name);
                tail = Some(next);
                at = next;
            }
        }
        let folder = tail.map_or(&entry.path, |t| &t.path);
        rows.push(Row {
            path: entry.path.clone(),
            tail: tail.map(|t| t.path.clone()),
            name: name.into(),
            depth,
            kind,
            ignored: entry.ignored,
            expanded,
            loading: expanded && matches!(self.listings.get(folder), None | Some(Listing::Loading)),
        });
        if expanded {
            self.push_rows(folder, depth + 1, rows);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(dir: &str, name: &str, kind: FsKind, ignored: bool) -> FsEntry {
        FsEntry {
            name: name.to_owned(),
            path: PathBuf::from(dir).join(name),
            kind,
            size: 0,
            mtime_ns: 0,
            ino: 0,
            ignored,
        }
    }

    fn listing(dir: &str, entries: Vec<FsEntry>) -> FsListing {
        FsListing {
            dir: PathBuf::from(dir),
            entries,
            truncated: false,
        }
    }

    fn names(tree: &Tree) -> Vec<String> {
        tree.rows()
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.name))
            .collect()
    }

    fn tree(root: &str) -> Tree {
        Tree::new(PathBuf::from(root), TreeOptions::default())
    }

    fn list(tree: &mut Tree, dir: &str, entries: &[(&str, FsKind)]) {
        let entries = entries
            .iter()
            .map(|(name, kind)| entry(dir, name, *kind, false))
            .collect();
        tree.set_listing(Path::new(dir), listing(dir, entries));
        spliced_like_a_rebuild(tree);
    }

    /// The rows a splice left are the rows a full rebuild builds.
    fn spliced_like_a_rebuild(tree: &Tree) {
        let mut full = tree.clone();
        full.rebuild();
        assert_eq!(tree.rows(), full.rows(), "a splice differs from a rebuild");
    }

    #[test]
    fn folders_open_lazily_and_ignored_entries_hide_until_asked_for() {
        let mut tree = Tree::new(
            PathBuf::from("/repo"),
            TreeOptions {
                show_ignored: false,
                ..TreeOptions::default()
            },
        );
        assert!(tree.rows().is_empty(), "the root is not listed yet");
        tree.set_listing(
            Path::new("/repo"),
            listing(
                "/repo",
                vec![
                    entry("/repo", "src", FsKind::Dir, false),
                    entry("/repo", "target", FsKind::Dir, true),
                    entry("/repo", "Cargo.toml", FsKind::File, false),
                ],
            ),
        );
        assert_eq!(names(&tree), ["src", "Cargo.toml"]);
        assert!(tree.expand(Path::new("/repo/src")), "listed on first open");
        assert!(tree.rows()[0].loading);
        tree.set_listing(
            Path::new("/repo/src"),
            listing(
                "/repo/src",
                vec![
                    entry("/repo/src", "main.rs", FsKind::File, false),
                    entry("/repo/src", "lib.rs", FsKind::File, false),
                ],
            ),
        );
        assert_eq!(names(&tree), ["src", "  lib.rs", "  main.rs", "Cargo.toml"]);
        tree.set_options(TreeOptions {
            show_ignored: true,
            ..TreeOptions::default()
        });
        assert_eq!(
            names(&tree),
            ["src", "  lib.rs", "  main.rs", "target", "Cargo.toml"]
        );
        assert!(tree.rows()[3].ignored);

        // Closing keeps the listing: opening again shows its rows at once.
        tree.collapse(Path::new("/repo/src"));
        assert_eq!(names(&tree), ["src", "target", "Cargo.toml"]);
        assert!(!tree.expand(Path::new("/repo/src")));
        assert_eq!(
            names(&tree),
            ["src", "  lib.rs", "  main.rs", "target", "Cargo.toml"]
        );
        assert_eq!(tree.parent_index(1), Some(0));
        assert_eq!(tree.index_of(Path::new("/repo/src/main.rs")), Some(2));
    }

    #[test]
    fn dotfiles_hide_when_asked() {
        let mut tree = Tree::new(
            PathBuf::from("/r"),
            TreeOptions {
                show_hidden: false,
                ..TreeOptions::default()
            },
        );
        list(
            &mut tree,
            "/r",
            &[
                (".env", FsKind::File),
                ("a.txt", FsKind::File),
                (".github", FsKind::Dir),
            ],
        );
        assert_eq!(names(&tree), ["a.txt"]);
        tree.set_options(TreeOptions::default());
        assert_eq!(names(&tree), [".github", ".env", "a.txt"]);
    }

    #[test]
    fn the_ancestors_of_a_file_run_from_the_root_down() {
        let tree = tree("/repo");
        assert_eq!(
            tree.ancestors_of(Path::new("/repo/src/bin/tool.rs")),
            Some(vec![
                PathBuf::from("/repo"),
                PathBuf::from("/repo/src"),
                PathBuf::from("/repo/src/bin"),
            ])
        );
        assert_eq!(
            tree.ancestors_of(Path::new("/repo/README.md")),
            Some(vec![PathBuf::from("/repo")])
        );
        assert_eq!(tree.ancestors_of(Path::new("/elsewhere/x.rs")), None);
    }

    #[test]
    fn closing_a_folder_closes_the_folders_inside_it() {
        let mut tree = tree("/r");
        tree.expand(Path::new("/r/a"));
        tree.expand(Path::new("/r/a/b"));
        tree.expand(Path::new("/r/c"));
        let mut closed = tree.collapse(Path::new("/r/a"));
        closed.sort();
        assert_eq!(closed, [PathBuf::from("/r/a"), PathBuf::from("/r/a/b")]);
        assert!(
            tree.collapse(Path::new("/r")).is_empty(),
            "the root stays open"
        );
        assert_eq!(
            tree.expanded_dirs(),
            [PathBuf::from("/r"), PathBuf::from("/r/c")]
        );
        let mut all = tree.collapse_all();
        all.sort();
        assert_eq!(all, [PathBuf::from("/r/c")]);
        assert_eq!(tree.expanded_dirs(), [PathBuf::from("/r")]);
    }

    #[test]
    fn files_have_an_icon_by_their_kind() {
        assert_eq!(file_icon("main.rs"), Icon::ItemFileRust);
        assert_eq!(file_icon("App.TSX"), Icon::ItemFileTypescript);
        assert_eq!(file_icon("Cargo.lock"), Icon::ItemFileLock);
        assert_eq!(file_icon("Cargo.toml"), Icon::ItemFileConfig);
        assert_eq!(file_icon("README.md"), Icon::ItemFileMarkdown);
        assert_eq!(file_icon("logo.PNG"), Icon::ItemFileImage);
        assert_eq!(file_icon("a.tar.gz"), Icon::ItemFileArchive);
        assert_eq!(file_icon("Makefile"), Icon::ItemFileConfig);
        assert_eq!(file_icon("noextension"), Icon::ItemFile);
        assert_eq!(file_icon("weird.zzz"), Icon::ItemFile);
        assert_eq!(file_icon(".bashrc"), Icon::ItemFile);
    }

    #[test]
    fn names_sort_naturally_folders_first() {
        let mut names_in_order = vec![
            "file10.txt",
            "File2.txt",
            "file1.txt",
            "file01.txt",
            "a",
            "B",
            "item-9",
            "item-10",
            "x2y10",
            "x2y9",
        ];
        names_in_order.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names_in_order,
            [
                "a",
                "B",
                "file1.txt",
                "file01.txt",
                "File2.txt",
                "file10.txt",
                "item-9",
                "item-10",
                "x2y9",
                "x2y10"
            ]
        );
        let mut tree = tree("/r");
        list(
            &mut tree,
            "/r",
            &[
                ("z10", FsKind::File),
                ("z9", FsKind::File),
                ("b", FsKind::Dir),
                ("a2", FsKind::Dir),
                ("a10", FsKind::Dir),
            ],
        );
        assert_eq!(names(&tree), ["a2", "a10", "b", "z9", "z10"]);
    }

    #[test]
    fn a_folder_that_holds_one_folder_shares_its_row_with_it() {
        let mut tree = tree("/r");
        list(
            &mut tree,
            "/r",
            &[("src", FsKind::Dir), ("README.md", FsKind::File)],
        );
        assert!(tree.expand(Path::new("/r/src")));
        list(&mut tree, "/r/src", &[("utils", FsKind::Dir)]);
        // `utils` was opened by folding and has to be listed.
        assert_eq!(tree.take_opened(), [PathBuf::from("/r/src/utils")]);
        assert_eq!(
            names(&tree),
            ["src", "  utils", "README.md"],
            "not folded before it is listed"
        );
        list(&mut tree, "/r/src/utils", &[("helpers", FsKind::Dir)]);
        assert_eq!(names(&tree), ["src/utils", "  helpers", "README.md"]);
        assert_eq!(tree.take_opened(), [PathBuf::from("/r/src/utils/helpers")]);
        list(
            &mut tree,
            "/r/src/utils/helpers",
            &[("a.rs", FsKind::File), ("b.rs", FsKind::File)],
        );
        assert_eq!(
            names(&tree),
            ["src/utils/helpers", "  a.rs", "  b.rs", "README.md"]
        );
        let row = &tree.rows()[0];
        assert_eq!(row.path, Path::new("/r/src"));
        assert_eq!(row.folder(), Path::new("/r/src/utils/helpers"));
        assert!(row.expanded);
        // A folder inside the chain is found on the chain's row.
        assert_eq!(tree.index_of(Path::new("/r/src/utils")), Some(0));
        // A second entry anywhere unfolds the chain from there on.
        list(
            &mut tree,
            "/r/src/utils",
            &[("helpers", FsKind::Dir), ("mod.rs", FsKind::File)],
        );
        assert_eq!(
            names(&tree),
            [
                "src/utils",
                "  helpers",
                "    a.rs",
                "    b.rs",
                "  mod.rs",
                "README.md"
            ]
        );
        // Closing the chain closes what it holds; opening it again folds it again.
        tree.collapse(Path::new("/r/src"));
        assert_eq!(names(&tree), ["src", "README.md"]);
        spliced_like_a_rebuild(&tree);
        tree.expand(Path::new("/r/src"));
        assert_eq!(&*tree.rows()[0].name, "src/utils");
        spliced_like_a_rebuild(&tree);
        // Folding off shows every level.
        tree.set_options(TreeOptions {
            compact: false,
            ..TreeOptions::default()
        });
        assert_eq!(&*tree.rows()[0].name, "src");
        assert_eq!(&*tree.rows()[1].name, "utils");
    }

    #[test]
    fn a_listing_replaces_only_its_own_rows() {
        let mut tree = tree("/r");
        list(
            &mut tree,
            "/r",
            &[
                ("a", FsKind::Dir),
                ("b", FsKind::Dir),
                ("c.txt", FsKind::File),
            ],
        );
        for dir in ["/r/a", "/r/b"] {
            tree.expand(Path::new(dir));
            list(
                &mut tree,
                dir,
                &[("one", FsKind::File), ("two", FsKind::File)],
            );
        }
        assert_eq!(
            names(&tree),
            ["a", "  one", "  two", "b", "  one", "  two", "c.txt"]
        );
        let before = tree.rows().to_vec();
        // The same entries again (size and time may differ): nothing moves.
        let mut again = entry("/r/a", "one", FsKind::File, false);
        again.size = 9;
        tree.set_listing(
            Path::new("/r/a"),
            listing(
                "/r/a",
                vec![again, entry("/r/a", "two", FsKind::File, false)],
            ),
        );
        assert_eq!(tree.rows(), before);
        // A new file in `b` shows in `b` and nowhere else.
        list(
            &mut tree,
            "/r/b",
            &[
                ("one", FsKind::File),
                ("three", FsKind::File),
                ("two", FsKind::File),
            ],
        );
        assert_eq!(
            names(&tree),
            [
                "a", "  one", "  two", "b", "  one", "  three", "  two", "c.txt"
            ]
        );
        assert_eq!(tree.rows()[..3], before[..3]);
    }

    #[test]
    fn a_folder_that_is_gone_takes_its_open_folders_with_it() {
        let mut tree = tree("/r");
        list(&mut tree, "/r", &[("a", FsKind::Dir), ("b", FsKind::Dir)]);
        tree.expand(Path::new("/r/a"));
        list(
            &mut tree,
            "/r/a",
            &[("x", FsKind::Dir), ("f", FsKind::File)],
        );
        tree.expand(Path::new("/r/a/x"));
        list(&mut tree, "/r/a/x", &[("g", FsKind::File)]);
        let forgotten = tree.set_listing(
            Path::new("/r"),
            listing("/r", vec![entry("/r", "b", FsKind::Dir, false)]),
        );
        let mut forgotten = forgotten;
        forgotten.sort();
        assert_eq!(forgotten, [PathBuf::from("/r/a"), PathBuf::from("/r/a/x")]);
        assert_eq!(names(&tree), ["b"]);
        assert!(!tree.knows(Path::new("/r/a/x")));
    }

    #[test]
    fn expand_all_opens_what_is_known_and_refuses_what_is_too_much() {
        let mut tree = tree("/r");
        list(&mut tree, "/r", &[("a", FsKind::Dir), ("b", FsKind::Dir)]);
        let unlisted = tree.expand_all().unwrap();
        assert_eq!(unlisted.len(), 2, "both folders open, to be listed");
        assert!(tree.is_expanded(Path::new("/r/a")));
        list(
            &mut tree,
            "/r/a",
            &[("c", FsKind::Dir), ("f", FsKind::File)],
        );
        list(&mut tree, "/r/b", &[("d", FsKind::File)]);
        assert_eq!(tree.expand_all().unwrap(), [PathBuf::from("/r/a/c")]);

        // More than ten folders to list first.
        let mut wide = self::tree("/w");
        let dirs: Vec<(String, FsKind)> = (0..11).map(|n| (format!("d{n}"), FsKind::Dir)).collect();
        let dirs: Vec<(&str, FsKind)> = dirs.iter().map(|(n, k)| (n.as_str(), *k)).collect();
        list(&mut wide, "/w", &dirs);
        assert_eq!(wide.expand_all(), Err(ExpandRefusal::TooManyUnlisted));
        assert_eq!(wide.expanded_dirs().len(), 1, "nothing changed");

        // More than 2 000 rows.
        let mut big = self::tree("/b");
        let files: Vec<(String, FsKind)> = (0..=EXPAND_ALL_ROWS)
            .map(|n| (format!("f{n}"), FsKind::File))
            .collect();
        let files: Vec<(&str, FsKind)> = files.iter().map(|(n, k)| (n.as_str(), *k)).collect();
        list(&mut big, "/b", &files);
        assert_eq!(big.expand_all(), Err(ExpandRefusal::TooManyRows));
    }

    #[test]
    fn a_listed_folder_says_its_names_and_kinds_whatever_the_filters_hide() {
        let mut tree = tree("/r");
        assert_eq!(tree.entry_names(Path::new("/r")), None, "not listed yet");
        list(
            &mut tree,
            "/r",
            &[
                (".hidden", FsKind::File),
                ("src", FsKind::Dir),
                ("a.rs", FsKind::File),
            ],
        );
        tree.set_options(TreeOptions {
            show_hidden: false,
            ..TreeOptions::default()
        });
        assert_eq!(tree.rows().len(), 2, "the dotfile has no row");
        let mut names = tree.entry_names(Path::new("/r")).unwrap();
        names.sort_unstable();
        assert_eq!(
            names,
            [".hidden", "a.rs", "src"],
            "a new name collides with the hidden one too"
        );
        assert_eq!(tree.kind_of(Path::new("/r/src")), Some(FsKind::Dir));
        assert_eq!(tree.kind_of(Path::new("/r/a.rs")), Some(FsKind::File));
        assert_eq!(tree.kind_of(Path::new("/r/none")), None);
        assert_eq!(tree.kind_of(Path::new("/elsewhere/a.rs")), None);
    }
}
