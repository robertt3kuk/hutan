//! Git blame in file tabs (GIT-HISTORY): the workbench asks the daemon (`git.blame`, with the
//! buffer's own text as `contents`, so unsaved edits are blamed as what they are) whenever a tab
//! turns blame on, when a tab of the file loads or saves, and drops what the buffer holds when no
//! tab of the file wants it. The buffer keeps the answer in step with the text
//! (`hutan_code_view::blame`); the tab draws it. A click on a line's blame opens the file's
//! history at that commit (`file_history.rs`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use gpui::Context;
use hutan_buffer::LineEnding;
use hutan_code_view::{BlameCommit, BlameData, FileBuffer};
use hutan_protocol::git::{GIT_UNCOMMITTED, GitBlame, GitBlameResult};

use super::view::Workbench;

/// `git.blame`'s answer as the buffer holds it: the commits, and the commit of every line of
/// `text` (the text that was blamed).
pub fn blame_data(result: &GitBlameResult, text: Arc<str>) -> BlameData {
    let commits: Vec<BlameCommit> = result
        .commits
        .iter()
        .map(|c| BlameCommit {
            id: c.id.clone(),
            author: c.author.clone(),
            email: c.email.clone(),
            time: c.time,
            subject: c.subject.clone(),
            uncommitted: c.id == GIT_UNCOMMITTED,
        })
        .collect();
    let index: HashMap<&str, u32> = commits
        .iter()
        .enumerate()
        .filter_map(|(at, c)| Some((c.id.as_str(), u32::try_from(at).ok()?)))
        .collect();
    let mut lines: Vec<u32> = Vec::new();
    for hunk in &result.hunks {
        let first = (hunk.line as usize).saturating_sub(1);
        // A hunk past what was covered (never the case for an answer in order) leaves a gap.
        let commit = index.get(hunk.commit.as_str()).copied().unwrap_or(u32::MAX);
        lines.resize(first, u32::MAX);
        lines.extend(std::iter::repeat_n(commit, hunk.count as usize));
    }
    BlameData {
        commits,
        lines,
        text,
    }
}

/// The text the daemon blames for `buffer`: what a save would write, so that lines match the
/// repository's byte for byte (a CRLF file, a byte-order mark).
fn blamed_contents(buffer: &FileBuffer) -> String {
    let text = buffer.text().to_string();
    let mut contents = String::with_capacity(text.len() + 3);
    if buffer.has_bom() {
        contents.push('\u{feff}');
    }
    match buffer.line_ending() {
        LineEnding::Lf => contents.push_str(&text),
        LineEnding::CrLf => contents.push_str(&text.replace('\n', "\r\n")),
    }
    contents
}

impl Workbench {
    /// Brings the blame of `path`'s buffer up to date: asks the daemon when a tab of the file
    /// wants one, drops it when none does.
    pub(crate) fn refresh_blame(&mut self, path: &str, cx: &mut Context<Self>) {
        let wanted = self
            .file_views
            .iter()
            .any(|((_, p), view)| p == path && view.read(cx).wants_blame());
        let Some(stored) = self.buffers.buffers.get_mut(path) else {
            return;
        };
        let buffer = stored.buffer.clone();
        if !wanted {
            stored.blame_task = None;
            if buffer.read(cx).has_blame() {
                buffer.update(cx, |b, cx| b.set_blame(None, cx));
            }
            return;
        }
        if !buffer.read(cx).is_ready() {
            // Asked again when the text arrives (`loaded`).
            return;
        }
        if !self.sessions.read(cx).connection().supports_git_history() {
            self.set_notice(
                "This daemon does not answer git history. Restart it to update.",
                cx,
            );
            return;
        }
        let (size, contents) = {
            let b = buffer.read(cx);
            (b.text().len(), blamed_contents(b))
        };
        if size > hutan_code_view::blame::MAX_BLAME_BYTES {
            self.set_notice("This file is too large to blame.", cx);
            return;
        }
        let text: Arc<str> = Arc::from(buffer.read(cx).text().to_string());
        let request = GitBlame {
            path: Path::new(path).to_owned(),
            rev: None,
            contents: Some(contents),
        };
        let asking = self
            .sessions
            .update(cx, |store, cx| store.git_blame(request, cx));
        let key = path.to_owned();
        let task = cx.spawn(async move |this, cx| {
            let answer = asking.await;
            this.update(cx, |this, cx| {
                let Some(stored) = this.buffers.buffers.get_mut(&key) else {
                    return;
                };
                stored.blame_task = None;
                let buffer = stored.buffer.clone();
                match answer {
                    Ok(result) => {
                        let data = blame_data(&result, text);
                        buffer.update(cx, |b, cx| b.set_blame(Some(data), cx));
                    }
                    Err(error) => {
                        buffer.update(cx, |b, cx| b.set_blame(None, cx));
                        this.set_notice(blame_failure(&key, &error), cx);
                    }
                }
            })
            .ok();
        });
        if let Some(stored) = self.buffers.buffers.get_mut(path) {
            stored.blame_task = Some(task);
        }
    }

    /// *Toggle Blame* from the file tab's menu.
    pub(crate) fn toggle_blame_of(
        &mut self,
        pane: super::model::PaneId,
        path: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.file_views.get(&(pane, path.to_owned())).cloned() {
            view.update(cx, |view, cx| view.toggle_blame(cx));
        }
    }
}

/// What the notice says when the daemon could not blame `path`.
pub fn blame_failure(path: &str, error: &str) -> String {
    let name = Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned());
    if error.contains("not a tracked file") || error.contains("in no git checkout") {
        format!("{name} is not tracked by git, so it has no blame.")
    } else {
        format!("Cannot blame {name}: {error}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_protocol::git::{BlameCommit as Wire, BlameHunk};

    fn wire(id: &str, author: &str) -> Wire {
        Wire {
            id: id.to_owned(),
            author: author.to_owned(),
            email: String::new(),
            time: 1,
            subject: String::new(),
            path: "f.rs".into(),
            boundary: false,
        }
    }

    #[test]
    fn hunks_become_a_commit_per_line() {
        let result = GitBlameResult {
            root: "/r".into(),
            path: "f.rs".into(),
            rev: None,
            lines: 4,
            hunks: vec![
                BlameHunk {
                    line: 1,
                    count: 2,
                    commit: "a".repeat(40),
                },
                BlameHunk {
                    line: 3,
                    count: 1,
                    commit: GIT_UNCOMMITTED.into(),
                },
                BlameHunk {
                    line: 4,
                    count: 1,
                    commit: "a".repeat(40),
                },
            ],
            commits: vec![
                wire(&"a".repeat(40), "Ada"),
                wire(GIT_UNCOMMITTED, "Not Committed Yet"),
            ],
            truncated: false,
        };
        let data = blame_data(&result, Arc::from("a\nb\nc\nd\n"));
        assert_eq!(data.lines, vec![0, 0, 1, 0]);
        assert!(data.commits[1].uncommitted);
        assert!(!data.commits[0].uncommitted);
    }

    #[test]
    fn the_notice_says_why_a_file_has_no_blame() {
        assert_eq!(
            blame_failure("/p/a.rs", "a.rs is not a tracked file"),
            "a.rs is not tracked by git, so it has no blame."
        );
        assert!(blame_failure("/p/a.rs", "boom").starts_with("Cannot blame a.rs"));
    }
}
