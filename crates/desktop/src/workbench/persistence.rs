//! `desktop-state.json` location, load, and atomic save.

use super::changes::ReviewComment;
use super::model::{ModelError, WorkbenchState};
use crate::sessions::Marks;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const STATE_FILE: &str = "desktop-state.json";

/// `$HUTAN_HOME/desktop-state.json`, else
/// `~/Library/Application Support/Hutan/desktop-state.json`. `None` when neither a Hutan home
/// nor a user home is known.
pub fn state_path_from(
    hutan_home: Option<OsString>,
    user_home: Option<OsString>,
) -> Option<PathBuf> {
    match hutan_home.filter(|h| !h.is_empty()) {
        Some(home) => Some(PathBuf::from(home).join(STATE_FILE)),
        None => user_home.filter(|h| !h.is_empty()).map(|home| {
            PathBuf::from(home)
                .join("Library/Application Support/Hutan")
                .join(STATE_FILE)
        }),
    }
}

/// [`state_path_from`] over the process environment.
pub fn default_state_path() -> Option<PathBuf> {
    state_path_from(std::env::var_os("HUTAN_HOME"), std::env::var_os("HOME"))
}

/// Read the saved state. Missing file → default layout; corrupt or unsupported → default layout
/// plus a logged warning.
pub fn load_state(path: &Path) -> WorkbenchState {
    match std::fs::read_to_string(path) {
        Ok(json) => {
            let outcome = WorkbenchState::from_json_or_default(&json);
            match outcome.warning {
                Some(warning) => {
                    tracing::warn!(path = %path.display(), "{warning}; using the default layout")
                }
                None => tracing::info!(path = %path.display(), "restored desktop layout"),
            }
            outcome.state
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(path = %path.display(), "no saved desktop layout; using the default layout");
            WorkbenchState::default()
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), "cannot read desktop layout: {err}; using the default layout");
            WorkbenchState::default()
        }
    }
}

/// Blocks whose output a terminal view folds, per terminal (terminal id → block ids). View state
/// like the layout, saved beside it (`collapsed_blocks` in the state file) so folds survive a
/// restart of the desktop while the daemon keeps the terminal; never sent to the daemon.
pub type CollapsedBlocks = BTreeMap<String, Vec<u64>>;

/// The folded blocks saved in `path`; none when the file is missing or has none.
pub fn load_collapsed(path: &Path) -> CollapsedBlocks {
    load_key(path, "collapsed_blocks")
}

/// Review comments of the Changes views (RC-1), per checkout (its directory): drafts, sent and
/// resolved ones. Like the folds, view state saved beside the layout (`review_comments` in the
/// state file) so they survive a restart of the desktop; never sent to the daemon.
pub type ReviewComments = BTreeMap<String, Vec<ReviewComment>>;

/// The review comments saved in `path`; none when the file is missing or has none.
pub fn load_review_comments(path: &Path) -> ReviewComments {
    load_key(path, "review_comments")
}
/// What the Start… sheet remembers (UX-B, `launcher` in the state file): the What / Agent /
/// Model / Where of the last start, which the next sheet opens on, and the agent + model pairs
/// whose first prompt-carrying chat the user already confirmed. View state like the layout;
/// never sent to the daemon. Values are the sheet's own words (`LaunchKind::key`, a harness id
/// or `direct:<provider>`, a model id, `main` / `new_worktree` / `worktree:<branch>`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LauncherMemory {
    pub kind: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub place: Option<String>,
    /// `<agent>/<model or default>` keys that no longer ask before a chat starts.
    pub confirmed: BTreeSet<String>,
}

