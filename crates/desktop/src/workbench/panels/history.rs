//! The `history` panel: search over the session history index (protocol 0.8, docs/history-index.md)
//! — Hutan terminal blocks and the transcripts of Claude Code, Codex and OMP.
//!
//! While indexing is off the panel explains what turning it on reads and offers to do it. While
//! it is on: a debounced search field, filter chips, one row per matching session with its best
//! snippet, and a footer with the index's size and per-source state, **Clear** and **Rebuild**
//! (both ask first). The panel only sends `history.*` requests; the actions that start sessions
//! (`ResumeHistoryHit`, `OpenHistoryHit`, `RevealHistoryFile`) are the workbench's.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, Context, FocusHandle, Focusable, HighlightStyle, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, StyledText, Task, Window, div, prelude::*, px,
};
use hutan_client::{ClientError, ConnectionState};
use hutan_core::SessionId;
use hutan_protocol::ErrorCode;
use hutan_protocol::history::{HistoryHit, HistorySource, HistoryStatusInfo};

use super::common::{age_label, panel_header, time_tooltip};
use super::history_model::{
    DEBOUNCE, Filters, HistoryResume, HutanTarget, PANEL_LIMIT, ROLES, SOURCES, format_bytes,
    hit_title, hutan_target, parse_snippet, resume_blocker, role_label, shell_join, short_path,
    source_icon, source_label, transcript_path,
};
use crate::settings::{HistoryRange, HistoryScope, HistorySettings};
use crate::theme::{DensityExt as _, Theme, Tokens};
use crate::workbench::Region;
use crate::workbench::actions::{
    CopyHistoryCommand, OpenHistoryHit, OpenSettingsFor, ResumeHistoryHit, RevealHistoryFile,
};
use crate::workbench::chat::vim::Outcome;
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::UiStyle;
use crate::workbench::text_field::FieldRef;
use crate::workbench::ui;
use crate::workbench::vim_ui::{self, FieldEdit, VimConfig};

/// How often the index status is read while indexing settles after Enable or Rebuild.
const SETTLE_POLL: Duration = Duration::from_millis(500);
/// The polls stop after the row count has not changed for this long once it is above zero …
const SETTLE_QUIET: Duration = Duration::from_secs(2);
/// … or this long while it is still zero (a first scan of large directories takes a while).
const SETTLE_QUIET_EMPTY: Duration = Duration::from_secs(10);
/// Longest a settle poll runs.
const SETTLE_MAX: Duration = Duration::from_secs(60);

/// What the panel knows about the index.
#[derive(Debug, Clone, PartialEq)]
enum Index {
    /// Not asked yet, or the daemon is not reachable.
    Unknown,
    /// The connected daemon predates `history.*` (protocol 0.8).
    Unsupported,
    Known(HistoryStatusInfo),
}

#[derive(Debug, Clone, PartialEq)]
enum Results {
    /// No query.
    Idle,
    Hits(Vec<HistoryHit>),
    Failed(String),
}

/// A destructive action waiting for its confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    Clear,
    Rebuild,
}

pub struct HistoryPanel {
    env: PanelEnv,
    search_focus: FocusHandle,
    /// Drawn since the age timer last looked.
    drawn: bool,
    query: String,
    /// Vim state of the search field (`editor.vim_mode`).
    query_edit: FieldEdit,
    filters: Filters,
    index: Index,
    results: Results,
    /// A search is on its way (the old rows stay until it answers).
    searching: bool,
    selected: Option<usize>,
    /// The hit to select once the results arrive (⌘J `?` fallback).
    pick: Option<(HistorySource, String)>,
    confirm: Option<Confirm>,
    /// What a running request is called ("Enabling…"); the buttons wait for it.
    busy: Option<&'static str>,
    /// The last failed request, shown in the footer.
    error: Option<String>,
    was_connected: bool,
    /// Focus the search field once it is on screen.
    focus_wanted: bool,
    search_task: Option<Task<()>>,
    status_task: Option<Task<()>>,
    settle_task: Option<Task<()>>,
    _ages: Task<()>,
}

impl std::fmt::Debug for HistoryPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryPanel")
            .field("query", &self.query)
            .field("index", &self.index)
            .finish_non_exhaustive()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn is_disabled(error: &ClientError) -> bool {
    matches!(error, ClientError::Remote(body) if body.code == ErrorCode::InvalidState)
}

