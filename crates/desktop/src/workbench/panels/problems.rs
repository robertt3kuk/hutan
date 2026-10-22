//! The `problems` panel (E-12a): what the language servers found, from the daemon
//! (`lsp.diagnostics {}`, the same items `hutan diagnostics` prints), grouped by file.
//!
//! - **Live.** It reads the list when it is made, when the connection comes (back), and again
//!   after every `lsp.state` and `lsp.diagnostics` notice (one read at a time: a notice during a
//!   read asks for one more afterwards), plus the header's Refresh for files only another
//!   connection has open (their notices go there).
//! - **Filter.** `panels.problems.severity` (the header's control cycles it) is the least
//!   serious severity listed; the daemon filters (`min_severity`), so its 5 000-item bound
//!   counts only what shows.
//! - **No colour-only cue.** Every row carries its severity's glyph shape (the gutter's).
//! - **Keyboard.** ↑↓ move between diagnostics, Return opens the file with the caret on the
//!   diagnostic and moves the keyboard there; a click does the same.
//! - The empty states are text only.
//! - **Servers.** Under the list, one line per language server the answer comes from, with
//!   its state (`lsp.status` adds the reason and the languages): `Rust · failed: <reason>`
//!   with **Restart** (`lsp.restart`), `Plain Text · ready`. "Still starting" shows only while
//!   one is starting or indexing.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, Render, ScrollStrategy, SharedString, StatefulInteractiveElement,
    Styled, Task, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use hutan_client::{ConnectionState, Notice};
use hutan_code_view::Severity;
use hutan_protocol::lsp::{
    LspDiagnosticsRequest, LspDiagnosticsResult, LspServerInfo, LspServerState, LspSeverity,
    LspStatusRequest,
};
use tokio::sync::broadcast::error::RecvError;

use super::common::panel_header_with;
use crate::icons::Icon;
use crate::settings::{ProblemsPanelSettings, ProblemsSeverity};
use crate::theme::DensityExt as _;
use crate::workbench::Region;
use crate::workbench::actions::{OpenAtPosition, PositionTarget};
use crate::workbench::lsp::{languages_of, severity_of};
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::{TextTooltip, UiStyle};
use crate::workbench::ui;

/// Height of every row (file headers and diagnostics alike: the list is uniform).
const ROW_HEIGHT: f32 = 24.0;
/// The note while a server's diagnostics may still be on their way.
pub const STILL_STARTING: &str =
    "Some language servers are still starting: the list may be incomplete.";

/// One diagnostic as the panel lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Absolute.
    pub path: String,
    /// Zero-based.
    pub line: u32,
    /// Zero-based, UTF-16.
    pub character: u32,
    pub severity: Severity,
    pub message: String,
    /// `source(code)` when the server said.
    pub origin: Option<String>,
}

impl Problem {
    /// `path:line:col` one-based, the way `hutan diagnostics` prints it.
    pub fn place(&self) -> String {
        format!("{}:{}:{}", self.path, self.line + 1, self.character + 1)
    }
}

/// A row of the list: a file, then its diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProblemRow {
    File {
        path: String,
        errors: usize,
        warnings: usize,
        others: usize,
    },
    /// `index` counts the diagnostics only (the keyboard's selection, [`Region::ProblemsRow`]).
    Problem { index: usize },
}

/// What the panel knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problems {
    /// Not connected, or the daemon has no language servers.
    Idle,
    Loading,
    Ready {
        problems: Vec<Problem>,
        rows: Vec<ProblemRow>,
        /// The servers the answer comes from, one line each.
        servers: Vec<ServerLine>,
        truncated: bool,
    },
    Failed(String),
}

/// A language server as the panel's footer shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerLine {
    pub server_id: String,
    /// Its languages by name (`Rust`, `C, C++`), else its name when `lsp.status` did not say.
    pub label: String,
    /// Its name in `daemon.toml` (`rust-analyzer`).
    pub name: String,
    pub root: Option<PathBuf>,
    pub state: LspServerState,
    pub reason: Option<String>,
    pub install: Option<String>,
}

