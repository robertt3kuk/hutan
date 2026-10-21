//! The `resources` panel (M13-D): an Activity Monitor for everything Hutan runs. One block per
//! live session (title, folder, placement, totals), its process tree below with CPU, memory,
//! threads and command, the agents that reported a process marked, and a row for `hutand`.
//!
//! The daemon samples (M13-C, `docs/resources.md`); the panel only draws, and acts through the
//! daemon: signals, `session.kill`, `agent.interrupt`. It subscribes (`resources.subscribe`, on
//! the shared connection, which subscribes again after a reconnect and announces the samples as
//! `Notice::ResourcesUpdated`) **only while it is drawn**: every draw marks the panel as seen, a
//! timer ends the subscription once a period passes without one, and the next draw starts it
//! again. Hidden, it costs the daemon nothing.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast::error::RecvError;

use gpui::{
    AnyElement, App, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, ParentElement, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Task, Window, canvas, div, prelude::*, px, relative,
};
use hutan_client::{ClientError, Connection, ConnectionState, Notice};
use hutan_core::{NodeKind, SessionId};
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::Request;
use hutan_protocol::resources::{
    MIN_INTERVAL_MS, ProcessState, ResourceSample, ResourcesListRequest,
};

use super::common::{panel_header_with, placement};
use super::resources_model::{
    self as model, AgentLine, Command, Confirmation, Fold, Item, Line, Options, ProcessLine,
    SessionLine, SessionMeta, Target,
};
use crate::sessions::placement_badge;
use crate::settings::{ResourcesSettings, ResourcesSort};
use crate::theme::{DensityExt as _, Theme, Tokens};
use crate::workbench::Region;
use crate::workbench::actions::{ResourceMenuAt, ShowResourceMenu};
use crate::workbench::chat::vim::Outcome;
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::{TextTooltip, UiStyle};
use crate::workbench::text_field::FieldRef;
use crate::workbench::ui::{self, FocusRingExt as _, Mascot, StyledExt as _};
use crate::workbench::vim_ui::{self, FieldEdit, VimConfig};

/// How often the panel checks that it is still drawn; the subscription ends at the first check
/// that finds it was not.
pub const WATCH: Duration = Duration::from_millis(500);

/// A little after an action, the panel asks for a fresh sample instead of waiting for the next
/// one (the process needs a moment to leave the table).
pub const REFRESH_AFTER: Duration = Duration::from_millis(300);

/// Longest interval the setting may ask for.
pub const MAX_INTERVAL_MS: u32 = 60_000;

/// Narrower than this (previous frame), the threads and command columns are left out.
const COMPACT_BELOW: f32 = 520.0;

const COL_PID: f32 = 52.0;
const COL_CPU: f32 = 92.0;
const COL_MEMORY: f32 = 72.0;
const COL_THREADS: f32 = 34.0;
const COL_ACTION: f32 = 24.0;
/// Space the process tree indents by, per level.
const INDENT: f32 = 14.0;

/// How the panel stands with the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// Not drawn yet, or waiting for the first sample.
    Connecting,
    /// Samples arrive.
    Live,
    /// The daemon is older than protocol 0.18.
    Unsupported,
    /// Not connected to the daemon (the connection retries), or it refused the subscription.
    Down(String),
}

pub struct ResourcesPanel {
    env: PanelEnv,
    /// The interval of the subscription the panel wants: set while it is drawn. A task applies
    /// the latest value to the connection, one request at a time, so a quick hide and show
    /// cannot leave the daemon in the wrong state.
    wanted: tokio::sync::watch::Sender<Option<u32>>,
    watcher: Option<Task<()>>,
    /// Drawn since the watcher last looked.
    drawn: bool,
    link: Link,
    sample: Option<Arc<ResourceSample>>,
    samples: usize,
    /// The opposite of the sort setting's order (a second click on the active header).
    reversed: bool,
    filter: String,
    /// Vim state of the filter field (`editor.vim_mode`).
    filter_edit: FieldEdit,
    filter_focus: FocusHandle,
    folds: HashSet<Fold>,
    errors: HashMap<Target, String>,
    busy: HashSet<Target>,
    /// The session "Show in Resources" pointed at.
    highlight: Option<SessionId>,
    scroll_to: Option<SessionId>,
    scroll: ScrollHandle,
    _tasks: Vec<Task<()>>,
}

impl std::fmt::Debug for ResourcesPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourcesPanel")
            .field("subscribed", &self.subscribed())
            .field("link", &self.link)
            .field("samples", &self.samples)
            .finish_non_exhaustive()
    }
}

/// `unsupported`, `forbidden`, … as the protocol spells the code.
fn code_name(code: ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{code:?}"))
}

/// What the row says when the daemon refused (`forbidden: …`, `invalid_state: …`).
fn describe(error: &ClientError) -> String {
    match error {
        ClientError::Remote(body) => format!("{}: {}", code_name(body.code), body.message),
        other => other.to_string(),
    }
}

