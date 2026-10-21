//! Diagnostics through the daemon, for `hutan diagnostics` and the MCP tool `diagnostics`
//! (protocol 0.39, `docs/lsp.md`): files are opened as documents (`doc.open`, no window
//! needed), the servers are given time to start and publish, the answer is read with
//! `lsp.diagnostics` and the documents are closed again.
//!
//! Also here: which files an agent changed (`git status` of its checkout) and which files
//! import them directly (a text search through `fs.search`: LSP is not used as a references
//! engine, `docs/spikes/2026-10-22-editor-research/lsp.md` §6).

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use hutan_client::{Client, ClientError};
use hutan_protocol::control::ErrorCode;
use hutan_protocol::fs::FsSearch;
use hutan_protocol::lsp::{
    DocOpen, DocServer, LspDiagnosticsRequest, LspDiagnosticsResult, LspFileDiagnostics,
    LspServerState, LspSeverity, LspStatusRequest,
};
use hutan_protocol::messages::Event;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;
use tokio::time::sleep;

/// How long a ready server may stay quiet before no diagnostics are taken to mean none.
const SETTLE: Duration = Duration::from_millis(1500);
/// How often the servers are looked at while waiting.
const POLL: Duration = Duration::from_millis(250);
/// Files one call opens at most.
pub const MAX_FILES: usize = 40;
/// Importers one call adds at most.
pub const MAX_IMPORTERS: usize = 30;

/// What was found.
#[derive(Debug, Clone, Default)]
pub struct Collected {
    pub files: Vec<LspFileDiagnostics>,
    /// Some server was not ready when the answer was read: it may be incomplete.
    pub stale: bool,
    pub truncated: bool,
    /// The wait ran out before the servers were done.
    pub timed_out: bool,
    /// The servers the documents went to, once each.
    pub servers: Vec<DocServer>,
    /// Why a file or the whole call got no language server.
    pub reasons: Vec<String>,
    /// Files that could not be opened, with the reason.
    pub skipped: Vec<(PathBuf, String)>,
    /// Files whose diagnostics were asked for.
    pub checked: Vec<PathBuf>,
    /// Notes for the reader (what the scope was, what is left out).
    pub notes: Vec<String>,
}

fn working(state: LspServerState) -> bool {
    matches!(
        state,
        LspServerState::Starting | LspServerState::Indexing | LspServerState::Ready
    )
}

