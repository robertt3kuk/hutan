//! The `context` panel (CONTEXT-PANEL; protocol 0.59 `agent.insight`): how the context of the
//! agent session in view is filled, what each turn used, which tools and files it touched, what
//! it cost and how long it took, and the timeline of its turns — each with *Fork…* and
//! *Rewind…*.
//!
//! - **It follows the session in view** (the active tab of the active pane; a tab that is no
//!   session, such as a file, leaves it where it was). A chat is read with `agent.insight` once
//!   the daemon knows the window looks at it (`client.focus`), then kept current by
//!   `agent.insight_updated`, which the daemon sends only to connections that look at the
//!   session. A terminal agent has no insight yet: the panel says what Hutan sees of it.
//! - **Honest.** A block nobody reported is absent, never zero. What a harness never reports
//!   (`unsupported`) is named in words: *Codex does not report cost*. A figure the harness
//!   estimated is written with `≈`, a cost is the harness's own estimate and says so.
//! - **Measure context** sends `/context` to OMP and Claude Code (`agent.measure_context`), a
//!   turn that costs no tokens and shows in the chat as an exchange, so it is only ever sent by
//!   this button.
//! - **Cheap.** The panel does nothing while it is not on screen; the turns are a `list`, so a
//!   frame builds the rows in view only; one read at a time.

use std::collections::HashMap;

use gpui::{
    AnyElement, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, ListAlignment,
    ListOffset, ListState, MouseButton, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Task, Window, div, prelude::*, px, relative,
};
use hutan_client::{ConnectionState, Notice};
use hutan_core::SessionId;
use hutan_core::TurnId;
use hutan_core::insight::{AgentInsight, BreakdownKind};
use hutan_protocol::control::ErrorCode;
use hutan_protocol::messages::SessionInfo;
use tokio::sync::broadcast::error::RecvError;

use super::common::{panel_toolbar, session_icon_with};
use super::context_model::{self as model, Swatch, TurnLine, TurnState};
use crate::sessions::card;
use crate::settings::ContextPanelSettings;
use crate::theme::{DensityExt as _, Theme, Tokens};
use crate::workbench::actions::{
    self, ChangedFileAt, CheckpointAt, ChosenFile, OpenChosenFile, OpenSettingsFor,
};
use crate::workbench::files::FilePlace;
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::{TextTooltip, UiStyle};
use crate::workbench::ui;
use crate::workbench::view::Region;

/// What an item of the list is about this tall until it is drawn.
const ROW_ESTIMATE: f32 = 56.0;
/// Below this width the fill chart and the sources fold away (the dock is 350 pt by default).
pub const FOLD_WIDTH: f32 = 320.0;
/// Bars of the fill chart: the newest turns.
const CHART_TURNS: usize = 40;
/// Files listed before *Show more*.
const FILES_SHOWN: usize = 8;

/// What the panel has read of the followed session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Load {
    /// Nothing was asked: no chat in view, not shown, or waiting for the daemon to know the focus.
    Idle,
    Loading,
    Ready,
    Failed(String),
}

/// How a *Measure context* stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Measure {
    Idle,
    /// `/context` is out; its answer is not in.
    Running,
    /// The harness answered with something that is not a context report.
    Unreadable,
    Failed(String),
}

/// What the panel has to say instead of an insight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Empty {
    /// No agent session is in view.
    NoSession,
    /// A session that is neither a chat nor a terminal with an agent in it.
    NotAnAgent,
    /// A terminal with an agent: its state is all Hutan has.
    Terminal {
        app: Option<String>,
        hooks: bool,
    },
    NotConnected,
    /// The daemon predates `agent.insight`.
    Unsupported,
    Loading,
    Failed(String),
    /// A chat nothing has reported on yet.
    Nothing,
}

/// One block of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    Identity,
    Empty(Empty),
    Context,
    Chart,
    TurnsHeader,
    /// Index into `turns` (newest first).
    Turn(usize),
    TurnsNote,
    Tools,
    Files,
    Totals,
    Notes,
    Sources,
}

pub struct ContextPanel {
    env: PanelEnv,
    focus: FocusHandle,
    /// On screen: the panel does nothing otherwise.
    visible: bool,
    /// The window is active: the daemon is told this window looks at the session, and the panel
    /// reads the insight once it knows.
    active: bool,
    /// The session followed.
    session: Option<SessionId>,
    insight: Option<AgentInsight>,
    load: Load,
    /// The session and connection the last read was asked for.
    asked: Option<(SessionId, u64)>,
    reading: Option<Task<()>>,
    measure: Measure,
    measuring: Option<Task<()>>,
    /// First lines of the prompts of the followed session's turns, from the transcript the window
    /// holds (the insight carries no message text), and what they were read from.
    prompts: HashMap<TurnId, String>,
    prompts_from: Option<(SessionId, usize)>,
    items: Vec<Item>,
    turns: Vec<TurnLine>,
    list: ListState,
    bar: hutan_ui::Scrollbar,
    listener: Option<Task<()>>,
    /// Files the user asked to see past the first few.
    all_files: bool,
    /// What the rebuilt list was made from, so a store change that changes none of it rebuilds
    /// nothing.
    built_from: Option<Built>,
    /// How many insight snapshots were read or pushed (tests wait on it).
    updates: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Built {
    session: Option<SessionId>,
    seq: Option<u64>,
    load: Load,
    connected: bool,
    prompts: usize,
    all_files: bool,
    sources: bool,
    chart: bool,
}

impl std::fmt::Debug for ContextPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextPanel")
            .field("session", &self.session)
            .field("load", &self.load)
            .finish_non_exhaustive()
    }
}

