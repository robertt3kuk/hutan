//! The Agents page (AG-1, `docs/spikes/2026-10-20-chrome-v3.md` § 2): one retained tab of the
//! center that answers "find the agent that needs a decision, understand why, and return to its
//! actual work". An attention-first ledger: the groups **Requests**, **Failed**, **Ready for review**, **Working**,
//! **Unconfirmed**, **Idle** and **Finished** (folded), one 64 pt row per agent session with its
//! reason and its safe next action, subagents on disclosure.
//!
//! Everything it shows is the shared agent presentation (`sessions/presentation.rs`): the same
//! records the Agents panel and Mawas's Today read. The list is virtualized (only the rows in
//! view are built), its order is frozen while a pointer rests on an action, a menu is open or a
//! request is being read, its selection is a domain id that survives updates, and a page that
//! is not on screen does no work at all when the store changes.
//!
//! The page never decides for the user: **Review request** expands the exact request with its
//! option buttons, and only a click on an option sends the answer. Return on a row opens the
//! session.

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, ClickEvent, Context, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, ListAlignment, ListState, MouseButton, MouseDownEvent, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Task, Window, canvas, div, prelude::*, px,
};
use hutan_core::{ApprovalId, SessionId, WorkspaceId};

use super::common::{now_ms, tone_color};
use crate::icons::{Icon, glyph, icon};
use crate::sessions::agents_list::{self, Empty, Key, Listing, Query, Row};
use crate::sessions::agents_text;
use crate::sessions::presentation::{AgentPresentation, AgentRecord, Group, Link};
use crate::sessions::{Counts, Scope, StoreChanged};
use crate::settings::AgentsSettings;
use crate::theme::{Density, DensityExt as _, Theme, Tokens};
use crate::workbench::actions::{
    AgentNodeAt, LauncherOpen, OpenLauncher, OpenSessionFromOverview, OpenSettingsFor,
    SessionMenuAt, ShowAgentMenu, ShowAgentNodeMenu, ShowAgentsPageMenu, ShowAgentsScopeMenu,
    daemon,
};
use crate::workbench::chat::vim::Outcome;
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::UiStyle;
use crate::workbench::text_field::FieldRef;
use crate::workbench::ui::{self, FocusRingExt as _, Mascot, StyledExt as _};
use crate::workbench::view::Region;
use crate::workbench::vim_ui::{self, FieldEdit, VimConfig};

/// How often the durations and ages refresh while the page is drawn and an agent runs.
pub const TICK: Duration = Duration::from_secs(1);

/// Rows built beyond the viewport, in pixels.
const OVERDRAW: f32 = 240.0;
/// Content widths (at the default rem) where the columns fold: full, medium, narrow.
const WIDE_FROM: f32 = 900.0;
const MEDIUM_FROM: f32 = 640.0;
/// A compact row (two 16 pt lines and 8 pt insets) and a comfortable one (three lines).
const ROW_COMPACT: f32 = 64.0;
const ROW_COMFORTABLE: f32 = 80.0;
const CHILD_COMPACT: f32 = 32.0;
const CHILD_COMFORTABLE: f32 = 40.0;
/// The header's height and the filter band's.
const HEADER: f32 = 44.0;
const BAND: f32 = 36.0;

/// What a request to open the page asks for. Every field is optional; an empty request only
/// shows the page.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct AgentsRequest {
    /// Select this session (an id).
    pub session: Option<String>,
    /// Expand its request (the *Review request* button of a panel or a notification).
    pub review: bool,
    /// Show only this group (`needs-you`, `working`, `unconfirmed`, `idle`, `finished`).
    pub group: Option<String>,
    /// Scope: `all`, or a workspace id.
    pub scope: Option<String>,
}

/// How much of the row fits: the columns fold with the width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// Identity and reason, location, harness and model, time, action.
    Wide,
    /// Location and harness move to the second line; time and action stay.
    Medium,
    /// Two lines; the time and the details wait for the selection.
    Narrow,
}

impl Layout {
    /// The layout of a page `width` px wide at a UI scale of `scale` (the rem against its
    /// default of 16 px): the thresholds scale with the type.
    fn of(width: f32, scale: f32) -> Self {
        if width >= WIDE_FROM * scale {
            Layout::Wide
        } else if width >= MEDIUM_FROM * scale {
            Layout::Medium
        } else {
            Layout::Narrow
        }
    }
}

/// What the rows were built from: any difference rebuilds them.
#[derive(Debug, Clone, PartialEq)]
struct ListKey {
    revision: u64,
    query: Query,
    open: Vec<SessionId>,
}

pub struct AgentsPage {
    env: PanelEnv,
    /// The list's roving focus stop.
    focus: FocusHandle,
    search_focus: FocusHandle,
    query: String,
    query_edit: FieldEdit,
    scope: Scope,
    group: Option<Group>,
    /// The row the keyboard is on, by domain id.
    selected: Option<Key>,
    /// Where that row was last seen, to land near it when it goes.
    selected_at: usize,
    /// Sessions whose details are expanded below their row.
    details: HashSet<SessionId>,
    /// Sessions whose subagents are listed under them.
    children_open: HashSet<SessionId>,
    /// The session whose request is expanded with its option buttons.
    reviewing: Option<SessionId>,
    /// The last answer that failed, with the session it was for.
    answer_error: Option<(SessionId, String)>,
    visible: bool,
    menu_open: bool,
    /// The pointer rests on an action: the order of the rows is frozen.
    hovering_action: bool,
    /// The store changed while the page was hidden.
    stale: bool,
    /// The records the rows are drawn from (always the latest).
    presentation: Arc<AgentPresentation>,
    listing: Rc<Listing>,
    listed_key: Option<ListKey>,
    list: ListState,
    layout: Layout,
    width: Rc<Cell<f32>>,
    renders: usize,
    listing_builds: Cell<usize>,
    rows_rendered: Cell<usize>,
    /// Drawn since the ticker last looked.
    drawn: bool,
    ticker: Option<Task<()>>,
    ticks: usize,
}