impl ResourcesPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        cx.observe(&env.sessions, |this, store, cx| {
            // A lost connection takes its subscription with it; the connection subscribes
            // again after reconnecting and announces the first sample.
            if store.read(cx).connection_state() != ConnectionState::Connected {
                this.sample = None;
            }
            cx.notify();
        })
        .detach();
        let (wanted, wanted_rx) = tokio::sync::watch::channel(None);
        let connection = env.sessions.read(cx).connection().clone();
        let tasks = vec![
            Self::drive(connection.clone(), wanted_rx, cx),
            Self::listen(&connection, cx),
        ];
        Self {
            env,
            wanted,
            watcher: None,
            drawn: false,
            link: Link::Connecting,
            sample: None,
            samples: 0,
            reversed: false,
            filter: String::new(),
            filter_edit: FieldEdit::new(),
            filter_focus: cx.focus_handle(),
            folds: HashSet::new(),
            errors: HashMap::new(),
            busy: HashSet::new(),
            highlight: None,
            scroll_to: None,
            scroll: ScrollHandle::new(),
            _tasks: tasks,
        }
    }

    // ---- state the tests and the workbench read -----------------------------------------------

    /// Whether the panel holds a `resources.subscribe` right now.
    pub fn subscribed(&self) -> bool {
        self.wanted.borrow().is_some()
    }

    /// The interval of the subscription the panel asked for.
    pub fn subscribed_interval_ms(&self) -> Option<u32> {
        *self.wanted.borrow()
    }

    /// How the panel stands with the daemon right now.
    pub fn link(&self, cx: &App) -> Link {
        let store = self.env.sessions.read(cx);
        match store.connection_state() {
            ConnectionState::Connected => {}
            ConnectionState::Connecting => return Link::Connecting,
            ConnectionState::Reconnecting { .. } => {
                return Link::Down("not connected".to_owned());
            }
            ConnectionState::Stopped => return Link::Down("the connection is closed".to_owned()),
        }
        if !store.connection().supports_resources() {
            return Link::Unsupported;
        }
        self.link.clone()
    }

    /// How many samples the panel took (tests wait for the next one).
    pub fn samples(&self) -> usize {
        self.samples
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// The order of the panel: the setting, flipped by a second click on its header.
    pub fn sort(&self) -> (ResourcesSort, bool) {
        (
            self.env.settings.get::<ResourcesSettings>().sort,
            self.reversed,
        )
    }

    /// The session "Show in Resources" last pointed at.
    pub fn highlighted(&self) -> Option<SessionId> {
        self.highlight
    }

    /// The inline error of a row, if its last action failed.
    pub fn error(&self, target: &Target) -> Option<&str> {
        self.errors.get(target).map(String::as_str)
    }

    /// Whether an action on `target` is on its way to the daemon.
    pub fn busy(&self, target: &Target) -> bool {
        self.busy.contains(target)
    }

    /// What the sessions store knows about each session of the sample.
    fn meta(
        store: &crate::sessions::SessionsStore,
    ) -> impl Fn(SessionId) -> Option<SessionMeta> + '_ {
        move |id| {
            let info = store.session(id)?;
            Some(SessionMeta {
                title: info.title.clone(),
                workspace: store
                    .workspace(info.workspace_id)
                    .map(|w| w.name.clone())
                    .unwrap_or_default(),
                folder: placement(info),
                kind: info.kind,
                chat_agent: info.chat.as_ref().map(|chat| chat.agent.clone()),
                placement: placement_badge(info),
            })
        }
    }

    /// The lines of the table, in display order, as the panel draws them.
    pub fn lines(&self, cx: &App) -> Vec<Line> {
        let Some(sample) = &self.sample else {
            return Vec::new();
        };
        let settings = self.env.settings.get::<ResourcesSettings>();
        let store = self.env.sessions.read(cx);
        let mut lines = model::lines(
            sample,
            &Self::meta(store),
            &Options {
                sort: settings.sort,
                reversed: self.reversed,
                filter: &self.filter,
                folds: &self.folds,
            },
        );
        // A harness assistant that is open but has no agent running (it starts with the first
        // message, or was put to sleep) has no process to list: one line says so, instead of
        // leaving the user to wonder where Mawas is.
        let filter = self.filter.trim().to_lowercase();
        for info in store.sessions() {
            let idle_assistant = info.state == hutan_core::SessionState::Idle
                && info.chat.as_ref().is_some_and(|chat| chat.assistant);
            if idle_assistant
                && !sample.sessions.iter().any(|s| s.session_id == info.id)
                && (filter.is_empty()
                    || info.title.to_lowercase().contains(&filter)
                    || "idle".contains(&filter))
            {
                lines.push(Line::Note(format!("{} · idle, no process", info.title)));
            }
        }
        lines
    }

    /// The entries of the context menu of a row, from the lines on screen.
    pub fn items_for(
        &self,
        session: SessionId,
        pid: Option<u32>,
        node: Option<&str>,
        cx: &App,
    ) -> Vec<Item> {
        let lines = self.lines(cx);
        for line in &lines {
            match (line, pid, node) {
                (Line::Process { session: s, line }, Some(pid), _)
                    if *s == session && line.pid == pid =>
                {
                    return model::process_items(session, line);
                }
                (Line::Agent(line), None, Some(node))
                    if line.session == session && line.mark.node_id == node =>
                {
                    return model::agent_line_items(line);
                }
                (Line::Session(line), None, None) if line.session == session => {
                    return model::session_items(line);
                }
                _ => {}
            }
        }
        Vec::new()
    }

    /// What to ask before `command`, naming what it ends.
    pub fn confirmation(&self, command: &Command, cx: &App) -> Option<Confirmation> {
        model::confirmation(command, &self.lines(cx))
    }

    // ---- feed ---------------------------------------------------------------------------------

    fn interval_ms(&self) -> u32 {
        self.env
            .settings
            .get::<ResourcesSettings>()
            .interval_ms
            .clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS)
    }

    /// Applies the latest wish of `wanted` to the connection, one request at a time. The
    /// connection remembers an active subscription and renews it after a reconnect.
    fn drive(
        connection: Connection,
        mut wanted: tokio::sync::watch::Receiver<Option<u32>>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut current: Option<u32> = None;
            loop {
                let want = *wanted.borrow_and_update();
                if want != current {
                    match want {
                        Some(interval) => {
                            let answer = connection.resources_subscribe(Some(interval)).await;
                            let alive = this
                                .update(cx, |this, cx| {
                                    match answer {
                                        Ok(subscribed) => this.accept(Arc::new(subscribed.sample)),
                                        // Not connected yet: the connection subscribes (and
                                        // announces the first sample) once it is.
                                        Err(ClientError::Disconnected) => {}
                                        Err(error) => this.link = Link::Down(describe(&error)),
                                    }
                                    cx.notify();
                                })
                                .is_ok();
                            if !alive {
                                break;
                            }
                        }
                        None => connection.resources_unsubscribe().await,
                    }
                    current = want;
                }
                if wanted.changed().await.is_err() {
                    break;
                }
            }
            // The panel is gone: let go of the subscription.
            connection.resources_unsubscribe().await;
        })
    }

    /// Takes the samples the connection announces while the panel wants them.
    fn listen(connection: &Connection, cx: &mut Context<Self>) -> Task<()> {
        let mut notices = connection.notices();
        cx.spawn(async move |this, cx| {
            loop {
                match notices.recv().await {
                    Ok(Notice::ResourcesUpdated { sample }) => {
                        let alive = this
                            .update(cx, |this, cx| {
                                // A sample on its way when the panel was hidden is not shown.
                                if this.subscribed() {
                                    this.accept(Arc::new(*sample));
                                    cx.notify();
                                }
                            })
                            .is_ok();
                        if !alive {
                            break;
                        }
                    }
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => break,
                }
            }
        })
    }

    /// Called from `render`, so it only ever runs while the panel is drawn: wants the
    /// subscription (renewed for a changed interval) and starts the watcher that ends it again
    /// once the panel is no longer drawn.
    fn ensure_feed(&mut self, cx: &mut Context<Self>) {
        self.drawn = true;
        let interval = self.interval_ms();
        self.wanted.send_if_modified(|wanted| {
            let changed = *wanted != Some(interval);
            *wanted = Some(interval);
            changed
        });
        if self.watcher.is_some() {
            return;
        }
        self.watcher = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(WATCH).await;
                let keep = this
                    .update(cx, |this, _| {
                        // Not drawn since the last look: hidden. Unsubscribe; the next draw
                        // subscribes again.
                        if std::mem::take(&mut this.drawn) {
                            true
                        } else {
                            this.stop_feed();
                            false
                        }
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }

    /// Ends the subscription and forgets what it showed.
    fn stop_feed(&mut self) {
        self.wanted.send_replace(None);
        self.watcher = None;
        self.sample = None;
        self.link = Link::Connecting;
    }

    /// Takes a sample that is not older than the one shown; drops the errors and busy marks of
    /// rows that are gone.
    fn accept(&mut self, sample: Arc<ResourceSample>) {
        if self
            .sample
            .as_ref()
            .is_some_and(|current| current.at_ms > sample.at_ms)
        {
            return;
        }
        let live: HashSet<(SessionId, u32)> = sample
            .sessions
            .iter()
            .flat_map(|s| s.processes.iter().map(|p| (s.session_id, p.pid)))
            .collect();
        let sessions: HashSet<SessionId> = sample.sessions.iter().map(|s| s.session_id).collect();
        let nodes: HashSet<(SessionId, String)> = sample
            .sessions
            .iter()
            .flat_map(|s| s.agents.iter().map(|a| (s.session_id, a.node_id.clone())))
            .collect();
        let keep = |target: &Target| match target {
            Target::Session(session) => sessions.contains(session),
            Target::Process(session, pid) => live.contains(&(*session, *pid)),
            // The main agent's interrupt has no node id; it lives as long as its session.
            Target::Agent(session, node) => {
                sessions.contains(session)
                    && (node.is_empty() || nodes.contains(&(*session, node.clone())))
            }
        };
        self.errors.retain(|target, _| keep(target));
        self.folds.retain(|fold| match fold {
            Fold::Session(session) => sessions.contains(session),
            Fold::Process(session, pid) => live.contains(&(*session, *pid)),
        });
        self.link = Link::Live;
        self.samples += 1;
        self.sample = Some(sample);
    }

    // ---- user input ---------------------------------------------------------------------------

    /// Sets the filter text (name, command, pid, agent or session).
    pub fn set_filter(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.filter != text {
            self.filter = text.to_owned();
            self.highlight = None;
            cx.notify();
        }
    }

    /// Sorts by `sort`: the first click picks it (and saves it as `resources.sort`), a second
    /// click on the active one flips the order.
    pub fn sort_by(&mut self, sort: ResourcesSort, cx: &mut Context<Self>) {
        if self.env.settings.get::<ResourcesSettings>().sort == sort {
            self.reversed = !self.reversed;
        } else {
            self.reversed = false;
            if let Err(err) = self
                .env
                .settings
                .write_user("resources.sort", toml::Value::String(sort.key().to_owned()))
            {
                tracing::warn!("cannot save resources.sort: {err}");
            }
        }
        cx.notify();
    }

    /// Folds or unfolds a session's tree, or a process's descendants.
    pub fn toggle_fold(&mut self, fold: Fold, cx: &mut Context<Self>) {
        if !self.folds.remove(&fold) {
            self.folds.insert(fold);
        }
        cx.notify();
    }

    /// "Show in Resources": clears the filter, unfolds the session, marks it and scrolls to it.
    pub fn reveal(&mut self, session: SessionId, cx: &mut Context<Self>) {
        self.filter.clear();
        self.folds.remove(&Fold::Session(session));
        self.highlight = Some(session);
        self.scroll_to = Some(session);
        cx.notify();
    }

    /// Drops the inline error of a row.
    pub fn dismiss_error(&mut self, target: &Target, cx: &mut Context<Self>) {
        if self.errors.remove(target).is_some() {
            cx.notify();
        }
    }

    /// Sends `command` to the daemon; its refusal (`forbidden`, `invalid_state`, `not_found`)
    /// shows on the row. Confirmation is the caller's (the workbench asks first).
    pub fn run(&mut self, command: Command, cx: &mut Context<Self>) {
        let target = command.target();
        self.errors.remove(&target);
        if !self.busy.insert(target.clone()) {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        let request = command.request();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = connection.call::<serde_json::Value>(request).await;
            let sent = outcome.is_ok();
            let updated = this.update(cx, |this, cx| {
                this.busy.remove(&target);
                if let Err(error) = outcome {
                    this.errors.insert(target, describe(&error));
                }
                cx.notify();
            });
            if updated.is_err() || !sent {
                return;
            }
            // The process needs a moment to leave the table; then ask for a fresh sample
            // instead of waiting for the next one.
            cx.background_executor().timer(REFRESH_AFTER).await;
            let fresh = connection
                .call::<ResourceSample>(Request::ResourcesList(ResourcesListRequest {
                    session_id: None,
                }))
                .await;
            if let Ok(sample) = fresh {
                this.update(cx, |this, cx| {
                    // Only while subscribed: a hidden panel keeps nothing.
                    if this.subscribed() {
                        this.accept(Arc::new(sample));
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// How the filter's element reaches its text: the platform's text input and the mouse.
    fn filter_field(&self, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.filter_focus.clone(),
            |this, cx, f| {
                let vim_on = VimConfig::of(&this.env.settings).enabled;
                let mut text = this.filter.clone();
                this.filter_edit.with(&mut text, vim_on, false, f);
                this.set_filter(&text, cx);
            },
        )
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let vim = VimConfig::of(&self.env.settings);
        // Vim's commands and the editing keys; typed text arrives through the filter's
        // platform text input (`filter_field`).
        let mut text = self.filter.clone();
        if self.filter_edit.key(&mut text, event, vim, true, cx) == Outcome::Handled {
            self.set_filter(&text, cx);
            cx.notify();
            cx.stop_propagation();
            return;
        }
        let ks = &event.keystroke;
        if ks.modifiers.platform || ks.modifiers.control || ks.modifiers.function {
            return;
        }
        // Escape (in Normal mode, under Vim mode) clears the filter.
        if ks.key == "escape" {
            self.set_filter("", cx);
            cx.stop_propagation();
        }
    }
}

// ---- rendering ----------------------------------------------------------------------------------

/// A sortable column header: the label, an arrow while it is the sort, a click that sorts.
#[allow(clippy::too_many_arguments)] // one call site per column
fn header_cell(
    label: &'static str,
    width: Option<f32>,
    sort_key: ResourcesSort,
    right: bool,
    theme: &Theme,
    panel: &ResourcesPanel,
    cx: &mut Context<ResourcesPanel>,
) -> AnyElement {
    let hover = theme.surface_hover;
    let (setting, reversed) = panel.sort();
    let on = setting == sort_key;
    // Cpu and memory: the largest first is the unflipped order (▾); names run A to Z (▴).
    let descending = (sort_key != ResourcesSort::Name) != reversed;
    let region = match sort_key {
        ResourcesSort::Cpu => "sort-cpu",
        ResourcesSort::Memory => "sort-memory",
        ResourcesSort::Name => "sort-name",
    };
    div()
        .id(SharedString::from(format!("resources-header-{region}")))
        .relative()
        .flex()
        .items_center()
        .gap(px(2.0))
        .rounded(px(3.0))
        .cursor_pointer()
        .when(right, |s| s.justify_end())
        .when_some(width, |s, w| s.flex_none().w(px(w)))
        .when(width.is_none(), |s| s.flex_1().min_w_0())
        .when(on, |s| s.text_color(theme.fg))
        .hover(move |s| s.bg(hover))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| this.sort_by(sort_key, cx)))
        .child(label)
        .when(on, |s| {
            s.child(crate::icons::glyph(if descending {
                crate::icons::Icon::ActionSortDescending
            } else {
                crate::icons::Icon::ActionSortAscending
            }))
        })
        .child(probe(&panel.env.regions, Region::ResourceControl(region)))
        .into_any_element()
}

/// Text in a fixed-width right-aligned numeric column.
fn number_cell(text: impl Into<SharedString>, width: f32, theme: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .w(px(width))
        .flex()
        .justify_end()
        .text_color(theme.muted)
        .child(text.into())
}

impl ResourcesPanel {
    fn actions_cell(
        &self,
        index: usize,
        session: SessionId,
        pid: Option<u32>,
        node: Option<String>,
        theme: &Theme,
    ) -> AnyElement {
        let hover = theme.surface_hover;
        div()
            .id(SharedString::from(format!("resources-more-{index}")))
            .relative()
            .flex_none()
            .w(px(COL_ACTION))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(3.0))
            .text_color(theme.muted)
            .cursor_pointer()
            .hover(move |s| s.bg(hover).text_color(theme.fg))
            .on_mouse_down(MouseButton::Left, {
                move |event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    window.dispatch_action(
                        Box::new(ShowResourceMenu(ResourceMenuAt {
                            session: session.to_string(),
                            pid,
                            node: node.clone(),
                            position: event.position,
                        })),
                        cx,
                    );
                }
            })
            .child(crate::icons::glyph(crate::icons::Icon::ActionMore))
            .child(probe(&self.env.regions, Region::ResourceMenu(index)))
            .into_any_element()
    }

    /// The right-click handler that opens the same menu as the `⋯` button.
    fn menu_on_right_click(
        row: gpui::Stateful<gpui::Div>,
        session: SessionId,
        pid: Option<u32>,
        node: Option<String>,
    ) -> gpui::Stateful<gpui::Div> {
        row.on_mouse_down(
            MouseButton::Right,
            move |event: &MouseDownEvent, window, cx| {
                window.dispatch_action(
                    Box::new(ShowResourceMenu(ResourceMenuAt {
                        session: session.to_string(),
                        pid,
                        node: node.clone(),
                        position: event.position,
                    })),
                    cx,
                );
            },
        )
    }

    fn render_line(
        &self,
        index: usize,
        line: &Line,
        compact: bool,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let target = line.target();
        let error = target.as_ref().and_then(|t| self.errors.get(t)).cloned();
        let busy = target.as_ref().is_some_and(|t| self.busy.contains(t));
        let body = match line {
            Line::Daemon(process) => {
                self.render_process(index, None, process, compact, theme, tokens, cx)
            }
            Line::Process { session, line } => {
                self.render_process(index, Some(*session), line, compact, theme, tokens, cx)
            }
            Line::Session(session) => {
                self.render_session(index, session, compact, theme, tokens, cx)
            }
            Line::Agent(agent) => self.render_agent(index, agent, compact, theme, tokens),
            Line::Note(text) => div()
                .px(px(tokens.spacing * 3.0))
                .py(px(tokens.spacing))
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .child(SharedString::from(text.clone()))
                .into_any_element(),
        };
        let mut block = div()
            .flex()
            .flex_col()
            .w_full()
            .when(busy, |s| s.opacity(0.55))
            .child(body);
        if let (Some(error), Some(target)) = (error, target) {
            block = block.child(
                div()
                    .id(SharedString::from(format!("resources-error-{index}")))
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing))
                    .mx(px(tokens.spacing))
                    .px(px(tokens.spacing * 2.0))
                    .py(px(2.0))
                    .rounded(px(tokens.radius))
                    .bg(theme.danger.opacity(0.10))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.danger)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.dismiss_error(&target, cx)))
                    .child(div().flex_1().min_w_0().child(SharedString::from(error)))
                    .child(
                        div()
                            .flex_none()
                            .child(crate::icons::glyph(crate::icons::Icon::ActionClose)),
                    ),
            );
        }
        block.into_any_element()
    }

    fn row_base(&self, index: usize, tokens: &Tokens) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!("resources-row-{index}")))
            .relative()
            .flex()
            .items_center()
            .gap(px(tokens.spacing))
            .h(px(tokens.sidebar_row - 4.0))
            .mx(px(tokens.spacing))
            .px(px(tokens.spacing))
            .rounded(px(tokens.radius))
    }

    #[allow(clippy::too_many_arguments)] // one call site
    fn render_process(
        &self,
        index: usize,
        session: Option<SessionId>,
        process: &ProcessLine,
        compact: bool,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = theme.surface_hover;
        let indent = process.depth as f32 * INDENT;
        let stopped = process.state == ProcessState::Stopped;
        let dim = stopped || process.state == ProcessState::Zombie;
        let command = SharedString::from(process.command.clone());
        let mut name = div()
            .flex_1()
            .min_w(px(110.0))
            .flex()
            .items_center()
            .gap(px(tokens.spacing))
            .pl(px(indent))
            .child(match (session, process.foldable) {
                (Some(session), true) => {
                    let fold = Fold::Process(session, process.pid);
                    div()
                        .id(SharedString::from(format!("resources-fold-{index}")))
                        .relative()
                        .flex_none()
                        .w(px(12.0))
                        .cursor_pointer()
                        .text_color(theme.muted)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_fold(fold, cx)))
                        .child(crate::icons::glyph(if process.collapsed {
                            crate::icons::Icon::ActionDisclose
                        } else {
                            crate::icons::Icon::ActionExpand
                        }))
                        .child(probe(&self.env.regions, Region::ResourceFold(index)))
                        .into_any_element()
                }
                _ => div().flex_none().w(px(12.0)).into_any_element(),
            })
            .child(
                div()
                    .flex_none()
                    .max_w(px(180.0))
                    .truncate()
                    .when(session.is_none(), |s| s.medium())
                    .child(SharedString::from(process.name.clone())),
            );
        if session.is_none() {
            name = name.child(
                div()
                    .flex_none()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child("Hutan daemon"),
            );
        }
        match process.state {
            ProcessState::Stopped => {
                name = name.child(ui::state_label(
                    crate::icons::Icon::StateStopped,
                    "stopped",
                    theme.warning,
                    false,
                    theme,
                    tokens,
                ));
            }
            ProcessState::Zombie => {
                name = name.child(ui::state_label(
                    crate::icons::Icon::StateFailed,
                    "zombie",
                    theme.danger,
                    false,
                    theme,
                    tokens,
                ));
            }
            _ => {}
        }
        if let Some(agent) = &process.agent {
            let tip = SharedString::from(format!(
                "Agent {} ({})",
                agent.label,
                if agent.kind == NodeKind::Main {
                    "main agent"
                } else {
                    "subagent"
                }
            ));
            name = name.child(
                div()
                    .id(SharedString::from(format!("resources-agent-{index}")))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.accent)
                    .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
                    .child(div().size(px(7.0)).rounded_full().bg(theme.accent))
                    .child(SharedString::from(agent.label.clone())),
            );
        }
        let cpu_text = model::format_cpu(process.cpu);
        let bar = model::cpu_fraction(process.cpu);
        let mut row = self
            .row_base(index, tokens)
            .when(dim, |s| s.opacity(0.6))
            .hover(move |s| s.bg(hover))
            .child(name)
            .child(number_cell(process.pid.to_string(), COL_PID, theme))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .w(px(COL_CPU))
                    .h(px(tokens.font_size + 2.0))
                    .flex()
                    .items_center()
                    .justify_end()
                    .pr(px(4.0))
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top(px(2.0))
                            .bottom(px(2.0))
                            .w(relative(bar))
                            .rounded(px(2.0))
                            .bg(theme.accent_soft),
                    )
                    .child(div().relative().child(cpu_text)),
            )
            .child(number_cell(
                model::format_memory(process.rss),
                COL_MEMORY,
                theme,
            ));
        if !compact {
            row = row
                .child(number_cell(process.threads.to_string(), COL_THREADS, theme))
                .child(
                    div()
                        .id(SharedString::from(format!("resources-command-{index}")))
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .pl(px(tokens.spacing))
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .tooltip({
                            let command = command.clone();
                            move |_, cx| cx.new(|_| TextTooltip(command.clone())).into()
                        })
                        .child(command),
                );
        }
        let Some(session) = session else {
            return row
                .child(div().flex_none().w(px(COL_ACTION)))
                .child(probe(&self.env.regions, Region::ResourceRow(index)))
                .into_any_element();
        };
        let node = None;
        let row = row
            .child(self.actions_cell(index, session, Some(process.pid), node.clone(), theme))
            .child(probe(&self.env.regions, Region::ResourceRow(index)));
        Self::menu_on_right_click(row, session, Some(process.pid), node).into_any_element()
    }

    fn render_session(
        &self,
        index: usize,
        line: &SessionLine,
        compact: bool,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let session = line.session;
        let hover = theme.surface_hover;
        let highlighted = self.highlight == Some(session);
        let mut name = div()
            .flex_1()
            .min_w(px(110.0))
            .flex()
            .items_center()
            .gap(px(tokens.spacing))
            .child(div().flex_none().w(px(12.0)).text_color(theme.muted).child(
                crate::icons::glyph(if line.collapsed {
                    crate::icons::Icon::ActionDisclose
                } else {
                    crate::icons::Icon::ActionExpand
                }),
            ))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .medium()
                    .child(SharedString::from(line.title.clone())),
            );
        if !line.folder.is_empty() {
            name = name.child(
                div()
                    .flex_none()
                    .max_w(px(140.0))
                    .truncate()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(line.folder.clone())),
            );
        }
        if let Some(badge) = &line.placement {
            let (color, text) = if badge.shared {
                (theme.warning, badge.text.clone())
            } else {
                (theme.muted, badge.text.clone())
            };
            let tip = SharedString::from(badge.tooltip.clone());
            name = name.child(
                div()
                    .id(SharedString::from(format!("resources-placement-{index}")))
                    .flex_none()
                    .max_w(px(130.0))
                    .truncate()
                    .px(px(tokens.spacing * 2.0))
                    .rounded(px(crate::theme::pill_radius()))
                    .bg(color.opacity(if badge.shared { 0.14 } else { 0.0 }))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(color)
                    .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
                    .child(SharedString::from(text)),
            );
        }
        let processes = match line.totals.processes {
            1 => "1 proc".to_owned(),
            n => format!("{n} procs"),
        };
        let mut row = self
            .row_base(index, tokens)
            .mt(px(tokens.spacing))
            .bg(if highlighted {
                theme.accent_soft
            } else {
                theme.surface
            })
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.highlight = None;
                this.toggle_fold(Fold::Session(session), cx);
            }))
            .child(name)
            .child(number_cell(processes, COL_PID, theme))
            .child(number_cell(
                model::format_cpu(Some(line.totals.cpu_percent)),
                COL_CPU,
                theme,
            ))
            .child(number_cell(
                model::format_memory(line.totals.rss_bytes),
                COL_MEMORY,
                theme,
            ));
        if !compact {
            row = row.child(div().flex_none().w(px(COL_THREADS))).child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .pl(px(tokens.spacing))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(line.workspace.clone())),
            );
        }
        let row = row
            .child(self.actions_cell(index, session, None, None, theme))
            .child(probe(&self.env.regions, Region::ResourceRow(index)));
        Self::menu_on_right_click(row, session, None, None).into_any_element()
    }

    fn render_agent(
        &self,
        index: usize,
        line: &AgentLine,
        compact: bool,
        theme: &Theme,
        tokens: &Tokens,
    ) -> AnyElement {
        let session = line.session;
        let hover = theme.surface_hover;
        let node = line.mark.node_id.clone();
        let kind = if line.mark.kind == NodeKind::Main {
            "main agent"
        } else {
            "subagent"
        };
        let name = div()
            .flex_1()
            .min_w(px(110.0))
            .flex()
            .items_center()
            .gap(px(tokens.spacing))
            .pl(px(INDENT + 12.0 + tokens.spacing))
            .text_color(theme.accent)
            .child(
                div()
                    .flex_none()
                    .size(px(7.0))
                    .rounded_full()
                    .bg(theme.accent),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(line.mark.label.clone())),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(kind),
            );
        let mut row = self
            .row_base(index, tokens)
            .hover(move |s| s.bg(hover))
            .child(name)
            .child(div().flex_none().w(px(COL_PID)))
            .child(div().flex_none().w(px(COL_CPU)))
            .child(div().flex_none().w(px(COL_MEMORY)));
        if !compact {
            row = row.child(div().flex_none().w(px(COL_THREADS))).child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .pl(px(tokens.spacing))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child("no process of its own"),
            );
        }
        let row = row
            .child(self.actions_cell(index, session, None, Some(node.clone()), theme))
            .child(probe(&self.env.regions, Region::ResourceRow(index)));
        Self::menu_on_right_click(row, session, None, Some(node)).into_any_element()
    }
}

