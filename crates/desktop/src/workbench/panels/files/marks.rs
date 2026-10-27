//! What the Files panel marks beside names besides git's letters (EX-5): the language servers'
//! **diagnostics** — a file shows how many errors (else warnings) it has and takes the colour of
//! the worse one, every folder above it shows the sum, never colour alone (a count and a tooltip
//! with words).
//!
//! The read is the Problems panel's (`lsp.diagnostics`, the same rows: [`problems_of`]); it
//! happens when the panel has a root and a connection, again after every `lsp.state` and
//! `lsp.diagnostics` notice, one read at a time and at most one every [`DIAGNOSTICS_READ_EVERY`]
//! (a trigger meanwhile is one read when the time is up). `panels.files.show_diagnostics` picks
//! the least serious severity, or `off`: then nothing is asked.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{AppContext as _, Context, Task};
use hutan_client::{ConnectionState, Notice};
use hutan_core::WorkspaceId;
use hutan_protocol::lsp::{LspDiagnosticsRequest, LspDiagnosticsResult, LspSeverity};
use tokio::sync::broadcast::error::RecvError;

use super::FilesPanel;
use crate::settings::{FilesDiagnostics, FilesPanelSettings};
use crate::workbench::panels::problems::{ProblemRow, problems_of};

/// The diagnostics are read at most this often.
pub const DIAGNOSTICS_READ_EVERY: Duration = Duration::from_millis(250);

/// How many errors and warnings a file (or everything in a folder) has.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiagCounts {
    pub errors: usize,
    pub warnings: usize,
}

impl DiagCounts {
    pub fn is_empty(self) -> bool {
        self.errors == 0 && self.warnings == 0
    }

    /// Whether the worse severity is an error.
    pub fn has_errors(self) -> bool {
        self.errors > 0
    }

    /// The number beside the name: the errors when there are any, else the warnings.
    pub fn shown(self) -> usize {
        if self.has_errors() {
            self.errors
        } else {
            self.warnings
        }
    }

    /// `2 errors, 1 warning`: the tooltip.
    pub fn words(self) -> String {
        let one = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
        match (self.errors, self.warnings) {
            (0, w) => one(w, "warning"),
            (e, 0) => one(e, "error"),
            (e, w) => format!("{}, {}", one(e, "error"), one(w, "warning")),
        }
    }

    fn add(&mut self, other: Self) {
        self.errors += other.errors;
        self.warnings += other.warnings;
    }
}

/// The diagnostics of a root: its files, and the folders that hold them (below the root; the
/// root itself has no row).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagMarks {
    files: HashMap<PathBuf, DiagCounts>,
    dirs: HashMap<PathBuf, DiagCounts>,
}

impl DiagMarks {
    /// The marks of `result`'s files below `root`; severities below warning do not count.
    pub fn from_result(root: &Path, result: &LspDiagnosticsResult) -> Self {
        let (_, rows) = problems_of(result);
        let mut marks = Self::default();
        for row in rows {
            let ProblemRow::File {
                path,
                errors,
                warnings,
                ..
            } = row
            else {
                continue;
            };
            let counts = DiagCounts { errors, warnings };
            let path = PathBuf::from(path);
            if counts.is_empty() || !path.starts_with(root) {
                continue;
            }
            let mut dir = path.parent();
            while let Some(parent) = dir.filter(|d| d.starts_with(root) && *d != root) {
                marks.dirs.entry(parent.to_owned()).or_default().add(counts);
                dir = parent.parent();
            }
            marks.files.insert(path, counts);
        }
        marks
    }

