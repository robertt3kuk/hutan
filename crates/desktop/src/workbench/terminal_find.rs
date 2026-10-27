//! Find in a terminal (TERM-FIND): ⌘F in a terminal opens this bar over its top right; the
//! daemon, which holds the whole scrollback, searches (`terminal.search`, protocol 0.43), and the
//! view draws the matches and scrolls to the current one, loading that part of the scrollback.
//! The bar looks like the file find bar (`Aa`, `.*`, `n of m`, ↑ ↓, ×). Return and ⌘G go to the
//! next match (towards newer output), ⇧Return and ⌘⇧G to the previous one, both wrap; Escape
//! closes the bar, drops the highlights and gives the keyboard back to the terminal. New output
//! keeps the matches and their count current (searched again at most four times a second);
//! without a bar the terminal costs nothing extra. *Block* limits the search to the output of
//! the block that is selected (⌘⇧↑ ⌘⇧↓).

use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, Render, Styled,
    Subscription, Task, Window, div, px,
};
use hutan_client::{ClientError, Connection};
use hutan_core::TerminalId;
use hutan_protocol::ErrorCode;
use hutan_protocol::terminal_search::{TerminalSearch, TerminalSearchMatch};
use hutan_settings::SettingsStore;
use hutan_terminal_view::{FindHighlights, FindSpan, SharedEvent, TerminalView};

use super::chat::vim::Outcome;
use super::style::UiStyle;
use super::text_field::FieldRef;
use super::ui::{self, FocusRingExt as _};
use super::vim_ui::{self, FieldEdit, VimConfig};
use crate::sessions::TerminalSession;
use crate::theme::{DensityExt as _, Theme, Tokens};

/// The key context of the find box: ⌘F, ⌘G and ⌘⇧G are bound here too, so they work from the
/// field.
pub const FIND_CONTEXT: &str = "TerminalFind";
/// Matches asked for per call of the daemon.
const PAGE: u32 = 1_000;
/// Matches the bar keeps; more are counted as `10000+`.
pub const MAX_MATCHES: usize = 10_000;
/// Output that arrives while the bar is open is searched again at most this often.
pub const REFRESH: Duration = Duration::from_millis(250);

/// What the next bar starts with: the query and options the last one ended with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindMemory {
    pub query: String,
    pub case_sensitive: bool,
    pub regex: bool,
}

/// What the bar asks of the workbench.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalFindEvent {
    /// Escape or ×: the bar is done; the workbench drops it and keeps what it was asked.
    Closed(FindMemory),
}

/// Which match is current after a search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keep {
    /// The newest match (a new query, a changed option).
    Newest,
    /// The one the bar had, or the nearest after it (output arrived).
    Current,
}

struct Found {
    matches: Vec<TerminalSearchMatch>,
    truncated: bool,
}

