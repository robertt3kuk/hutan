//! What the Files panel's file operations need that has nothing to do with drawing (EX-7): which
//! names may be typed, what part of a name is selected for a rename, the names of copies
//! (`a copy.txt`, `a copy 2.txt`), where a drop lands, how fast the tree scrolls while a row is
//! dragged near its edge, and the session-only undo journal.
//!
//! The daemon is the judge of every name (`fs.create`, `fs.rename` never overwrite and refuse
//! what is outside the roots); what is checked here only spares the user a round trip and says it
//! in words next to the field.

use std::ops::Range;
use std::path::{Path, PathBuf};

/// The longest name the file system takes (APFS and most others: 255 bytes).
pub const NAME_MAX_BYTES: usize = 255;

/// Steps the journal keeps (a session's worth; nothing is persisted).
pub const JOURNAL_MAX: usize = 100;

/// Why a typed name is not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameProblem {
    Empty,
    /// A `/` (a name is one component; to make a file in a folder, open the folder first).
    Slash,
    /// `.` and `..` name folders that exist already.
    Dots,
    /// A NUL byte.
    Nul,
    TooLong,
    /// A sibling of that name exists (the name, as it is spelled there).
    Exists(String),
}

impl NameProblem {
    /// What the field says under itself.
    pub fn message(&self) -> String {
        match self {
            Self::Empty => "A name is required".to_owned(),
            Self::Slash => "A name cannot contain “/”".to_owned(),
            Self::Dots => "“.” and “..” are not names for a file or folder".to_owned(),
            Self::Nul => "A name cannot contain a NUL character".to_owned(),
            Self::TooLong => format!("A name can be at most {NAME_MAX_BYTES} bytes long"),
            Self::Exists(name) => format!("“{name}” already exists in this folder"),
        }
    }
}

/// Checks the shape of a typed `name` (never its neighbours): the name is taken as typed
/// except that surrounding spaces are not part of it.
pub fn check_name(name: &str) -> Result<&str, NameProblem> {
    let name = name.trim();
    if name.is_empty() {
        return Err(NameProblem::Empty);
    }
    if name.contains('/') {
        return Err(NameProblem::Slash);
    }
    if name.contains('\0') {
        return Err(NameProblem::Nul);
    }
    if name == "." || name == ".." {
        return Err(NameProblem::Dots);
    }
    if name.len() > NAME_MAX_BYTES {
        return Err(NameProblem::TooLong);
    }
    Ok(name)
}

/// The sibling that `name` would collide with: the macOS volumes are case-insensitive, so
/// `readme.md` collides with `README.md`. `except` is the entry being renamed (it does not
/// collide with itself, which is what lets `Readme.md` become `README.md`).
pub fn collision<'a>(
    siblings: impl IntoIterator<Item = &'a str>,
    name: &str,
    except: Option<&str>,
) -> Option<&'a str> {
    let lower = name.to_lowercase();
    siblings
        .into_iter()
        .find(|s| Some(*s) != except && (*s == name || s.to_lowercase() == lower))
}

/// The bytes of `name` a rename selects first: a file's name without its last extension (`main`
/// of `main.rs`, `archive.tar` of `archive.tar.gz`), the whole name of a folder, of a dotfile
/// and of a name without an extension.
pub fn stem_range(name: &str, is_dir: bool) -> Range<usize> {
    if is_dir {
        return 0..name.len();
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => 0..dot,
        _ => 0..name.len(),
    }
}

/// Splits `name` into the part a copy's suffix goes after and the extension (with its dot):
/// `("a", ".txt")`, `("a.tar", ".gz")`, `(".env", "")`, `("src", "")` for a folder.
fn split_ext(name: &str, is_dir: bool) -> (&str, &str) {
    let stem = stem_range(name, is_dir);
    name.split_at(stem.end)
}

/// The `n`th candidate for the name of a copy of `name`: `0` is the name itself, `1` is `a copy.txt`,
/// then `a copy 2.txt`, `a copy 3.txt` (Finder and Zed spell it so).
pub fn copy_name(name: &str, is_dir: bool, n: usize) -> String {
    if n == 0 {
        return name.to_owned();
    }
    let (stem, ext) = split_ext(name, is_dir);
    if n == 1 {
        format!("{stem} copy{ext}")
    } else {
        format!("{stem} copy {n}{ext}")
    }
}

/// The most candidates tried for one copy before it is reported as a conflict.
pub const COPY_TRIES: usize = 12;

/// Where a row dropped onto `target` lands: a folder takes it inside, a file's row means the
/// folder that holds the file.
pub fn drop_folder(target: &Path, target_is_dir: bool) -> Option<PathBuf> {
    if target_is_dir {
        Some(target.to_owned())
    } else {
        target.parent().map(Path::to_owned)
    }
}