    /// The counts of the file or folder at `path`.
    pub fn of(&self, path: &Path) -> Option<DiagCounts> {
        self.files
            .get(path)
            .or_else(|| self.dirs.get(path))
            .copied()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// The diagnostics read of one panel.
#[derive(Default)]
pub(super) struct DiagState {
    marks: DiagMarks,
    /// What the last read was asked for: root, connected, the setting, the container.
    asked: Option<(PathBuf, bool, FilesDiagnostics, Option<WorkspaceId>)>,
    task: Option<Task<()>>,
    again: bool,
    cooldown: Option<Task<()>>,
    listener: Option<Task<()>>,
    reads: usize,
}

fn min_severity(setting: FilesDiagnostics) -> Option<LspSeverity> {
    match setting {
        FilesDiagnostics::ErrorsWarnings => Some(LspSeverity::Warning),
        FilesDiagnostics::Errors => Some(LspSeverity::Error),
        FilesDiagnostics::Off => None,
    }
}

impl FilesPanel {
    /// The diagnostics marks of the root's files and folders.
    pub fn diagnostics(&self) -> &DiagMarks {
        &self.diag.marks
    }

    /// Reads of the diagnostics answered so far.
    pub fn diagnostics_reads(&self) -> usize {
        self.diag.reads
    }

    /// No diagnostics read is in flight or waiting.
    pub fn diagnostics_settled(&self) -> bool {
        self.diag.task.is_none() && !self.diag.again
    }

    /// Asks for the diagnostics when the root, the connection, the setting or the container is
    /// not what the last read was asked for; without a root, a connection with language
    /// servers, or with the setting `off`, the marks go.
    pub(super) fn sync_diagnostics(&mut self, cx: &mut Context<Self>) {
        let setting = self
            .env
            .settings
            .get::<FilesPanelSettings>()
            .show_diagnostics;
        let store = self.env.sessions.read(cx);
        let connected = store.connection_state() == ConnectionState::Connected
            && store.connection().supports_lsp();
        let wanted = self.root().map(|root| {
            (
                root.to_owned(),
                connected,
                setting,
                store.scope_container_id(),
            )
        });
        if wanted == self.diag.asked {
            return;
        }
        self.diag.asked = wanted.clone();
        match wanted {
            Some((_, true, setting, _)) if min_severity(setting).is_some() => {
                self.listen_diagnostics(cx);
                self.diag.again = false;
                self.diag.cooldown = None;
                self.diag.task = None;
                self.read_diagnostics(cx);
            }
            _ => {
                self.diag.task = None;
                self.diag.again = false;
                self.diag.cooldown = None;
                if !self.diag.marks.is_empty() {
                    self.diag.marks = DiagMarks::default();
                    cx.notify();
                }
            }
        }
    }

    /// A trigger nobody asked for (a notice): one read, not overlapping another and not more
    /// than every [`DIAGNOSTICS_READ_EVERY`].
    fn refresh_diagnostics(&mut self, cx: &mut Context<Self>) {
        if !matches!(&self.diag.asked, Some((_, true, setting, _)) if min_severity(*setting).is_some())
        {
            return;
        }
        if self.diag.task.is_some() || self.diag.cooldown.is_some() {
            self.diag.again = true;
            return;
        }
        self.read_diagnostics(cx);
    }

    fn read_diagnostics(&mut self, cx: &mut Context<Self>) {
        let Some((root, true, setting, container)) = self.diag.asked.clone() else {
            return;
        };
        let Some(min) = min_severity(setting) else {
            return;
        };
        let connection = self.env.sessions.read(cx).connection().clone();
        let request = LspDiagnosticsRequest {
            // The servers that serve the container the window shows, as the Problems panel asks.
            workspace_id: container,
            min_severity: Some(min),
            ..LspDiagnosticsRequest::default()
        };
        let call = cx.background_spawn(async move { connection.lsp_diagnostics(request).await });
        self.diag.cooldown = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DIAGNOSTICS_READ_EVERY).await;
            this.update(cx, |this, cx| {
                this.diag.cooldown = None;
                if this.diag.task.is_none() && std::mem::take(&mut this.diag.again) {
                    this.refresh_diagnostics(cx);
                }
            })
            .ok();
        }));
        self.diag.task = Some(cx.spawn(async move |this, cx| {
            let answer = call.await;
            this.update(cx, |this, cx| {
                this.diag.task = None;
                this.diag.reads += 1;
                if this.root() != Some(root.as_path()) {
                    return;
                }
                // A read that failed leaves the marks on screen.
                if let Ok(result) = answer {
                    let marks = DiagMarks::from_result(&root, &result);
                    if marks != this.diag.marks {
                        this.diag.marks = marks;
                        cx.notify();
                    }
                }
                if this.diag.cooldown.is_none() && std::mem::take(&mut this.diag.again) {
                    this.refresh_diagnostics(cx);
                }
            })
            .ok();
        }));
    }

    /// Takes the connection's language-server notices.
    fn listen_diagnostics(&mut self, cx: &mut Context<Self>) {
        if self.diag.listener.is_some() {
            return;
        }
        let mut notices = self.env.sessions.read(cx).connection().notices();
        self.diag.listener = Some(cx.spawn(async move |this, cx| {
            loop {
                match notices.recv().await {
                    Ok(
                        Notice::LspState { .. }
                        | Notice::LspDiagnostics { .. }
                        | Notice::DaemonRestarted { .. },
                    )
                    | Err(RecvError::Lagged(_)) => {}
                    Ok(_) => continue,
                    Err(RecvError::Closed) => break,
                }
                if this
                    .update(cx, |this, cx| this.refresh_diagnostics(cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_protocol::lsp::{LspDiagnostic, LspFileDiagnostics, LspPosition, LspRange};

    fn diagnostic(severity: LspSeverity) -> LspDiagnostic {
        let at = LspPosition {
            line: 0,
            character: 0,
        };
        LspDiagnostic {
            range: LspRange { start: at, end: at },
            severity: Some(severity),
            code: None,
            source: None,
            message: "m".to_owned(),
            related: Vec::new(),
        }
    }

    fn file(path: &str, severities: &[LspSeverity]) -> LspFileDiagnostics {
        LspFileDiagnostics {
            path: PathBuf::from(path),
            version: None,
            server_id: "s".to_owned(),
            diagnostics: severities.iter().copied().map(diagnostic).collect(),
            truncated: false,
        }
    }

    fn result(files: Vec<LspFileDiagnostics>) -> LspDiagnosticsResult {
        LspDiagnosticsResult {
            files,
            truncated: false,
            stale: false,
            servers: Vec::new(),
        }
    }

    #[test]
    fn counts_roll_up_to_every_folder_below_the_root_and_the_root_has_none() {
        use LspSeverity::{Error, Hint, Warning};
        let marks = DiagMarks::from_result(
            Path::new("/r"),
            &result(vec![
                file("/r/a/b/x.rs", &[Error, Error, Warning]),
                file("/r/a/y.rs", &[Warning]),
                file("/r/z.rs", &[Hint]),
                file("/elsewhere/q.rs", &[Error]),
            ]),
        );
        assert_eq!(
            marks.of(Path::new("/r/a/b/x.rs")),
            Some(DiagCounts {
                errors: 2,
                warnings: 1
            })
        );
        assert_eq!(
            marks.of(Path::new("/r/a/b")),
            Some(DiagCounts {
                errors: 2,
                warnings: 1
            })
        );
        assert_eq!(
            marks.of(Path::new("/r/a")),
            Some(DiagCounts {
                errors: 2,
                warnings: 2
            })
        );
        assert_eq!(marks.of(Path::new("/r")), None, "the root has no row");
        assert_eq!(marks.of(Path::new("/r/z.rs")), None, "a hint is no mark");
        assert_eq!(marks.of(Path::new("/elsewhere/q.rs")), None);
    }

    #[test]
    fn the_number_is_the_errors_else_the_warnings_and_the_words_are_plural() {
        let both = DiagCounts {
            errors: 2,
            warnings: 1,
        };
        assert_eq!((both.shown(), both.has_errors()), (2, true));
        assert_eq!(both.words(), "2 errors, 1 warning");
        let warned = DiagCounts {
            errors: 0,
            warnings: 3,
        };
        assert_eq!((warned.shown(), warned.has_errors()), (3, false));
        assert_eq!(warned.words(), "3 warnings");
        assert_eq!(
            DiagCounts {
                errors: 1,
                warnings: 0
            }
            .words(),
            "1 error"
        );
    }
}