/// The bar of one terminal view.
pub struct TerminalFind {
    settings: SettingsStore,
    view: Entity<TerminalView>,
    session: Entity<TerminalSession>,
    query: String,
    edit: FieldEdit,
    focus: FocusHandle,
    case_sensitive: bool,
    regex: bool,
    /// *Block*: only the output of this block.
    block: Option<u64>,
    matches: Vec<TerminalSearchMatch>,
    truncated: bool,
    current: Option<usize>,
    error: Option<String>,
    searching: bool,
    /// Bumped by every search; an answer of an older one is dropped.
    generation: u64,
    search_task: Task<()>,
    /// The timer that searches again after output, while one runs.
    refresh: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for TerminalFind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalFind")
            .field("query", &self.query)
            .field("matches", &self.matches.len())
            .field("current", &self.current)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<TerminalFindEvent> for TerminalFind {}

impl Focusable for TerminalFind {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Every page of the matches of `request`, up to [`MAX_MATCHES`].
async fn fetch(connection: Connection, mut request: TerminalSearch) -> Result<Found, ClientError> {
    let mut matches: Vec<TerminalSearchMatch> = Vec::new();
    loop {
        let page = connection.terminal_search(request.clone()).await?;
        matches.extend(page.matches);
        match page.next_from.filter(|_| page.truncated) {
            Some(next) if matches.len() < MAX_MATCHES => request.from = Some(next),
            Some(_) => {
                matches.truncate(MAX_MATCHES);
                return Ok(Found {
                    matches,
                    truncated: true,
                });
            }
            None => {
                return Ok(Found {
                    matches,
                    truncated: false,
                });
            }
        }
    }
}

fn span_of(found: &TerminalSearchMatch) -> FindSpan {
    FindSpan {
        row: found.row,
        col: found.col,
        end_row: found.end_row,
        end_col: found.end_col,
    }
}

impl TerminalFind {
    pub fn new(
        view: Entity<TerminalView>,
        session: Entity<TerminalSession>,
        settings: SettingsStore,
        cx: &mut Context<Self>,
    ) -> Self {
        let shared = session.read(cx).shared().clone();
        let subscriptions = vec![cx.subscribe(&shared, |this, _, event, cx| {
            if matches!(event, SharedEvent::Applied(_) | SharedEvent::Reset) {
                this.on_output(cx);
            }
        })];
        Self {
            settings,
            view,
            session,
            query: String::new(),
            edit: FieldEdit::new(),
            focus: cx.focus_handle(),
            case_sensitive: false,
            regex: false,
            block: None,
            matches: Vec::new(),
            truncated: false,
            current: None,
            error: None,
            searching: false,
            generation: 0,
            search_task: Task::ready(()),
            refresh: None,
            _subscriptions: subscriptions,
        }
    }

    // ---- state (tests, the workbench) ---------------------------------------------------------

    pub fn query(&self) -> &str {
        &self.query
    }

    /// The matches of the last search, oldest first.
    pub fn matches(&self) -> &[TerminalSearchMatch] {
        &self.matches
    }

    /// Index of the current match.
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// `3 of 41`, `No matches`, `10000+ matches`, the pattern error, or nothing without a query.
    pub fn summary(&self) -> String {
        if let Some(error) = &self.error {
            return error.clone();
        }
        if self.query.is_empty() {
            return String::new();
        }
        if self.searching && self.matches.is_empty() {
            return "Searching…".to_owned();
        }
        let count = self.matches.len();
        let total = if self.truncated {
            format!("{count}+")
        } else {
            count.to_string()
        };
        match (count, self.current) {
            (0, _) => "No matches".to_owned(),
            (_, Some(index)) => format!("{} of {total}", index + 1),
            (_, None) => format!("{total} matches"),
        }
    }

    pub fn case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    pub fn regex(&self) -> bool {
        self.regex
    }

    /// The block *Block* limits the search to.
    pub fn block(&self) -> Option<u64> {
        self.block
    }

    /// Whether a search is under way.
    pub fn searching(&self) -> bool {
        self.searching
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    // ---- opening, closing -----------------------------------------------------------------------

    /// Starts from what the last bar ended with.
    pub fn remember(&mut self, memory: FindMemory) {
        self.query = memory.query;
        self.case_sensitive = memory.case_sensitive;
        self.regex = memory.regex;
    }

    /// ⌘F: the field gets the keyboard with its text selected; a one-line selection in the
    /// terminal becomes the query.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self
            .view
            .read(cx)
            .selection_text()
            .filter(|text| !text.contains('\n') && !text.trim().is_empty());
        let mut changed = selected.is_some_and(|text| {
            let changed = text != self.query;
            self.query = text;
            changed
        });
        // A bar that starts from the last query shows its matches at once.
        changed |= !self.query.is_empty() && self.matches.is_empty() && !self.searching;
        self.edit.select_all(&self.query);
        window.focus(&self.focus, cx);
        if changed {
            self.search(Keep::Newest, true, cx);
        }
        cx.notify();
    }