impl ContextPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        cx.observe(&env.sessions, |this, _, cx| this.follow(cx))
            .detach();
        let checkpoints = env.sessions.read(cx).checkpoints().clone();
        cx.observe(&checkpoints, |this, _, cx| {
            if this.visible && this.session.is_some() {
                cx.notify();
            }
        })
        .detach();
        let list = ListState::new(0, ListAlignment::Top, px(400.0));
        let bar = hutan_ui::Scrollbar::for_list(&list, ROW_ESTIMATE);
        Self {
            env,
            focus: cx.focus_handle(),
            visible: false,
            active: false,
            session: None,
            insight: None,
            load: Load::Idle,
            asked: None,
            reading: None,
            measure: Measure::Idle,
            measuring: None,
            prompts: HashMap::new(),
            prompts_from: None,
            items: Vec::new(),
            turns: Vec::new(),
            list,
            bar,
            listener: None,
            all_files: false,
            built_from: None,
            updates: 0,
        }
    }

    // ---- what tests and the workbench read ------------------------------------------------------

    /// The session the panel follows.
    pub fn session(&self) -> Option<SessionId> {
        self.session
    }

    /// What was read of it.
    pub fn insight(&self) -> Option<&AgentInsight> {
        self.insight.as_ref()
    }

    pub fn load(&self) -> &Load {
        &self.load
    }

    pub fn measure_state(&self) -> &Measure {
        &self.measure
    }

    /// The turn rows, newest first.
    pub fn turn_lines(&self) -> &[TurnLine] {
        &self.turns
    }

    /// What the panel says instead of an insight, when it does.
    pub fn empty(&self) -> Option<&Empty> {
        self.items.iter().find_map(|item| match item {
            Item::Empty(empty) => Some(empty),
            _ => None,
        })
    }

    /// Snapshots read or pushed so far.
    pub fn updates(&self) -> u64 {
        self.updates
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Turns of the followed session whose prompt the panel does not have.
    pub fn turns_without_prompt(&self) -> Vec<TurnId> {
        self.insight
            .iter()
            .flat_map(|insight| insight.turns.iter())
            .filter_map(|turn| turn.turn_id)
            .filter(|id| !self.prompts.contains_key(id))
            .collect()
    }

    /// What the prompts were last read from: the session and how many entries its transcript had.
    pub fn prompts_from(&self) -> Option<(SessionId, usize)> {
        self.prompts_from
    }

    /// The prompts of `session`'s turns, read from its transcript when it had `entries` entries.
    pub fn set_prompts(
        &mut self,
        session: SessionId,
        entries: usize,
        found: HashMap<TurnId, String>,
        cx: &mut Context<Self>,
    ) {
        if self.session != Some(session) {
            return;
        }
        self.prompts_from = Some((session, entries));
        self.prompts.extend(found);
        self.rebuild(cx);
    }

    /// The workbench says whether the panel is on screen and the window active.
    pub fn set_visible(&mut self, visible: bool, active: bool, cx: &mut Context<Self>) {
        if self.visible == visible && self.active == active {
            return;
        }
        let looked = |visible: bool, active: bool| visible && active;
        // Looking at the session again: what happened meanwhile is read once the daemon knows.
        if looked(visible, active) && !looked(self.visible, self.active) {
            self.asked = None;
        }
        self.visible = visible;
        self.active = active;
        if visible {
            self.follow(cx);
        }
        cx.notify();
    }

    // ---- following ------------------------------------------------------------------------------

    /// The session the panel should show: the one in view when it is a session, else the one it
    /// showed while that still exists.
    fn target(&self, cx: &gpui::App) -> Option<SessionId> {
        let store = self.env.sessions.read(cx);
        let shown = store
            .shown_session()
            .filter(|id| store.session(*id).is_some());
        shown.or(self.session.filter(|id| store.session(*id).is_some()))
    }

    fn follow(&mut self, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }
        let target = self.target(cx);
        if target != self.session {
            self.session = target;
            self.insight = None;
            self.load = Load::Idle;
            self.asked = None;
            self.reading = None;
            self.measure = Measure::Idle;
            self.measuring = None;
            self.prompts.clear();
            self.prompts_from = None;
            self.all_files = false;
            self.built_from = None;
        }
        self.listen(cx);
        self.read_if_needed(cx);
        self.rebuild_if_changed(cx);
    }

    /// Reads the insight of a chat once the daemon knows the window looks at it, and again on a
    /// new connection (the daemon's counters start over there).
    fn read_if_needed(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session else {
            return;
        };
        let store = self.env.sessions.read(cx);
        let is_chat = store
            .session(session)
            .is_some_and(|info| info.chat.is_some());
        if !is_chat
            || store.connection_state() != ConnectionState::Connected
            || !store.connection().supports_agent_insight()
        {
            return;
        }
        let generation = store.connection().generation();
        let waiting = self.visible && self.active && !store.focus_settled_for(session);
        if self.asked == Some((session, generation)) || waiting {
            return;
        }
        self.asked = Some((session, generation));
        self.read(cx);
    }

    fn read(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session else {
            return;
        };
        let connection = self.env.sessions.read(cx).connection().clone();
        if self.insight.is_none() {
            self.load = Load::Loading;
        }
        self.reading = Some(cx.spawn(async move |this, cx| {
            let answer = connection.agent_insight(session, None).await;
            this.update(cx, |this, cx| {
                this.reading = None;
                if this.session != Some(session) {
                    return;
                }
                match answer {
                    Ok(answer) => {
                        this.load = Load::Ready;
                        this.insight = Some(answer.insight);
                        this.updates += 1;
                    }
                    Err(error) => {
                        this.load = Load::Failed(error.to_string());
                    }
                }
                this.rebuild(cx);
            })
            .ok();
        }));
        self.rebuild(cx);
    }

    /// Follows `agent.insight_updated`: a snapshot of the followed session replaces what the
    /// panel holds when it is newer. A receiver that fell behind reads again.
    fn listen(&mut self, cx: &mut Context<Self>) {
        if self.listener.is_some() {
            return;
        }
        let mut notices = self.env.sessions.read(cx).connection().notices();
        self.listener = Some(cx.spawn(async move |this, cx| {
            loop {
                let notice = match notices.recv().await {
                    Ok(Notice::AgentInsight {
                        session_id,
                        insight,
                    }) => Some((session_id, insight)),
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => None,
                    Err(RecvError::Closed) => break,
                };
                if this
                    .update(cx, |this, cx| this.on_insight(notice, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn on_insight(
        &mut self,
        notice: Option<(SessionId, Box<AgentInsight>)>,
        cx: &mut Context<Self>,
    ) {
        match notice {
            Some((session, insight)) => {
                if self.session != Some(session) {
                    return;
                }
                if self
                    .insight
                    .as_ref()
                    .is_some_and(|held| held.seq >= insight.seq)
                {
                    return;
                }
                self.insight = Some(*insight);
                self.load = Load::Ready;
                self.updates += 1;
                self.rebuild(cx);
            }
            // Missed some: ask again.
            None => {
                self.asked = None;
                self.read_if_needed(cx);
            }
        }
    }

    // ---- measuring ------------------------------------------------------------------------------

    /// *Measure context*: `/context` goes to the harness; the answer's breakdown replaces the
    /// panel's insight.
    pub fn measure(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session else {
            return;
        };
        if self.measure == Measure::Running {
            return;
        }
        self.measure = Measure::Running;
        let connection = self.env.sessions.read(cx).connection().clone();
        self.measuring = Some(cx.spawn(async move |this, cx| {
            let answer = connection.agent_measure_context(session).await;
            this.update(cx, |this, cx| {
                this.measuring = None;
                if this.session != Some(session) {
                    return;
                }
                this.measure = match answer {
                    Ok(measured) => {
                        let outcome = if measured.measured {
                            Measure::Idle
                        } else {
                            Measure::Unreadable
                        };
                        if this
                            .insight
                            .as_ref()
                            .is_none_or(|held| held.seq <= measured.insight.seq)
                        {
                            this.insight = Some(measured.insight);
                            this.updates += 1;
                        }
                        outcome
                    }
                    Err(hutan_client::ClientError::Remote(body))
                        if body.code == ErrorCode::InvalidState =>
                    {
                        Measure::Failed(
                            "The agent is busy: measure when its turn has ended.".to_owned(),
                        )
                    }
                    Err(error) => Measure::Failed(error.to_string()),
                };
                this.rebuild(cx);
            })
            .ok();
        }));
        self.rebuild(cx);
    }

    // ---- the list -------------------------------------------------------------------------------

    fn info<'a>(&self, cx: &'a gpui::App) -> Option<&'a SessionInfo> {
        self.env.sessions.read(cx).session(self.session?)
    }

    /// What the panel says in place of an insight, or `None` when it has one to show.
    fn situation(&self, cx: &gpui::App) -> Option<Empty> {
        let store = self.env.sessions.read(cx);
        let Some(info) = self.session.and_then(|id| store.session(id)) else {
            return Some(Empty::NoSession);
        };
        if info.chat.is_none() {
            return Some(match hutan_agents_view::attention::agent_app(info) {
                Some(app) => Empty::Terminal {
                    app: Some(app.to_owned()),
                    hooks: !card::without_hooks(info),
                },
                None if info.terminal_agent.is_some() => Empty::Terminal {
                    app: None,
                    hooks: false,
                },
                None => Empty::NotAnAgent,
            });
        }
        if store.connection_state() != ConnectionState::Connected {
            return Some(Empty::NotConnected);
        }
        if !store.connection().supports_agent_insight() {
            return Some(Empty::Unsupported);
        }
        match (&self.load, &self.insight) {
            (_, Some(insight)) if insight.sources.is_empty() && insight.turns.is_empty() => {
                Some(Empty::Nothing)
            }
            (_, Some(_)) => None,
            (Load::Failed(message), None) => Some(Empty::Failed(message.clone())),
            _ => Some(Empty::Loading),
        }
    }

    fn rebuild_if_changed(&mut self, cx: &mut Context<Self>) {
        let built = self.built(cx);
        if self.built_from.as_ref() != Some(&built) {
            self.rebuild(cx);
        }
    }

    fn built(&self, cx: &gpui::App) -> Built {
        let settings = self.env.settings.get::<ContextPanelSettings>();
        Built {
            session: self.session,
            seq: self.insight.as_ref().map(|insight| insight.seq),
            load: self.load.clone(),
            connected: self.env.sessions.read(cx).connection_state() == ConnectionState::Connected,
            prompts: self.prompts.len(),
            all_files: self.all_files,
            sources: settings.show_sources,
            chart: settings.show_fill_chart,
        }
    }

    /// Lays the blocks out again after anything they are made of changed, keeping the scroll.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let settings = self.env.settings.get::<ContextPanelSettings>();
        let mut items = Vec::new();
        let mut turns = Vec::new();
        match self.situation(cx) {
            Some(Empty::NoSession) => items.push(Item::Empty(Empty::NoSession)),
            Some(empty) => {
                items.push(Item::Identity);
                items.push(Item::Empty(empty));
            }
            None => {
                if let Some(insight) = self.insight.as_ref() {
                    let prompts = &self.prompts;
                    turns = model::turn_lines(insight, &|id| prompts.get(&id).cloned());
                    items.push(Item::Identity);
                    if insight.context.is_some() || insight.can_measure {
                        items.push(Item::Context);
                    }
                    if settings.show_fill_chart && model::fill_chart(insight, CHART_TURNS).is_some()
                    {
                        items.push(Item::Chart);
                    }
                    if !turns.is_empty() {
                        items.push(Item::TurnsHeader);
                        items.extend((0..turns.len()).map(Item::Turn));
                        if insight.turns_truncated {
                            items.push(Item::TurnsNote);
                        }
                    }
                    if insight.tools.as_ref().is_some_and(|tools| tools.calls > 0) {
                        items.push(Item::Tools);
                    }
                    // The block is there when the turns say files changed, whether or not the
                    // agent named them (OMP's edit events carry no path): a count with a line
                    // that says so, not nothing (FX-UI-B).
                    if insight.files.as_ref().is_some_and(|files| files.count > 0)
                        || model::files_changed_in_turns(insight) > 0
                    {
                        items.push(Item::Files);
                    }
                    if !model::totals_pairs(insight).is_empty()
                        || !model::time_pairs(insight).is_empty()
                    {
                        items.push(Item::Totals);
                    }
                    if !insight.unsupported.is_empty() {
                        items.push(Item::Notes);
                    }
                    if settings.show_sources && !insight.sources.is_empty() {
                        items.push(Item::Sources);
                    }
                }
            }
        }
        let top = self.list.logical_scroll_top();
        self.list.reset(items.len());
        self.list.scroll_to(ListOffset {
            item_ix: top.item_ix.min(items.len().saturating_sub(1)),
            offset_in_item: if top.item_ix < items.len() {
                top.offset_in_item
            } else {
                px(0.0)
            },
        });
        self.items = items;
        self.turns = turns;
        self.built_from = Some(self.built(cx));
        cx.notify();
    }
}

impl Focusable for ContextPanel {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

// ---- drawing --------------------------------------------------------------------------------------

/// The colour a category is drawn in; the list names it in words as well, so no meaning rests on
/// the colour alone.
fn swatch_color(swatch: Swatch, theme: &Theme) -> gpui::Hsla {
    match swatch {
        Swatch::System | Swatch::Fixed => theme.accent,
        Swatch::Tools => theme.warning,
        Swatch::Project => theme.success,
        Swatch::Skills => theme.fg.opacity(0.55),
        Swatch::Messages => theme.muted,
        Swatch::Mcp => theme.faint,
        Swatch::Agents => theme.muted.opacity(0.6),
        Swatch::Other => theme.faint,
        Swatch::Buffer => theme.border,
        Swatch::Free => gpui::transparent_black(),
    }
}

/// A block of the panel: its group label (with a count) over its body.
fn block(
    title: &str,
    count: Option<String>,
    body: impl IntoElement,
    theme: &Theme,
    tokens: &Tokens,
) -> gpui::Div {
    let space = tokens.spacing;
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap(px(space * 1.5))
        .px(px(space * 3.0))
        .pt(px(space * 3.0))
        .pb(px(space))
        .child(ui::group_header(
            title,
            count.map(SharedString::from),
            theme,
            tokens,
        ))
        .child(body)
}

fn muted_line(text: impl Into<SharedString>, theme: &Theme, tokens: &Tokens) -> gpui::Div {
    div()
        .whitespace_normal()
        .text_size(px(tokens.secondary_font_size))
        .text_color(theme.muted)
        .child(text.into())
}

/// Label on the left, value on the right.
fn pair_row(label: &str, value: &str, theme: &Theme, tokens: &Tokens) -> gpui::Div {
    div()
        .flex()
        .items_baseline()
        .justify_between()
        .gap(px(tokens.spacing * 2.0))
        .text_size(px(tokens.font_size))
        .child(
            div()
                .flex_none()
                .text_color(theme.muted)
                .child(SharedString::from(label.to_owned())),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(theme.fg)
                .child(SharedString::from(value.to_owned())),
        )
}

impl ContextPanel {
    fn render_item(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(item) = self.items.get(ix).cloned() else {
            return div().into_any_element();
        };
        let element = match item {
            Item::Identity => self.render_identity(cx),
            Item::Empty(empty) => self.render_empty(&empty, cx),
            Item::Context => self.render_context(cx),
            Item::Chart => self.render_chart(window, cx),
            Item::TurnsHeader => self.render_turns_header(cx),
            Item::Turn(index) => self.render_turn(index, cx),
            Item::TurnsNote => self.render_turns_note(cx),
            Item::Tools => self.render_tools(cx),
            Item::Files => self.render_files(cx),
            Item::Totals => self.render_totals(cx),
            Item::Notes => self.render_notes(cx),
            Item::Sources => self.render_sources(window, cx),
        };
        // A list lays an item out as a root with the list's width only available: the wrapper
        // takes that width.
        div()
            .flex()
            .flex_col()
            .w_full()
            .child(element)
            .into_any_element()
    }

    fn render_identity(&self, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let Some(info) = self.info(cx) else {
            return div().into_any_element();
        };
        let store = self.env.sessions.read(cx);
        let presentation = store.presentation(info);
        let state = card::badge_state(info, &presentation, None);
        let lane = ui::row_lane(
            SharedString::from("context-lane"),
            session_icon_with(info, store.remembered_agent(info.id)),
            state,
            theme.right_sidebar,
            false,
            &theme,
        );
        let harness = self
            .insight
            .as_ref()
            .and_then(|insight| insight.harness.clone())
            .or_else(|| info.chat.as_ref().map(|chat| chat.agent.clone()))
            .or_else(|| hutan_agents_view::attention::agent_app(info).map(str::to_owned));
        let model_chip = self
            .insight
            .as_ref()
            .and_then(|insight| insight.model.as_ref())
            .map(|m| match &m.thinking {
                Some(thinking) => format!("{} · {thinking}", m.id),
                None => m.id.clone(),
            })
            .or_else(|| {
                info.chat
                    .as_ref()
                    .map(|chat| chat.model.clone())
                    .filter(|m| !m.is_empty())
            });
        let mut facts: Vec<String> = Vec::new();
        if let Some(harness) = &harness {
            facts.push(model::harness_label(harness));
        }
        if let Some(turns) = self
            .insight
            .as_ref()
            .and_then(|insight| insight.totals.as_ref())
            .map(|totals| totals.turns)
        {
            facts.push(format!("turn {turns}"));
        }
        facts.push(presentation.label.clone());
        facts.push(hutan_client::time::relative_to_now(info.created_at_ms));
        div()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .px(px(tokens.spacing * 3.0))
            .pt(px(tokens.spacing * 2.0))
            .child(lane)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(tokens.row_font_size))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.fg)
                            .child(SharedString::from(info.title.clone())),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(tokens.spacing))
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .children(model_chip.map(|chip| ui::chip_mono(chip, &theme, &tokens)))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .child(SharedString::from(facts.join(" · "))),
                            ),
                    ),
            )
            .child(probe(&self.env.regions, Region::Context("identity")))
            .into_any_element()
    }

    fn render_empty(&self, empty: &Empty, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let (title, detail, action): (String, String, Option<AnyElement>) = match empty {
            Empty::NoSession => (
                "No agent in view".to_owned(),
                "Open a chat or a terminal with an agent. The panel follows the session you \
                 look at: how full its context is, what each turn used, and where to fork it."
                    .to_owned(),
                None,
            ),
            Empty::NotAnAgent => (
                "Not an agent session".to_owned(),
                "A shell has no context to show. Start an agent in a terminal or open a chat."
                    .to_owned(),
                None,
            ),
            Empty::Terminal { app, hooks } => {
                let who = app
                    .as_deref()
                    .map_or_else(|| "The agent".to_owned(), model::harness_label);
                let detail = if *hooks {
                    format!(
                        "{who} reports its state to Hutan from this terminal, not its tokens, \
                         tools, files or cost. A chat shows all of them."
                    )
                } else {
                    format!(
                        "Hutan sees only the state {who} shows in this terminal's title. Not \
                         its tokens, tools, files or cost. Install its integration to get its \
                         state and resume command; a chat shows everything."
                    )
                };
                let action = (!*hooks).then(|| {
                    ui::Button::new(
                        "context-integrations",
                        "Agent integrations…",
                        &theme,
                        &tokens,
                    )
                    .compact()
                    .on_click(|_, window, cx| {
                        window.dispatch_action(
                            Box::new(OpenSettingsFor("integrations".to_owned())),
                            cx,
                        );
                    })
                    .child(probe(&self.env.regions, Region::Context("integrations")))
                    .into_any_element()
                });
                ("Terminal agent".to_owned(), detail, action)
            }
            Empty::NotConnected => (
                "Not connected".to_owned(),
                "The context is read once hutand is connected.".to_owned(),
                None,
            ),
            Empty::Unsupported => (
                "This daemon does not report insight".to_owned(),
                "Update hutand to see the context, the turns and what they used.".to_owned(),
                None,
            ),
            Empty::Loading => ("Reading…".to_owned(), String::new(), None),
            Empty::Failed(message) => (
                "Cannot read the context".to_owned(),
                message.clone(),
                Some(
                    ui::Button::new("context-retry", "Try again", &theme, &tokens)
                        .compact()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.asked = None;
                            this.read_if_needed(cx);
                        }))
                        .into_any_element(),
                ),
            ),
            Empty::Nothing => {
                let harness = self
                    .insight
                    .as_ref()
                    .and_then(|insight| insight.harness.clone())
                    .unwrap_or_default();
                let note = self
                    .insight
                    .as_ref()
                    .and_then(|insight| model::unsupported_note(&harness, &insight.unsupported))
                    .unwrap_or_default();
                (
                    "Nothing reported yet".to_owned(),
                    format!(
                        "{} reports its context and what a turn used when the first turn ends. \
                         {note}",
                        model::harness_label(&harness)
                    )
                    .trim()
                    .to_owned(),
                    None,
                )
            }
        };
        div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .px(px(tokens.spacing * 3.0))
            .py(px(tokens.spacing * 3.0))
            .text_size(px(tokens.font_size))
            .child(
                div()
                    .text_color(theme.muted)
                    .child(SharedString::from(title)),
            )
            .when(!detail.is_empty(), |s| {
                s.child(
                    div()
                        .whitespace_normal()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.faint)
                        .child(SharedString::from(detail)),
                )
            })
            .children(action.map(|action| div().flex().pt(px(tokens.spacing)).child(action)))
            .child(probe(&self.env.regions, Region::Context("empty")))
            .into_any_element()
    }

    /// The meter, the breakdown and *Measure context*.
    fn render_context(&self, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let Some(insight) = self.insight.as_ref() else {
            return div().into_any_element();
        };
        let space = tokens.spacing;
        let mut body = div().flex().flex_col().gap(px(space * 1.5));
        let breakdown = insight.context.as_ref().and_then(model::breakdown);
        if let Some(context) = insight.context.as_ref() {
            let mut head = div()
                .flex()
                .items_baseline()
                .justify_between()
                .gap(px(space * 2.0))
                .child(
                    div()
                        .id("context-meter-text")
                        .text_size(px(tokens.font_size + 1.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.fg)
                        .child(SharedString::from(model::meter_text(context)))
                        .tooltip({
                            let tip = SharedString::from(match context.window {
                                Some(window) => format!(
                                    "{} of {} tokens in the context",
                                    model::thousands(context.used),
                                    model::thousands(window)
                                ),
                                None => format!(
                                    "{} tokens in the context",
                                    model::thousands(context.used)
                                ),
                            });
                            move |_, cx| cx.new(|_| TextTooltip::new(tip.clone())).into()
                        })
                        .child(probe(&self.env.regions, Region::Context("meter"))),
                );
            if let Some(at) = context.compact_at {
                head = head.child(
                    div()
                        .flex_none()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child(SharedString::from(format!(
                            "compacts at {}",
                            crate::workbench::chat::widgets::compact_tokens(at)
                        ))),
                );
            }
            body = body.child(head);
            body = body.child(self.meter_bar(context, breakdown.as_ref(), &theme));
        }
        if let Some(breakdown) = breakdown.as_ref() {
            let rows = breakdown.lines.iter().map(|line| {
                let words = match model::kind_note(line.kind) {
                    Some(note) if line.kind != BreakdownKind::Free => {
                        format!("{} · {note}", line.label)
                    }
                    _ => line.label.clone(),
                };
                let faint = matches!(line.kind, BreakdownKind::Free | BreakdownKind::Buffer);
                div()
                    .flex()
                    .items_center()
                    .gap(px(space * 1.5))
                    .text_size(px(tokens.font_size))
                    .text_color(if faint { theme.muted } else { theme.fg })
                    .child(
                        div()
                            .flex_none()
                            .size(px(8.0))
                            .rounded(px(2.0))
                            .bg(swatch_color(line.swatch, &theme))
                            .when(matches!(line.swatch, Swatch::Free | Swatch::Buffer), |s| {
                                s.border_1().border_color(theme.border)
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(words)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.muted)
                            .child(SharedString::from(line.tokens_text.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(40.0))
                            .flex()
                            .justify_end()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.faint)
                            .child(SharedString::from(line.share_text.clone())),
                    )
            });
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(space * 0.5))
                    .children(rows)
                    .child(probe(&self.env.regions, Region::Context("breakdown"))),
            );
        }
        // Where the breakdown stands, and the button that makes one.
        let turn_running = self
            .info(cx)
            .and_then(|info| info.chat.as_ref())
            .is_some_and(|chat| chat.turn_active);
        let measuring = self.measure == Measure::Running;
        let mut foot = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(space * 2.0));
        let note = match (&breakdown, insight.can_measure) {
            (Some(b), _) => {
                let when = b
                    .as_of_ms
                    .map(|ms| {
                        let text = hutan_client::time::local_time_text(ms);
                        text.split_whitespace().nth(1).unwrap_or(&text).to_owned()
                    })
                    .unwrap_or_default();
                let estimated = if b.estimated {
                    "estimated by the harness, "
                } else {
                    ""
                };
                format!("{estimated}as of /context at {when}; the context has moved since")
            }
            (None, true) => {
                "No breakdown yet. Measuring sends /context to the agent: it costs no tokens \
                 and appears in the chat as a message."
                    .to_owned()
            }
            (None, false) => model::unsupported_note(
                insight.harness.as_deref().unwrap_or_default(),
                &insight
                    .unsupported
                    .iter()
                    .copied()
                    .filter(|field| *field == hutan_core::insight::InsightField::Breakdown)
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_default(),
        };
        if !note.is_empty() {
            foot = foot.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(muted_line(note, &theme, &tokens)),
            );
        }
        if insight.can_measure {
            let label = if breakdown.is_some() {
                "Measure again"
            } else {
                "Measure context"
            };
            foot = foot.child(
                ui::Button::new("context-measure", label, &theme, &tokens)
                    .compact()
                    .loading(measuring)
                    .disabled(turn_running || measuring)
                    .tooltip(if turn_running {
                        "The agent is working: measure when its turn has ended"
                    } else {
                        "Send /context to the agent and show its context by category"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.measure(cx)))
                    .child(probe(&self.env.regions, Region::Context("measure"))),
            );
        }
        body = body.child(foot);
        match &self.measure {
            Measure::Unreadable => {
                body = body.child(
                    div()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.warning)
                        .child(
                            "The agent answered, but not with a context report (is a model \
                             selected?). The breakdown is as it was.",
                        )
                        .child(probe(&self.env.regions, Region::Context("measure-note"))),
                );
            }
            Measure::Failed(message) => {
                body = body.child(
                    div()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.danger)
                        .child(SharedString::from(message.clone()))
                        .child(probe(&self.env.regions, Region::Context("measure-note"))),
                );
            }
            Measure::Idle | Measure::Running => {}
        }
        block("Context", None, body, &theme, &tokens).into_any_element()
    }

    /// The fill bar; once a breakdown exists, the same bar split by category.
    fn meter_bar(
        &self,
        context: &hutan_core::insight::InsightContext,
        breakdown: Option<&model::Breakdown>,
        theme: &Theme,
    ) -> AnyElement {
        let track = div()
            .relative()
            .w_full()
            .h(px(8.0))
            .rounded_full()
            .overflow_hidden()
            .bg(theme.surface_hover);
        let bar = match breakdown {
            Some(breakdown) if context.window.is_some() => {
                // Used categories first (left), then the reserved buffer; the free space is the
                // track itself.
                let mut row = div().flex().size_full();
                for line in breakdown
                    .lines
                    .iter()
                    .filter(|line| matches!(line.kind, BreakdownKind::Used))
                    .chain(
                        breakdown
                            .lines
                            .iter()
                            .filter(|line| line.kind == BreakdownKind::Buffer),
                    )
                {
                    row = row.child(
                        div()
                            .h_full()
                            .w(relative(line.share))
                            .flex_none()
                            .bg(swatch_color(line.swatch, theme)),
                    );
                }
                track.child(row)
            }
            _ => match model::fill(context) {
                Some(fraction) => track.child(
                    div()
                        .h_full()
                        .w(relative(fraction))
                        .rounded_full()
                        .bg(theme.accent),
                ),
                None => track,
            },
        };
        let marker = model::compact_marker(context);
        div()
            .relative()
            .w_full()
            .child(bar)
            .when_some(marker, |s, at| {
                s.child(
                    div()
                        .absolute()
                        .top(px(-2.0))
                        .left(relative(at))
                        .w(px(1.5))
                        .h(px(12.0))
                        .bg(theme.fg),
                )
            })
            .into_any_element()
    }

    fn render_chart(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let Some(chart) = self
            .insight
            .as_ref()
            .and_then(|insight| model::fill_chart(insight, CHART_TURNS))
        else {
            return div().into_any_element();
        };
        if self.narrow(window) {
            return div().into_any_element();
        }
        let bars = chart.bars.iter().map(|share| {
            div().flex_1().h_full().flex().items_end().child(
                div()
                    .w_full()
                    .h(relative(share.max(0.04)))
                    .rounded_t(px(1.5))
                    .bg(theme.accent.opacity(0.6)),
            )
        });
        block(
            "Fill by turn",
            Some(format!(
                "peak {}",
                crate::workbench::chat::widgets::compact_tokens(chart.peak)
            )),
            div()
                .flex()
                .items_end()
                .gap(px(1.5))
                .w_full()
                .h(px(36.0))
                .children(bars)
                .child(probe(&self.env.regions, Region::Context("chart"))),
            &theme,
            &tokens,
        )
        .into_any_element()
    }

    /// The panel is narrower than [`FOLD_WIDTH`] (measured from the last frame).
    fn narrow(&self, _window: &Window) -> bool {
        self.env
            .regions
            .get(Region::Context("panel"))
            .is_some_and(|bounds| bounds.size.width < px(FOLD_WIDTH))
    }

    fn render_turns_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 0.5))
            .px(px(tokens.spacing * 3.0))
            .pt(px(tokens.spacing * 3.0))
            .pb(px(tokens.spacing))
            .child(ui::group_header(
                "Turns",
                Some(SharedString::from(self.turns.len().to_string())),
                &theme,
                &tokens,
            ))
            .child(muted_line("Fork or rewind from any turn", &theme, &tokens))
            .into_any_element()
    }

    fn render_turns_note(&self, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        div()
            .px(px(tokens.spacing * 3.0))
            .pb(px(tokens.spacing))
            .child(muted_line("Older turns are not listed.", &theme, &tokens))
            .into_any_element()
    }

    fn render_tools(&self, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let Some(insight) = self.insight.as_ref() else {
            return div().into_any_element();
        };
        let rows = model::tool_lines(insight).into_iter().map(|tool| {
            let mut row = div()
                .flex()
                .items_baseline()
                .gap(px(tokens.spacing * 2.0))
                .text_size(px(tokens.font_size))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.fg)
                        .child(SharedString::from(tool.name)),
                );
            if let Some(detail) = tool.detail {
                row = row.child(
                    div()
                        .flex_none()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child(SharedString::from(detail)),
                );
            }
            row.child(
                div()
                    .flex_none()
                    .text_color(theme.muted)
                    .child(SharedString::from(tool.calls.to_string())),
            )
        });
        block(
            "Tools",
            model::tools_summary(insight),
            div()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing * 0.5))
                .children(rows)
                .child(probe(&self.env.regions, Region::Context("tools"))),
            &theme,
            &tokens,
        )
        .into_any_element()
    }

    /// Where a file of the insight is on disk: its path when absolute, else under the session's
    /// folder.
    fn file_path(&self, relative: &str, cx: &gpui::App) -> Option<std::path::PathBuf> {
        let path = std::path::Path::new(relative);
        if path.is_absolute() {
            return Some(path.to_owned());
        }
        Some(self.info(cx)?.cwd.as_ref()?.join(path))
    }

    /// The checkout of the session and the file's path in it, for the diff.
    fn file_diff(&self, relative: &str, cx: &gpui::App) -> Option<ChangedFileAt> {
        let absolute = self.file_path(relative, cx)?;
        let store = self.env.sessions.read(cx);
        let root = store
            .agent_presentation()
            .get(self.session?)?
            .checkout
            .as_ref()?
            .root
            .clone();
        let inside = absolute.strip_prefix(&root).ok()?;
        Some(ChangedFileAt {
            checkout: root.display().to_string(),
            file: inside.display().to_string(),
        })
    }

    fn render_files(&self, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let Some(insight) = self.insight.as_ref() else {
            return div().into_any_element();
        };
        let lines = model::file_lines(insight);
        let total = lines.len();
        let shown = if self.all_files {
            total
        } else {
            total.min(FILES_SHOWN)
        };
        let mut rows = Vec::new();
        for (index, file) in lines.into_iter().take(shown).enumerate() {
            let open = self.file_path(&file.path, cx);
            let diff = self.file_diff(&file.path, cx);
            let path_text = file.path.clone();
            let tip = SharedString::from(file.path.clone());
            let name = file
                .path
                .rsplit_once('/')
                .map(|(dir, name)| (dir.to_owned(), name.to_owned()));
            let label = match &name {
                Some((_, name)) => name.clone(),
                None => path_text.clone(),
            };
            let dir = name.map(|(dir, _)| dir);
            rows.push(
                div()
                    .id(SharedString::from(format!("context-file-{index}")))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 1.5))
                    .py(px(tokens.spacing * 0.5))
                    .px(px(tokens.spacing))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .hover({
                        let hover = theme.right_sidebar_accent;
                        move |s| s.bg(hover)
                    })
                    .tooltip(move |_, cx| cx.new(|_| TextTooltip::new(tip.clone())).into())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, window, cx| {
                        if let Some(path) = &open {
                            window.dispatch_action(
                                Box::new(OpenChosenFile(ChosenFile {
                                    path: path.display().to_string(),
                                    line: None,
                                    column: None,
                                    place: FilePlace::Focused,
                                })),
                                cx,
                            );
                        }
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .font_family(ui::mono_family())
                                    .text_size(px(tokens.secondary_font_size))
                                    .text_color(theme.fg)
                                    .child(SharedString::from(label)),
                            )
                            .children(dir.filter(|d| !d.is_empty()).map(|dir| {
                                div()
                                    .truncate()
                                    .text_size(px(tokens.chip_font_size))
                                    .text_color(theme.faint)
                                    .child(SharedString::from(dir))
                            })),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child(SharedString::from(match &file.stat {
                                Some(stat) => format!("{} · {stat}", file.kind),
                                None => file.kind.to_owned(),
                            })),
                    )
                    .children(diff.map(|diff| {
                        ui::Button::new(
                            SharedString::from(format!("context-file-diff-{index}")),
                            "Diff",
                            &theme,
                            &tokens,
                        )
                        .ghost()
                        .compact()
                        .tooltip("Show this file's changes in the Changes view")
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            window
                                .dispatch_action(Box::new(actions::ReviewFileAt(diff.clone())), cx);
                        })
                        .child(probe(&self.env.regions, Region::ContextFile(index, "diff")))
                    }))
                    .child(probe(&self.env.regions, Region::ContextFile(index, "open")))
                    .into_any_element(),
            );
        }
        let more = (total > shown).then(|| {
            ui::Button::new(
                "context-files-more",
                format!("Show {} more", total - shown),
                &theme,
                &tokens,
            )
            .link()
            .on_click(cx.listener(|this, _, _, cx| {
                this.all_files = true;
                this.rebuild(cx);
            }))
        });
        let truncated = insight.files.as_ref().is_some_and(|files| files.truncated);
        let unnamed = total == 0;
        let changed = model::files_changed_in_turns(insight);
        block(
            "Files touched",
            model::files_summary(insight)
                .or_else(|| (unnamed && changed > 0).then(|| format!("{changed} changed"))),
            div()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing * 0.5))
                .children(rows)
                .children(more)
                .when(unnamed, |s| {
                    s.child(muted_line(
                        format!(
                            "{} did not say which files. The turns above count them.",
                            insight
                                .harness
                                .as_deref()
                                .map_or_else(|| "The agent".to_owned(), model::harness_label)
                        ),
                        &theme,
                        &tokens,
                    ))
                })
                .when(truncated, |s| {
                    s.child(muted_line(
                        "More files were touched than are listed.",
                        &theme,
                        &tokens,
                    ))
                })
                .child(probe(&self.env.regions, Region::Context("files"))),
            &theme,
            &tokens,
        )
        .into_any_element()
    }

    fn render_totals(&self, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let Some(insight) = self.insight.as_ref() else {
            return div().into_any_element();
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        for (label, value) in model::totals_pairs(insight)
            .into_iter()
            .chain(model::time_pairs(insight))
        {
            rows.push(pair_row(&label, &value, &theme, &tokens).into_any_element());
        }
        block(
            "Tokens, cost and time",
            None,
            div()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing * 0.5))
                .children(rows)
                .child(probe(&self.env.regions, Region::Context("totals"))),
            &theme,
            &tokens,
        )
        .into_any_element()
    }

    fn render_notes(&self, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let Some(insight) = self.insight.as_ref() else {
            return div().into_any_element();
        };
        let note = model::unsupported_note(
            insight.harness.as_deref().unwrap_or_default(),
            &insight.unsupported,
        )
        .unwrap_or_default();
        div()
            .px(px(tokens.spacing * 3.0))
            .pt(px(tokens.spacing * 2.0))
            .child(muted_line(note, &theme, &tokens))
            .child(probe(&self.env.regions, Region::Context("notes")))
            .into_any_element()
    }

    fn render_sources(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let Some(insight) = self.insight.as_ref() else {
            return div().into_any_element();
        };
        if self.narrow(window) {
            return div().into_any_element();
        }
        let chips = model::source_lines(insight)
            .into_iter()
            .map(|line| ui::chip_mono(line, &theme, &tokens));
        block(
            "Sources",
            None,
            div()
                .flex()
                .flex_wrap()
                .gap(px(tokens.spacing))
                .children(chips)
                .child(probe(&self.env.regions, Region::Context("sources"))),
            &theme,
            &tokens,
        )
        .into_any_element()
    }
}