impl ServerLine {
    /// `failed: <reason>`, `ready`, `not installed: <command>`.
    pub fn state_text(&self) -> String {
        let with = |state: &str, detail: Option<&String>| match detail {
            Some(detail) if !detail.is_empty() => format!("{state}: {detail}"),
            _ => state.to_owned(),
        };
        match self.state {
            LspServerState::Starting => "starting".to_owned(),
            LspServerState::Indexing => "indexing".to_owned(),
            LspServerState::Ready => "ready".to_owned(),
            LspServerState::Failed => with("failed", self.reason.as_ref()),
            LspServerState::Stopped => with("stopped", self.reason.as_ref()),
            LspServerState::NotInstalled => with(
                "not installed",
                self.install.as_ref().or(self.reason.as_ref()),
            ),
            LspServerState::Untrusted => "folder not trusted".to_owned(),
        }
    }

    /// `Rust · failed: <reason>`.
    pub fn text(&self) -> String {
        format!("{} · {}", self.label, self.state_text())
    }

    /// Whether the line offers Restart: the server ended (it crashed too often, or stopped).
    pub fn restartable(&self) -> bool {
        matches!(self.state, LspServerState::Failed | LspServerState::Stopped)
    }

    /// Whether its diagnostics may still be on their way.
    pub fn busy(&self) -> bool {
        matches!(
            self.state,
            LspServerState::Starting | LspServerState::Indexing
        )
    }
}

/// The lines of the servers an answer comes from, with what `lsp.status` adds (`status`: its
/// servers; empty when it could not be read), by label then name.
pub fn server_lines(result: &LspDiagnosticsResult, status: &[LspServerInfo]) -> Vec<ServerLine> {
    let mut lines: Vec<ServerLine> = result
        .servers
        .iter()
        .map(|brief| {
            let info = status.iter().find(|s| s.server_id == brief.server_id);
            let label = info
                .map(|i| languages_of(&i.languages).join(", "))
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| brief.name.clone());
            ServerLine {
                server_id: brief.server_id.clone(),
                label,
                name: brief.name.clone(),
                root: info.map(|i| i.root.clone()),
                // The answer's state is the one its diagnostics belong to.
                state: brief.state,
                reason: info.and_then(|i| i.reason.clone()),
                install: info.and_then(|i| i.install.clone()),
            }
        })
        .collect();
    lines.sort_by(|a, b| (&a.label, &a.name, &a.server_id).cmp(&(&b.label, &b.name, &b.server_id)));
    lines
}

/// The rows of an answer: files by path (two servers publishing for one file make one group),
/// each file's diagnostics by position, the most serious first among equals.
pub fn problems_of(result: &LspDiagnosticsResult) -> (Vec<Problem>, Vec<ProblemRow>) {
    let mut by_file: std::collections::BTreeMap<String, Vec<Problem>> = Default::default();
    for file in &result.files {
        let path = file.path.display().to_string();
        by_file
            .entry(path.clone())
            .or_default()
            .extend(file.diagnostics.iter().map(|d| Problem {
                path: path.clone(),
                line: d.range.start.line,
                character: d.range.start.character,
                severity: severity_of(d.severity),
                message: d.message.clone(),
                origin: match (&d.source, &d.code) {
                    (Some(source), Some(code)) => Some(format!("{source}({code})")),
                    (Some(source), None) => Some(source.clone()),
                    (None, Some(code)) => Some(code.clone()),
                    (None, None) => None,
                },
            }));
    }
    let mut problems = Vec::new();
    let mut rows = Vec::new();
    for (path, mut items) in by_file {
        if items.is_empty() {
            continue;
        }
        items.sort_by(|a, b| {
            (a.line, a.character, a.severity).cmp(&(b.line, b.character, b.severity))
        });
        let count = |f: fn(Severity) -> bool| items.iter().filter(|p| f(p.severity)).count();
        rows.push(ProblemRow::File {
            path,
            errors: count(|s| s == Severity::Error),
            warnings: count(|s| s == Severity::Warning),
            others: count(|s| matches!(s, Severity::Info | Severity::Hint)),
        });
        for item in items {
            rows.push(ProblemRow::Problem {
                index: problems.len(),
            });
            problems.push(item);
        }
    }
    (problems, rows)
}