    /// Sets the query (tests, the field) and searches.
    pub fn set_query(&mut self, text: &str, cx: &mut Context<Self>) {
        self.query = text.to_owned();
        self.search(Keep::Newest, true, cx);
        cx.notify();
    }

    /// Escape: drops the highlights and gives the keyboard back to the terminal.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        self.search_task = Task::ready(());
        self.refresh = None;
        self.view
            .update(cx, |view, cx| view.set_find_highlights(None, cx));
        let focus = self.view.focus_handle(cx);
        window.focus(&focus, cx);
        cx.emit(TerminalFindEvent::Closed(FindMemory {
            query: self.query.clone(),
            case_sensitive: self.case_sensitive,
            regex: self.regex,
        }));
    }

    // ---- searching -----------------------------------------------------------------------------

    fn on_output(&mut self, cx: &mut Context<Self>) {
        if self.query.is_empty() || self.refresh.is_some() {
            return;
        }
        self.refresh = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REFRESH).await;
            this.update(cx, |this, cx| {
                this.refresh = None;
                this.search(Keep::Current, false, cx);
            })
            .ok();
        }));
    }

    /// Asks the daemon; `reveal` scrolls the terminal to the current match when the answer comes
    /// (the user did something; output that arrived does not move the view).
    fn search(&mut self, keep: Keep, reveal: bool, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        if self.query.is_empty() {
            self.search_task = Task::ready(());
            self.matches.clear();
            self.truncated = false;
            self.current = None;
            self.error = None;
            self.searching = false;
            self.publish(false, cx);
            return;
        }
        let Some(terminal) = self.session.read(cx).terminal() else {
            return;
        };
        let connection = self.session.read(cx).connection();
        let request = TerminalSearch {
            terminal_id: terminal,
            query: self.query.clone(),
            case: self.case_sensitive,
            regex: self.regex,
            limit: PAGE,
            from: None,
            block_id: self.block,
            text: false,
        };
        self.searching = true;
        self.search_task = cx.spawn(async move |this, cx| {
            let outcome = fetch(connection, request).await;
            this.update(cx, |this, cx| {
                this.finished(generation, terminal, keep, reveal, outcome, cx);
            })
            .ok();
        });
    }

    fn finished(
        &mut self,
        generation: u64,
        terminal: TerminalId,
        keep: Keep,
        reveal: bool,
        outcome: Result<Found, ClientError>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        if self.session.read(cx).terminal() != Some(terminal) {
            // The terminal was replaced (restart): the matches are of another one.
            self.search(Keep::Newest, false, cx);
            return;
        }
        self.searching = false;
        match outcome {
            Ok(found) => {
                let previous = self.current.and_then(|i| self.matches.get(i)).cloned();
                self.current = pick_current(&found.matches, previous.as_ref(), keep);
                self.matches = found.matches;
                self.truncated = found.truncated;
                self.error = None;
                self.publish(reveal, cx);
            }
            // A newer search of this connection took its place; its answer follows.
            Err(ClientError::Remote(error)) if error.code == ErrorCode::Conflict => {
                self.searching = true;
            }
            Err(error) => {
                self.matches.clear();
                self.truncated = false;
                self.current = None;
                self.error = Some(match error {
                    ClientError::Remote(error) if error.code == ErrorCode::BadRequest => {
                        // "invalid pattern: regex parse error: …" — the first line says it.
                        error.message.lines().next().unwrap_or_default().to_owned()
                    }
                    other => other.to_string(),
                });
                self.publish(false, cx);
            }
        }
        cx.notify();
    }

    /// Hands the matches and the current one to the terminal view.
    fn publish(&mut self, reveal: bool, cx: &mut Context<Self>) {
        let spans: Vec<FindSpan> = self.matches.iter().map(span_of).collect();
        let highlights = (!spans.is_empty()).then(|| FindHighlights::new(spans, self.current));
        let current = highlights.as_ref().and_then(FindHighlights::current_span);
        self.view.update(cx, |view, cx| {
            view.set_find_highlights(highlights, cx);
            if let Some(span) = current.filter(|_| reveal) {
                view.reveal_find_match(span, cx);
            }
        });
        cx.notify();
    }

    /// Return / ⌘G: the next match, towards newer output; ⇧Return / ⌘⇧G: the previous one. Both
    /// wrap around.
    pub fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.matches.len();
        if count == 0 {
            return;
        }
        let next = match (self.current, forward) {
            (None, true) => 0,
            (None, false) => count - 1,
            (Some(index), true) => (index + 1) % count,
            (Some(index), false) => (index + count - 1) % count,
        };
        self.current = Some(next);
        self.publish(true, cx);
    }

    /// *Aa* (tests, the button).
    pub fn set_case_sensitive(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.case_sensitive != on {
            self.toggle(false, cx);
        }
    }

    /// `.*` (tests, the button).
    pub fn set_regex(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.regex != on {
            self.toggle(true, cx);
        }
    }

    fn toggle(&mut self, regex: bool, cx: &mut Context<Self>) {
        if regex {
            self.regex = !self.regex;
        } else {
            self.case_sensitive = !self.case_sensitive;
        }
        self.search(Keep::Newest, true, cx);
        cx.notify();
    }

    /// *Block*: limits the search to the output of the selected block, or lifts the limit.
    pub fn toggle_block(&mut self, cx: &mut Context<Self>) {
        self.block = match self.block {
            Some(_) => None,
            None => self.view.read(cx).highlighted_block(),
        };
        self.search(Keep::Newest, true, cx);
        cx.notify();
    }

    // ---- keys ------------------------------------------------------------------------------------

    fn field(&self, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(cx.weak_entity(), self.focus.clone(), |this, cx, f| {
            let vim_on = VimConfig::of(&this.settings).enabled;
            let mut text = this.query.clone();
            this.edit.with(&mut text, vim_on, false, f);
            if text != this.query {
                this.query = text;
                this.search(Keep::Newest, true, cx);
            }
            cx.notify();
        })
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let vim = VimConfig::of(&self.settings);
        let mut text = self.query.clone();
        if self.edit.key(&mut text, event, vim, true, cx) == Outcome::Handled {
            if text != self.query {
                self.query = text;
                self.search(Keep::Newest, true, cx);
            }
            cx.notify();
            cx.stop_propagation();
            return;
        }
        let ks = &event.keystroke;
        if ks.modifiers.platform || ks.modifiers.control || ks.modifiers.function {
            return;
        }
        match ks.key.as_str() {
            "enter" => {
                self.step(!ks.modifiers.shift, cx);
                cx.stop_propagation();
            }
            "escape" => {
                self.close(window, cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    // ---- rendering -------------------------------------------------------------------------------

    fn render_bar(
        &mut self,
        theme: &Theme,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focused = self.focus.is_focused(window);
        let summary = self.summary();
        let failed = self.error.is_some();
        let block_available =
            self.block.is_some() || self.view.read(cx).highlighted_block().is_some();
        let toggle = |id: &'static str, label: &'static str, tip: &'static str, on: bool| {
            ui::Button::new(id, label, theme, tokens)
                .compact()
                .selected(on)
                .tooltip(tip)
        };
        let step = |id: &'static str, icon: crate::icons::Icon, tip: &'static str| {
            ui::IconButton::new(id, icon, tip, theme, tokens).compact()
        };
        div()
            .id("term-find")
            .key_context(FIND_CONTEXT)
            .flex()
            .items_center()
            .gap(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .py(px(tokens.spacing))
            .rounded(px(tokens.radius))
            .border_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .text_size(px(tokens.secondary_font_size))
            .child(
                div()
                    .id("term-find-box")
                    .track_focus(&self.focus)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        this.on_key(event, window, cx);
                    }))
                    .w(px(200.0))
                    .flex()
                    .items_center()
                    .h(px(tokens.sidebar_row - 4.0))
                    .px(px(tokens.spacing * 2.0))
                    .rounded(px(tokens.radius))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .focus_ring(focused, theme, tokens)
                    .cursor_text()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _: &MouseDownEvent, window, cx| {
                            cx.defer_in(window, |this, window, cx| {
                                window.focus(&this.focus, cx);
                            });
                        }),
                    )
                    .children(vim_ui::string_field(
                        "term-find-text",
                        &self.edit,
                        &self.query,
                        VimConfig::of(&self.settings).enabled,
                        "Find in terminal",
                        focused,
                        false,
                        self.field(cx),
                        theme,
                        tokens,
                    )),
            )
            .child(
                div()
                    .id("term-find-summary")
                    .flex_none()
                    .min_w(px(72.0))
                    .text_color(if failed { theme.danger } else { theme.muted })
                    .child(summary),
            )
            .child(
                toggle("term-find-case", "Aa", "Match case", self.case_sensitive)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle(false, cx))),
            )
            .child(
                toggle("term-find-regex", ".*", "Regular expression", self.regex)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle(true, cx))),
            )
            .when(block_available, |bar| {
                bar.child(
                    toggle(
                        "term-find-block",
                        "Block",
                        "Only in this block's output",
                        self.block.is_some(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_block(cx))),
                )
            })
            .child(
                step(
                    "term-find-prev",
                    crate::icons::Icon::ActionCollapse,
                    "Previous match (⇧Return, ⌘⇧G)",
                )
                .on_click(cx.listener(|this, _, _, cx| this.step(false, cx))),
            )
            .child(
                step(
                    "term-find-next",
                    crate::icons::Icon::ActionExpand,
                    "Next match (Return, ⌘G)",
                )
                .on_click(cx.listener(|this, _, _, cx| this.step(true, cx))),
            )
            .child(
                step(
                    "term-find-close",
                    crate::icons::Icon::ActionClose,
                    "Close (Escape)",
                )
                .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
            )
            .into_any_element()
    }
}