/// Whether moving `from` into the folder `dir` would be a no-op or an error the user did not
/// mean: it is already there, or `dir` is the item itself or inside it.
pub fn pointless_move(from: &Path, dir: &Path) -> bool {
    from.parent() == Some(dir) || dir.starts_with(from)
}

/// Pixels the tree scrolls per tick while a dragged row is within [`EDGE`] pixels of the top
/// (negative: the content moves down, the list goes up) or the bottom of the tree; the closer to
/// the edge, the faster. `y` is the pointer's distance from the top of the area.
pub fn edge_scroll(y: f32, height: f32) -> f32 {
    if height <= 2.0 * EDGE {
        return 0.0;
    }
    if y < EDGE {
        -(1.0 + (EDGE - y.max(0.0)) / EDGE * (EDGE_MAX_STEP - 1.0))
    } else if y > height - EDGE {
        1.0 + (y.min(height) - (height - EDGE)) / EDGE * (EDGE_MAX_STEP - 1.0)
    } else {
        0.0
    }
}

/// The hot zone at the top and the bottom of the tree while dragging.
pub const EDGE: f32 = 28.0;
/// The fastest tick of the auto-scroll, in pixels.
pub const EDGE_MAX_STEP: f32 = 18.0;

/// One undoable thing the user did to files, as the inverse can be done again (undo and redo
/// both flip the step).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Items that were renamed or moved: `(from, to)`; `to` is where each is now. Flipping moves
    /// every one back and swaps the pair.
    Moved(Vec<(PathBuf, PathBuf)>),
    /// Items that were made (a new file, copies) or sent to the Trash. `ticket` is `Some` while
    /// they are in the Trash: flipping restores them (`fs.restore`) when it is, else sends them
    /// there (`fs.trash`).
    Presence {
        paths: Vec<PathBuf>,
        ticket: Option<String>,
    },
}

/// The window's undo and redo stacks for file operations. A step is one batch: a paste of five
/// files, a drag of three rows is one.
#[derive(Debug, Default, Clone)]
pub struct Journal {
    undo: Vec<Step>,
    redo: Vec<Step>,
}

impl Journal {
    /// A new thing was done: it can be undone, what could be redone cannot any more.
    pub fn record(&mut self, step: Step) {
        self.redo.clear();
        self.push_undo(step);
    }

    /// Puts `step` back on the undo stack (after a redo, or a failed undo).
    pub fn push_undo(&mut self, step: Step) {
        self.undo.push(step);
        if self.undo.len() > JOURNAL_MAX {
            self.undo.remove(0);
        }
    }

    pub fn push_redo(&mut self, step: Step) {
        self.redo.push(step);
        if self.redo.len() > JOURNAL_MAX {
            self.redo.remove(0);
        }
    }

    pub fn pop_undo(&mut self) -> Option<Step> {
        self.undo.pop()
    }

    pub fn pop_redo(&mut self) -> Option<Step> {
        self.redo.pop()
    }

    pub fn undoable(&self) -> usize {
        self.undo.len()
    }

    pub fn redoable(&self) -> usize {
        self.redo.len()
    }