impl LauncherMemory {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The launcher memory saved in `path`; empty when the file is missing or has none.
pub fn load_launcher(path: &Path) -> LauncherMemory {
    load_key(path, "launcher")
}

/// What each chat's composer holds, by session id (`chat_drafts` in the state file): unsent text
/// survives closing the tab and restarting the desktop. View state like the layout, saved beside
/// it in **plaintext** (this file is not a place for secrets: nothing typed into a key field is
/// ever put here, only composer text) and never sent to the daemon. Empty drafts are not kept,
/// each is cut at [`MAX_DRAFT_BYTES`], and drafts of sessions that no longer exist are dropped
/// once the session list is known, so the map stays as small as the user's chats.
pub type ChatDrafts = BTreeMap<String, String>;

/// Longest composer text kept in the state file.
pub const MAX_DRAFT_BYTES: usize = 32 * 1024;

/// `text` cut to [`MAX_DRAFT_BYTES`] at a character boundary.
pub fn bounded_draft(text: &str) -> &str {
    if text.len() <= MAX_DRAFT_BYTES {
        return text;
    }
    let mut end = MAX_DRAFT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The chat drafts saved in `path`; none when the file is missing or has none.
pub fn load_chat_drafts(path: &Path) -> ChatDrafts {
    let mut drafts: ChatDrafts = load_key(path, "chat_drafts");
    drafts.retain(|_, text| !text.is_empty());
    for text in drafts.values_mut() {
        let keep = bounded_draft(text).len();
        text.truncate(keep);
    }
    drafts
}

/// What the user did to the attention queue (`attention` in the state file): the sessions marked
/// unread and the snoozes. Client read state like the layout; never sent to the daemon.
pub fn load_marks(path: &Path) -> Marks {
    load_key(path, "attention")
}

/// What the state file holds besides the layout, read from top-level key `key`.
fn load_key<T: serde::de::DeserializeOwned + Default>(path: &Path, key: &str) -> T {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|mut value| value.get_mut(key).map(serde_json::Value::take))
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// The state file's JSON: the layout plus, when there are any, the folded blocks, the review
/// comments, the launcher memory, the chat drafts and what the user did to the attention queue
/// (`attention`: marked unread, snoozed).
pub fn state_json(
    state: &WorkbenchState,
    collapsed: &CollapsedBlocks,
    comments: &ReviewComments,
    launcher: &LauncherMemory,
    drafts: &ChatDrafts,
    marks: &Marks,
) -> Result<String, ModelError> {
    let json = state.to_json()?;
    if collapsed.is_empty()
        && comments.is_empty()
        && launcher.is_empty()
        && drafts.is_empty()
        && marks.is_empty()
    {
        return Ok(json);
    }
    let invalid = |err: serde_json::Error| ModelError::Invalid(err.to_string());
    let mut value: serde_json::Value = serde_json::from_str(&json).map_err(invalid)?;
    if let Some(map) = value.as_object_mut() {
        if !collapsed.is_empty() {
            map.insert(
                "collapsed_blocks".to_owned(),
                serde_json::to_value(collapsed).map_err(invalid)?,
            );
        }
        if !comments.is_empty() {
            map.insert(
                "review_comments".to_owned(),
                serde_json::to_value(comments).map_err(invalid)?,
            );
        }
        if !launcher.is_empty() {
            map.insert(
                "launcher".to_owned(),
                serde_json::to_value(launcher).map_err(invalid)?,
            );
        }
        if !marks.is_empty() {
            map.insert(
                "attention".to_owned(),
                serde_json::to_value(marks).map_err(invalid)?,
            );
        }
        if !drafts.is_empty() {
            map.insert(
                "chat_drafts".to_owned(),
                serde_json::to_value(drafts).map_err(invalid)?,
            );
        }
    }
    serde_json::to_string_pretty(&value).map_err(invalid)
}

/// Write `json` next to `path` and rename it into place so a crash never leaves a torn file.
pub fn save_json(path: &Path, json: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hutan_home_wins() {
        let path = state_path_from(Some("/tmp/hh".into()), Some("/Users/x".into()));
        assert_eq!(path, Some(PathBuf::from("/tmp/hh/desktop-state.json")));
    }

    #[test]
    fn falls_back_to_application_support() {
        let path = state_path_from(None, Some("/Users/x".into()));
        assert_eq!(
            path,
            Some(PathBuf::from(
                "/Users/x/Library/Application Support/Hutan/desktop-state.json"
            ))
        );
        assert_eq!(
            state_path_from(Some("".into()), Some("/Users/x".into())),
            path
        );
        assert_eq!(state_path_from(None, None), None);
    }

    #[test]
    fn folded_blocks_travel_in_the_state_file_beside_the_layout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let state = WorkbenchState::default();
        // Without folds the file is exactly the layout.
        let none = LauncherMemory::default();
        assert_eq!(
            state_json(
                &state,
                &CollapsedBlocks::new(),
                &ReviewComments::new(),
                &none,
                &ChatDrafts::new(),
                &Marks::default()
            )
            .unwrap(),
            state.to_json().unwrap()
        );
        let mut collapsed = CollapsedBlocks::new();
        collapsed.insert("01TERMINAL".to_owned(), vec![3, 7]);
        save_json(
            &path,
            &state_json(
                &state,
                &collapsed,
                &ReviewComments::new(),
                &none,
                &ChatDrafts::new(),
                &Marks::default(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(load_collapsed(&path), collapsed);
        // The layout still loads from the same file.
        assert_eq!(load_state(&path), state);
        // Missing and corrupt files hold none.
        assert!(load_collapsed(&dir.path().join("none.json")).is_empty());
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load_collapsed(&path).is_empty());
    }

    #[test]
    fn the_launcher_memory_travels_beside_the_layout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let state = WorkbenchState::default();
        let memory = LauncherMemory {
            kind: Some("command".to_owned()),
            agent: Some("omp".to_owned()),
            model: Some("claude-opus-4-5".to_owned()),
            place: Some("new_worktree".to_owned()),
            confirmed: BTreeSet::from(["omp/claude-opus-4-5".to_owned()]),
        };
        save_json(
            &path,
            &state_json(
                &state,
                &CollapsedBlocks::new(),
                &ReviewComments::new(),
                &memory,
                &ChatDrafts::new(),
                &Marks::default(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(load_launcher(&path), memory);
        assert_eq!(
            load_state(&path),
            state,
            "the layout loads from the same file"
        );
        assert!(load_launcher(&dir.path().join("none.json")).is_empty());
    }

    #[test]
    fn chat_drafts_travel_beside_the_layout_bounded_and_without_empty_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let state = WorkbenchState::default();
        let mut drafts = ChatDrafts::new();
        drafts.insert(
            "01CHAT".to_owned(),
            "half a thought\nsecond line".to_owned(),
        );
        save_json(
            &path,
            &state_json(
                &state,
                &CollapsedBlocks::new(),
                &ReviewComments::new(),
                &LauncherMemory::default(),
                &drafts,
                &Marks::default(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(load_chat_drafts(&path), drafts);
        assert_eq!(
            load_state(&path),
            state,
            "the layout loads from the same file"
        );
        assert!(load_chat_drafts(&dir.path().join("none.json")).is_empty());

        // A file edited by hand or written by a buggy build is held to the same bounds.
        let long = "é".repeat(MAX_DRAFT_BYTES);
        std::fs::write(
            &path,
            serde_json::json!({"chat_drafts": {"01A": long, "01B": ""}}).to_string(),
        )
        .unwrap();
        let loaded = load_chat_drafts(&path);
        assert_eq!(loaded.keys().collect::<Vec<_>>(), ["01A"]);
        assert_eq!(
            loaded["01A"].len(),
            MAX_DRAFT_BYTES,
            "cut at a character boundary"
        );
        assert_eq!(bounded_draft("short"), "short");
    }

    #[test]
    fn missing_corrupt_and_good_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(STATE_FILE);
        assert_eq!(load_state(&path), WorkbenchState::default());

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ nope").unwrap();
        assert_eq!(load_state(&path), WorkbenchState::default());

        let mut state = WorkbenchState::default();
        state.toggle_dock(super::super::model::DockPosition::Left);
        save_json(&path, &state.to_json().unwrap()).unwrap();
        assert_eq!(load_state(&path), state);
        assert!(!path.with_extension("json.tmp").exists());
    }
}