/// The match that is current after a search: with [`Keep::Current`] the one the bar had (the
/// same place), else the first one after it, else the newest; otherwise the newest.
fn pick_current(
    matches: &[TerminalSearchMatch],
    previous: Option<&TerminalSearchMatch>,
    keep: Keep,
) -> Option<usize> {
    let newest = matches.len().checked_sub(1);
    match (keep, previous) {
        (Keep::Current, Some(previous)) => {
            let at = matches.partition_point(|m| (m.row, m.col) < (previous.row, previous.col));
            if at < matches.len() { Some(at) } else { newest }
        }
        _ => newest,
    }
}

impl Render for TerminalFind {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        self.render_bar(&theme, &tokens, window, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(row: u64, col: u16) -> TerminalSearchMatch {
        TerminalSearchMatch {
            row,
            col,
            end_row: row,
            end_col: col + 1,
            block_id: None,
            line: None,
        }
    }

    #[test]
    fn output_keeps_the_current_match_or_the_next_one_after_it() {
        let matches = [found(1, 0), found(4, 2), found(4, 9), found(8, 0)];
        // The same place.
        assert_eq!(
            pick_current(&matches, Some(&found(4, 9)), Keep::Current),
            Some(2)
        );
        // Its text scrolled out of the scrollback: the first match after where it was.
        assert_eq!(
            pick_current(&matches, Some(&found(3, 0)), Keep::Current),
            Some(1)
        );
        // Nothing after it: the newest.
        assert_eq!(
            pick_current(&matches, Some(&found(20, 0)), Keep::Current),
            Some(3)
        );
        // No current match yet, or a new query: the newest.
        assert_eq!(pick_current(&matches, None, Keep::Current), Some(3));
        assert_eq!(
            pick_current(&matches, Some(&found(1, 0)), Keep::Newest),
            Some(3)
        );
        assert_eq!(pick_current(&[], None, Keep::Newest), None);
    }
}