fn min_severity(filter: ProblemsSeverity) -> Option<LspSeverity> {
    match filter {
        ProblemsSeverity::All => None,
        ProblemsSeverity::Warning => Some(LspSeverity::Warning),
        ProblemsSeverity::Error => Some(LspSeverity::Error),
    }
}

/// What the header's filter control says.
pub fn filter_label(filter: ProblemsSeverity) -> &'static str {
    match filter {
        ProblemsSeverity::All => "All",
        ProblemsSeverity::Warning => "Errors and warnings",
        ProblemsSeverity::Error => "Errors",
    }
}

/// The Problems panel.
pub struct ProblemsPanel {
    env: PanelEnv,
    focus: FocusHandle,
    problems: Problems,
    /// The diagnostic the keyboard is on (an index into the problems).
    selected: Option<usize>,
    /// The keyboard came while the list was not read yet: its first row is selected once it is.
    select_first: bool,
    scroll: UniformListScrollHandle,
    /// The connection state and filter the list was read for.
    asked: Option<(ConnectionState, ProblemsSeverity)>,
    loading: bool,
    reload_again: bool,
    generation: u64,
    task: Option<Task<()>>,
    listener: Option<Task<()>>,
    /// Answers taken so far (tests wait on it).
    reads: usize,
    /// Servers whose `lsp.restart` is on its way.
    restarting: HashSet<String>,
    /// Why the last Restart did not go through, until the next one.
    restart_failed: Option<String>,
}