    /// Forgets that `path` (and what is inside it) ever was: nothing in the journal can be done
    /// to it any more.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

/// How many names the delete sheet lists before it says "and N more".
pub const SHEET_NAMES: usize = 5;

/// The names a sheet lists for `paths`: at most [`SHEET_NAMES`], then how many more there are.
pub fn listed_names(paths: &[PathBuf]) -> (Vec<String>, usize) {
    let names: Vec<String> = paths
        .iter()
        .take(SHEET_NAMES)
        .map(|p| {
            p.file_name().map_or_else(
                || p.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        })
        .collect();
    (names, paths.len().saturating_sub(SHEET_NAMES))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_one_component_that_is_not_empty_or_dots() {
        assert_eq!(check_name("  main.rs "), Ok("main.rs"));
        assert_eq!(check_name(""), Err(NameProblem::Empty));
        assert_eq!(check_name("   "), Err(NameProblem::Empty));
        assert_eq!(check_name("a/b"), Err(NameProblem::Slash));
        assert_eq!(check_name(".."), Err(NameProblem::Dots));
        assert_eq!(check_name("."), Err(NameProblem::Dots));
        assert_eq!(check_name(".gitignore"), Ok(".gitignore"));
        assert_eq!(check_name(&"x".repeat(256)), Err(NameProblem::TooLong));
        assert_eq!(check_name(&"x".repeat(255)).map(str::len), Ok(255));
    }

    #[test]
    fn a_name_collides_without_regard_to_case_but_not_with_the_entry_being_renamed() {
        let siblings = ["README.md", "src", "notes.txt"];
        assert_eq!(collision(siblings, "readme.md", None), Some("README.md"));
        assert_eq!(collision(siblings, "main.rs", None), None);
        // Renaming README.md to Readme.md only changes the case of the same entry.
        assert_eq!(collision(siblings, "Readme.md", Some("README.md")), None);
        assert_eq!(
            collision(siblings, "notes.txt", Some("README.md")),
            Some("notes.txt")
        );
    }

    #[test]
    fn a_rename_selects_the_stem_of_a_file_and_the_whole_name_of_the_rest() {
        assert_eq!(stem_range("main.rs", false), 0..4);
        assert_eq!(stem_range("archive.tar.gz", false), 0..11);
        assert_eq!(stem_range(".gitignore", false), 0..10);
        assert_eq!(stem_range("Makefile", false), 0..8);
        assert_eq!(stem_range("trailing.", false), 0..9);
        assert_eq!(stem_range("v1.2", true), 0..4, "a folder is selected whole");
        assert_eq!(stem_range("héllo.rs", false), 0..6, "bytes, on a boundary");
    }

    #[test]
    fn copies_are_named_copy_then_copy_n_before_the_extension() {
        assert_eq!(copy_name("a.txt", false, 0), "a.txt");
        assert_eq!(copy_name("a.txt", false, 1), "a copy.txt");
        assert_eq!(copy_name("a.txt", false, 2), "a copy 2.txt");
        assert_eq!(copy_name("a.tar.gz", false, 3), "a.tar copy 3.gz");
        assert_eq!(copy_name(".env", false, 1), ".env copy");
        assert_eq!(copy_name("src", true, 1), "src copy");
        assert_eq!(copy_name("v1.2", true, 2), "v1.2 copy 2");
    }

    #[test]
    fn a_drop_on_a_file_lands_in_its_folder_and_pointless_moves_are_seen() {
        assert_eq!(
            drop_folder(Path::new("/r/src"), true),
            Some(PathBuf::from("/r/src"))
        );
        assert_eq!(
            drop_folder(Path::new("/r/src/a.rs"), false),
            Some(PathBuf::from("/r/src"))
        );
        assert!(pointless_move(
            Path::new("/r/src/a.rs"),
            Path::new("/r/src")
        ));
        assert!(pointless_move(Path::new("/r/src"), Path::new("/r/src")));
        assert!(pointless_move(
            Path::new("/r/src"),
            Path::new("/r/src/deep")
        ));
        assert!(!pointless_move(Path::new("/r/a.rs"), Path::new("/r/src")));
        // A sibling whose name merely starts the same way is not inside the folder.
        assert!(!pointless_move(Path::new("/r/src"), Path::new("/r/src2")));
    }

    #[test]
    fn the_tree_scrolls_near_its_edges_faster_the_closer_to_them() {
        assert_eq!(edge_scroll(100.0, 400.0), 0.0);
        let slow_up = edge_scroll(EDGE - 1.0, 400.0);
        let fast_up = edge_scroll(0.0, 400.0);
        assert!(slow_up < 0.0 && fast_up < slow_up, "{slow_up} {fast_up}");
        assert_eq!(fast_up, -EDGE_MAX_STEP);
        let slow_down = edge_scroll(400.0 - EDGE + 1.0, 400.0);
        let fast_down = edge_scroll(400.0, 400.0);
        assert!(slow_down > 0.0 && fast_down > slow_down);
        assert_eq!(fast_down, EDGE_MAX_STEP);
        assert_eq!(
            edge_scroll(5.0, 40.0),
            0.0,
            "a stub of a list never scrolls"
        );
    }

    #[test]
    fn the_journal_keeps_a_step_per_batch_and_a_new_step_ends_the_redo() {
        let mut journal = Journal::default();
        let a = Step::Moved(vec![("/r/a".into(), "/r/b".into())]);
        let b = Step::Presence {
            paths: vec!["/r/c".into(), "/r/d".into()],
            ticket: None,
        };
        journal.record(a.clone());
        journal.record(b.clone());
        assert_eq!(journal.undoable(), 2);
        assert_eq!(journal.pop_undo(), Some(b.clone()));
        journal.push_redo(b);
        assert_eq!(journal.redoable(), 1);
        journal.record(a);
        assert_eq!(journal.redoable(), 0, "doing something new ends the redo");
        for _ in 0..(JOURNAL_MAX + 10) {
            journal.record(Step::Moved(Vec::new()));
        }
        assert_eq!(journal.undoable(), JOURNAL_MAX);
    }

    #[test]
    fn the_sheet_lists_five_names_and_counts_the_rest() {
        let paths: Vec<PathBuf> = (0..8).map(|i| PathBuf::from(format!("/r/f{i}"))).collect();
        let (names, more) = listed_names(&paths);
        assert_eq!(names, ["f0", "f1", "f2", "f3", "f4"]);
        assert_eq!(more, 3);
        assert_eq!(
            listed_names(&paths[..2]),
            (vec!["f0".into(), "f1".into()], 0)
        );
    }
}