impl Render for ResourcesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.env.regions.rendered(cx.entity_id());
        self.ensure_feed(cx);
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let lines = self.lines(cx);
        let link = self.link(cx);
        let compact = self
            .env
            .regions
            .get(Region::ResourceControl("panel"))
            .is_some_and(|bounds| bounds.size.width < px(COMPACT_BELOW));

        // The headline numbers: what the sessions add up to.
        let (mut processes, mut cpu, mut rss) = (0u32, 0.0f64, 0u64);
        for line in &lines {
            if let Line::Session(s) = line {
                processes += s.totals.processes;
                cpu += s.totals.cpu_percent;
                rss += s.totals.rss_bytes;
            }
        }
        let has_sessions = lines.iter().any(|l| matches!(l, Line::Session(_)));
        let summary = ui::counter(
            format!(
                "{processes} {} · {} · {}",
                if processes == 1 {
                    "process"
                } else {
                    "processes"
                },
                model::format_cpu(Some(cpu)),
                model::format_memory(rss)
            ),
            &theme,
            &tokens,
        );

        // A reveal that finds nothing running says so.
        let missing = self.highlight.filter(|session| {
            self.sample.is_some()
                && !lines
                    .iter()
                    .any(|l| matches!(l, Line::Session(s) if s.session == *session))
        });
        let missing_title = missing.map(|session| {
            self.env
                .sessions
                .read(cx)
                .session(session)
                .map_or_else(|| session.to_string(), |s| s.title.clone())
        });

        let filter_focused = self.filter_focus.is_focused(window);
        let filter_box = div()
            .id("resources-filter")
            .key_context("ResourcesFilter")
            .track_focus(&self.filter_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .relative()
            .flex_none()
            .flex()
            .items_center()
            .mx(px(tokens.spacing * 2.0))
            .mb(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .h(px(tokens.sidebar_row))
            .rounded(px(tokens.radius))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .focus_ring(filter_focused, &theme, &tokens)
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, window, cx| {
                    // The dock re-focuses the workbench on this same mouse down; take focus
                    // after the event has been handled.
                    cx.defer_in(window, |this, window, cx| {
                        window.focus(&this.filter_focus, cx);
                    });
                }),
            )
            .children(vim_ui::string_field(
                "resources-filter-text",
                &self.filter_edit,
                &self.filter,
                VimConfig::of(&self.env.settings).enabled,
                "Filter by process, command or session",
                filter_focused,
                false,
                self.filter_field(cx),
                &theme,
                &tokens,
            ))
            .child(probe(&self.env.regions, Region::ResourceControl("filter")));

        let columns = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing))
            .mx(px(tokens.spacing))
            .px(px(tokens.spacing))
            .h(px(tokens.sidebar_row - 4.0))
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.muted)
            .child(
                div()
                    .flex_1()
                    .min_w(px(110.0))
                    .flex()
                    .pl(px(INDENT))
                    .child(header_cell(
                        "Name",
                        None,
                        ResourcesSort::Name,
                        false,
                        &theme,
                        self,
                        cx,
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(COL_PID))
                    .flex()
                    .justify_end()
                    .child("PID"),
            )
            .child(header_cell(
                "CPU",
                Some(COL_CPU),
                ResourcesSort::Cpu,
                true,
                &theme,
                self,
                cx,
            ))
            .child(header_cell(
                "Memory",
                Some(COL_MEMORY),
                ResourcesSort::Memory,
                true,
                &theme,
                self,
                cx,
            ))
            .when(!compact, |s| {
                s.child(
                    div()
                        .flex_none()
                        .w(px(COL_THREADS))
                        .flex()
                        .justify_end()
                        .child("Thr"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .pl(px(tokens.spacing))
                        .child("Command"),
                )
            })
            .child(div().flex_none().w(px(COL_ACTION)));

        let mut list = div()
            .id("resources-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .pb(px(tokens.spacing));
        if let Some(title) = missing_title {
            list = list.child(
                div()
                    .px(px(tokens.spacing * 3.0))
                    .py(px(tokens.spacing))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.warning)
                    .child(SharedString::from(format!(
                        "“{title}” has no running process."
                    ))),
            );
        }
        let mut scroll_index = None;
        for (index, line) in lines.iter().enumerate() {
            if self.scroll_to.is_some()
                && matches!(line, Line::Session(s) if Some(s.session) == self.scroll_to)
            {
                scroll_index = Some(index);
            }
            list = list.child(self.render_line(index, line, compact, &theme, &tokens, cx));
        }
        if let Some(index) = scroll_index {
            self.scroll_to = None;
            // The daemon row (and a note above) come first; the child index is the line's.
            self.scroll.scroll_to_item(index);
        } else if self.sample.is_some() {
            self.scroll_to = None;
        }
        if !has_sessions {
            let (mascot, text): (Mascot, String) = match &link {
                Link::Connecting => (Mascot::Thinking, "Reading the process table…".to_owned()),
                Link::Unsupported => (
                    Mascot::Error,
                    "This daemon does not report resources (it is older than protocol 0.18). \
                     Restart it from this version of Hutan."
                        .to_owned(),
                ),
                Link::Down(reason) => (
                    Mascot::Sleep,
                    format!("Cannot reach the daemon ({reason}). Trying again."),
                ),
                Link::Live if !self.filter.trim().is_empty() => (
                    Mascot::Empty,
                    format!("Nothing matches “{}”.", self.filter.trim()),
                ),
                Link::Live => (
                    Mascot::Sleep,
                    "Nothing is running. Sessions with a process appear here.".to_owned(),
                ),
            };
            list = list.child(ui::empty_state(mascot, text, None, &theme, &tokens).h(px(280.0)));
        }

        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .child(panel_header_with(
                "Resources",
                "resources",
                if has_sessions {
                    vec![summary.into_any_element()]
                } else {
                    Vec::new()
                },
                cx,
            ))
            .child(filter_box)
            .child(columns)
            .child(list)
            .child(probe(&self.env.regions, Region::ResourceControl("panel")))
            .child(
                // The layout above was chosen from the width of the last frame; when this
                // frame's width is on the other side of the line, draw once more.
                canvas(
                    |bounds, _, _| bounds,
                    move |_, bounds, window, cx| {
                        if (bounds.size.width < px(COMPACT_BELOW)) != compact {
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