// ---- turns ----------------------------------------------------------------------------------------

/// What a turn row offers for its files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewindOffer {
    Nothing,
    Rewind(CheckpointAt),
    /// The turn stands restored; Undo restores that restore.
    Undo(CheckpointAt),
}

impl ContextPanel {
    /// What the row of `turn` offers for rewinding: the same decision as the turn's footer in the
    /// chat (the checkpoint store's), so the two never disagree. A turn whose checkpoint is
    /// unsupported, late or off offers nothing (the row then offers Fork only).
    pub fn rewind_offer(&self, turn: TurnId, cx: &mut Context<Self>) -> RewindOffer {
        let Some(session) = self.session else {
            return RewindOffer::Nothing;
        };
        let availability = self
            .info(cx)
            .and_then(|info| info.chat.as_ref())
            .and_then(|chat| chat.checkpoints.as_ref())
            .map(|status| status.state);
        if matches!(
            availability,
            Some(
                hutan_core::CheckpointAvailability::Off
                    | hutan_core::CheckpointAvailability::Unsupported
            )
        ) {
            return RewindOffer::Nothing;
        }
        let store = self.env.sessions.read(cx).checkpoints().clone();
        let (supported, waiting) = {
            let store = store.read(cx);
            (
                store.supported(),
                !store.loaded(session) && store.error(session).is_none(),
            )
        };
        if !supported {
            return RewindOffer::Nothing;
        }
        if waiting {
            store.update(cx, |store, cx| store.ensure(session, cx));
            return RewindOffer::Nothing;
        }
        let store = store.read(cx);
        let Some(checkpoint) = store.of_turn(session, turn) else {
            return RewindOffer::Nothing;
        };
        if checkpoint.files_changed == 0
            || !store.restore_supported()
            || checkpoint.state != hutan_core::CheckpointState::Ok
        {
            return RewindOffer::Nothing;
        }
        let at = CheckpointAt {
            session,
            checkpoint: checkpoint.id,
        };
        match store.restored_by(checkpoint.id) {
            Some(restore) => RewindOffer::Undo(CheckpointAt {
                session,
                checkpoint: restore,
            }),
            None => RewindOffer::Rewind(at),
        }
    }