impl std::fmt::Debug for ProblemsPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProblemsPanel")
            .field("problems", &self.problems)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl ProblemsPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        cx.observe(&env.sessions, |this, _, cx| this.sync(cx))
            .detach();
        let mut panel = Self {
            env,
            focus: cx.focus_handle(),
            problems: Problems::Idle,
            selected: None,
            select_first: false,
            scroll: UniformListScrollHandle::new(),
            asked: None,
            loading: false,
            reload_again: false,
            generation: 0,
            task: None,
            listener: None,
            reads: 0,
            restarting: HashSet::new(),
            restart_failed: None,
        };
        panel.sync(cx);
        panel
    }

    // ---- read access (also for tests) -----------------------------------------------------

    pub fn problems(&self) -> &Problems {
        &self.problems
    }

    /// The listed diagnostics, top to bottom.
    pub fn listed(&self) -> &[Problem] {
        match &self.problems {
            Problems::Ready { problems, .. } => problems,
            _ => &[],
        }
    }

    pub fn rows(&self) -> &[ProblemRow] {
        match &self.problems {
            Problems::Ready { rows, .. } => rows,
            _ => &[],
        }
    }

    /// The server lines under the list, top to bottom.
    pub fn servers(&self) -> &[ServerLine] {
        match &self.problems {
            Problems::Ready { servers, .. } => servers,
            _ => &[],
        }
    }

    /// The "still starting" note shows: some server's diagnostics may still be on their way.
    pub fn still_starting(&self) -> bool {
        self.servers().iter().any(ServerLine::busy)
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// `lsp.diagnostics` answers taken so far.
    pub fn reads(&self) -> usize {
        self.reads
    }

    /// No read is in flight or wanted.
    pub fn settled(&self) -> bool {
        !self.loading && !self.reload_again
    }

    // ---- reading --------------------------------------------------------------------------

    fn filter(&self) -> ProblemsSeverity {
        self.env.settings.get::<ProblemsPanelSettings>().severity
    }

    /// Reads again when the connection or the filter is not the one listed.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let (state, supported) = {
            let store = self.env.sessions.read(cx);
            (store.connection_state(), store.connection().supports_lsp())
        };
        let wanted = (state, self.filter());
        if self.asked == Some(wanted) {
            return;
        }
        self.asked = Some(wanted);
        if state == ConnectionState::Connected && supported {
            self.listen(cx);
            self.load(cx);
        } else {
            self.generation += 1;
            self.task = None;
            self.loading = false;
            self.reload_again = false;
            self.problems = Problems::Idle;
            self.selected = None;
            cx.notify();
        }
    }

    /// A trigger nobody asked for: waits for a read in flight and reads once more afterwards.
    fn refresh_soon(&mut self, cx: &mut Context<Self>) {
        if matches!(self.problems, Problems::Idle) {
            return;
        }
        if self.loading {
            self.reload_again = true;
        } else {
            self.load(cx);
        }
    }

    /// Reads the list again (the Refresh button).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.problems, Problems::Idle) {
            self.load(cx);
        }
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.loading = true;
        self.reload_again = false;
        if !matches!(self.problems, Problems::Ready { .. }) {
            self.problems = Problems::Loading;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        let request = LspDiagnosticsRequest {
            min_severity: min_severity(self.filter()),
            ..LspDiagnosticsRequest::default()
        };
        // The servers' reasons and languages come from `lsp.status`; without it the lines
        // still show each server's state, by name.
        let call = cx.background_spawn(async move {
            let answer = connection.lsp_diagnostics(request).await;
            let status = match &answer {
                Ok(result) if !result.servers.is_empty() => connection
                    .lsp_status(LspStatusRequest::default())
                    .await
                    .map(|s| s.servers)
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            (answer, status)
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let (answer, status) = call.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                this.reads += 1;
                match answer {
                    Ok(result) => {
                        let (problems, rows) = problems_of(&result);
                        // A selection (the keyboard or a click made it) stays in the list.
                        this.selected = this
                            .selected
                            .map(|i| i.min(problems.len().saturating_sub(1)))
                            .filter(|_| !problems.is_empty());
                        if this.select_first && !problems.is_empty() {
                            this.select_first = false;
                            this.selected = Some(0);
                        }
                        this.problems = Problems::Ready {
                            problems,
                            rows,
                            servers: server_lines(&result, &status),
                            truncated: result.truncated,
                        };
                    }
                    // The list on screen stays until a read works.
                    Err(error) if matches!(this.problems, Problems::Ready { .. }) => {
                        tracing::debug!(%error, "lsp.diagnostics failed");
                    }
                    Err(error) => {
                        this.problems =
                            Problems::Failed(crate::workbench::files::error_text(&error));
                    }
                }
                if std::mem::take(&mut this.reload_again) {
                    this.load(cx);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Restart on a server line: `lsp.restart`; its `lsp.state` notices read the list again.
    fn restart(&mut self, server_id: String, cx: &mut Context<Self>) {
        if !self.restarting.insert(server_id.clone()) {
            return;
        }
        self.restart_failed = None;
        let connection = self.env.sessions.read(cx).connection().clone();
        let id = server_id.clone();
        let call = cx.background_spawn(async move { connection.lsp_restart(&id).await });
        cx.spawn(async move |this, cx| {
            let result = call.await;
            this.update(cx, |this, cx| {
                this.restarting.remove(&server_id);
                if let Err(error) = result {
                    this.restart_failed = Some(format!(
                        "Could not restart {server_id}: {}",
                        crate::workbench::files::error_text(&error)
                    ));
                }
                this.refresh_soon(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Takes the connection's language-server notices.
    fn listen(&mut self, cx: &mut Context<Self>) {
        if self.listener.is_some() {
            return;
        }
        let mut notices = self.env.sessions.read(cx).connection().notices();
        self.listener = Some(cx.spawn(async move |this, cx| {
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
                if this.update(cx, |this, cx| this.refresh_soon(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    // ---- the keyboard ---------------------------------------------------------------------

    /// Puts the keyboard in the list, on its first diagnostic when none is selected.
    pub fn focus_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if self.selected.is_none() {
            if self.listed().is_empty() {
                self.select_first = true;
            } else {
                self.select(0);
            }
        }
        cx.notify();
    }

    fn select(&mut self, index: usize) {
        self.selected = Some(index);
        if let Some(row) = self
            .rows()
            .iter()
            .position(|r| *r == ProblemRow::Problem { index })
        {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
    }

    /// Opens diagnostic `index` in the focused pane, the caret on it.
    fn open(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(problem) = self.listed().get(index).cloned() else {
            return;
        };
        self.select(index);
        window.dispatch_action(
            Box::new(OpenAtPosition(PositionTarget {
                path: problem.path,
                line: problem.line,
                character: problem.character,
            })),
            cx,
        );
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let keystroke = &event.keystroke;
        let mods = &keystroke.modifiers;
        if mods.platform || mods.control || mods.alt || mods.shift {
            return false;
        }
        let count = self.listed().len();
        if count == 0 {
            return false;
        }
        let current = self.selected;
        match keystroke.key.as_str() {
            "down" => self.select(current.map_or(0, |i| (i + 1).min(count - 1))),
            "up" => self.select(current.map_or(0, |i| i.saturating_sub(1))),
            "enter" => {
                let index = current.unwrap_or(0);
                self.open(index, window, cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    fn cycle_filter(&mut self, cx: &mut Context<Self>) {
        let next = match self.filter() {
            ProblemsSeverity::All => "warning",
            ProblemsSeverity::Warning => "error",
            ProblemsSeverity::Error => "all",
        };
        if let Err(error) = self.env.settings.write_user(
            "panels.problems.severity",
            toml::Value::String(next.to_owned()),
        ) {
            tracing::warn!("cannot save panels.problems.severity: {error}");
        }
        self.sync(cx);
    }

    // ---- rendering ------------------------------------------------------------------------

    /// The footer: one line per server (its languages, its state, Restart when it ended), and
    /// why the last Restart failed.
    fn render_servers(
        &self,
        theme: &crate::theme::Theme,
        tokens: &crate::theme::Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let servers = self.servers();
        if servers.is_empty() && self.restart_failed.is_none() {
            return None;
        }
        let lines = servers.iter().enumerate().map(|(index, line)| {
            let tone = match line.state {
                LspServerState::Failed => theme.danger,
                LspServerState::NotInstalled | LspServerState::Untrusted => theme.warning,
                _ => theme.muted,
            };
            let mut tip = format!("{}: {}", line.name, line.state_text());
            if let Some(root) = &line.root {
                tip.push('\n');
                tip.push_str(&crate::workbench::workspaces::tilde(root));
            }
            let restart = line.restartable().then(|| {
                let busy = self.restarting.contains(&line.server_id);
                let id = line.server_id.clone();
                ui::Button::new(
                    SharedString::from(format!("problems-restart-{index}")),
                    "Restart",
                    theme,
                    tokens,
                )
                .compact()
                .ghost()
                .loading(busy)
                .tooltip("Start this language server again")
                .on_click(cx.listener(move |this, _, _, cx| this.restart(id.clone(), cx)))
                .child(probe(
                    &self.env.regions,
                    Region::ProblemsServerRestart(index),
                ))
            });
            div()
                .id(SharedString::from(format!("problems-server-{index}")))
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .min_h(px(ROW_HEIGHT))
                .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone().into())).into())
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.fg)
                        .child(SharedString::from(line.label.clone())),
                )
                .child(div().flex_none().text_color(theme.faint).child("·"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(tone)
                        .child(SharedString::from(line.state_text())),
                )
                .children(restart)
        });
        let failed = self.restart_failed.clone().map(|text| {
            div()
                .text_color(theme.danger)
                .child(SharedString::from(text))
        });
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .px(px(tokens.spacing * 3.0))
                .py(px(tokens.spacing))
                .border_t_1()
                .border_color(theme.border)
                .text_size(px(tokens.secondary_font_size))
                .children(lines)
                .children(failed)
                .into_any_element(),
        )
    }

    fn render_rows(
        &mut self,
        range: std::ops::Range<usize>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let hover = theme.sidebar_hover;
        let selected_bg = theme.sidebar_accent;
        let focused = self.focus.contains_focused(window, cx);
        let rows: Vec<ProblemRow> = range.filter_map(|i| self.rows().get(i).cloned()).collect();
        rows.into_iter()
            .map(|row| match row {
                ProblemRow::File {
                    path,
                    errors,
                    warnings,
                    others,
                } => {
                    let name = Path::new(&path)
                        .file_name()
                        .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
                    let dir = Path::new(&path)
                        .parent()
                        .map(crate::workbench::workspaces::tilde)
                        .unwrap_or_default();
                    let mut counts = Vec::new();
                    for (severity, n) in [
                        (Severity::Error, errors),
                        (Severity::Warning, warnings),
                        (Severity::Info, others),
                    ] {
                        if n > 0 {
                            counts.push(format!("{} {n}", severity.symbol()));
                        }
                    }
                    div()
                        .w_full()
                        .h(px(ROW_HEIGHT))
                        .flex()
                        .items_center()
                        .gap(px(tokens.spacing * 2.0))
                        .px(px(tokens.spacing * 3.0))
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.fg)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(SharedString::from(name)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.muted)
                                .child(SharedString::from(dir)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.muted)
                                .child(SharedString::from(counts.join("  "))),
                        )
                        .into_any_element()
                }
                ProblemRow::Problem { index } => {
                    let Some(problem) = self.listed().get(index).cloned() else {
                        return div().h(px(ROW_HEIGHT)).into_any_element();
                    };
                    let color = match problem.severity {
                        Severity::Error => theme.danger,
                        Severity::Warning => theme.warning,
                        Severity::Info | Severity::Hint => theme.accent,
                    };
                    let is_selected = self.selected == Some(index);
                    let first_line = problem
                        .message
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    let tip = match &problem.origin {
                        Some(origin) => format!(
                            "{} {}: {}\n{origin}",
                            problem.severity.symbol(),
                            problem.severity.label(),
                            problem.message
                        ),
                        None => format!(
                            "{} {}: {}",
                            problem.severity.symbol(),
                            problem.severity.label(),
                            problem.message
                        ),
                    };
                    let place = format!("{}:{}", problem.line + 1, problem.character + 1);
                    let row = div()
                        .id(SharedString::from(format!("problem-{index}")))
                        .relative()
                        .w_full()
                        .h(px(ROW_HEIGHT))
                        .flex()
                        .items_center()
                        .gap(px(tokens.spacing * 2.0))
                        .pl(px(tokens.spacing * 3.0))
                        .pr(px(tokens.spacing))
                        .rounded(px(tokens.radius))
                        .cursor_pointer()
                        .when(is_selected && focused, |s| s.bg(selected_bg))
                        .hover(move |s| s.bg(hover))
                        .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone().into())).into())
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open(index, window, cx);
                        }))
                        .child(
                            div()
                                .flex_none()
                                .w(px(14.0))
                                .text_color(color)
                                .child(problem.severity.symbol()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.fg)
                                .child(SharedString::from(first_line)),
                        )
                        .children(problem.origin.clone().map(|origin| {
                            div()
                                .flex_none()
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.faint)
                                .child(SharedString::from(origin))
                        }))
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.muted)
                                .child(SharedString::from(place)),
                        )
                        .child(probe(&self.env.regions, Region::ProblemsRow(index)));
                    // The row's hover fill stops short of the panel's edges.
                    div()
                        .w_full()
                        .px(px(tokens.spacing * 2.0))
                        .child(row)
                        .into_any_element()
                }
            })
            .collect()
    }
}

impl Focusable for ProblemsPanel {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ProblemsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.env.regions.rendered(cx.entity_id());
        // The filter may have changed in Settings (or settings.toml).
        self.sync(cx);
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let filter = self.filter();
        let severity = ui::Button::new("problems-severity", filter_label(filter), &theme, &tokens)
            .compact()
            .ghost()
            .tooltip("Which diagnostics to list (click to change)")
            .on_click(cx.listener(|this, _, _, cx| this.cycle_filter(cx)))
            .child(probe(&self.env.regions, Region::Problems("severity")))
            .into_any_element();
        let refresh = ui::IconButton::new(
            "problems-refresh",
            Icon::ActionRefresh,
            "Read the problems again",
            &theme,
            &tokens,
        )
        .compact()
        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
        .child(probe(&self.env.regions, Region::Problems("refresh")))
        .into_any_element();
        let header = panel_header_with("Problems", "panels.problems", vec![severity, refresh], cx);

        let starting = self.still_starting();
        let (summary, note) = match &self.problems {
            Problems::Ready {
                problems,
                rows,
                truncated,
                ..
            } if !problems.is_empty() => {
                let files = rows
                    .iter()
                    .filter(|r| matches!(r, ProblemRow::File { .. }))
                    .count();
                let count = |s: Severity| problems.iter().filter(|p| p.severity == s).count();
                let mut parts = Vec::new();
                for (severity, one, many) in [
                    (Severity::Error, "error", "errors"),
                    (Severity::Warning, "warning", "warnings"),
                    (Severity::Info, "info", "info"),
                    (Severity::Hint, "hint", "hints"),
                ] {
                    let n = count(severity);
                    if n > 0 {
                        parts.push(format!("{n} {}", if n == 1 { one } else { many }));
                    }
                }
                let summary = format!(
                    "{} in {files} {}",
                    parts.join(", "),
                    if files == 1 { "file" } else { "files" }
                );
                let note = if *truncated {
                    Some("Only the first 5 000 are listed.")
                } else if starting {
                    Some(STILL_STARTING)
                } else {
                    None
                };
                (Some(summary), note)
            }
            Problems::Ready { .. } if starting => (None, Some(STILL_STARTING)),
            _ => (None, None),
        };
        let servers = self.render_servers(&theme, &tokens, cx);

        let count = self.rows().len();
        let body: AnyElement = match &self.problems {
            Problems::Ready { .. } if count > 0 => uniform_list(
                "problems-list",
                count,
                cx.processor(|this, range, window, cx| this.render_rows(range, window, cx)),
            )
            .track_scroll(&self.scroll)
            .flex_1()
            .min_h_0()
            .into_any_element(),
            Problems::Ready { .. } => empty_state(
                "No problems",
                match filter {
                    ProblemsSeverity::All => {
                        "What language servers find in your files is listed here."
                    }
                    ProblemsSeverity::Warning => "No errors or warnings.",
                    ProblemsSeverity::Error => "No errors.",
                },
                &theme,
                &tokens,
            ),
            Problems::Loading => empty_state("Reading problems…", "", &theme, &tokens),
            Problems::Failed(error) => {
                empty_state_owned("Cannot read the problems", error.clone(), &theme, &tokens)
            }
            Problems::Idle => {
                let connected =
                    self.env.sessions.read(cx).connection_state() == ConnectionState::Connected;
                if connected {
                    empty_state(
                        "No language servers",
                        "This hutand does not run language servers.",
                        &theme,
                        &tokens,
                    )
                } else {
                    empty_state(
                        "Not connected",
                        "Problems are listed once hutand is connected.",
                        &theme,
                        &tokens,
                    )
                }
            }
        };

        div()
            .id("problems-panel")
            .key_context("ProblemsPanel")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.key(event, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(header)
            .children(summary.map(|summary| {
                div()
                    .flex_none()
                    .px(px(tokens.spacing * 3.0))
                    .pb(px(tokens.spacing))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .truncate()
                    .child(SharedString::from(summary))
            }))
            .children(note.map(|note| {
                div()
                    .flex_none()
                    .px(px(tokens.spacing * 3.0))
                    .pb(px(tokens.spacing))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.faint)
                    .child(note)
            }))
            .child(body)
            .children(servers)
    }
}

fn empty_state(
    title: &'static str,
    detail: &'static str,
    theme: &crate::theme::Theme,
    tokens: &crate::theme::Tokens,
) -> AnyElement {
    empty_state_owned(title, detail.to_owned(), theme, tokens)
}

fn empty_state_owned(
    title: &'static str,
    detail: String,
    theme: &crate::theme::Theme,
    tokens: &crate::theme::Tokens,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(tokens.spacing))
        .px(px(tokens.spacing * 3.0))
        .py(px(tokens.spacing * 2.0))
        .text_size(px(tokens.font_size))
        .child(div().text_color(theme.muted).child(title))
        .when(!detail.is_empty(), |s| {
            s.child(
                div()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.faint)
                    .child(SharedString::from(detail)),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_protocol::lsp::{LspDiagnostic, LspFileDiagnostics, LspPosition, LspRange};

    fn diagnostic(
        line: u32,
        character: u32,
        severity: LspSeverity,
        message: &str,
    ) -> LspDiagnostic {
        let at = LspPosition { line, character };
        LspDiagnostic {
            range: LspRange { start: at, end: at },
            severity: Some(severity),
            code: None,
            source: Some("fake".into()),
            message: message.into(),
            related: Vec::new(),
        }
    }

    #[test]
    fn rows_group_by_file_and_order_by_position() {
        let result = LspDiagnosticsResult {
            files: vec![
                LspFileDiagnostics {
                    path: "/w/a.rs".into(),
                    version: None,
                    server_id: "s".into(),
                    diagnostics: vec![
                        diagnostic(4, 0, LspSeverity::Warning, "unused"),
                        diagnostic(1, 2, LspSeverity::Error, "found error"),
                    ],
                    truncated: false,
                },
                LspFileDiagnostics {
                    path: "/w/b.rs".into(),
                    version: None,
                    server_id: "s".into(),
                    diagnostics: vec![diagnostic(0, 0, LspSeverity::Hint, "rename")],
                    truncated: false,
                },
            ],
            truncated: false,
            stale: false,
            servers: Vec::new(),
        };
        let (problems, rows) = problems_of(&result);
        assert_eq!(
            problems.iter().map(Problem::place).collect::<Vec<_>>(),
            ["/w/a.rs:2:3", "/w/a.rs:5:1", "/w/b.rs:1:1"]
        );
        assert_eq!(problems[0].origin.as_deref(), Some("fake"));
        assert_eq!(
            rows,
            [
                ProblemRow::File {
                    path: "/w/a.rs".into(),
                    errors: 1,
                    warnings: 1,
                    others: 0
                },
                ProblemRow::Problem { index: 0 },
                ProblemRow::Problem { index: 1 },
                ProblemRow::File {
                    path: "/w/b.rs".into(),
                    errors: 0,
                    warnings: 0,
                    others: 1
                },
                ProblemRow::Problem { index: 2 },
            ]
        );
    }

    #[test]
    fn the_filter_asks_the_daemon_for_the_least_serious_severity() {
        assert_eq!(min_severity(ProblemsSeverity::All), None);
        assert_eq!(
            min_severity(ProblemsSeverity::Warning),
            Some(LspSeverity::Warning)
        );
        assert_eq!(
            min_severity(ProblemsSeverity::Error),
            Some(LspSeverity::Error)
        );
    }

    #[test]
    fn server_lines_name_languages_and_say_the_state_with_its_reason() {
        use hutan_protocol::lsp::LspServerBrief;
        let brief = |id: &str, name: &str, state| LspServerBrief {
            server_id: id.into(),
            name: name.into(),
            state,
        };
        let info = |id: &str, name: &str, languages: &[&str], reason: Option<&str>| LspServerInfo {
            server_id: id.into(),
            name: name.into(),
            root: "/w".into(),
            languages: languages.iter().map(|l| (*l).to_owned()).collect(),
            state: LspServerState::Stopped,
            reason: reason.map(str::to_owned),
            pid: None,
            restarts: 0,
            rss_bytes: None,
            documents: 1,
            install: None,
            setting: None,
            trust_pending: false,
            capabilities: Vec::new(),
            completion_triggers: Vec::new(),
            signature_triggers: Vec::new(),
        };
        let result = LspDiagnosticsResult {
            files: Vec::new(),
            truncated: false,
            stale: true,
            servers: vec![
                brief("ra", "rust-analyzer", LspServerState::Failed),
                brief("fake", "fake", LspServerState::Ready),
                brief("clangd", "clangd", LspServerState::Indexing),
            ],
        };
        let status = [
            info("ra", "rust-analyzer", &["rust"], Some("exited with code 1")),
            info("fake", "fake", &["fake"], Some("idle")),
        ];
        let lines = server_lines(&result, &status);
        assert_eq!(
            lines.iter().map(ServerLine::text).collect::<Vec<_>>(),
            [
                "Plain Text · ready",
                "Rust · failed: exited with code 1",
                // Not in `lsp.status`: its name stands in for its languages.
                "clangd · indexing",
            ]
        );
        assert_eq!(
            lines
                .iter()
                .map(ServerLine::restartable)
                .collect::<Vec<_>>(),
            [false, true, false]
        );
        assert!(lines[2].busy() && !lines[0].busy() && !lines[1].busy());
    }
}