impl HistoryPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        cx.observe(&env.sessions, |this, sessions, cx| {
            let connected = sessions.read(cx).connection_state() == ConnectionState::Connected;
            if connected && !this.was_connected {
                this.refresh_status(cx);
            }
            this.was_connected = connected;
            cx.notify();
        })
        .detach();
        let ages = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(30))
                    .await;
                // The ages of a panel nobody saw since the last look are not worth a frame.
                let alive = this.update(cx, |this, cx| {
                    if std::mem::take(&mut this.drawn) {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let filters = Filters::from_settings(&env.settings.get::<HistorySettings>());
        let was_connected = env.sessions.read(cx).connection_state() == ConnectionState::Connected;
        let mut panel = Self {
            env,
            search_focus: cx.focus_handle(),
            drawn: false,
            query: String::new(),
            query_edit: FieldEdit::new(),
            filters,
            index: Index::Unknown,
            results: Results::Idle,
            searching: false,
            selected: None,
            pick: None,
            confirm: None,
            busy: None,
            error: None,
            was_connected,
            focus_wanted: false,
            search_task: None,
            status_task: None,
            settle_task: None,
            _ages: ages,
        };
        if was_connected {
            panel.refresh_status(cx);
        }
        panel
    }

    // ---- read access (tests, the workbench) ------------------------------------------------

    /// The search text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The sessions the last search found, best first.
    pub fn hits(&self) -> &[HistoryHit] {
        match &self.results {
            Results::Hits(hits) => hits,
            _ => &[],
        }
    }

    /// The index status as the daemon last reported it; `None` while unknown or unsupported.
    pub fn status(&self) -> Option<&HistoryStatusInfo> {
        match &self.index {
            Index::Known(info) => Some(info),
            _ => None,
        }
    }

    /// True while a search (debounce included) has not answered yet.
    pub fn searching(&self) -> bool {
        self.searching
    }

    /// The filter chips as they are now.
    pub fn filters(&self) -> &Filters {
        &self.filters
    }

    /// The row that is selected.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The confirmation that is pending, if any.
    pub fn pending_confirmation(&self) -> Option<Confirm> {
        self.confirm
    }

    /// The text of the last failed request.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Focuses the search field; while it is not on screen yet (the index state is still
    /// unknown, or indexing is off) the focus moves there as soon as it appears. The index
    /// state is read again, so a `hutan index enable` made meanwhile shows up.
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_status(cx);
        if self.status().is_some_and(|info| info.enabled) {
            self.focus_wanted = false;
            window.focus(&self.search_focus, cx);
        } else {
            self.focus_wanted = true;
        }
    }

    // ---- searching -------------------------------------------------------------------------

    /// Sets the search text and searches after the debounce.
    pub fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        if self.query != query {
            self.query = query.to_owned();
            self.search(true, cx);
        }
        cx.notify();
    }

    /// Replaces the search text, searches at once and selects the hit of `source` / `key` when
    /// the answer has it (the ⌘J `?` fallback).
    pub fn show_hit(
        &mut self,
        query: &str,
        source: HistorySource,
        key: &str,
        cx: &mut Context<Self>,
    ) {
        self.query = query.to_owned();
        self.pick = Some((source, key.to_owned()));
        self.search(false, cx);
        cx.notify();
    }

    /// Replaces a filter chip and searches again.
    pub fn edit_filters(&mut self, edit: impl FnOnce(&mut Filters), cx: &mut Context<Self>) {
        edit(&mut self.filters);
        self.search(false, cx);
        cx.notify();
    }

    fn project_of_active_workspace(&self, cx: &gpui::App) -> Option<std::path::PathBuf> {
        let store = self.env.sessions.read(cx);
        store
            .active_workspace()
            .or_else(|| store.home_workspace())
            .and_then(|w| w.roots.first().cloned())
    }

    /// Runs the search for the current query and filters (after the debounce when `debounce`);
    /// an earlier search that has not answered is dropped.
    fn search(&mut self, debounce: bool, cx: &mut Context<Self>) {
        self.search_task = None;
        let query = self.query.trim().to_owned();
        if query.is_empty() {
            self.results = Results::Idle;
            self.searching = false;
            return;
        }
        if matches!(&self.index, Index::Known(info) if !info.enabled) {
            self.searching = false;
            return;
        }
        self.searching = true;
        let project = self.project_of_active_workspace(cx);
        let request = self
            .filters
            .request(&query, PANEL_LIMIT, now_ms(), project.as_deref());
        let connection = self.env.sessions.read(cx).connection().clone();
        self.search_task = Some(cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor().timer(DEBOUNCE).await;
            }
            let result = cx
                .background_executor()
                .spawn(async move { connection.history_search(request).await })
                .await;
            this.update(cx, |this, cx| this.finish_search(result, cx))
                .ok();
        }));
    }

    fn finish_search(
        &mut self,
        result: Result<hutan_protocol::history::HistoryHits, ClientError>,
        cx: &mut Context<Self>,
    ) {
        self.searching = false;
        match result {
            Ok(found) => {
                self.results = Results::Hits(found.hits);
                self.selected = match self.pick.take() {
                    Some((source, key)) => self
                        .hits()
                        .iter()
                        .position(|h| h.source == source && h.session_key == key),
                    None => self.selected.filter(|i| *i < self.hits().len()),
                };
            }
            Err(error) if is_disabled(&error) => {
                // Indexing was switched off behind our back.
                self.results = Results::Idle;
                self.refresh_status(cx);
            }
            Err(error) => self.results = Results::Failed(error.to_string()),
        }
        cx.notify();
    }

    // ---- the index -------------------------------------------------------------------------

    /// Reads `history.status`.
    pub fn refresh_status(&mut self, cx: &mut Context<Self>) {
        let store = self.env.sessions.read(cx);
        let connection = store.connection().clone();
        if store.connection_state() != ConnectionState::Connected {
            return;
        }
        if !connection.supports_history() {
            self.index = Index::Unsupported;
            cx.notify();
            return;
        }
        self.status_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { connection.history_status().await })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(info) => {
                        let was_enabled = this.status().is_some_and(|s| s.enabled);
                        this.index = Index::Known(info);
                        if was_enabled && !this.status().is_some_and(|s| s.enabled) {
                            // Indexing was switched off elsewhere (the settings, the CLI).
                            this.results = Results::Idle;
                            this.selected = None;
                        }
                        let rows: u64 = this
                            .status()
                            .map_or(0, |s| s.sources.iter().map(|s| s.rows).sum());
                        if !was_enabled && this.status().is_some_and(|s| s.enabled) {
                            this.search(false, cx);
                        }
                        // Nothing indexed yet: the first scan is probably still running.
                        if rows == 0
                            && this.status().is_some_and(|s| s.enabled)
                            && this.settle_task.is_none()
                        {
                            this.settle(cx);
                        }
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// **Enable indexing**: `history.set_enabled`, then follows the first scan.
    pub fn enable_indexing(&mut self, cx: &mut Context<Self>) {
        self.set_indexing(true, cx);
    }

    /// **Disable**: `history.set_enabled` off. The daemon stops reading transcripts and keeps
    /// the index file.
    pub fn disable_indexing(&mut self, cx: &mut Context<Self>) {
        self.set_indexing(false, cx);
    }

    fn set_indexing(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        self.busy = Some(if enabled {
            "Enabling…"
        } else {
            "Disabling…"
        });
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { connection.history_set_enabled(enabled).await })
                .await;
            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(info) => {
                        this.index = Index::Known(info);
                        if enabled {
                            this.settle(cx);
                            this.search(false, cx);
                        } else {
                            this.settle_task = None;
                            this.search_task = None;
                            this.searching = false;
                            this.results = Results::Idle;
                            this.selected = None;
                        }
                    }
                    Err(error) => {
                        this.error = Some(format!(
                            "Cannot {} indexing: {error}",
                            if enabled { "enable" } else { "disable" }
                        ));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Asks before running `what`.
    pub fn ask(&mut self, what: Confirm, cx: &mut Context<Self>) {
        self.confirm = Some(what);
        cx.notify();
    }

    pub fn cancel_confirmation(&mut self, cx: &mut Context<Self>) {
        self.confirm = None;
        cx.notify();
    }

    /// Runs the confirmed action.
    pub fn confirmed(&mut self, cx: &mut Context<Self>) {
        match self.confirm.take() {
            Some(Confirm::Clear) => self.clear(cx),
            Some(Confirm::Rebuild) => self.rebuild(cx),
            None => {}
        }
        cx.notify();
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        self.busy = Some("Clearing…");
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { connection.history_clear().await })
                .await;
            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(()) => {
                        this.results = Results::Idle;
                        this.selected = None;
                        this.refresh_status(cx);
                        this.settle(cx);
                        this.search(false, cx);
                    }
                    Err(error) => this.error = Some(format!("Cannot clear the index: {error}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        self.busy = Some("Rebuilding…");
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { connection.history_rebuild().await })
                .await;
            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(()) => this.settle(cx),
                    Err(error) => {
                        this.error = Some(format!("Cannot rebuild the index: {error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Follows the index while it is being (re)built: reads the status every
    /// [`SETTLE_POLL`], searches again when the rows changed, and stops once they stay put.
    fn settle(&mut self, cx: &mut Context<Self>) {
        let connection = self.env.sessions.read(cx).connection().clone();
        self.settle_task = Some(cx.spawn(async move |this, cx| {
            let mut rows_before: Option<u64> = None;
            let mut quiet = Duration::ZERO;
            let mut elapsed = Duration::ZERO;
            while elapsed < SETTLE_MAX {
                let asked = connection.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { asked.history_status().await })
                    .await;
                let Ok(info) = result else { break };
                let rows: u64 = info.sources.iter().map(|s| s.rows).sum();
                let changed = rows_before != Some(rows);
                if changed {
                    quiet = Duration::ZERO;
                }
                let enabled = info.enabled;
                let alive = this.update(cx, |this, cx| {
                    this.index = Index::Known(info);
                    if changed && rows_before.is_some() {
                        this.search(false, cx);
                    }
                    cx.notify();
                });
                let limit = if rows == 0 {
                    SETTLE_QUIET_EMPTY
                } else {
                    SETTLE_QUIET
                };
                if alive.is_err() || !enabled || quiet >= limit {
                    break;
                }
                rows_before = Some(rows);
                cx.background_executor().timer(SETTLE_POLL).await;
                quiet += SETTLE_POLL;
                elapsed += SETTLE_POLL;
            }
            this.update(cx, |this, _| this.settle_task = None).ok();
        }));
    }

    // ---- hit actions -----------------------------------------------------------------------

    /// What opening the selected row does by default: resume it, or show the Hutan session.
    fn primary(&self, hit: &HistoryHit, cx: &gpui::App) -> Option<Box<dyn gpui::Action>> {
        if let Some(resume) = HistoryResume::of(hit) {
            return Some(Box::new(ResumeHistoryHit(resume)));
        }
        let target = hutan_target(hit)?;
        self.session_exists(&target, cx)
            .then(|| Box::new(OpenHistoryHit(target)) as Box<dyn gpui::Action>)
    }

    fn session_exists(&self, target: &HutanTarget, cx: &gpui::App) -> bool {
        target
            .session
            .parse::<SessionId>()
            .ok()
            .is_some_and(|id| self.env.sessions.read(cx).session(id).is_some())
    }

    /// Moves the selection by `delta` rows (the arrow keys).
    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.hits().len();
        if count == 0 {
            return;
        }
        let next = match self.selected {
            None if delta >= 0 => 0,
            None => count - 1,
            Some(at) => at.saturating_add_signed(delta).min(count - 1),
        };
        self.selected = Some(next);
        cx.notify();
    }

    /// How the search box's element reaches its text: the platform's text input and the mouse.
    fn search_field(&self, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.search_focus.clone(),
            |this, cx, f| {
                let vim_on = VimConfig::of(&this.env.settings).enabled;
                let mut query = this.query.clone();
                this.query_edit.with(&mut query, vim_on, false, f);
                this.set_query(&query, cx);
            },
        )
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let vim = VimConfig::of(&self.env.settings);
        // Vim's commands and the editing keys; typed text arrives through the box's platform
        // text input (`search_field`).
        let mut query = self.query.clone();
        if self.query_edit.key(&mut query, event, vim, true, cx) == Outcome::Handled {
            self.set_query(&query, cx);
            cx.stop_propagation();
            return;
        }
        // Enter, ↑ ↓ and Escape (in Normal mode, under Vim mode) are the panel's own.
        let ks = &event.keystroke;
        if ks.modifiers.platform || ks.modifiers.control || ks.modifiers.function {
            return;
        }
        match ks.key.as_str() {
            "escape" => self.set_query("", cx),
            "down" => self.move_selection(1, cx),
            "up" => self.move_selection(-1, cx),
            "enter" => {
                let action = self
                    .selected
                    .and_then(|i| self.hits().get(i))
                    .and_then(|hit| self.primary(hit, cx));
                match action {
                    Some(action) => window.dispatch_action(action, cx),
                    None => {
                        self.search(false, cx);
                        cx.notify();
                    }
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Focusable for HistoryPanel {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.search_focus.clone()
    }
}

// ---- rendering --------------------------------------------------------------------------------

pub(crate) fn pill(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    active: bool,
    theme: &Theme,
    tokens: &Tokens,
) -> gpui::Stateful<gpui::Div> {
    let (hover, accent) = (theme.surface_hover, theme.accent);
    div()
        .id(id.into())
        .flex_none()
        .px(px(tokens.spacing * 2.0))
        .h(px(tokens.font_size + 6.0))
        .flex()
        .items_center()
        .rounded(px(crate::theme::PILL_RADIUS))
        .border_1()
        .border_color(if active {
            accent.opacity(0.45)
        } else {
            theme.border
        })
        .text_size(px(tokens.secondary_font_size))
        .text_color(if active { theme.fg } else { theme.muted })
        .when(active, move |s| s.bg(theme.accent_soft))
        .cursor_pointer()
        .when(!active, move |s| s.hover(move |s| s.bg(hover)))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(label.into())
}

/// A text button; `enabled == false` draws it dim and makes it inert.
fn button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    enabled: bool,
    theme: &Theme,
    tokens: &Tokens,
) -> gpui::Stateful<gpui::Div> {
    ui::button(
        id.into(),
        label,
        ui::ButtonKind::Secondary,
        enabled,
        theme,
        tokens,
    )
    .h(px(tokens.font_size + 8.0))
    .px(px(tokens.spacing * 2.0))
    .text_size(px(tokens.secondary_font_size))
}

fn row_meta(hit: &HistoryHit, home: Option<&std::path::Path>) -> String {
    let mut parts = vec![
        source_label(hit.source).to_owned(),
        role_label(hit.role).to_owned(),
    ];
    if let Some(cwd) = hit.cwd.as_deref().filter(|c| !c.is_empty()) {
        parts.push(short_path(cwd, home));
    }
    parts.push(match hit.matches {
        1 => "1 hit".to_owned(),
        n => format!("{n} hits"),
    });
    parts.join(" · ")
}

impl Render for HistoryPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drawn = true;
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let body = match &self.index {
            Index::Unsupported => self.render_note(
                "This hutand does not offer history search (protocol 0.8). Restart it from the \
                 Daemon menu after updating Hutan.",
                &theme,
                &tokens,
            ),
            Index::Unknown => {
                let connected =
                    self.env.sessions.read(cx).connection_state() == ConnectionState::Connected;
                let text = if connected {
                    "Reading the index state…"
                } else {
                    "Waiting for the daemon…"
                };
                self.render_note(text, &theme, &tokens)
            }
            Index::Known(info) if !info.enabled => self.render_disabled(&theme, &tokens, cx),
            Index::Known(info) => {
                let info = info.clone();
                self.render_enabled(&info, window, &theme, &tokens, cx)
            }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .child(panel_header("History", "history", cx))
            .child(body)
    }
}

impl HistoryPanel {
    fn render_note(&self, text: &str, theme: &Theme, tokens: &Tokens) -> AnyElement {
        let mascot = if text.starts_with("Searching") {
            ui::Mascot::Thinking
        } else {
            ui::Mascot::Empty
        };
        ui::empty_state(mascot, text.to_owned(), None, theme, tokens)
            .h(px(260.0))
            .into_any_element()
    }

    /// What indexing reads, what stays local, and the button.
    fn render_disabled(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = self.busy.is_some();
        let line = |text: &str| {
            div()
                .text_color(theme.muted)
                .child(SharedString::from(text.to_owned()))
        };
        let source = |glyph: &str, name: &str, place: &str| {
            div()
                .flex()
                .gap(px(tokens.spacing * 1.5))
                .child(
                    div()
                        .w(px(12.0))
                        .flex_none()
                        .child(SharedString::from(glyph.to_owned())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(SharedString::from(name.to_owned()))
                        .child(
                            div()
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.muted)
                                .child(SharedString::from(place.to_owned())),
                        ),
                )
        };
        div()
            .id("history-disabled")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 2.0))
            .px(px(tokens.spacing * 3.0))
            .py(px(tokens.spacing * 2.0))
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child("Search what you and your agents did"),
            )
            .child(line("History indexing is off. Turning it on reads:"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing))
                    .child(source(
                        source_icon(HistorySource::Hutan),
                        "Hutan terminals",
                        "commands, their output and agent reports",
                    ))
                    .child(source(
                        source_icon(HistorySource::Claude),
                        "Claude Code",
                        "transcripts in ~/.claude/projects",
                    ))
                    .child(source(
                        source_icon(HistorySource::Codex),
                        "Codex",
                        "transcripts in ~/.codex/sessions",
                    ))
                    .child(source(
                        source_icon(HistorySource::Omp),
                        "OMP",
                        "transcripts in ~/.omp/agent/sessions",
                    )),
            )
            .child(line(
                "Nothing leaves this machine. The index is one local file with secrets masked \
                 as far as possible; Clear deletes it. Which sources are read is set in the \
                 [history] section of daemon.toml.",
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        button(
                            "history-enable",
                            self.busy.unwrap_or("Enable indexing"),
                            !busy,
                            theme,
                            tokens,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.enable_indexing(cx)))
                        .child(probe(&self.env.regions, Region::History("enable"))),
                    )
                    .child(
                        div()
                            .id("history-settings-link")
                            .cursor_pointer()
                            .text_color(theme.accent)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(|_, window, cx| {
                                window.dispatch_action(
                                    Box::new(OpenSettingsFor("history".to_owned())),
                                    cx,
                                );
                            })
                            .child("Settings ›")
                            .child(probe(&self.env.regions, Region::History("settings"))),
                    ),
            )
            .when_some(self.error.clone(), |s, error| {
                s.child(
                    div()
                        .text_color(theme.danger)
                        .child(SharedString::from(error)),
                )
            })
            .into_any_element()
    }

    fn render_enabled(
        &mut self,
        info: &HistoryStatusInfo,
        window: &mut Window,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let settings = self.env.settings.get::<HistorySettings>();
        if self.focus_wanted {
            self.focus_wanted = false;
            cx.defer_in(window, |this, window, cx| {
                window.focus(&this.search_focus, cx);
            });
        }
        let searching_field = self.search_focus.is_focused(window);
        let search = div()
            .id("history-search")
            .key_context("HistorySearch")
            .track_focus(&self.search_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_key(event, window, cx);
            }))
            .relative()
            .mx(px(tokens.spacing * 2.0))
            .mb(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .h(px(tokens.sidebar_row))
            .flex_none()
            .flex()
            .items_center()
            .rounded(px(tokens.radius))
            .bg(theme.surface)
            .border_1()
            .border_color(if searching_field {
                theme.accent
            } else {
                theme.border
            })
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, window, cx| {
                    // The dock re-focuses the workbench on this same mouse down; take focus
                    // after the event has been handled.
                    cx.defer_in(window, |this, window, cx| {
                        window.focus(&this.search_focus, cx);
                    });
                }),
            )
            .children(vim_ui::string_field(
                "history-search-text",
                &self.query_edit,
                &self.query,
                VimConfig::of(&self.env.settings).enabled,
                "⌕ Search sessions, commands, transcripts",
                searching_field,
                false,
                self.search_field(cx),
                theme,
                tokens,
            ))
            .child(probe(&self.env.regions, Region::History("search")));

        let mut list = div()
            .id("history-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col();
        match &self.results {
            Results::Idle => {
                list = list.child(self.render_note(
                    "Type a phrase, a command or a file name. Results are one row per session.",
                    theme,
                    tokens,
                ));
            }
            Results::Failed(error) => {
                list = list.child(
                    div()
                        .px(px(tokens.spacing * 3.0))
                        .py(px(tokens.spacing * 2.0))
                        .text_color(theme.danger)
                        .child(SharedString::from(format!("Search failed: {error}"))),
                );
            }
            Results::Hits(hits) if hits.is_empty() => {
                let text = if self.searching {
                    "Searching…"
                } else if self.filters.narrows() {
                    "No session matches. A filter may be hiding some."
                } else {
                    "No session matches."
                };
                list = list.child(self.render_note(text, theme, tokens));
            }
            Results::Hits(hits) => {
                let home = std::env::home_dir();
                for (index, hit) in hits.iter().enumerate() {
                    list = list.child(self.render_hit(
                        index,
                        hit,
                        home.as_deref(),
                        settings.show_snippets,
                        theme,
                        tokens,
                        cx,
                    ));
                }
            }
        }

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(search)
            .child(self.render_filters(theme, tokens, cx))
            .child(list)
            .child(self.render_footer(info, theme, tokens, cx))
            .into_any_element()
    }

    fn render_filters(&self, theme: &Theme, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let group = |label: &str| {
            div()
                .flex_none()
                .text_size(px(tokens.section_font_size))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.muted)
                .child(SharedString::from(label.to_owned()))
        };
        let row = |label: &str| {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(tokens.spacing))
                .child(group(label))
        };
        let mut sources = row("Source");
        for source in SOURCES {
            sources = sources.child(
                pill(
                    format!("history-source-{}", source.as_str()),
                    source_label(source),
                    self.filters.sources.contains(&source),
                    theme,
                    tokens,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.edit_filters(|f| f.toggle_source(source), cx);
                })),
            );
        }
        let mut roles = row("Role");
        for role in ROLES {
            roles = roles.child(
                pill(
                    format!("history-role-{}", role.as_str()),
                    role_label(role),
                    self.filters.roles.contains(&role),
                    theme,
                    tokens,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.edit_filters(|f| f.toggle_role(role), cx);
                })),
            );
        }
        let mut dates = row("Date");
        for range in HistoryRange::ALL {
            let label = match range {
                HistoryRange::All => "all",
                other => other.key(),
            };
            dates = dates.child(
                pill(
                    format!("history-range-{}", range.key()),
                    label,
                    self.filters.range == range,
                    theme,
                    tokens,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.edit_filters(|f| f.range = range, cx);
                })),
            );
        }
        let mut project = row("Project");
        for (scope, label) in [
            (HistoryScope::Workspace, "this workspace"),
            (HistoryScope::All, "all"),
        ] {
            project = project.child(
                pill(
                    format!("history-scope-{label}"),
                    label,
                    self.filters.scope == scope,
                    theme,
                    tokens,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.edit_filters(|f| f.scope = scope, cx);
                })),
            );
        }
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .pb(px(tokens.spacing * 2.0))
            .child(sources)
            .child(roles)
            .child(dates)
            .child(project)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)] // one call site; a struct would only rename them
    fn render_hit(
        &self,
        index: usize,
        hit: &HistoryHit,
        home: Option<&std::path::Path>,
        show_snippets: bool,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selected == Some(index);
        let hover = theme.surface_hover;
        let snippet = parse_snippet(&hit.snippet);
        let marks: Vec<_> = snippet
            .marks
            .iter()
            .map(|range| {
                (
                    range.clone(),
                    HighlightStyle {
                        color: Some(theme.accent),
                        font_weight: Some(gpui::FontWeight::MEDIUM),
                        ..HighlightStyle::default()
                    },
                )
            })
            .collect();

        let mut actions = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(tokens.spacing));
        let mut note: Option<String> = None;
        match HistoryResume::of(hit) {
            Some(resume) => {
                actions = actions.child(
                    button(
                        format!("history-resume-{index}"),
                        "Resume",
                        true,
                        theme,
                        tokens,
                    )
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(Box::new(ResumeHistoryHit(resume.clone())), cx);
                    }),
                );
                if let Some(argv) = &hit.resume_argv {
                    let line = shell_join(argv);
                    actions = actions.child(
                        button(
                            format!("history-copy-{index}"),
                            "Copy command",
                            true,
                            theme,
                            tokens,
                        )
                        .on_click(move |_, window, cx| {
                            window.dispatch_action(Box::new(CopyHistoryCommand(line.clone())), cx);
                        }),
                    );
                }
            }
            None => {
                actions = actions.child(button(
                    format!("history-resume-{index}"),
                    "Resume",
                    false,
                    theme,
                    tokens,
                ));
                note = resume_blocker(hit).map(|why| format!("Cannot resume: {why}"));
            }
        }
        if let Some(path) = transcript_path(hit) {
            let path = path.to_string_lossy().into_owned();
            actions = actions.child(
                button(
                    format!("history-reveal-{index}"),
                    "Reveal File",
                    true,
                    theme,
                    tokens,
                )
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(RevealHistoryFile(path.clone())), cx);
                }),
            );
        }
        if let Some(target) = hutan_target(hit) {
            let label = if target.block.is_some() {
                "Open Block"
            } else {
                "Open Session"
            };
            if self.session_exists(&target, cx) {
                actions = actions.child(
                    button(format!("history-open-{index}"), label, true, theme, tokens).on_click(
                        move |_, window, cx| {
                            window.dispatch_action(Box::new(OpenHistoryHit(target.clone())), cx);
                        },
                    ),
                );
            } else {
                actions = actions.child(button(
                    format!("history-open-{index}"),
                    label,
                    false,
                    theme,
                    tokens,
                ));
                note = Some("That terminal session no longer exists.".to_owned());
            }
        }

        let age = hit.ts_ms.map(age_label).unwrap_or_default();
        div()
            .id(SharedString::from(format!("history-row-{index}")))
            .flex()
            .flex_col()
            .gap(px(tokens.spacing / 2.0))
            .mx(px(tokens.spacing))
            .px(px(tokens.spacing))
            .py(px(tokens.spacing))
            .rounded(px(tokens.radius))
            .when(selected, move |s| s.bg(theme.accent_soft))
            .when(!selected, move |s| s.hover(move |s| s.bg(hover)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.selected = Some(index);
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 1.5))
                    .child(
                        div()
                            .flex_none()
                            .w(px(12.0))
                            .text_color(theme.muted)
                            .child(source_icon(hit.source)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(SharedString::from(hit_title(hit))),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("history-age-{index}")))
                            .flex_none()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .when_some(hit.ts_ms, |s, ts| s.tooltip(time_tooltip(ts)))
                            .child(SharedString::from(age)),
                    ),
            )
            .child(
                div()
                    .truncate()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(row_meta(hit, home))),
            )
            .when(show_snippets && !snippet.text.is_empty(), |s| {
                s.child(
                    div()
                        .line_clamp(3)
                        .text_size(px(tokens.secondary_font_size))
                        .child(
                            StyledText::new(SharedString::from(snippet.text))
                                .with_highlights(marks),
                        ),
                )
            })
            .child(actions)
            .when_some(note, |s, note| {
                s.child(
                    div()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child(SharedString::from(note)),
                )
            })
            .into_any_element()
    }

    fn render_footer(
        &self,
        info: &HistoryStatusInfo,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows: u64 = info.sources.iter().map(|s| s.rows).sum();
        let errors: u64 = info.sources.iter().map(|s| s.errors).sum();
        let per_source = info
            .sources
            .iter()
            .map(|s| {
                let indexed = s
                    .last_indexed_ms
                    .map_or_else(|| "—".to_owned(), hutan_client::time::relative_to_now);
                format!("{} {indexed}", s.name)
            })
            .collect::<Vec<_>>()
            .join(" · ");
        let busy = self.busy.is_some();
        let small = px(tokens.secondary_font_size);

        let controls: AnyElement = match self.confirm {
            Some(what) => {
                let question = match what {
                    Confirm::Clear => {
                        "Delete the whole index? Hutan's own records cannot be rebuilt."
                    }
                    Confirm::Rebuild => "Read every transcript again?",
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing))
                    .child(div().text_color(theme.warning).child(question))
                    .child(
                        div()
                            .flex()
                            .gap(px(tokens.spacing))
                            .child(
                                button(
                                    "history-confirm",
                                    match what {
                                        Confirm::Clear => "Clear index",
                                        Confirm::Rebuild => "Rebuild",
                                    },
                                    true,
                                    theme,
                                    tokens,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.confirmed(cx)))
                                .child(probe(&self.env.regions, Region::History("confirm"))),
                            )
                            .child(
                                button("history-cancel", "Cancel", true, theme, tokens).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.cancel_confirmation(cx);
                                    }),
                                ),
                            ),
                    )
                    .into_any_element()
            }
            None => div()
                .flex()
                .items_center()
                .gap(px(tokens.spacing))
                .child(
                    button("history-clear", "Clear…", !busy, theme, tokens)
                        .on_click(cx.listener(|this, _, _, cx| this.ask(Confirm::Clear, cx)))
                        .child(probe(&self.env.regions, Region::History("clear"))),
                )
                .child(
                    button("history-rebuild", "Rebuild…", !busy, theme, tokens)
                        .on_click(cx.listener(|this, _, _, cx| this.ask(Confirm::Rebuild, cx)))
                        .child(probe(&self.env.regions, Region::History("rebuild"))),
                )
                .child(
                    button("history-disable", "Disable", !busy, theme, tokens)
                        .on_click(cx.listener(|this, _, _, cx| this.disable_indexing(cx)))
                        .child(probe(&self.env.regions, Region::History("disable"))),
                )
                .when_some(self.busy, |s, busy| {
                    s.child(div().text_color(theme.muted).child(busy))
                })
                .into_any_element(),
        };

        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .py(px(tokens.spacing * 2.0))
            .border_t_1()
            .border_color(theme.border)
            .text_size(small)
            .text_color(theme.muted)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(tokens.spacing * 1.5))
                    .child(SharedString::from(format!("{rows} rows")))
                    .child(SharedString::from(format_bytes(info.db_bytes)))
                    .child(
                        div()
                            .text_color(if errors > 0 {
                                theme.warning
                            } else {
                                theme.muted
                            })
                            .child(SharedString::from(match errors {
                                0 => "no errors".to_owned(),
                                1 => "1 unreadable line".to_owned(),
                                n => format!("{n} unreadable lines"),
                            })),
                    ),
            )
            .when(!per_source.is_empty(), |s| {
                s.child(div().child(SharedString::from(per_source)))
            })
            .child(controls)
            .when_some(self.error.clone(), |s, error| {
                s.child(
                    div()
                        .text_color(theme.danger)
                        .child(SharedString::from(error)),
                )
            })
            .into_any_element()
    }
}