    fn render_turn(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let Some(line) = self.turns.get(index).cloned() else {
            return div().into_any_element();
        };
        let space = tokens.spacing;
        let group = SharedString::from(format!("context-turn-{index}"));
        let rewind = line
            .turn_id
            .filter(|_| line.state != TurnState::Running)
            .map_or(RewindOffer::Nothing, |turn| self.rewind_offer(turn, cx));
        let mut stats: Vec<String> = Vec::new();
        stats.extend(line.tokens.clone());
        stats.extend(line.tools.clone());
        stats.extend(line.files.clone());
        stats.extend(line.duration.clone());
        stats.extend(line.cost.clone());
        stats.push(hutan_client::time::relative_to_now(line.started_ms));
        let state_word = match line.state {
            TurnState::Running => Some(("running", theme.accent)),
            TurnState::Failed => Some(("failed", theme.danger)),
            TurnState::Cancelled => Some(("stopped", theme.muted)),
            TurnState::Done => None,
        };
        let title = line
            .prompt
            .clone()
            .unwrap_or_else(|| format!("Turn {}", line.n));
        let tip = SharedString::from(if line.detail.is_empty() {
            title.clone()
        } else {
            format!("{title}\n{}", line.detail)
        });
        let buttons = self.turn_buttons(index, &line, rewind, &theme, &tokens, cx);
        div()
            .id(SharedString::from(format!("context-turn-row-{index}")))
            .group(group.clone())
            .relative()
            .flex()
            .flex_col()
            .gap(px(space * 0.5))
            .mx(px(space * 2.0))
            .px(px(space))
            .py(px(space * 1.5))
            .rounded(px(tokens.radius))
            .hover({
                let hover = theme.right_sidebar_accent;
                move |s| s.bg(hover)
            })
            .tooltip(move |_, cx| cx.new(|_| TextTooltip::new(tip.clone())).into())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(space * 1.5))
                    .child(ui::chip_mono(line.label.clone(), &theme, &tokens))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(tokens.font_size))
                            .text_color(theme.fg)
                            .child(SharedString::from(title)),
                    )
                    .children(state_word.map(|(word, color)| {
                        div()
                            .flex_none()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(color)
                            .child(word)
                    }))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(space))
                            .opacity(0.0)
                            .group_hover(group, |s| s.opacity(1.0))
                            .children(buttons),
                    ),
            )
            .child(muted_line(stats.join(" · "), &theme, &tokens))
            .child(probe(&self.env.regions, Region::ContextTurn(index)))
            .into_any_element()
    }

    /// Whether the rows offer *Fork…*, and whether it works now: the same gate as the chat's turn
    /// footers (nothing without `chat.fork`, for an assistant, or for a chat that cannot be
    /// forked; greyed while a turn runs, which the daemon refuses a fork during).
    pub fn fork_gate(&self, cx: &gpui::App) -> ForkGate {
        let store = self.env.sessions.read(cx);
        if !store.supports_chat_fork() {
            return ForkGate::Hidden;
        }
        let Some(chat) = self
            .session
            .and_then(|id| store.session(id))
            .and_then(|info| info.chat.as_ref())
        else {
            return ForkGate::Hidden;
        };
        if chat.is_assistant()
            || chat
                .fork
                .as_ref()
                .is_some_and(|fork| fork.support == hutan_core::ForkSupport::None)
        {
            return ForkGate::Hidden;
        }
        if chat.turn_active {
            ForkGate::Waiting
        } else {
            ForkGate::Ready
        }
    }

    /// *Fork…* and *Rewind…* (or *Undo*) of one turn row.
    fn turn_buttons(
        &self,
        index: usize,
        line: &TurnLine,
        rewind: RewindOffer,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut buttons: Vec<AnyElement> = Vec::new();
        let gate = self.fork_gate(cx);
        if let (Some(session), Some(turn), true) = (
            self.session,
            line.turn_id,
            gate != ForkGate::Hidden && line.state != TurnState::Running,
        ) {
            buttons.push(
                ui::Button::new(
                    SharedString::from(format!("context-turn-fork-{index}")),
                    "Fork…",
                    theme,
                    tokens,
                )
                .ghost()
                .compact()
                .tooltip(if gate == ForkGate::Waiting {
                    "A turn is running: fork when it ends"
                } else {
                    "Fork: a new chat with the conversation up to this turn"
                })
                .disabled(gate == ForkGate::Waiting)
                .on_click(move |event, window, cx| {
                    window.dispatch_action(
                        Box::new(actions::ForkChat(actions::ForkAt {
                            session,
                            turn: Some(turn),
                            position: event.position(),
                        })),
                        cx,
                    );
                })
                .child(probe(
                    &self.env.regions,
                    Region::ContextTurnAction(index, "fork"),
                ))
                .into_any_element(),
            );
        }
        match rewind {
            RewindOffer::Nothing => {}
            RewindOffer::Rewind(at) => buttons.push(
                ui::Button::new(
                    SharedString::from(format!("context-turn-rewind-{index}")),
                    "Rewind…",
                    theme,
                    tokens,
                )
                .ghost()
                .compact()
                .tooltip("Restore the files to how they were before this turn")
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(actions::RewindCheckpoint(at)), cx);
                })
                .child(probe(
                    &self.env.regions,
                    Region::ContextTurnAction(index, "rewind"),
                ))
                .into_any_element(),
            ),
            RewindOffer::Undo(at) => buttons.push(
                ui::Button::new(
                    SharedString::from(format!("context-turn-undo-{index}")),
                    "Undo restore",
                    theme,
                    tokens,
                )
                .ghost()
                .compact()
                .tooltip("Put back what the restore of this turn changed")
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(actions::UndoRestore(at)), cx);
                })
                .child(probe(
                    &self.env.regions,
                    Region::ContextTurnAction(index, "undo"),
                ))
                .into_any_element(),
            ),
        }
        buttons
    }
}

impl Render for ContextPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.env.regions.rendered(cx.entity_id());
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let header = panel_toolbar("panels.context", Vec::new(), Vec::new(), cx);
        div()
            .id("context-panel")
            .key_context("ContextPanel")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .child(header)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        gpui::list(self.list.clone(), cx.processor(Self::render_item)).size_full(),
                    )
                    .child(self.bar.element()),
            )
            .child(probe(&self.env.regions, Region::Context("panel")))
    }
}

/// What the *Fork…* buttons of the turn rows do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkGate {
    /// Not offered: an older daemon, an assistant, a chat that cannot be forked.
    Hidden,
    /// A turn runs: forking waits for it.
    Waiting,
    Ready,
}