/// Opens `paths` as documents, waits for their servers (at most `wait`), reads their diagnostics
/// and closes the documents. Files that cannot be read or are too large to send are `skipped`.
pub async fn collect(
    client: &Client,
    paths: &[PathBuf],
    min_severity: Option<LspSeverity>,
    wait: Duration,
) -> Result<Collected, ClientError> {
    let mut out = Collected::default();
    let mut events = client.events();
    let mut open: Vec<(String, PathBuf)> = Vec::new();
    let mut server_ids: BTreeSet<String> = BTreeSet::new();
    for path in paths.iter().take(MAX_FILES) {
        let file = match client.fs_read_all(path, true).await {
            Ok(file) => file,
            Err(ClientError::Remote(body)) => {
                out.skipped.push((path.clone(), body.message));
                continue;
            }
            Err(error) => return Err(error),
        };
        let opened = match client
            .doc_open(DocOpen {
                path: file.path.clone(),
                language: None,
                version: 1,
                text: file.text,
            })
            .await
        {
            Ok(opened) => opened,
            Err(ClientError::Remote(body))
                if matches!(
                    body.code,
                    ErrorCode::Unsupported | ErrorCode::Forbidden | ErrorCode::InvalidState
                ) =>
            {
                out.skipped.push((path.clone(), body.message));
                continue;
            }
            Err(error) => return Err(error),
        };
        if opened.servers.is_empty()
            && let Some(reason) = &opened.reason
            && !out.reasons.contains(reason)
        {
            out.reasons.push(reason.clone());
        }
        for server in opened.servers {
            if server_ids.insert(server.server_id.clone()) {
                out.servers.push(server);
            }
        }
        out.checked.push(file.path.clone());
        open.push((opened.doc_id, file.path));
    }
    if paths.len() > MAX_FILES {
        out.notes.push(format!(
            "{} files were given: only the first {MAX_FILES} were checked",
            paths.len()
        ));
    }
    if !server_ids.is_empty() {
        let deadline = Instant::now() + wait;
        let mut published: HashSet<PathBuf> = HashSet::new();
        let mut ready_since: Option<Instant> = None;
        loop {
            let status = client.lsp_status(LspStatusRequest::default()).await?;
            let ours: Vec<_> = status
                .servers
                .iter()
                .filter(|s| server_ids.contains(&s.server_id))
                .collect();
            if !ours.iter().any(|s| working(s.state)) {
                // Nothing can answer: not installed, not trusted, failed or stopped.
                break;
            }
            if ours
                .iter()
                .all(|s| s.state == LspServerState::Ready || !working(s.state))
            {
                let since = *ready_since.get_or_insert_with(Instant::now);
                let all_published = out.checked.iter().all(|p| published.contains(p));
                if all_published || since.elapsed() >= SETTLE {
                    break;
                }
            } else {
                ready_since = None;
            }
            if Instant::now() >= deadline {
                out.timed_out = true;
                break;
            }
            tokio::select! {
                event = events.recv() => match event {
                    Ok((_, Event::LspDiagnosticsChanged { path, .. })) => {
                        published.insert(path);
                    }
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return Err(ClientError::Disconnected),
                },
                () = sleep(POLL) => {}
            }
        }
        let result: LspDiagnosticsResult = client
            .lsp_diagnostics(LspDiagnosticsRequest {
                path: None,
                workspace_id: None,
                min_severity,
            })
            .await?;
        let checked: HashSet<&PathBuf> = out.checked.iter().collect();
        out.files = result
            .files
            .into_iter()
            .filter(|file| checked.contains(&file.path))
            .collect();
        out.truncated = result.truncated;
        out.stale = result
            .servers
            .iter()
            .any(|s| server_ids.contains(&s.server_id) && s.state != LspServerState::Ready)
            || out.timed_out;
    }
    // The documents are closed again: a server with none goes idle.
    for (doc_id, _) in open {
        let _ = client.doc_close(&doc_id).await;
    }
    Ok(out)
}

impl Collected {
    /// What a folder's diagnostics are: the servers' stored answer below `folder`.
    pub fn of_folder(folder: &Path, result: LspDiagnosticsResult) -> Self {
        let mut out = Self {
            stale: result.stale,
            truncated: result.truncated,
            ..Self::default()
        };
        out.files = result
            .files
            .into_iter()
            .filter(|file| file.path.starts_with(folder))
            .collect();
        if result.servers.is_empty() {
            out.notes.push(
                "no language server runs: `hutan diagnostics FILE` opens a file and starts the \
                 server it needs"
                    .to_owned(),
            );
        }
        out.servers = result
            .servers
            .into_iter()
            .map(|s| DocServer {
                server_id: s.server_id,
                name: s.name,
                state: s.state,
                reason: None,
                install: None,
                setting: None,
                trust_pending: false,
            })
            .collect();
        out
    }

    pub fn json(&self) -> Value {
        json!({
            "files": self.files,
            "stale": self.stale,
            "truncated": self.truncated,
            "timed_out": self.timed_out,
            "servers": self.servers,
            "reasons": self.reasons,
            "skipped": self.skipped.iter().map(|(p, why)| json!({"path": p, "reason": why})).collect::<Vec<_>>(),
            "checked": self.checked,
            "notes": self.notes,
        })
    }