impl std::fmt::Debug for AgentsPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentsPage")
            .field("scope", &self.scope)
            .field("query", &self.query)
            .finish_non_exhaustive()
    }
}

impl AgentsPage {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        // Only what the page shows rebuilds it; a hidden page is not woken at all.
        cx.subscribe(&env.sessions, |this, _, change: &StoreChanged, cx| {
            if !change.intersects(StoreChanged::PRESENTED) {
                return;
            }
            if this.visible {
                cx.notify();
            } else {
                this.stale = true;
            }
        })
        .detach();
        // The Usage section follows what the daemon says about limits.
        cx.observe(&env.usage, |this, _, cx| {
            if this.visible {
                cx.notify();
            } else {
                this.stale = true;
            }
        })
        .detach();
        let presentation = Arc::new(AgentPresentation::empty(Link::Loading, 0));
        Self {
            env,
            focus: cx.focus_handle(),
            search_focus: cx.focus_handle(),
            query: String::new(),
            query_edit: FieldEdit::new(),
            scope: Scope::All,
            group: None,
            selected: None,
            selected_at: 0,
            details: HashSet::new(),
            children_open: HashSet::new(),
            reviewing: None,
            answer_error: None,
            visible: false,
            menu_open: false,
            hovering_action: false,
            stale: false,
            presentation,
            listing: Rc::new(Listing::default()),
            listed_key: None,
            list: ListState::new(0, ListAlignment::Top, px(OVERDRAW)),
            layout: Layout::Wide,
            width: Rc::new(Cell::new(0.0)),
            renders: 0,
            listing_builds: Cell::new(0),
            rows_rendered: Cell::new(0),
            drawn: false,
            ticker: None,
            ticks: 0,
        }
    }

    // ---- what the workbench and the tests ask ---------------------------------------------

    /// How many times the page rendered (tests: a hidden page renders nothing).
    pub fn renders(&self) -> usize {
        self.renders
    }

    /// How many times the rows were built (tests: a frame builds none).
    pub fn listing_builds(&self) -> usize {
        self.listing_builds.get()
    }

    /// How many rows were built for the list so far (tests: only the rows in view are).
    pub fn rows_rendered(&self) -> usize {
        self.rows_rendered.get()
    }

    /// Whether the durations are ticking right now (tests).
    pub fn ticking(&self) -> bool {
        self.ticker.is_some()
    }

    /// How many once-a-second refreshes ran (tests).
    pub fn ticks(&self) -> usize {
        self.ticks
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// Whether the toolbar is folded: the page is narrower than 640 pt, the scope is in ⋯.
    pub fn folded(&self) -> bool {
        self.layout == Layout::Narrow
    }

    /// ⋯ → *Group by workspace*: `agents.page_group_by_workspace`.
    pub fn toggle_group_by_workspace(&mut self, cx: &mut Context<Self>) {
        let on = self.page_settings().page_group_by_workspace;
        self.write_setting("agents.page_group_by_workspace", toml::Value::Boolean(!on));
        cx.notify();
    }

    /// ⋯ → *Show finished*: `agents.page_show_finished`.
    pub fn toggle_show_finished(&mut self, cx: &mut Context<Self>) {
        self.toggle_finished();
        cx.notify();
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn group_filter(&self) -> Option<Group> {
        self.group
    }

    /// The session the keyboard is on, if it is on a session row.
    pub fn selected_session(&self) -> Option<SessionId> {
        match &self.selected {
            Some(Key::Agent(session) | Key::Child(session, _)) => Some(*session),
            _ => None,
        }
    }

    /// The session whose request is expanded.
    pub fn reviewing(&self) -> Option<SessionId> {
        self.reviewing
    }

    /// Whether `session`'s details are expanded.
    pub fn details_open(&self, session: SessionId) -> bool {
        self.details.contains(&session)
    }

    /// The sessions of the rows, top to bottom (tests).
    pub fn listed_sessions(&self) -> Vec<SessionId> {
        self.listing
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Agent { session } => Some(*session),
                _ => None,
            })
            .collect()
    }

    /// The group headings of the rows, top to bottom (tests).
    pub fn listed_groups(&self) -> Vec<Group> {
        self.listing
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Group { group, .. } => Some(*group),
                _ => None,
            })
            .collect()
    }

    /// The totals of the scope the page shows, from the shared presentation.
    pub fn counts(&self, cx: &gpui::App) -> Counts {
        self.env
            .sessions
            .read(cx)
            .agent_presentation()
            .counts(self.scope)
    }

    /// Selects the row of `session`.
    pub fn select(&mut self, session: SessionId, cx: &mut Context<Self>) {
        self.selected = Some(Key::Agent(session));
        self.reveal_selected();
        cx.notify();
    }

    pub fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            cx.notify();
        }
    }

    /// Sets the search text.
    pub fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        self.query = query.to_owned();
        cx.notify();
    }

    /// The workbench tells the page whether it is on screen. A page that was hidden draws again
    /// when it is shown if anything changed meanwhile; one that is hidden is not woken.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            // Hidden pages kept no ticker; the draw starts it again.
            self.stale = false;
            // Looking at the limits: ask the harnesses what is left (at most every 30 s).
            if self.env.settings.get::<AgentsSettings>().page_usage {
                let usage = self.env.usage.clone();
                cx.defer(move |cx| usage.update(cx, |usage, cx| usage.refresh(false, cx)));
            }
            cx.notify();
        } else {
            self.ticker = None;
        }
    }

    /// The workbench tells the page a menu is open: the order of the rows is frozen meanwhile.
    pub fn set_menu_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.menu_open != open {
            self.menu_open = open;
            if !open && self.visible {
                cx.notify();
            }
        }
    }

    /// Applies what a request to open the page asked for.
    pub fn apply_request(&mut self, request: AgentsRequest, cx: &mut Context<Self>) {
        if let Some(scope) = request.scope.as_deref() {
            self.scope = if scope == "all" {
                Scope::All
            } else {
                scope
                    .parse::<WorkspaceId>()
                    .map_or(Scope::All, Scope::Workspace)
            };
        }
        if let Some(group) = request.group.as_deref() {
            self.group = Group::from_key(group);
        }
        if let Some(session) = request.session.as_deref().and_then(|s| s.parse().ok()) {
            // A session outside the scope or the filters would be selected and invisible.
            self.group = None;
            self.query.clear();
            self.scope = Scope::All;
            self.selected = Some(Key::Agent(session));
            self.reviewing = request.review.then_some(session);
            // A folded Finished group opens for the one the request names.
            self.reveal_selected();
        }
        cx.notify();
    }

    // ---- model ------------------------------------------------------------------------------

    fn page_settings(&self) -> Arc<AgentsSettings> {
        self.env.settings.get::<AgentsSettings>()
    }

    fn query_now(&self) -> Query {
        let settings = self.page_settings();
        Query {
            scope: self.scope,
            text: self.query.clone(),
            group: self.group,
            by_workspace: settings.page_group_by_workspace,
            finished_open: settings.page_show_finished,
        }
    }

    /// Opens what the selected row sits in (a folded Finished group), so the selection shows.
    fn reveal_selected(&mut self) {
        let Some(Key::Agent(session)) = &self.selected else {
            return;
        };
        let finished = self
            .presentation
            .get(*session)
            .is_some_and(|r| r.group == Group::Finished);
        if finished && !self.page_settings().page_show_finished {
            self.write_setting("agents.page_show_finished", toml::Value::Boolean(true));
        }
    }

    fn write_setting(&self, key: &str, value: toml::Value) {
        if let Err(err) = self.env.settings.write_user(key, value) {
            tracing::warn!("cannot save {key}: {err}");
        }
    }

    /// Brings the rows up to date with the store, the search, the scope and the filters, unless
    /// the order is frozen (a pointer on an action, a menu, a request being read): then only
    /// what the rows show changes, never where they are.
    fn refresh(&mut self, cx: &gpui::App) {
        let latest = self.env.sessions.read(cx).agent_presentation();
        let frozen = self.menu_open || self.hovering_action || self.reviewing.is_some();
        self.presentation = latest;
        let mut open: Vec<SessionId> = self.children_open.iter().copied().collect();
        open.sort();
        let key = ListKey {
            revision: self.presentation.revision,
            query: self.query_now(),
            open,
        };
        let query_changed = self
            .listed_key
            .as_ref()
            .is_none_or(|k| k.query != key.query);
        if self.listed_key.as_ref() == Some(&key) || (frozen && !query_changed) {
            return;
        }
        let listing = Rc::new(agents_list::list(
            &self.presentation,
            &key.query,
            &self.children_open,
        ));
        self.listing_builds.set(self.listing_builds.get() + 1);
        self.sync_list(&listing, query_changed);
        self.listing = listing;
        self.listed_key = Some(key);
        self.reconcile_selection();
    }

    /// Brings the list's row heights in line with the new rows: a filter that changed starts
    /// the list over; otherwise only the rows that differ are measured again, so the reader
    /// keeps their place while sessions come and go.
    fn sync_list(&mut self, new: &Listing, restart: bool) {
        let old = self.listing.clone();
        if restart || old.rows.is_empty() {
            self.list.reset(new.rows.len());
            return;
        }
        if old.rows == new.rows {
            // Same rows; a row that is expanded may have changed height with its record.
            for (ix, row) in new.rows.iter().enumerate() {
                if let Row::Agent { session } = row
                    && (self.details.contains(session) || self.reviewing == Some(*session))
                {
                    self.list.splice(ix..ix + 1, 1);
                }
            }
            return;
        }
        let prefix = old
            .rows
            .iter()
            .zip(&new.rows)
            .take_while(|(a, b)| a == b)
            .count();
        let room = old.rows.len().min(new.rows.len()) - prefix;
        let suffix = old
            .rows
            .iter()
            .rev()
            .zip(new.rows.iter().rev())
            .take(room)
            .take_while(|(a, b)| a == b)
            .count();
        let (removed, added) = (
            old.rows.len() - prefix - suffix,
            new.rows.len() - prefix - suffix,
        );
        if removed > 0 || added > 0 {
            self.list.splice(prefix..prefix + removed, added);
        }
    }

    /// The selection names a row that may have gone: land on the nearest one.
    fn reconcile_selection(&mut self) {
        let Some(key) = &self.selected else {
            return;
        };
        match self.listing.position(key) {
            Some(at) => self.selected_at = at,
            None => {
                let near = self
                    .listing
                    .step(
                        Some(
                            self.selected_at
                                .min(self.listing.rows.len().saturating_sub(1)),
                        ),
                        1,
                        0,
                    )
                    .or_else(|| self.listing.last());
                self.selected = near.and_then(|ix| self.listing.key(ix).cloned());
                if let Some(ix) = near {
                    self.selected_at = ix;
                }
            }
        }
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected
            .as_ref()
            .and_then(|key| self.listing.position(key))
    }

    fn select_row(&mut self, ix: Option<usize>) {
        if let Some(ix) = ix {
            self.selected = self.listing.key(ix).cloned();
            self.selected_at = ix;
            self.list.scroll_to_reveal_item(ix);
        }
    }

    /// Re-measures the row of `session` after its height changed.
    fn remeasure(&self, session: SessionId) {
        if let Some(ix) = self.listing.position(&Key::Agent(session)) {
            self.list.splice(ix..ix + 1, 1);
        }
    }

    fn toggle_details(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if !self.details.remove(&session) {
            self.details.insert(session);
        }
        self.remeasure(session);
        cx.notify();
    }

    fn toggle_review(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let was = self.reviewing;
        self.reviewing = if was == Some(session) {
            None
        } else {
            Some(session)
        };
        self.answer_error = None;
        for changed in was.into_iter().chain(self.reviewing) {
            self.remeasure(changed);
        }
        cx.notify();
    }

    fn toggle_children(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if !self.children_open.remove(&session) {
            self.children_open.insert(session);
        }
        cx.notify();
    }

    fn open_session(&self, session: SessionId, window: &mut Window, cx: &mut Context<Self>) {
        window.dispatch_action(Box::new(OpenSessionFromOverview(session.to_string())), cx);
    }

    fn record(&self, session: SessionId) -> Option<&AgentRecord> {
        self.presentation.get(session)
    }

    /// Sends the answer a button names: the existing approval decision, through the store, and
    /// nothing else. The result (or its failure) is the daemon's.
    fn answer(
        &mut self,
        session: SessionId,
        approval: ApprovalId,
        option: String,
        cx: &mut Context<Self>,
    ) {
        self.answer_error = None;
        let task = self
            .env
            .sessions
            .update(cx, |store, cx| store.respond_approval(approval, option, cx));
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                this.update(cx, |this, cx| {
                    this.answer_error = Some((session, format!("Could not answer: {error}")));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    // ---- keyboard ---------------------------------------------------------------------------

    fn page_rows(&self) -> usize {
        let height: f32 = self.list.viewport_bounds().size.height.into();
        let row = ROW_COMPACT;
        ((height / row).floor() as usize).max(1)
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_focus.is_focused(window) {
            return;
        }
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;
        if mods.platform && key == "f" {
            // ⌘F searches here only while this surface has the keyboard.
            window.focus(&self.search_focus, cx);
            cx.stop_propagation();
            return;
        }
        if mods.platform || mods.control || mods.alt {
            return;
        }
        let from = self.selected_index();
        match key {
            "down" => self.select_row(self.listing.step(from, 1, 1)),
            "up" => self.select_row(self.listing.step(from, -1, 1)),
            "pagedown" => self.select_row(self.listing.step(from, 1, self.page_rows())),
            "pageup" => self.select_row(self.listing.step(from, -1, self.page_rows())),
            "home" => self.select_row(self.listing.first()),
            "end" => self.select_row(self.listing.last()),
            "enter" => self.activate_selected(window, cx),
            "space" => self.space_selected(cx),
            "right" => self.expand_selected(cx),
            "left" => self.collapse_selected(cx),
            "f10" if mods.shift => self.menu_for_selected(window, cx),
            "escape" => {
                if !self.escape(window, cx) {
                    return;
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Return: opens the session (a group heading folds). Never answers a request.
    fn activate_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.selected.clone() {
            Some(Key::Agent(session) | Key::Child(session, _)) => {
                self.open_session(session, window, cx);
            }
            Some(Key::Group(_, Group::Finished)) => self.toggle_finished(),
            _ => {}
        }
    }

    fn space_selected(&mut self, cx: &mut Context<Self>) {
        match self.selected.clone() {
            Some(Key::Agent(session)) => self.toggle_details(session, cx),
            Some(Key::Group(_, Group::Finished)) => self.toggle_finished(),
            _ => {}
        }
    }

    fn expand_selected(&mut self, cx: &mut Context<Self>) {
        match self.selected.clone() {
            Some(Key::Agent(session)) => {
                let has_children = self.record(session).is_some_and(|r| !r.children.is_empty());
                if has_children && !self.children_open.contains(&session) {
                    self.toggle_children(session, cx);
                } else if !self.details.contains(&session) {
                    self.toggle_details(session, cx);
                }
            }
            Some(Key::Group(_, Group::Finished)) if !self.page_settings().page_show_finished => {
                self.toggle_finished();
            }
            _ => {}
        }
    }

    fn collapse_selected(&mut self, cx: &mut Context<Self>) {
        match self.selected.clone() {
            // From a subagent: back to its session.
            Some(Key::Child(session, _)) => {
                self.selected = Some(Key::Agent(session));
            }
            Some(Key::Agent(session)) => {
                if self.details.contains(&session) {
                    self.toggle_details(session, cx);
                } else if self.children_open.contains(&session) {
                    self.toggle_children(session, cx);
                }
            }
            Some(Key::Group(_, Group::Finished)) if self.page_settings().page_show_finished => {
                self.toggle_finished();
            }
            _ => {}
        }
    }

    fn toggle_finished(&mut self) {
        let open = self.page_settings().page_show_finished;
        self.write_setting("agents.page_show_finished", toml::Value::Boolean(!open));
    }

    /// Escape closes what is open, innermost first: an expanded request, then the details of
    /// the selected row, then its subagents, then the filters. It never closes the page.
    /// Returns whether anything was open.
    fn escape(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        if let Some(session) = self.reviewing {
            self.toggle_review(session, cx);
            return true;
        }
        if let Some(Key::Agent(session) | Key::Child(session, _)) = self.selected.clone() {
            if self.details.contains(&session) {
                self.toggle_details(session, cx);
                return true;
            }
            if self.children_open.contains(&session) {
                self.toggle_children(session, cx);
                if matches!(self.selected, Some(Key::Child(..))) {
                    self.selected = Some(Key::Agent(session));
                }
                return true;
            }
        }
        if !self.query.is_empty() || self.group.is_some() {
            self.query.clear();
            self.group = None;
            return true;
        }
        false
    }

    fn menu_for_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.selected_session() else {
            return;
        };
        let at = self
            .selected_index()
            .and_then(|ix| self.list.bounds_for_item(ix))
            .map_or_else(
                || self.list.viewport_bounds().origin,
                |bounds| bounds.origin + gpui::point(px(40.0), bounds.size.height),
            );
        window.dispatch_action(
            Box::new(ShowAgentMenu(SessionMenuAt {
                session: session.to_string(),
                position: at,
            })),
            cx,
        );
    }

    /// Keys of the search field: editing, Escape clears (then leaves) and ↓ or Return go to
    /// the list.
    fn on_search_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let vim = VimConfig::of(&self.env.settings);
        if self.query_edit.key(&mut self.query, event, vim, true, cx) == Outcome::Handled {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" => {
                if self.query.is_empty() {
                    window.focus(&self.focus, cx);
                } else {
                    self.query.clear();
                }
            }
            "down" | "enter" => {
                window.focus(&self.focus, cx);
                if self.selected_index().is_none() {
                    self.select_row(self.listing.first());
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn search_field(&self, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.search_focus.clone(),
            |this, cx, f| {
                let vim_on = VimConfig::of(&this.env.settings).enabled;
                this.query_edit.with(&mut this.query, vim_on, false, f);
                cx.notify();
            },
        )
    }

    // ---- the ticker -------------------------------------------------------------------------

    /// Starts the once-a-second refresh of the durations and ages, or stops it. Called from
    /// `render`, so it only ever runs while the page is drawn and an agent is alive.
    fn sync_ticker(&mut self, running: bool, cx: &mut Context<Self>) {
        self.drawn = true;
        if !running {
            self.ticker = None;
            return;
        }
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let keep = this
                    .update(cx, |this, cx| {
                        // Not drawn since the last tick: the page is hidden. Stop; the next
                        // draw starts the ticker again.
                        if !std::mem::take(&mut this.drawn) {
                            this.ticker = None;
                            return false;
                        }
                        this.ticks += 1;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }
}

impl Focusable for AgentsPage {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

// ---- rendering ----------------------------------------------------------------------------

/// What a render closure needs to know about the look.
#[derive(Clone, Copy)]
struct Look {
    theme: Theme,
    tokens: Tokens,
    comfortable: bool,
}

impl Look {
    fn row(self) -> f32 {
        if self.comfortable {
            ROW_COMFORTABLE
        } else {
            ROW_COMPACT
        }
    }

    fn child(self) -> f32 {
        if self.comfortable {
            CHILD_COMFORTABLE
        } else {
            CHILD_COMPACT
        }
    }
}

impl Render for AgentsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.env.regions.rendered(cx.entity_id());
        self.renders += 1;
        let style = UiStyle::get(cx);
        let look = Look {
            theme: style.theme,
            tokens: style.density.ui(),
            comfortable: style.density == Density::Comfortable,
        };
        let (theme, tokens) = (look.theme, look.tokens);
        // The width of the last frame chooses the layout; a frame on the other side of a
        // threshold draws once more (the canvas below).
        let scale = f32::from(window.rem_size()) / 16.0;
        let measured = self.width.get();
        let layout = if measured > 0.0 {
            Layout::of(measured, scale)
        } else {
            Layout::of(f32::from(window.viewport_size().width) * 0.6, scale)
        };
        self.layout = layout;
        self.refresh(cx);
        let presentation = self.presentation.clone();
        let listing = self.listing.clone();
        let empty = agents_list::empty(&presentation, &listing);
        let running = presentation
            .in_scope(self.scope)
            .any(|r| r.live && !r.state.is_finished());
        self.sync_ticker(running, cx);
        self.stale = false;

        let counts = presentation.counts(self.scope);
        let searching = self.search_focus.is_focused(window);

        let body: AnyElement = match empty {
            Empty::Loading => self.skeleton(look),
            Empty::NoAgents => self.teaching_state(look, cx),
            Empty::NoMatch => self.no_match_state(look, cx),
            Empty::No => div()
                .id("agents-list")
                .relative()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(gpui::list(self.list.clone(), cx.processor(Self::render_entry)).size_full())
                .child(probe(&self.env.regions, Region::AgentsPage("list")))
                .into_any_element(),
        };

        let layout_for_paint = layout;
        let width_cell = self.width.clone();
        div()
            .id("agents-page")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .key_context("AgentsPage")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_key(event, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, window, cx| {
                    // The workbench re-focuses itself on a mouse down; take the keyboard back
                    // once the event has been handled.
                    cx.defer_in(window, |this, window, cx| {
                        if !this.search_focus.is_focused(window) {
                            window.focus(&this.focus, cx);
                        }
                    });
                }),
            )
            .child(self.header(&counts, look, cx))
            .children(match presentation.link {
                Link::Offline { last_confirmed_ms } => {
                    Some(self.offline_banner(last_confirmed_ms, look))
                }
                _ => None,
            })
            .child(self.filter_band(look, searching, cx))
            .children(self.usage_section(look, cx))
            .child(body)
            .child(probe(&self.env.regions, Region::AgentsPage("page")))
            .child(
                // The layout above was chosen from the width of the last frame; when this
                // frame's width is on the other side of a line, draw once more.
                canvas(
                    |bounds, _, _| bounds,
                    move |_, bounds, window, cx| {
                        let width: f32 = bounds.size.width.into();
                        let scale = f32::from(window.rem_size()) / 16.0;
                        width_cell.set(width);
                        if Layout::of(width, scale) != layout_for_paint {
                            cx.notify(window.current_view());
                        }
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }
}

impl AgentsPage {
    /// The Usage section (USAGE-LIMITS): per harness its limit windows as bars with their reset
    /// times, and the tokens and cost of the turns Hutan ran today and this week, a cost always
    /// labelled as the estimate it is. Nothing while the daemon has reported no harness, or
    /// while `agents.page_usage` is off.
    fn usage_section(&self, look: Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.env.settings.get::<AgentsSettings>().page_usage {
            return None;
        }
        let (theme, tokens) = (look.theme, look.tokens);
        let usage = self.env.usage.read(cx);
        let refreshing = usage.refreshing();
        let rows = crate::sessions::usage::section(usage.summary()?, now_ms());
        if rows.is_empty() {
            return None;
        }
        let regions = &self.env.regions;
        let rows: Vec<AnyElement> = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let bars = row.limits.iter().enumerate().map(|(n, limit)| {
                    let color = match limit.severity {
                        crate::sessions::usage::Severity::Calm => theme.accent,
                        crate::sessions::usage::Severity::Warning => theme.warning,
                        crate::sessions::usage::Severity::Critical => theme.danger,
                    };
                    let name = if row.harness == "omp" {
                        format!("{} ({})", limit.label, limit.provider)
                    } else {
                        limit.label.clone()
                    };
                    // Two accounts of one harness have a window each: each says whose it is.
                    let name = match &limit.account {
                        Some(account) => format!("{name} · {account}"),
                        None => name,
                    };
                    div()
                        .id(("usage-limit", index * 16 + n))
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .text_color(theme.muted)
                                .child(SharedString::from(name)),
                        )
                        .child(
                            div()
                                .relative()
                                .w(px(96.0))
                                .h(px(6.0))
                                .rounded_full()
                                .bg(theme.border)
                                .child(
                                    div()
                                        .h_full()
                                        .rounded_full()
                                        .w(gpui::relative(limit.fraction))
                                        .bg(color)
                                        .child(probe(regions, Region::UsageBar(index, n))),
                                ),
                        )
                        .child(
                            div()
                                .text_color(match limit.severity {
                                    crate::sessions::usage::Severity::Calm => theme.fg,
                                    crate::sessions::usage::Severity::Warning => theme.warning,
                                    crate::sessions::usage::Severity::Critical => theme.danger,
                                })
                                .child(SharedString::from(limit.percent.clone())),
                        )
                        .children(limit.resets.clone().map(|resets| {
                            div()
                                .text_color(theme.muted)
                                .child(SharedString::from(resets))
                        }))
                });
                div()
                    .id(("usage-row", index))
                    .relative()
                    .flex()
                    .items_start()
                    .gap_x(px(16.0))
                    .child(
                        div()
                            .flex_none()
                            .w(px(96.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(SharedString::from(row.name.clone())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap_x(px(16.0))
                                    .gap_y(px(2.0))
                                    .children(bars)
                                    .children(row.note.clone().map(|note| {
                                        div()
                                            .text_color(theme.muted)
                                            .child(SharedString::from(note))
                                    })),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap_x(px(12.0))
                                    .text_color(theme.muted)
                                    .child(SharedString::from(row.today.clone()))
                                    .child(SharedString::from(row.week.clone())),
                            ),
                    )
                    .child(probe(regions, Region::UsageRow(index)))
                    .into_any_element()
            })
            .collect();
        Some(
            div()
                .id("agents-usage")
                .relative()
                .flex()
                .flex_none()
                .flex_col()
                .gap(px(6.0))
                .px(px(16.0))
                .py(px(8.0))
                .border_b_1()
                .border_color(theme.border)
                .text_size(px(tokens.secondary_font_size))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(div().text_color(theme.muted).child(if refreshing {
                            "Usage · asking the harnesses…"
                        } else {
                            "Usage"
                        }))
                        .child(
                            ui::Button::new("agents-usage-refresh", "Refresh", &theme, &tokens)
                                .ghost()
                                .compact()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.env
                                        .usage
                                        .update(cx, |usage, cx| usage.refresh(true, cx));
                                }))
                                .child(probe(regions, Region::AgentsPage("usage-refresh"))),
                        ),
                )
                .children(rows)
                .child(probe(regions, Region::AgentsPage("usage")))
                .into_any_element(),
        )
    }

    /// The header (44 pt): the title at the page-title size (15, medium: the tab names the page, so
    /// no icon repeats it), the totals in muted text, and `Start agent…` as a ghost button.
    fn header(&self, counts: &Counts, look: Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let scope = self.scope_label(cx);
        div()
            .id("agents-header")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.0))
            .h(px(HEADER))
            .px(px(16.0))
            .child(
                div()
                    .flex_none()
                    .text_size(px(tokens.prose_font_size))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child("Agents"),
            )
            .child(
                div()
                    .id("agents-summary")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(tokens.font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(format!(
                        "{} · {scope}",
                        counts.summary()
                    ))),
            )
            .child(
                ui::Button::new("agents-start", "Start agent…", &theme, &tokens)
                    .ghost()
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(OpenLauncher(LauncherOpen::default())), cx);
                    })
                    .child(probe(&self.env.regions, Region::AgentsPage("start"))),
            )
            .into_any_element()
    }

    /// The name of the scope the page shows, for the header and the scope button.
    fn scope_label(&self, cx: &gpui::App) -> String {
        match self.scope {
            Scope::All => "All workspaces".to_owned(),
            Scope::Workspace(id) => self
                .env
                .sessions
                .read(cx)
                .workspace(id)
                .map_or_else(|| "This workspace".to_owned(), |w| w.name.clone()),
        }
    }

    fn offline_banner(&self, last_confirmed_ms: Option<u64>, look: Look) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let since = match last_confirmed_ms {
            Some(ms) => format!(
                "Offline · last confirmed {}",
                hutan_client::time::relative_to_now(ms)
            ),
            None => "Offline · nothing was confirmed yet".to_owned(),
        };
        div()
            .id("agents-offline")
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.0))
            .h(px(BAND))
            .px(px(16.0))
            .bg(theme.warning.opacity(0.12))
            .text_color(theme.warning)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(since)),
            )
            .child(
                ui::Button::new("agents-daemon", "Daemon controls", &theme, &tokens)
                    .ghost()
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(daemon::Status), cx);
                    })
                    .child(probe(&self.env.regions, Region::AgentsPage("daemon"))),
            )
            .child(probe(&self.env.regions, Region::AgentsPage("offline")))
            .into_any_element()
    }

    /// The toolbar (36 pt): the scope as a ghost menu, the search, and ⋯ (Group by workspace,
    /// Show finished). Below 640 pt of page width it folds: the scope joins ⋯ and only the
    /// search stays beside it, so nothing clips and nothing wraps.
    fn filter_band(&self, look: Look, searching: bool, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let narrow = self.layout == Layout::Narrow;
        let search = div()
            .id("agents-search")
            .relative()
            .flex()
            .flex_1()
            .min_w(px(96.0))
            .max_w(px(360.0))
            .items_center()
            .h(px(28.0))
            .px(px(10.0))
            .outlined(&theme, &tokens)
            .focus_ring(searching, &theme, &tokens)
            .cursor_text()
            .track_focus(&self.search_focus)
            .key_context("AgentsSearch")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_search_key(event, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, window, cx| {
                    cx.defer_in(window, |this, window, cx| {
                        window.focus(&this.search_focus, cx);
                    });
                    cx.stop_propagation();
                }),
            )
            .children(vim_ui::string_field(
                "agents-search-text",
                &self.query_edit,
                &self.query,
                VimConfig::of(&self.env.settings).enabled,
                "Search…",
                searching,
                false,
                self.search_field(cx),
                &theme,
                &tokens,
            ))
            .child(probe(&self.env.regions, Region::AgentsPage("search")));
        let more = ui::IconButton::new("agents-more", Icon::ActionMore, "More", &theme, &tokens)
            .on_click(|event, window, cx| {
                window.dispatch_action(Box::new(ShowAgentsPageMenu(event.position())), cx);
            })
            .child(probe(&self.env.regions, Region::AgentsPage("more")));
        div()
            .id("agents-band")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .h(px(BAND))
            .px(px(16.0))
            .border_b_1()
            .border_color(theme.border)
            .when(!narrow, |s| {
                s.child(
                    ui::Button::new("agents-scope", self.scope_label(cx), &theme, &tokens)
                        .ghost()
                        .max_width(200.0)
                        .on_click(|event, window, cx| {
                            window.dispatch_action(
                                Box::new(ShowAgentsScopeMenu(event.position())),
                                cx,
                            );
                        })
                        .child(glyph(Icon::ActionMenuDown))
                        .child(probe(&self.env.regions, Region::AgentsPage("scope"))),
                )
            })
            .child(search)
            .children(self.group.map(|group| {
                ui::Button::new("agents-group-filter", group.label(), &theme, &tokens)
                    .ghost()
                    .selected(true)
                    .child(glyph(Icon::ActionClose))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.group = None;
                        cx.notify();
                    }))
                    .child(probe(&self.env.regions, Region::AgentsPage("clear-group")))
            }))
            .child(more)
            .into_any_element()
    }

    /// Rows with the shape of rows while the first snapshot has not arrived: never a false
    /// zero, and nothing moves.
    fn skeleton(&self, look: Look) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let bar = |w: f32, h: f32| {
            div()
                .h(px(h))
                .w(px(w))
                .rounded(px(tokens.radius))
                .bg(theme.surface_hover)
        };
        div()
            .id("agents-loading")
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(16.0))
                    .py(px(12.0))
                    .text_color(theme.muted)
                    .child("Connecting to hutand…"),
            )
            .children((0..4).map(|i| {
                div()
                    .id(SharedString::from(format!("agents-skeleton-{i}")))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .h(px(look.row()))
                    .px(px(16.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(bar(14.0, 14.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(bar(180.0, 12.0))
                            .child(bar(280.0, 10.0)),
                    )
            }))
            .child(probe(&self.env.regions, Region::AgentsPage("loading")))
            .into_any_element()
    }

    /// The one teaching empty state: no agent session anywhere.
    fn teaching_state(&self, look: Look, _cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let actions = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(12.0))
            .child(
                div()
                    .max_w(px(420.0))
                    .text_center()
                    .text_color(theme.muted)
                    .child(
                        "Start an agent in a workspace, or connect an integration for an agent \
                         already running in a terminal.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(
                        ui::Button::new(
                            "agents-empty-integrations",
                            "Agent integrations…",
                            &theme,
                            &tokens,
                        )
                        .on_click(|_, window, cx| {
                            window.dispatch_action(
                                Box::new(OpenSettingsFor("integrations".to_owned())),
                                cx,
                            );
                        })
                        .child(probe(&self.env.regions, Region::AgentsPage("integrations"))),
                    )
                    .child(
                        ui::Button::new("agents-empty-start", "Start agent…", &theme, &tokens)
                            .primary()
                            .on_click(|_, window, cx| {
                                window.dispatch_action(
                                    Box::new(OpenLauncher(LauncherOpen::default())),
                                    cx,
                                );
                            })
                            .child(probe(&self.env.regions, Region::AgentsPage("start-empty"))),
                    ),
            )
            .into_any_element();
        div()
            .id("agents-empty")
            .relative()
            .flex_1()
            .min_h_0()
            .child(ui::empty_state(
                Mascot::Empty,
                "Your agents will appear here.",
                Some(actions),
                &theme,
                &tokens,
            ))
            .child(probe(&self.env.regions, Region::AgentsPage("empty")))
            .into_any_element()
    }

    /// Sessions exist, but the scope, the search or the filter hides them all.
    fn no_match_state(&self, look: Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let scope = self.scope_label(cx);
        let actions = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(12.0))
            .child(
                div()
                    .text_color(theme.muted)
                    .child(SharedString::from(format!("Showing {scope}"))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(
                        ui::Button::new("agents-clear-filters", "Clear filters", &theme, &tokens)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.query.clear();
                                this.group = None;
                                cx.notify();
                            }))
                            .child(probe(&self.env.regions, Region::AgentsPage("clear"))),
                    )
                    .when(self.scope != Scope::All, |s| {
                        s.child(
                            ui::Button::new(
                                "agents-show-all",
                                "Show all workspaces",
                                &theme,
                                &tokens,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.scope = Scope::All;
                                cx.notify();
                            }))
                            .child(probe(&self.env.regions, Region::AgentsPage("show-all"))),
                        )
                    }),
            )
            .into_any_element();
        div()
            .id("agents-no-match")
            .relative()
            .flex_1()
            .min_h_0()
            .child(ui::empty_state(
                Mascot::Empty,
                "No agent sessions match this view",
                Some(actions),
                &theme,
                &tokens,
            ))
            .child(probe(&self.env.regions, Region::AgentsPage("no-match")))
            .into_any_element()
    }

    /// One row of the virtualized list. A list lays an item out as a root with the list's width
    /// only *available*; the wrapper takes the width and stretches the row in it.
    fn render_entry(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.listing.rows.get(ix).cloned() else {
            return div().into_any_element();
        };
        self.rows_rendered.set(self.rows_rendered.get() + 1);
        let style = UiStyle::get(cx);
        let look = Look {
            theme: style.theme,
            tokens: style.density.ui(),
            comfortable: style.density == Density::Comfortable,
        };
        let focused = self.focus.contains_focused(window, cx);
        let selected = self
            .selected
            .as_ref()
            .is_some_and(|key| self.listing.key(ix) == Some(key));
        let element = match row {
            Row::Group {
                group,
                count,
                foldable,
                open,
            } => self.group_row(group, count, foldable, open, selected, look, cx),
            Row::Workspace { name, count, .. } => self.workspace_row(&name, count, look),
            Row::Agent { session } => self.agent_row(session, selected, focused, look, cx),
            Row::Child { session, node } => self.child_row(session, &node, ix, selected, look, cx),
        };
        div()
            .flex()
            .flex_col()
            .w_full()
            .child(element)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)] // one call site; a struct would only rename them
    fn group_row(
        &self,
        group: Group,
        count: usize,
        foldable: bool,
        open: bool,
        selected: bool,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let hover = theme.surface_hover;
        // A section label (11 px medium, `muted`): the group and its count, one per group. The
        // warning colour belongs to the rows' glyph and state word, never to the label.
        div()
            .id(SharedString::from(format!("agents-group-{}", group.key())))
            .relative()
            .flex()
            .items_center()
            .gap(px(4.0))
            .h(px(28.0))
            .mt(px(12.0))
            .px(px(16.0))
            .when(selected, |s| s.bg(theme.accent_soft))
            .when(foldable, |s| {
                s.cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_finished();
                        cx.notify();
                    }))
            })
            .child(ui::section_label(
                agents_text::group_label(group, count),
                &theme,
                &tokens,
            ))
            .when(foldable, |s| {
                s.child(
                    div()
                        .flex_none()
                        .text_color(theme.muted)
                        .child(glyph(if open {
                            Icon::ActionExpand
                        } else {
                            Icon::ActionDisclose
                        })),
                )
            })
            .child(probe(&self.env.regions, Region::AgentGroup(group)))
            .into_any_element()
    }

    fn workspace_row(&self, name: &str, count: usize, look: Look) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        div()
            .id(SharedString::from(format!("agents-workspace-{name}")))
            .flex()
            .items_center()
            .gap(px(8.0))
            .h(px(32.0))
            .mt(px(12.0))
            .px(px(16.0))
            .text_color(theme.muted)
            .text_size(px(tokens.secondary_font_size))
            .child(icon(Icon::NavProjectFolder, 14.0, theme.muted))
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.fg)
                    .child(SharedString::from(name.to_owned())),
            )
            .child(SharedString::from(format!("{count}")))
            .into_any_element()
    }

    fn child_row(
        &self,
        session: SessionId,
        node: &str,
        ix: usize,
        selected: bool,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let Some((index, child)) = self.record(session).and_then(|r| {
            r.children
                .iter()
                .enumerate()
                .find(|(_, c)| c.node_id == node)
        }) else {
            return div().into_any_element();
        };
        let hover = theme.surface_hover;
        let color = tone_color(&theme, child.state.tone());
        let node_id = child.node_id.clone();
        let menu_node = child.node_id.clone();
        let blocked_path = (child.blocked && !child.ancestors.is_empty())
            .then(|| format!("{} › {}", child.ancestors.join(" › "), child.label));
        let now = now_ms();
        let time = child.started_at_ms.map(|start| {
            let end = child.ended_at_ms.unwrap_or(now);
            crate::sessions::agents::format_duration(end.saturating_sub(start))
        });
        let mut line = div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex_none()
                    .w(px(14.0))
                    .text_color(color)
                    .child(glyph(child.state.glyph())),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(220.0))
                    .truncate()
                    .child(SharedString::from(
                        blocked_path.unwrap_or_else(|| child.label.clone()),
                    )),
            );
        if let Some(agent_type) = &child.agent_type {
            line = line.child(
                div()
                    .flex_none()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(agent_type.clone())),
            );
        }
        line = line.child(
            div()
                .flex_none()
                .text_color(if child.blocked {
                    theme.warning
                } else {
                    theme.muted
                })
                .text_size(px(tokens.secondary_font_size))
                .child(child.state.label()),
        );
        if let Some(activity) = &child.activity {
            line = line.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(activity.clone())),
            );
        }
        div()
            .id(SharedString::from(format!(
                "agents-child-{session}-{node_id}"
            )))
            .relative()
            .flex()
            .items_center()
            .gap(px(8.0))
            .h(px(look.child()))
            .mx(px(16.0))
            .pl(px(12.0 + 24.0 * child.depth as f32))
            .pr(px(12.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .when(selected, |s| s.bg(theme.accent_soft))
            .when(!selected, move |s| s.hover(move |s| s.bg(hover)))
            .opacity(if child.finished { 0.6 } else { 1.0 })
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.select_row(Some(ix));
                if event.click_count() >= 2 {
                    this.open_session(session, window, cx);
                }
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                move |event: &MouseDownEvent, window, cx| {
                    window.dispatch_action(
                        Box::new(ShowAgentNodeMenu(AgentNodeAt {
                            session: session.to_string(),
                            node: menu_node.clone(),
                            position: event.position,
                        })),
                        cx,
                    );
                },
            )
            .child(line)
            .children(time.map(|t| {
                div()
                    .flex_none()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(t))
            }))
            .child(probe(&self.env.regions, Region::AgentChild(session, index)))
            .into_any_element()
    }
}

mod row;