    /// `path:line:col: severity: message` per diagnostic (one-based), a summary, and what the
    /// reader needs to know about servers that could not answer.
    pub fn render(&self) -> String {
        let mut lines = Vec::new();
        let (mut errors, mut warnings, mut others) = (0, 0, 0);
        for file in &self.files {
            for d in &file.diagnostics {
                match d.severity {
                    Some(LspSeverity::Error) | None => errors += 1,
                    Some(LspSeverity::Warning) => warnings += 1,
                    Some(_) => others += 1,
                }
                let origin = match (&d.source, &d.code) {
                    (Some(source), Some(code)) => format!(" [{source} {code}]"),
                    (Some(only), None) | (None, Some(only)) => format!(" [{only}]"),
                    (None, None) => String::new(),
                };
                lines.push(format!(
                    "{}:{}:{}: {}: {}{origin}",
                    file.path.display(),
                    d.range.start.line + 1,
                    d.range.start.character + 1,
                    d.severity.map_or("error", LspSeverity::as_str),
                    d.message.lines().next().unwrap_or_default(),
                ));
            }
        }
        if self.files.is_empty() && self.servers.iter().any(|s| working(s.state)) && !self.stale {
            lines.push("no problems".to_owned());
        } else if !self.files.is_empty() {
            lines.push(format!(
                "{errors} error(s), {warnings} warning(s), {others} other(s) in {} file(s)",
                self.files.len()
            ));
        }
        for server in &self.servers {
            if let Some(note) = server_note(server) {
                lines.push(note);
            }
        }
        for reason in &self.reasons {
            lines.push(format!("no language server: {reason}"));
        }
        for (path, why) in &self.skipped {
            lines.push(format!("skipped {}: {why}", path.display()));
        }
        lines.extend(self.notes.iter().cloned());
        if self.timed_out {
            lines.push("the server did not finish in time: this may be incomplete".to_owned());
        } else if self.stale && self.servers.iter().any(|s| working(s.state)) {
            lines.push("the server is still working: this may be incomplete".to_owned());
        }
        if self.truncated {
            lines.push("more diagnostics than are listed".to_owned());
        }
        lines.join("\n")
    }
}

/// What the user can do about a server that cannot answer.
fn server_note(server: &DocServer) -> Option<String> {
    match server.state {
        LspServerState::NotInstalled => Some(format!(
            "{} is not installed{}{}",
            server.name,
            server
                .install
                .as_ref()
                .map(|c| format!(": install it with `{c}`"))
                .unwrap_or_default(),
            server
                .setting
                .as_ref()
                .map(|s| format!(" (or set `{s}` in daemon.toml)"))
                .unwrap_or_default()
        )),
        LspServerState::Untrusted | LspServerState::Failed | LspServerState::Stopped => {
            Some(format!(
                "{} is {}{}",
                server.name,
                server.state.as_str(),
                server
                    .reason
                    .as_ref()
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
            ))
        }
        _ => None,
    }
}

// What an agent changed -------------------------------------------------------------------------

/// The files `git status` reports below `cwd`'s checkout (modified, added, renamed and
/// untracked, not deleted), absolute, and the checkout's root. `None` when `cwd` is in no
/// checkout or git is not there.
pub fn changed_files(cwd: &Path) -> Option<(PathBuf, Vec<PathBuf>)> {
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .ok()?;
        out.status.success().then_some(out.stdout)
    };
    let root = String::from_utf8(git(&["rev-parse", "--show-toplevel"])?).ok()?;
    let root = PathBuf::from(root.trim());
    let status = git(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
    let mut files = Vec::new();
    let mut entries = status.split(|b| *b == 0).filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let (code, name) = entry.split_at(3);
        let code = String::from_utf8_lossy(&code[..2]).into_owned();
        // A rename lists the old name as the next entry.
        if code.contains('R') || code.contains('C') {
            entries.next();
        }
        if code.contains('D') {
            continue;
        }
        let path = root.join(String::from_utf8_lossy(name).as_ref());
        if path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    Some((root, files))
}

/// The pattern and globs that find the files importing `file` directly, by its language; `None`
/// for languages without one.
pub fn import_pattern(file: &Path) -> Option<(String, &'static [&'static str])> {
    let ext = file.extension()?.to_str()?.to_ascii_lowercase();
    let stem = file.file_stem()?.to_str()?;
    let dir = file.parent()?.file_name()?.to_str()?;
    // A module named by its folder: `mod.rs`, `index.ts`, `__init__.py`.
    let name = match stem {
        "mod" | "index" | "__init__" => dir,
        "lib" | "main" if ext == "rs" => return None,
        other => other,
    };
    let name = regex_escape(name);
    Some(match ext.as_str() {
        "rs" => (
            format!(r"\b(mod|use)\s+([\w:{{}}, ]*[:{{,\s])?{name}\b"),
            &["*.rs"][..],
        ),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" => (
            format!(
                r#"(from\s+|require\(\s*|import\(\s*|import\s+)['"][^'"]*[/.]{name}(\.\w+)?['"]"#
            ),
            &[
                "*.ts", "*.tsx", "*.js", "*.jsx", "*.mjs", "*.cjs", "*.mts", "*.cts",
            ][..],
        ),
        "py" | "pyi" => (
            format!(r"^\s*(from|import)\s+[\w.]*\b{name}\b"),
            &["*.py", "*.pyi"][..],
        ),
        "go" => (format!(r#""[^"]*/{}""#, regex_escape(dir)), &["*.go"][..]),
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" => (
            format!(r#"#\s*include\s+["<][^">]*\b{name}\.\w+[">]"#),
            &["*.c", "*.h", "*.cc", "*.cpp", "*.cxx", "*.hpp", "*.hh"][..],
        ),
        _ => return None,
    })
}

fn regex_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The files below `root` that import one of `changed` directly, not counting `changed`
/// themselves: at most [`MAX_IMPORTERS`], sorted.
pub async fn importers_of(
    client: &Client,
    root: &Path,
    changed: &[PathBuf],
) -> Result<Vec<PathBuf>, ClientError> {
    let own: HashSet<&PathBuf> = changed.iter().collect();
    let mut found: BTreeSet<PathBuf> = BTreeSet::new();
    let mut seen = HashSet::new();
    for (n, file) in changed.iter().enumerate() {
        let Some((pattern, globs)) = import_pattern(file) else {
            continue;
        };
        if !seen.insert(pattern.clone()) {
            continue;
        }
        let result = match client
            .fs_search(FsSearch {
                search_id: format!("diag-{}-{n}", std::process::id()),
                root: root.to_owned(),
                pattern,
                regex: true,
                case_sensitive: Some(true),
                globs: globs.iter().map(|g| (*g).to_owned()).collect(),
                limit: Some(200),
            })
            .await
        {
            Ok(result) => result,
            // A root the agent may not search (outside the workspaces) has no importers to show.
            Err(ClientError::Remote(_)) => continue,
            Err(error) => return Err(error),
        };
        for hit in result.hits {
            let path = root.join(&hit.path);
            if !own.contains(&path) {
                found.insert(path);
            }
        }
        if found.len() >= MAX_IMPORTERS {
            break;
        }
    }
    Ok(found.into_iter().take(MAX_IMPORTERS).collect())
}

#[cfg(test)]
mod tests {
    use hutan_protocol::lsp::{LspDiagnostic, LspPosition, LspRange};

    use super::*;

    fn matches(pattern: &str, line: &str) -> bool {
        regex::Regex::new(pattern)
            .unwrap_or_else(|e| panic!("{pattern}: {e}"))
            .is_match(line)
    }

    #[test]
    fn imports_are_looked_for_by_the_modules_name_in_each_language() {
        let (rust, globs) = import_pattern(Path::new("/r/src/router.rs")).unwrap();
        assert_eq!(globs, ["*.rs"]);
        assert!(matches(&rust, "mod router;"));
        assert!(matches(&rust, "pub use crate::net::router::Route;"));
        assert!(matches(&rust, "use crate::{router, other};"));
        assert!(!matches(&rust, "let router = 1;"));
        assert!(!matches(&rust, "mod routers;"));
        assert!(!matches(&rust, "mod a_router;"));
        // A module named by its folder.
        let (modrs, _) = import_pattern(Path::new("/r/src/net/mod.rs")).unwrap();
        assert!(matches(&modrs, "pub mod net;") && !matches(&modrs, "mod mod;"));
        let (index, _) = import_pattern(Path::new("/r/web/ui/index.ts")).unwrap();
        assert!(matches(&index, r#"import { Button } from "../ui";"#));
        assert!(matches(&index, r#"const ui = require("./components/ui")"#));
        assert!(!matches(&index, r#"import x from "./uikit""#));
        let (py, _) = import_pattern(Path::new("/r/app/store.py")).unwrap();
        assert!(matches(&py, "from app.store import db"));
        assert!(matches(&py, "import store"));
        assert!(!matches(&py, "from app.restore import x"));
        let (go, _) = import_pattern(Path::new("/r/pkg/store/db.go")).unwrap();
        assert!(matches(&go, r#"	"example.com/me/pkg/store""#));
        let (c, _) = import_pattern(Path::new("/r/include/util.h")).unwrap();
        assert!(matches(&c, r#"#include "util.h""#));
        // The crate roots are imported by nobody; text files have no imports.
        assert!(import_pattern(Path::new("/r/src/lib.rs")).is_none());
        assert!(import_pattern(Path::new("/r/src/main.rs")).is_none());
        assert!(import_pattern(Path::new("/r/README.md")).is_none());
        // Names with regex characters are escaped.
        let (weird, _) = import_pattern(Path::new("/r/src/a+b.rs")).unwrap();
        assert!(matches(&weird, "mod a+b;") && !matches(&weird, "mod aab;"));
    }

    #[test]
    fn changed_files_are_the_checkouts_modified_added_and_untracked_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}: {out:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("kept.rs"), "a\n").unwrap();
        std::fs::write(dir.join("changed.rs"), "a\n").unwrap();
        std::fs::write(dir.join("gone.rs"), "a\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "x"]);
        std::fs::write(dir.join("changed.rs"), "b\n").unwrap();
        std::fs::remove_file(dir.join("gone.rs")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/new file.rs"), "c\n").unwrap();
        let (root, files) = changed_files(&dir.join("src")).unwrap();
        assert_eq!(root, dir);
        assert_eq!(files, [dir.join("changed.rs"), dir.join("src/new file.rs")]);
        assert!(changed_files(tmp.path().parent().unwrap().join("nowhere").as_path()).is_none());
    }

    #[test]
    fn diagnostics_print_one_based_positions_and_a_summary() {
        let file = LspFileDiagnostics {
            path: "/repo/src/main.rs".into(),
            version: Some(1),
            server_id: "rust-analyzer:/repo".into(),
            diagnostics: vec![LspDiagnostic {
                range: LspRange {
                    start: LspPosition {
                        line: 41,
                        character: 8,
                    },
                    end: LspPosition {
                        line: 41,
                        character: 15,
                    },
                },
                severity: Some(LspSeverity::Error),
                code: Some("E0425".into()),
                source: Some("rustc".into()),
                message: "cannot find value `missing`\nhelp: …".into(),
                related: vec![],
            }],
            truncated: false,
        };
        let outcome = Collected {
            files: vec![file],
            stale: true,
            truncated: false,
            timed_out: false,
            servers: vec![DocServer {
                server_id: "rust-analyzer:/repo".into(),
                name: "rust-analyzer".into(),
                state: LspServerState::Indexing,
                reason: None,
                install: None,
                setting: None,
                trust_pending: false,
            }],
            ..Collected::default()
        };
        let text = outcome.render();
        assert!(
            text.starts_with(
                "/repo/src/main.rs:42:9: error: cannot find value `missing` [rustc E0425]"
            ),
            "{text}"
        );
        assert!(text.contains("1 error(s), 0 warning(s), 0 other(s) in 1 file(s)"));
        assert!(text.contains("still working"), "{text}");
        let none = Collected {
            files: vec![],
            stale: false,
            servers: outcome.servers.clone(),
            ..outcome
        };
        assert!(none.render().contains("no problems"));
    }

    #[test]
    fn a_server_that_cannot_answer_is_explained_not_called_clean() {
        let outcome = Collected {
            files: vec![],
            stale: true,
            truncated: false,
            timed_out: false,
            servers: vec![DocServer {
                server_id: "x".into(),
                name: "rust-analyzer".into(),
                state: LspServerState::NotInstalled,
                reason: None,
                install: Some("rustup component add rust-analyzer".into()),
                setting: Some("lsp.servers.rust-analyzer.command".into()),
                trust_pending: false,
            }],
            ..Collected::default()
        };
        let text = outcome.render();
        assert!(!text.contains("no problems"));
        assert!(text.contains(
            "rust-analyzer is not installed: install it with `rustup component add rust-analyzer` (or set `lsp.servers.rust-analyzer.command` in daemon.toml)"
        ));
    }
}
