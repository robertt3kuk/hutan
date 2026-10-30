//! **Mawas Home** (M16-B, MW-1): the Global (Home) or the active workspace's Mawas chat, as one
//! tab of the center with the **Today** rail beside it (`assistant_rail.rs`).
//!
//! The header carries the scope, the backend·model chip, the **mode chip** (Ask before acting /
//! Advisor: `assistant.mode` of the daemon, applied from the next turn), **New chat** and
//! **Stop**, the kill switch: it denies what the chat has pending and interrupts its turn.
//!
//! The panel asks the daemon for the default assistant of a scope (`assistant.open`) and shows
//! the returned session with the ordinary chat model and view (`workbench::chat`): the
//! transcript, the composer and its context chips are the chat view's. Each scope keeps its own
//! conversation: switching scope shows another session and never moves a chip, a draft or a
//! transcript from one to the other. Opening an assistant starts no terminal and no vendor
//! agent: it is a direct chat of the provider named under `[assistant]` in the daemon's
//! `daemon.toml`. Without that setting the panel says what is missing and links to Models; it
//! never picks a provider or model of its own.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, canvas, div, prelude::*, px,
};
use hutan_client::{ClientError, ConnectionState};
use hutan_core::{SessionId, WorkspaceId};
use hutan_protocol::ErrorCode;
use hutan_protocol::assistant::{
    AssistantBackend, AssistantConfigure, AssistantOpen, AssistantScope,
};
use hutan_protocol::messages::{Empty, Request};
use hutan_protocol::messages::{WorkspaceInfo, WorkspaceKind};
use hutan_protocol::provider::{ProviderInfo, ProviderList, ProviderModels, ProviderRef};
use hutan_settings::{SettingsLayer, SettingsStore};
use serde_json::json;

use super::common::state_dot;
use crate::icons::{Icon, icon};
use crate::settings::{AssistantSettings, HomeScope};
use crate::workbench::actions::{
    ManageProviders, ShowAssistantChatsMenu, ShowAssistantFocusMenu, ShowAssistantModeMenu,
    ShowAssistantModelMenu, ShowAssistantMoreMenu, ShowAssistantScopeMenu,
};
use crate::workbench::agents::Models;
use crate::workbench::chat::{ChatModel, ChatView, ContextChip, Look};
use crate::workbench::harnesses::{self, Detection, HARNESSES, Listing};
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::TextTooltip;
use crate::workbench::ui::{self, Button, Mascot, StyledExt as _};
use crate::workbench::view::Region;
use hutan_agents_view::models::picker_rows;

/// A conversation the panel keeps: one per scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeKey {
    /// The Home workspace's assistant.
    Global,
    /// The assistant of one project workspace.
    Workspace(WorkspaceId),
}

impl ScopeKey {
    /// The scope the workspace's assistant has.
    pub fn of(workspace: &WorkspaceInfo) -> Self {
        if workspace.kind == WorkspaceKind::Home {
            Self::Global
        } else {
            Self::Workspace(workspace.id)
        }
    }
}

/// Which scope the panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// The Home assistant.
    Global,
    /// The assistant of the active workspace (the Home one while Home is active).
    Active,
    /// A workspace other than the active one, opened from a history hit.
    Pinned(WorkspaceId),
}

impl Choice {
    /// What Mawas Home shows when it opens: the Global chat, or what follows the active workspace
    /// (`assistant.home_scope`).
    pub fn opened_by(scope: HomeScope) -> Self {
        match scope {
            HomeScope::Global => Self::Global,
            HomeScope::FollowWorkspace => Self::Active,
        }
    }
}

/// Why a scope has no assistant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The daemon has no `[assistant] provider` (the daemon's own words).
    Unconfigured(String),
    /// The daemon does not speak protocol 0.12.
    Unsupported,
    /// Anything else the daemon or the connection answered.
    Other(String),
}

/// Where opening a scope's assistant stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    Idle,
    Opening,
    Ready(SessionId),
    Failed(Failure),
}

/// One row of the scope dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeEntry {
    pub label: String,
    pub selected: bool,
    pub enabled: bool,
    pub choose: Choice,
}

/// One row of the Chats menu (`Chats ▾`): a heading, or a Mawas conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatEntry {
    pub label: String,
    /// `None` for a heading.
    pub session: Option<SessionId>,
    pub selected: bool,
}

/// One row of the Focus picker: *All workspaces* (`None`) or a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusEntry {
    pub label: String,
    pub workspace: Option<WorkspaceId>,
    pub selected: bool,
}

/// What an assistant can run on: a harness, or a provider (`assistant.configure`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendChoice {
    Harness(AssistantBackend),
    Provider(String),
}

impl BackendChoice {
    fn label(&self) -> String {
        match self {
            Self::Harness(backend) => harnesses::harness_name(backend.as_str()),
            Self::Provider(id) => format!("Provider · {id}"),
        }
    }
}

/// The backend and model chooser of the panel header.
#[derive(Debug, Clone)]
pub struct Switcher {
    /// The backend whose models are listed.
    pub backend: Option<BackendChoice>,
    pub models: Models,
}

/// What Mawas may do (`assistant.mode` of the daemon).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MawasMode {
    /// Reads freely; every change asks first, grants permitting.
    Ask,
    /// Reads and advises; changes nothing.
    Advisor,
}

impl MawasMode {
    pub const ALL: [Self; 2] = [Self::Ask, Self::Advisor];

    /// The value of `assistant.mode`.
    pub fn setting(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Advisor => "advisor",
        }
    }

    pub fn from_setting(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.setting() == value)
    }

    /// The chip's words.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "Ask before acting",
            Self::Advisor => "Advisor",
        }
    }

    /// What the mode means, for the menu and the tooltip.
    pub fn help(self) -> &'static str {
        match self {
            Self::Ask => {
                "Mawas can act. Every change asks you first; your grants answer for what you allowed."
            }
            Self::Advisor => "Mawas reads and advises. It cannot change anything.",
        }
    }

    /// The note under the header after a switch: it applies from the next message.
    pub fn next_turn_note(self) -> String {
        format!(
            "{} applies from your next message: Mawas restarts its agent for it.",
            self.label()
        )
    }
}

/// Page widths (at the default rem) below which the header folds into ⋯ and Today leaves the page:
/// the composer keeps its width instead of wrapping letter by letter. Below it the column has no
/// handles either.
const FOLD_BELOW: f32 = 640.0;

/// The margin on each side of the conversation, in pt: the column never touches the page's edge,
/// and its two resize handles (this wide, just outside the column) stay reachable.
const GUTTER: f32 = 12.0;

/// The step of *Mawas: Wider Chat* and *Narrower Chat*, in rem.
pub const CHAT_WIDTH_STEP_REM: f32 = 4.0;

/// Which edge a drag holds: one of the conversation column's, or the Today column's left border.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    Chat,
    Rail,
}

/// A drag of one of the column's edges: the conversation grows (or shrinks) by twice the
/// pointer's travel, so that it stays centered and the other edge follows; the Today column
/// grows by the travel of its left border. Nothing is written while it runs; the release writes
/// `assistant.chat_width_rem` or `assistant.rail_width_rem`, and Escape ends it without a write.
#[derive(Debug, Clone, Copy)]
struct EdgeDrag {
    edge: Edge,
    /// Where the pointer was when it took hold (x, pt).
    from: f32,
    /// The column's width then, in rem.
    start_rem: f32,
    /// How much a point of travel (to the right) adds, in points of width: `2.0` for the
    /// conversation's right edge, `-2.0` for its left one, `-1.0` for the Today column's.
    factor: f32,
    /// The width the column has now, in rem.
    live_rem: f32,
}

/// `rem` kept within 30 to 90 and within the page: `area` is the width the column's parent
/// has (pt, `0.0` when it has not been measured), of which each side keeps a gutter.
fn clamp_chat_rem(rem: f32, area: f32, rem_px: f32) -> f32 {
    let (min, max) = (
        AssistantSettings::MIN_CHAT_WIDTH_REM,
        AssistantSettings::MAX_CHAT_WIDTH_REM,
    );
    let page = if area > 0.0 && rem_px > 0.0 {
        ((area - 2.0 * GUTTER) / rem_px).clamp(min, max)
    } else {
        max
    };
    rem.clamp(min, page)
}

/// A width rounded to half a rem, the grain of what is written.
fn round_half_rem(rem: f32) -> f32 {
    (rem * 2.0).round() / 2.0
}

/// The Today column's width in rem kept within 17.5 to 32.5 and leaving the conversation
/// [`RAIL_LEAVES`] pt: `page` is the width both share (pt, `0.0` when it has not been measured).
fn clamp_rail_rem(rem: f32, page: f32, rem_px: f32) -> f32 {
    let (min, max) = (
        AssistantSettings::MIN_RAIL_WIDTH_REM,
        AssistantSettings::MAX_RAIL_WIDTH_REM,
    );
    let room = if page > 0.0 && rem_px > 0.0 {
        ((page - RAIL_LEAVES) / rem_px).clamp(min, max)
    } else {
        max
    };
    rem.clamp(min, room)
}

/// What the conversation keeps of the page while the Today column is dragged wider, in pt.
const RAIL_LEAVES: f32 = 360.0;

/// Writes `assistant.rail_width_rem` (rounded to half a rem, within 17.5 to 32.5) to the user's
/// settings.
pub fn write_rail_width(settings: &SettingsStore, rem: f32) {
    let rem = round_half_rem(rem).clamp(
        AssistantSettings::MIN_RAIL_WIDTH_REM,
        AssistantSettings::MAX_RAIL_WIDTH_REM,
    );
    if let Err(error) = settings.write_user(
        "assistant.rail_width_rem",
        toml::Value::Float(f64::from(rem)),
    ) {
        tracing::warn!("cannot save assistant.rail_width_rem: {error}");
    }
}

/// Takes `assistant.rail_width_rem` out of the user's settings: the default width again.
pub fn reset_rail_width(settings: &SettingsStore) {
    if let Err(error) = settings.remove(SettingsLayer::User, "assistant", "rail_width_rem") {
        tracing::warn!("cannot reset assistant.rail_width_rem: {error}");
    }
}

/// Writes `assistant.chat_width_rem` (rounded to half a rem, within 30 to 90) to the user's
/// settings.
pub fn write_chat_width(settings: &SettingsStore, rem: f32) {
    let rem = round_half_rem(rem).clamp(
        AssistantSettings::MIN_CHAT_WIDTH_REM,
        AssistantSettings::MAX_CHAT_WIDTH_REM,
    );
    if let Err(error) = settings.write_user(
        "assistant.chat_width_rem",
        toml::Value::Float(f64::from(rem)),
    ) {
        tracing::warn!("cannot save assistant.chat_width_rem: {error}");
    }
}

/// Takes `assistant.chat_width_rem` out of the user's settings: the default width again.
pub fn reset_chat_width(settings: &SettingsStore) {
    if let Err(error) = settings.remove(SettingsLayer::User, "assistant", "chat_width_rem") {
        tracing::warn!("cannot reset assistant.chat_width_rem: {error}");
    }
}

/// Ends the drag in `cell`, if there is one: a drag that moved the column writes its width.
/// `true` when there was a drag.
fn finish_edge_drag(cell: &Cell<Option<EdgeDrag>>, settings: &SettingsStore) -> bool {
    let Some(drag) = cell.take() else {
        return false;
    };
    if (drag.live_rem - drag.start_rem).abs() > f32::EPSILON {
        match drag.edge {
            Edge::Chat => write_chat_width(settings, drag.live_rem),
            Edge::Rail => write_rail_width(settings, drag.live_rem),
        }
    }
    true
}

/// What the Memory tab of the Today column shows besides the proposals (which are requests, and
/// come from the sessions store): the latest entries Mawas keeps, read from the daemon when the
/// tab is shown and again when `memory.changed` says they moved.
#[derive(Debug, Default)]
pub(super) struct MemoryTab {
    pub(super) entries: Vec<hutan_protocol::memory::MemoryEntry>,
    /// The daemon answered since the last change.
    pub(super) loaded: bool,
    pub(super) loading: bool,
    /// A change arrived while a read was in flight.
    pub(super) again: bool,
    /// The daemon keeps no memory (it predates protocol 0.32).
    pub(super) unsupported: bool,
}

/// What the Recent tab of the Today column shows: the daemon's catch-up (`agents.activity`) since
/// the point the user last caught up at, read when the tab is shown and again when that point or
/// the agents moved.
#[derive(Debug, Default)]
pub(super) struct RecentTab {
    pub(super) activity: Option<hutan_protocol::activity::AgentsActivity>,
    /// The flow statistics of the last seven days (`agents.stats`), read with the catch-up; `None`
    /// until read or when the daemon predates protocol 0.66.
    pub(super) flow: Option<hutan_protocol::stats::AgentsStats>,
    /// The caught-up point the answer is for (`Some(None)`: never caught up).
    pub(super) for_seen: Option<Option<u64>>,
    /// What the store looked like when the answer was read (its revision, the number of sessions
    /// and of requests).
    pub(super) revision: (u64, usize, usize),
    pub(super) loading: bool,
    /// A read waits for the pause between two reads to end.
    pub(super) retry: bool,
    pub(super) last_load: Option<std::time::Instant>,
    /// The daemon has no activity feed (it predates protocol 0.65).
    pub(super) unsupported: bool,
    /// The last failure, in the daemon's words.
    pub(super) error: Option<String>,
}

struct Chat {
    model: Entity<ChatModel>,
    view: Entity<ChatView>,
}

pub struct AssistantPanel {
    pub(super) env: PanelEnv,
    choice: Choice,
    scopes: HashMap<ScopeKey, Opened>,
    chats: HashMap<SessionId, Chat>,
    /// Context to attach once a scope's assistant is open ("Send to assistant" of a block).
    waiting: Vec<(ScopeKey, ContextChip)>,
    /// Put the keyboard in the composer as soon as it is drawn.
    pub(super) focus_wanted: bool,
    /// The backend chooser, while it is open.
    switcher: Option<Switcher>,
    providers: Vec<ProviderInfo>,
    was_connected: bool,
    /// `assistant.mode` as the daemon says; `None` until it has been read.
    mode: Option<MawasMode>,
    /// The daemon's settings were asked for (once per connection).
    policy_asked: bool,
    /// Under the header: a mode switch applies from the next message.
    mode_note: Option<String>,
    /// A failure of the controls of this page (a refused mode switch, an answer the daemon
    /// refused).
    error: Option<String>,
    /// The Today rail is showing beside the conversation (a page below 640 pt has no room for
    /// it: see `today_only`). It starts from `assistant.rail_open` and writes it when toggled.
    pub(super) rail_open: bool,
    /// `assistant.rail_open` as this page last saw it: a different value (another window, the
    /// file) is adopted.
    pub(super) rail_seen: bool,
    /// The *Idle* section of the Today column's Now tab is unfolded (it starts folded: idle
    /// agents are not news).
    pub(super) idle_open: bool,
    /// What the Today column's Memory tab lists.
    pub(super) memory: MemoryTab,
    /// What the Today column's Recent tab shows.
    pub(super) recent: RecentTab,
    /// `assistant.home_scope` as this page last saw it: a different value (Settings, another
    /// window) sends Home back to what the new value opens.
    home_seen: HomeScope,
    /// The column's resize in progress; shared with the listeners the frame registers.
    edge_drag: Rc<Cell<Option<EdgeDrag>>>,
    /// A page below 640 pt: the header is folded into ⋯ and the rail leaves the page.
    folded: bool,
    /// Below 640 pt, Today replaces the conversation instead of sitting beside it (⋯ → Today).
    today_only: bool,
    /// The page's width as the last frame measured it.
    width: Rc<Cell<f32>>,
    /// The page is on screen (the active tab of a pane). A hidden page does no work: the store
    /// changes no longer wake it.
    visible: bool,
    /// How many times the page was drawn (tests).
    renders: usize,
    _subscription: Subscription,
    _memory_changed: Subscription,
    /// Escape ends a resize of the column, whatever has the keyboard.
    _keys: Subscription,
}

impl std::fmt::Debug for AssistantPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssistantPanel")
            .field("choice", &self.choice)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

impl AssistantPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&env.sessions, |this, _, cx| this.on_sessions(cx));
        let memory_changed = cx.subscribe(
            &env.sessions,
            |this, _, _: &crate::sessions::MemoryChanged, cx| {
                this.memory.loaded = false;
                cx.notify();
            },
        );
        let connected = env.sessions.read(cx).connection_state() == ConnectionState::Connected;
        let settings = env.settings.get::<AssistantSettings>();
        let (rail_open, home_scope) = (settings.rail_open, settings.home_scope);
        let edge_drag: Rc<Cell<Option<EdgeDrag>>> = Rc::default();
        let keys = {
            let drag = edge_drag.clone();
            let page = cx.weak_entity();
            cx.intercept_keystrokes(move |event, _, cx| {
                if event.keystroke.key == "escape" && drag.take().is_some() {
                    cx.stop_propagation();
                    page.update(cx, |_, cx| cx.notify()).ok();
                }
            })
        };
        Self {
            env,
            choice: Choice::opened_by(home_scope),
            scopes: HashMap::new(),
            chats: HashMap::new(),
            waiting: Vec::new(),
            focus_wanted: false,
            switcher: None,
            providers: Vec::new(),
            was_connected: connected,
            mode: None,
            policy_asked: false,
            mode_note: None,
            error: None,
            rail_open,
            rail_seen: rail_open,
            idle_open: false,
            memory: MemoryTab::default(),
            recent: RecentTab::default(),
            home_seen: home_scope,
            edge_drag,
            folded: false,
            today_only: false,
            width: Rc::new(Cell::new(0.0)),
            visible: false,
            renders: 0,
            _subscription: subscription,
            _memory_changed: memory_changed,
            _keys: keys,
        }
    }

    // ---- state ---------------------------------------------------------------------------

    /// The active workspace when it is a project (the Home workspace has the global scope).
    fn active_project(&self, cx: &App) -> Option<WorkspaceInfo> {
        self.env
            .sessions
            .read(cx)
            .active_workspace()
            .filter(|w| w.kind != WorkspaceKind::Home)
            .cloned()
    }

    /// The choice that shows `key`: a workspace's Mawas follows the active workspace only in
    /// `follow_workspace` mode and while it is the active one; otherwise the page stays on it.
    fn choice_for(&self, key: ScopeKey, active: Option<WorkspaceId>) -> Choice {
        match key {
            ScopeKey::Global => Choice::Global,
            ScopeKey::Workspace(id)
                if self.home_seen == HomeScope::FollowWorkspace && active == Some(id) =>
            {
                Choice::Active
            }
            ScopeKey::Workspace(id) => Choice::Pinned(id),
        }
    }

    /// The scope the panel shows now.
    pub fn key(&self, cx: &App) -> ScopeKey {
        match self.choice {
            Choice::Global => ScopeKey::Global,
            Choice::Pinned(id) => ScopeKey::Workspace(id),
            Choice::Active => self
                .active_project(cx)
                .map_or(ScopeKey::Global, |w| ScopeKey::Workspace(w.id)),
        }
    }

    pub fn choice(&self) -> Choice {
        self.choice
    }

    /// How opening `key`'s assistant stands.
    pub fn state(&self, key: ScopeKey) -> Opened {
        self.scopes.get(&key).cloned().unwrap_or(Opened::Idle)
    }

    /// The session of the scope the panel shows, once it is open.
    pub fn session(&self, cx: &App) -> Option<SessionId> {
        match self.state(self.key(cx)) {
            Opened::Ready(session) => Some(session),
            _ => None,
        }
    }

    /// The session `key`'s scope has, whether or not it is the one shown.
    pub fn session_of(&self, key: ScopeKey) -> Option<SessionId> {
        match self.state(key) {
            Opened::Ready(session) => Some(session),
            _ => None,
        }
    }

    /// The chat view of `session`, once the panel has opened it.
    pub fn chat_view(&self, session: SessionId) -> Option<Entity<ChatView>> {
        self.chats.get(&session).map(|chat| chat.view.clone())
    }

    /// The chat model of `session`, once the panel has opened it.
    pub fn chat_model(&self, session: SessionId) -> Option<Entity<ChatModel>> {
        self.chats.get(&session).map(|chat| chat.model.clone())
    }

    pub(super) fn connected(&self, cx: &App) -> bool {
        self.env.sessions.read(cx).connection_state() == ConnectionState::Connected
    }

    /// Reads what the store says: a reconnect tries again what failed, a daemon that forgot a
    /// session (another epoch without it) opens that scope again.
    fn on_sessions(&mut self, cx: &mut Context<Self>) {
        let (connected, synced) = {
            let store = self.env.sessions.read(cx);
            (
                store.connection_state() == ConnectionState::Connected,
                store.synced(),
            )
        };
        if connected && !self.was_connected {
            self.scopes
                .retain(|_, state| matches!(state, Opened::Ready(_)));
        }
        if synced {
            let store = self.env.sessions.read(cx);
            self.scopes.retain(|_, state| match state {
                Opened::Ready(session) => store.session(*session).is_some(),
                _ => true,
            });
        }
        if !connected {
            self.policy_asked = false;
        }
        self.was_connected = connected;
        // A hidden page does no work: it draws again when it is shown.
        if self.visible {
            cx.notify();
        }
    }

    // ---- visibility ------------------------------------------------------------------------

    /// The workbench tells the page whether it is on screen. A page that was hidden draws again
    /// when it is shown; one that is hidden is not woken by changes of the store.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            // What the daemon says about the mode may have changed (Settings) since it was shown.
            self.policy_asked = false;
            cx.notify();
        }
    }

    /// Whether the page is on screen.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// How many times the page was drawn.
    pub fn render_count(&self) -> usize {
        self.renders
    }

    // ---- the conversation's width ---------------------------------------------------------

    /// The width of the conversation column now, in rem: the live one while an edge is dragged,
    /// else `assistant.chat_width_rem` (kept within 30 to 90).
    pub fn chat_width_rem(&self) -> f32 {
        self.edge_drag
            .get()
            .filter(|drag| drag.edge == Edge::Chat)
            .map_or_else(
                || self.env.settings.get::<AssistantSettings>().chat_width(),
                |drag| drag.live_rem,
            )
    }

    /// Whether an edge of the column is being dragged.
    pub fn resizing_chat(&self) -> bool {
        self.edge_drag
            .get()
            .is_some_and(|drag| drag.edge == Edge::Chat)
    }

    /// The width of the Today column now, in rem: the live one while its left border is dragged,
    /// else `assistant.rail_width_rem` (kept within 17.5 to 32.5).
    pub fn rail_width_rem(&self) -> f32 {
        self.edge_drag
            .get()
            .filter(|drag| drag.edge == Edge::Rail)
            .map_or_else(
                || self.env.settings.get::<AssistantSettings>().rail_width(),
                |drag| drag.live_rem,
            )
    }

    /// Whether the Today column's left border is being dragged.
    pub fn resizing_rail(&self) -> bool {
        self.edge_drag
            .get()
            .is_some_and(|drag| drag.edge == Edge::Rail)
    }

    /// Double click on the Today column's border: the default width again.
    pub fn reset_rail_width(&mut self, cx: &mut Context<Self>) {
        self.edge_drag.set(None);
        reset_rail_width(&self.env.settings);
        cx.notify();
    }

    /// A press on the Today column's left border: a drag begins from the width the column has
    /// on screen; a double click resets instead.
    pub(super) fn start_rail_drag(
        &mut self,
        event: &gpui::MouseDownEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if event.click_count >= 2 {
            self.reset_rail_width(cx);
            return;
        }
        let rem_px = f32::from(window.rem_size());
        let start_rem = self
            .env
            .regions
            .get(Region::Assistant("today"))
            .map_or_else(
                || self.rail_width_rem(),
                |bounds| f32::from(bounds.size.width) / rem_px,
            );
        self.edge_drag.set(Some(EdgeDrag {
            edge: Edge::Rail,
            from: f32::from(event.position.x),
            start_rem,
            factor: -1.0,
            live_rem: start_rem,
        }));
        cx.notify();
    }

    /// Double click on an edge: the default width again (the key leaves the file).
    pub fn reset_chat_width(&mut self, cx: &mut Context<Self>) {
        self.edge_drag.set(None);
        reset_chat_width(&self.env.settings);
        cx.notify();
    }

    /// A press on an edge of the column (`sign` `1.0` for the right one, `-1.0` for the left):
    /// a drag begins from the width the column has on screen; a double click resets instead.
    fn start_chat_drag(
        &mut self,
        sign: f32,
        event: &gpui::MouseDownEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if event.click_count >= 2 {
            self.reset_chat_width(cx);
            return;
        }
        let rem_px = f32::from(window.rem_size());
        let start_rem = self
            .env
            .regions
            .get(Region::Assistant("column"))
            .map_or_else(
                || self.chat_width_rem(),
                |bounds| f32::from(bounds.size.width) / rem_px,
            );
        self.edge_drag.set(Some(EdgeDrag {
            edge: Edge::Chat,
            from: f32::from(event.position.x),
            start_rem,
            factor: 2.0 * sign,
            live_rem: start_rem,
        }));
        cx.notify();
    }

    /// One edge of the column: a strip in the gutter, just outside it, that takes the drag.
    fn chat_handle(
        &self,
        sign: f32,
        name: &'static str,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = look.theme;
        let (hover, held) = (theme.accent.opacity(0.45), theme.accent.opacity(0.8));
        let dragging = self.resizing_chat();
        // A 2 pt line in the middle of the strip: hidden at rest, drawn while the pointer is on
        // the strip, stronger while it drags.
        let line = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(GUTTER / 2.0 - 1.0))
            .w(px(2.0))
            .rounded(px(1.0))
            .group_hover(name, move |s| s.bg(hover))
            .when(dragging, move |s| s.bg(held));
        let strip = div()
            .id(name)
            .group(name)
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(GUTTER))
            .cursor_col_resize()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    this.start_chat_drag(sign, event, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(line)
            .child(probe(&self.env.regions, Region::Assistant(name)));
        if sign < 0.0 {
            strip.left(px(-GUTTER))
        } else {
            strip.right(px(-GUTTER))
        }
        .into_any_element()
    }

    /// Registers, every frame, the listeners that follow the pointer while an edge is dragged
    /// (wherever it goes) and end the drag when it is released. The width is written once, on
    /// the release.
    fn edge_drag_listeners(&self) -> AnyElement {
        let drag = self.edge_drag.clone();
        let regions = self.env.regions.clone();
        let settings = self.env.settings.clone();
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                let view = window.current_view();
                let (moving, area_of, store) = (drag.clone(), regions.clone(), settings.clone());
                window.on_mouse_event(
                    move |event: &gpui::MouseMoveEvent,
                          phase: gpui::DispatchPhase,
                          window: &mut Window,
                          cx: &mut App| {
                        if phase != gpui::DispatchPhase::Capture {
                            return;
                        }
                        let Some(mut state) = moving.get() else {
                            return;
                        };
                        if event.pressed_button != Some(gpui::MouseButton::Left) {
                            // The release happened where nobody saw it: it ends the drag here.
                            finish_edge_drag(&moving, &store);
                            cx.notify(view);
                            return;
                        }
                        let rem_px = f32::from(window.rem_size());
                        let area = area_of
                            .get(Region::Assistant("conversation"))
                            .map_or(0.0, |bounds| f32::from(bounds.size.width));
                        let travel = f32::from(event.position.x) - state.from;
                        let wanted = state.start_rem + state.factor * travel / rem_px;
                        state.live_rem = match state.edge {
                            Edge::Chat => clamp_chat_rem(wanted, area, rem_px),
                            // The conversation and the column share the page: the column may
                            // take what the conversation can spare.
                            Edge::Rail => clamp_rail_rem(
                                wanted,
                                if area > 0.0 {
                                    area + state.live_rem * rem_px
                                } else {
                                    0.0
                                },
                                rem_px,
                            ),
                        };
                        moving.set(Some(state));
                        cx.notify(view);
                        cx.stop_propagation();
                    },
                );
                let (up, store) = (drag.clone(), settings.clone());
                window.on_mouse_event(
                    move |event: &gpui::MouseUpEvent,
                          phase: gpui::DispatchPhase,
                          _: &mut Window,
                          cx: &mut App| {
                        if phase == gpui::DispatchPhase::Capture
                            && event.button == gpui::MouseButton::Left
                            && finish_edge_drag(&up, &store)
                        {
                            cx.notify(view);
                        }
                    },
                );
            },
        )
        .absolute()
        .size_full()
        .into_any_element()
    }

    // ---- mode, new chat, stop --------------------------------------------------------------

    /// What Mawas may do, as the daemon says; `None` until it answered.
    pub fn mode(&self) -> Option<MawasMode> {
        self.mode
    }

    /// The note under the header ("Advisor applies from your next message …").
    pub fn mode_note(&self) -> Option<&str> {
        self.mode_note.as_deref()
    }

    /// The failure of the last control, if any.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(super) fn set_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = Some(error);
        cx.notify();
    }

    /// Reads `assistant.mode` from the daemon (once per connection).
    fn ensure_policy(&mut self, cx: &mut Context<Self>) {
        if self.policy_asked || !self.connected(cx) {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        if !connection.supports_daemon_settings() {
            // An older daemon has one mode: the read-only one it always had.
            self.policy_asked = true;
            self.mode = Some(MawasMode::Advisor);
            return;
        }
        self.policy_asked = true;
        cx.spawn(async move |this, cx| {
            let settings = connection.daemon_settings().await;
            this.update(cx, |this, cx| {
                match settings {
                    Ok(settings) => {
                        this.mode = settings
                            .entries
                            .iter()
                            .find(|e| e.key == "assistant.mode")
                            .and_then(|e| e.value.as_str())
                            .and_then(MawasMode::from_setting);
                    }
                    Err(_) => this.policy_asked = false,
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Switches Mawas to `mode` (`assistant.mode`): the daemon applies it from the next turn,
    /// restarting the harness session as it needs, and the page says so.
    pub fn set_mode(&mut self, mode: MawasMode, cx: &mut Context<Self>) {
        if self.mode == Some(mode) || !self.connected(cx) {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        self.error = None;
        cx.spawn(async move |this, cx| {
            let stored = connection
                .daemon_setting_set("assistant.mode", Some(json!(mode.setting())))
                .await;
            this.update(cx, |this, cx| {
                match stored {
                    Ok(_) => {
                        this.mode = Some(mode);
                        this.mode_note = Some(mode.next_turn_note());
                    }
                    Err(error) => this.error = Some(format!("Could not switch the mode: {error}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Starts a new conversation in the shown scope and shows it.
    pub fn new_chat(&mut self, cx: &mut Context<Self>) {
        if !self.connected(cx) {
            return;
        }
        let key = self.key(cx);
        let connection = self.env.sessions.read(cx).connection().clone();
        let request = match key {
            ScopeKey::Global => AssistantOpen {
                scope: AssistantScope::Global,
                workspace_id: None,
                new: true,
            },
            ScopeKey::Workspace(id) => AssistantOpen {
                scope: AssistantScope::Workspace,
                workspace_id: Some(id),
                new: true,
            },
        };
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = connection.assistant_open(request).await;
            this.update(cx, |this, cx| {
                this.opened(key, result, cx);
                this.focus_composer(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Whether the page is below 640 pt: the header is folded into ⋯ and Today is a page of its
    /// own (tests, the ⋯ menu).
    pub fn folded(&self) -> bool {
        self.folded
    }

    /// Whether Today replaces the conversation (only below 640 pt, ⋯ → Today).
    pub fn today_only(&self) -> bool {
        self.today_only
    }

    /// ⋯ → Today below 640 pt: Today instead of the conversation, and back.
    pub fn toggle_today_only(&mut self, cx: &mut Context<Self>) {
        self.today_only = !self.today_only;
        cx.notify();
    }

    /// Whether Stop has something to stop: a turn runs, or a request of the shown chat waits.
    pub fn can_stop(&self, cx: &App) -> bool {
        let Some(session) = self.session(cx) else {
            return false;
        };
        let store = self.env.sessions.read(cx);
        store
            .session(session)
            .and_then(|info| info.chat.as_ref())
            .is_some_and(|chat| chat.turn_active || chat.pending_approvals > 0)
            || store.approvals().iter().any(|a| a.session_id == session)
    }

    /// **Stop Mawas**, the kill switch: denies every request the shown chat has pending (its
    /// own Hutan tools and its harness's native ones), then interrupts its turn.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session(cx) else {
            return;
        };
        let denials: Vec<(hutan_core::ApprovalId, String)> = self
            .env
            .sessions
            .read(cx)
            .approvals()
            .iter()
            .filter(|a| a.session_id == session)
            .filter_map(|a| {
                let refuse = a.options.iter().find(|o| {
                    matches!(
                        o.kind,
                        hutan_core::agent::PermissionOptionKind::RejectOnce
                            | hutan_core::agent::PermissionOptionKind::RejectAlways
                    )
                })?;
                Some((a.id, refuse.id.clone()))
            })
            .collect();
        let tasks: Vec<_> = denials
            .into_iter()
            .map(|(approval, option)| {
                self.env
                    .sessions
                    .update(cx, |store, cx| store.respond_approval(approval, option, cx))
            })
            .collect();
        let connection = self.env.sessions.read(cx).connection().clone();
        self.error = None;
        cx.spawn(async move |this, cx| {
            let mut failed = None;
            for task in tasks {
                if let Err(error) = task.await {
                    failed = Some(error);
                }
            }
            // The turn may have ended with its last request: nothing to interrupt then.
            let interrupted = connection.chat_interrupt(session).await;
            this.update(cx, |this, cx| {
                if let Some(error) = failed {
                    this.error = Some(format!("Could not deny a request: {error}"));
                } else if let Err(error) = interrupted
                    && !matches!(&error, ClientError::Remote(body) if body.code == ErrorCode::InvalidState)
                {
                    this.error = Some(format!("Could not stop Mawas: {error}"));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- opening -------------------------------------------------------------------------

    /// Asks the daemon for the shown scope's assistant when that has not been asked yet.
    pub fn ensure_open(&mut self, cx: &mut Context<Self>) {
        let key = self.key(cx);
        if self.state(key) == Opened::Idle {
            self.open(key, cx);
        }
    }

    fn open(&mut self, key: ScopeKey, cx: &mut Context<Self>) {
        let (connection, synced) = {
            let store = self.env.sessions.read(cx);
            (store.connection().clone(), store.synced())
        };
        if !self.connected(cx) || !synced {
            return;
        }
        if !connection.supports_assistant() {
            self.scopes
                .insert(key, Opened::Failed(Failure::Unsupported));
            return;
        }
        self.scopes.insert(key, Opened::Opening);
        let request = match key {
            ScopeKey::Global => AssistantOpen {
                scope: AssistantScope::Global,
                workspace_id: None,
                new: false,
            },
            ScopeKey::Workspace(id) => AssistantOpen {
                scope: AssistantScope::Workspace,
                workspace_id: Some(id),
                new: false,
            },
        };
        cx.spawn(async move |this, cx| {
            let result = connection.assistant_open(request).await;
            this.update(cx, |this, cx| this.opened(key, result, cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    fn opened(
        &mut self,
        key: ScopeKey,
        result: Result<hutan_protocol::messages::SessionInfo, ClientError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(info) => {
                self.ensure_chat(info.id, cx);
                self.scopes.insert(key, Opened::Ready(info.id));
                self.flush(key, info.id, cx);
            }
            Err(ClientError::Remote(body)) if body.code == ErrorCode::InvalidState => {
                self.scopes
                    .insert(key, Opened::Failed(Failure::Unconfigured(body.message)));
                self.waiting.retain(|(waiting, _)| *waiting != key);
            }
            Err(error) => {
                self.scopes
                    .insert(key, Opened::Failed(Failure::Other(error.to_string())));
                self.waiting.retain(|(waiting, _)| *waiting != key);
            }
        }
        cx.notify();
    }

    fn ensure_chat(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if self.chats.contains_key(&session) {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        let model = cx.new(|cx| ChatModel::new(session, connection, cx));
        let (sessions, settings, regions) = (
            self.env.sessions.clone(),
            self.env.settings.clone(),
            self.env.regions.clone(),
        );
        let shown = model.clone();
        // The conversation column (`assistant.chat_width_rem`) is the width, not the reading
        // width a chat of its own has.
        let view = cx.new(|cx| {
            let mut view = ChatView::new(session, shown, sessions, settings, regions, cx);
            view.set_fills_column(true, cx);
            view
        });
        self.chats.insert(session, Chat { model, view });
    }

    fn flush(&mut self, key: ScopeKey, session: SessionId, cx: &mut Context<Self>) {
        let (ready, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waiting)
            .into_iter()
            .partition(|(waiting, _)| *waiting == key);
        self.waiting = rest;
        if let Some(chat) = self.chats.get(&session) {
            for (_, chip) in ready {
                chat.model
                    .update(cx, |model, cx| model.attach_context(chip, cx));
            }
        }
    }

    /// Shows `choice`'s assistant (opening it on first use).
    pub fn choose(&mut self, choice: Choice, cx: &mut Context<Self>) {
        self.choice = choice;
        self.ensure_open(cx);
        cx.notify();
    }

    /// Tries again what failed for the shown scope.
    pub fn retry(&mut self, cx: &mut Context<Self>) {
        let key = self.key(cx);
        if matches!(self.state(key), Opened::Failed(_)) {
            self.scopes.remove(&key);
        }
        self.ensure_open(cx);
        cx.notify();
    }

    /// Shows the assistant `session` (a hit of the history index, say) in its own scope. `false`
    /// when it is not an assistant session this window knows.
    pub fn show_session(&mut self, session: SessionId, cx: &mut Context<Self>) -> bool {
        let found = {
            let store = self.env.sessions.read(cx);
            store
                .session(session)
                .filter(|info| info.chat.as_ref().is_some_and(|chat| chat.is_assistant()))
                .and_then(|info| store.workspace(info.workspace_id))
                .map(|workspace| {
                    let key = ScopeKey::of(workspace);
                    (key, self.choice_for(key, store.active_workspace_id()))
                })
        };
        let Some((key, choice)) = found else {
            return false;
        };
        self.ensure_chat(session, cx);
        self.scopes.insert(key, Opened::Ready(session));
        self.choice = choice;
        cx.notify();
        true
    }

    /// Attaches `chip` to the composer of the assistant `session` and shows it.
    pub fn attach_to_session(
        &mut self,
        session: SessionId,
        chip: ContextChip,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.show_session(session, cx) {
            return false;
        }
        match self.chats.get(&session) {
            Some(chat) => {
                chat.model
                    .update(cx, |model, cx| model.attach_context(chip, cx));
                true
            }
            None => false,
        }
    }

    /// Attaches `chip` to the composer of `key`'s assistant and shows it, opening the assistant
    /// first when need be. The chip waits for the answer of `assistant.open`.
    pub fn attach_to_scope(&mut self, key: ScopeKey, chip: ContextChip, cx: &mut Context<Self>) {
        self.choice = self.choice_for(key, self.env.sessions.read(cx).active_workspace_id());
        match self.state(key) {
            Opened::Ready(session) => {
                if let Some(chat) = self.chats.get(&session) {
                    chat.model
                        .update(cx, |model, cx| model.attach_context(chip, cx));
                }
            }
            _ => {
                self.waiting.push((key, chip));
                if matches!(self.state(key), Opened::Failed(_)) {
                    self.scopes.remove(&key);
                }
                self.open(key, cx);
            }
        }
        cx.notify();
    }

    // ---- backend ---------------------------------------------------------------------------

    /// The chooser of the backend and model of the shown scope's assistant.
    pub fn switcher(&self) -> Option<&Switcher> {
        self.switcher.as_ref()
    }

    /// Opens or closes the chooser (the header's backend button).
    pub fn toggle_switcher(&mut self, cx: &mut Context<Self>) {
        if self.switcher.take().is_some() {
            cx.notify();
            return;
        }
        self.switcher = Some(Switcher {
            backend: None,
            models: Models::Loading,
        });
        self.env
            .harnesses
            .update(cx, |directory, cx| directory.refresh(cx));
        let connection = self.env.sessions.read(cx).connection().clone();
        cx.spawn(async move |this, cx| {
            let list = connection
                .call::<ProviderList>(Request::ProviderList(Empty {}))
                .await;
            this.update(cx, |this, cx| {
                if let Ok(list) = list {
                    this.providers = list.providers;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Lists the models of `backend` in the chooser.
    pub fn choose_backend(&mut self, backend: BackendChoice, cx: &mut Context<Self>) {
        let Some(switcher) = &mut self.switcher else {
            return;
        };
        switcher.backend = Some(backend.clone());
        switcher.models = Models::Loading;
        match backend {
            BackendChoice::Harness(harness) => {
                let id = harness.as_str();
                self.env
                    .harnesses
                    .update(cx, |directory, cx| directory.load_models(id, cx));
                // The directory answers; the chooser shows what it holds when it is drawn.
            }
            BackendChoice::Provider(provider) => {
                let connection = self.env.sessions.read(cx).connection().clone();
                cx.spawn(async move |this, cx| {
                    let answer = connection
                        .call::<ProviderModels>(Request::ProviderModels(ProviderRef {
                            provider_id: provider.clone(),
                        }))
                        .await
                        .map(|m| m.models)
                        .map_err(|e| e.to_string());
                    this.update(cx, |this, cx| {
                        if let Some(switcher) = &mut this.switcher
                            && switcher.backend == Some(BackendChoice::Provider(provider))
                        {
                            switcher.models = match answer {
                                Ok(models) => Models::Ready(models),
                                Err(error) => Models::Failed(error),
                            };
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
        cx.notify();
    }

    /// The models the chooser lists for its backend.
    fn switcher_models(&self, cx: &App) -> Models {
        let Some(switcher) = &self.switcher else {
            return Models::Loading;
        };
        match &switcher.backend {
            Some(BackendChoice::Harness(harness)) => self
                .env
                .harnesses
                .read(cx)
                .models(harness.as_str())
                .cloned()
                .unwrap_or(Models::Loading),
            _ => switcher.models.clone(),
        }
    }

    /// Makes `backend` (and `model`) what the shown scope's assistants run on: stores it
    /// (`assistant.configure`) and opens the scope's assistant again.
    pub fn use_backend(
        &mut self,
        backend: BackendChoice,
        model: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let key = self.key(cx);
        let connection = self.env.sessions.read(cx).connection().clone();
        let (kind, provider) = match backend {
            BackendChoice::Harness(kind) => (kind, None),
            BackendChoice::Provider(id) => (AssistantBackend::Provider, Some(id)),
        };
        let (scope, workspace_id) = match key {
            ScopeKey::Global => (AssistantScope::Global, None),
            ScopeKey::Workspace(id) => (AssistantScope::Workspace, Some(id)),
        };
        self.switcher = None;
        self.scopes.insert(key, Opened::Opening);
        cx.spawn(async move |this, cx| {
            let stored = connection
                .assistant_configure(AssistantConfigure {
                    scope,
                    workspace_id,
                    backend: kind,
                    model,
                    provider,
                })
                .await;
            this.update(cx, |this, cx| match stored {
                Ok(_) => {
                    let pinned = match key {
                        ScopeKey::Global => None,
                        ScopeKey::Workspace(id) => Some(id),
                    };
                    this.env.sessions.update(cx, |store, cx| {
                        store.note_assistant_pinned(pinned, true, cx)
                    });
                    this.scopes.remove(&key);
                    this.open(key, cx);
                }
                Err(error) => this.opened(key, Err(error), cx),
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// The first-run card's "Use OMP": OMP for every assistant that has no choice of its own.
    pub fn use_omp_everywhere(&mut self, cx: &mut Context<Self>) {
        let key = self.key(cx);
        let connection = self.env.sessions.read(cx).connection().clone();
        self.scopes.insert(key, Opened::Opening);
        cx.spawn(async move |this, cx| {
            let stored = connection
                .assistant_configure(AssistantConfigure {
                    scope: AssistantScope::Global,
                    workspace_id: None,
                    backend: AssistantBackend::Omp,
                    model: None,
                    provider: None,
                })
                .await;
            this.update(cx, |this, cx| match stored {
                Ok(_) => {
                    this.env
                        .sessions
                        .update(cx, |store, cx| store.note_assistant_pinned(None, true, cx));
                    this.scopes.remove(&key);
                    this.open(key, cx);
                }
                Err(error) => this.opened(key, Err(error), cx),
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Puts the keyboard in the composer of the shown assistant (as soon as it is drawn).
    pub fn focus_composer(&mut self, cx: &mut Context<Self>) {
        self.focus_wanted = true;
        cx.notify();
    }

    // ---- rendering -----------------------------------------------------------------------

    /// The rows of the scope dropdown: Global, and the workspace the panel follows (greyed out
    /// while there is none).
    pub fn scope_entries(&self, cx: &App) -> Vec<ScopeEntry> {
        let key = self.key(cx);
        let project = self.active_project(cx);
        let pinned = matches!(self.choice, Choice::Pinned(_));
        let workspace_label = match (&self.choice, &project) {
            (Choice::Pinned(id), _) => self.env.sessions.read(cx).workspace(*id).map_or_else(
                || "Workspace".to_owned(),
                |w| format!("Workspace · {}", w.name),
            ),
            (_, Some(project)) => format!("Workspace · {}", project.name),
            _ => "Workspace".to_owned(),
        };
        vec![
            ScopeEntry {
                label: "Global".to_owned(),
                selected: !matches!(key, ScopeKey::Workspace(_)),
                enabled: true,
                choose: Choice::Global,
            },
            ScopeEntry {
                label: workspace_label,
                selected: matches!(key, ScopeKey::Workspace(_)),
                enabled: project.is_some() || pinned,
                choose: if pinned { self.choice } else { Choice::Active },
            },
        ]
    }

    // ---- the Focus and the chats list (ADR 0025) -----------------------------------------------

    /// What Home opens, as the page follows `assistant.home_scope`.
    pub fn home_scope(&self) -> HomeScope {
        self.home_seen
    }

    /// Whether the header shows the **Chats** menu and the **Focus** chip instead of the scope
    /// dropdown: Home opens the Global chat (`home_scope = global`).
    pub fn global_first(&self) -> bool {
        self.home_seen == HomeScope::Global
    }

    /// Whether the Focus chip is a control: the shown chat is a Global one, on a daemon that has
    /// Focus (capability `assistant.focus`). A workspace chat's focus is fixed by its workspace.
    pub fn focus_chip_shown(&self, cx: &App) -> bool {
        self.global_first()
            && self.key(cx) == ScopeKey::Global
            && self
                .env
                .sessions
                .read(cx)
                .connection()
                .supports_assistant_focus()
    }

    /// The Focus of the shown chat (`ChatInfo.focus`); `None`: *All workspaces*.
    pub fn focus(&self, cx: &App) -> Option<WorkspaceId> {
        let session = self.session(cx)?;
        self.env
            .sessions
            .read(cx)
            .session(session)?
            .chat
            .as_ref()?
            .focus
    }

    /// The Focus chip's words after `Focus: `: the workspace's name, or *All workspaces*.
    pub fn focus_label(&self, cx: &App) -> String {
        match self.focus(cx) {
            Some(id) => self
                .env
                .sessions
                .read(cx)
                .workspace(id)
                .map_or_else(|| "A workspace that is gone".to_owned(), |w| w.name.clone()),
            None => "All workspaces".to_owned(),
        }
    }

    /// The rows of the Focus picker: *All workspaces*, then every workspace and project (Home
    /// is never a Focus), the Focus of the shown chat checked.
    pub fn focus_entries(&self, cx: &App) -> Vec<FocusEntry> {
        let store = self.env.sessions.read(cx);
        let now = self.focus(cx);
        let mut rows: Vec<FocusEntry> = store
            .workspaces()
            .iter()
            .filter(|w| w.kind != WorkspaceKind::Home)
            .map(|w| {
                let parent = w
                    .parent_id
                    .and_then(|id| store.workspace(id))
                    .map(|parent| parent.name.as_str());
                FocusEntry {
                    label: match parent {
                        Some(parent) => format!("{parent} / {}", w.name),
                        None => w.name.clone(),
                    },
                    workspace: Some(w.id),
                    selected: now == Some(w.id),
                }
            })
            .collect();
        rows.sort_by_key(|row| row.label.to_lowercase());
        rows.insert(
            0,
            FocusEntry {
                label: "All workspaces".to_owned(),
                workspace: None,
                selected: now.is_none(),
            },
        );
        rows
    }

    /// Sets the shown chat's Focus (`assistant.focus`, the user's choice: Mawas cannot make it);
    /// `None` is *All workspaces*. The session's announcement redraws the chip.
    pub fn set_focus(&mut self, workspace: Option<WorkspaceId>, cx: &mut Context<Self>) {
        let Some(session) = self.session(cx) else {
            return;
        };
        if !self.connected(cx) || self.focus(cx) == workspace {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = connection.assistant_focus(session, workspace).await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.error = Some(format!("Could not set the Focus: {error}"));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The rows of the Chats menu: the Global chats (oldest first, each with its Focus), then
    /// the chats of workspaces that earlier versions made (opening one shows it as it was).
    pub fn chat_entries(&self, cx: &App) -> Vec<ChatEntry> {
        let store = self.env.sessions.read(cx);
        let shown = self.session(cx);
        let home = store.home_workspace().map(|w| w.id);
        let mut chats: Vec<&hutan_protocol::messages::SessionInfo> = store
            .sessions()
            .iter()
            .filter(|s| s.chat.as_ref().is_some_and(|chat| chat.is_assistant()))
            .collect();
        chats.sort_by_key(|s| s.created_at_ms);
        let (global, workspace): (Vec<_>, Vec<_>) = chats
            .into_iter()
            .partition(|s| Some(s.workspace_id) == home);
        let row = |s: &hutan_protocol::messages::SessionInfo, note: Option<String>| ChatEntry {
            label: match note {
                Some(note) if !s.title.contains(&note) => format!("{} · {note}", s.title),
                _ => s.title.clone(),
            },
            session: Some(s.id),
            selected: shown == Some(s.id),
        };
        let mut rows = vec![ChatEntry {
            label: "Global chats".to_owned(),
            session: None,
            selected: false,
        }];
        rows.extend(global.into_iter().map(|s| {
            let focus = s
                .chat
                .as_ref()
                .and_then(|chat| chat.focus)
                .and_then(|id| store.workspace(id))
                .map(|w| format!("Focus {}", w.name));
            ChatEntry {
                label: match focus {
                    Some(focus) => format!("{} · {focus}", s.title),
                    None => s.title.clone(),
                },
                ..row(s, None)
            }
        }));
        if !workspace.is_empty() {
            rows.push(ChatEntry {
                label: "Workspace chats".to_owned(),
                session: None,
                selected: false,
            });
            rows.extend(workspace.into_iter().map(|s| {
                let name = store.workspace(s.workspace_id).map(|w| w.name.clone());
                row(s, name)
            }));
        }
        rows
    }

    /// The session whose model the menu's Switch model… changes: the shown assistant when it
    /// runs on a harness or a provider and is between turns.
    pub fn switchable_session(&self, cx: &App) -> Option<SessionId> {
        if !self.connected(cx) {
            return None;
        }
        let session = self.session(cx)?;
        let chat = self.env.sessions.read(cx).session(session)?.chat.as_ref()?;
        (!chat.turn_active).then_some(session)
    }

    /// Mawas Home's header, one row with a hairline below: the scope and the mode as ghost
    /// menus; the backend·model as muted text and **New chat** while the page is 640 pt wide
    /// or more; **Stop** (the kill switch) only while there is something to stop, at any
    /// width; and ⋯, which holds New chat, Stop, Today and the backend below 640 pt (`folded`).
    fn render_header(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let key = self.key(cx);
        let scope_label = match key {
            ScopeKey::Global => "Global".to_owned(),
            ScopeKey::Workspace(id) => self
                .env
                .sessions
                .read(cx)
                .workspace(id)
                .map_or_else(|| "Workspace".to_owned(), |w| w.name.clone()),
        };
        let scope_note = match key {
            ScopeKey::Global => "Sees the Home folder only".to_owned(),
            ScopeKey::Workspace(id) => {
                let store = self.env.sessions.read(cx);
                store.workspace(id).map_or_else(String::new, |w| {
                    let roots: Vec<String> =
                        w.roots.iter().map(|r| r.display().to_string()).collect();
                    format!("Sees {}", roots.join(", "))
                })
            }
        };
        let state = {
            let store = self.env.sessions.read(cx);
            self.session(cx)
                .and_then(|session| store.session(session))
                .map(|info| store.presentation(info))
        };
        let global_first = self.global_first();
        let (scope_label, scope_note, scope_region) = if global_first {
            (
                "Chats".to_owned(),
                "Your Mawas chats: the Global ones, and the chats of workspaces from before. Global sees the Home folder only.".to_owned(),
                "chats",
            )
        } else {
            (scope_label, scope_note, "scope")
        };
        let scope = Button::new("assistant-scope", scope_label, theme, tokens)
            .compact()
            .ghost()
            .max_width(112.0)
            .tooltip(scope_note)
            .child(icon(Icon::ActionExpand, 12.0, theme.muted))
            .on_click(cx.listener(move |_, e: &gpui::ClickEvent, window, cx| {
                if global_first {
                    window.dispatch_action(Box::new(ShowAssistantChatsMenu(e.position())), cx);
                } else {
                    window.dispatch_action(Box::new(ShowAssistantScopeMenu(e.position())), cx);
                }
            }))
            .child(probe(&self.env.regions, Region::Assistant(scope_region)));
        let focus = self.focus_control(look, cx);
        let dot = state.map(|presentation| {
            let label = SharedString::from(presentation.label.clone());
            div()
                .id("assistant-state")
                .flex_none()
                .tooltip(move |_, cx| cx.new(|_| TextTooltip::new(label.clone())).into())
                .child(state_dot(theme, &presentation))
                .into_any_element()
        });
        let folded = self.folded;
        let more = ui::IconButton::new("assistant-more", Icon::ActionMore, "More", theme, tokens)
            .compact()
            .on_click(cx.listener(|_, e: &gpui::ClickEvent, window, cx| {
                window.dispatch_action(Box::new(ShowAssistantMoreMenu(e.position())), cx);
            }))
            .child(probe(&self.env.regions, Region::Assistant("more")));
        div()
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing))
            .h(px(tokens.pane_header))
            .px(px(tokens.spacing * 2.0))
            .border_b_1()
            .border_color(theme.border)
            .child(scope)
            .children(focus)
            .child(self.mode_chip(look, cx))
            .child(if folded {
                div().flex_1().min_w_0().into_any_element()
            } else {
                self.backend_chip(dot, look, cx)
            })
            .when(!folded, |s| {
                s.child(self.header_button(
                    "assistant-new",
                    "New chat",
                    "new",
                    self.connected(cx),
                    look,
                    cx.listener(|this, _, _, cx| this.new_chat(cx)),
                ))
            })
            .when(self.can_stop(cx), |s| s.child(self.stop_button(look, cx)))
            .when(!folded, |s| s.child(self.today_button(look, cx)))
            .child(more)
            .child(probe(&self.env.regions, Region::Assistant("header")))
            .into_any_element()
    }

    /// The header's Focus control (Home opens Global): the chip that picks the workspace the
    /// shown Global chat looks at, or — for a chat of a workspace from before — its fixed
    /// workspace as muted text. Nothing on a daemon without Focus.
    fn focus_control(&self, look: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (theme, tokens) = (&look.theme, &look.tokens);
        if !self.global_first() {
            return None;
        }
        if self.focus_chip_shown(cx) {
            let label = format!("Focus: {}", self.focus_label(cx));
            return Some(
                Button::new("assistant-focus", label, theme, tokens)
                    .compact()
                    .ghost()
                    .max_width(if self.folded { 168.0 } else { 200.0 })
                    .tooltip(
                        "Focus narrows what Mawas looks at to one workspace and adds its notes and project line. It changes no permission.",
                    )
                    .child(icon(Icon::ActionExpand, 12.0, theme.muted))
                    .on_click(cx.listener(|_, e: &gpui::ClickEvent, window, cx| {
                        window.dispatch_action(Box::new(ShowAssistantFocusMenu(e.position())), cx);
                    }))
                    .child(probe(&self.env.regions, Region::Assistant("focus")))
                    .into_any_element(),
            );
        }
        let ScopeKey::Workspace(id) = self.key(cx) else {
            return None;
        };
        let name = self
            .env
            .sessions
            .read(cx)
            .workspace(id)
            .map_or_else(|| "Workspace".to_owned(), |w| w.name.clone());
        let tip = SharedString::from(
            "This chat belongs to the workspace, so its focus is fixed. Choose a Global chat in Chats to look across workspaces.",
        );
        Some(
            div()
                .id("assistant-focus-fixed")
                .relative()
                .flex_none()
                .max_w(px(188.0))
                .px(px(tokens.spacing))
                .truncate()
                .text_color(theme.muted)
                .text_size(px(tokens.secondary_font_size))
                .tooltip(move |_, cx| cx.new(|_| TextTooltip::new(tip.clone())).into())
                .child(SharedString::from(format!("Workspace · {name}")))
                .child(probe(&self.env.regions, Region::Assistant("focus")))
                .into_any_element(),
        )
    }

    /// The header's Today button (a page of 640 pt or more): *Hide Today* while the rail shows,
    /// *Show Today* while it is hidden, and then, as a badge, how many sessions have a request.
    fn today_button(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let open = self.rail_open;
        let waiting = self.today_badge(cx);
        let badge = (waiting > 0).then(|| {
            div()
                .absolute()
                .top(px(-3.0))
                .right(px(-3.0))
                .flex()
                .items_center()
                .justify_center()
                .min_w(px(14.0))
                .h(px(14.0))
                .px(px(3.0))
                .rounded(px(crate::theme::pill_radius()))
                .bg(theme.accent)
                .text_color(theme.on_accent())
                .text_size(px(tokens.group_font_size))
                .child(SharedString::from(waiting.to_string()))
                .child(probe(&self.env.regions, Region::Assistant("today-badge")))
        });
        ui::IconButton::new(
            "assistant-today",
            Icon::DockRight,
            if open { "Hide Today" } else { "Show Today" },
            &theme,
            &tokens,
        )
        .compact()
        .toggled(open)
        .on_click(cx.listener(|this, _, _, cx| this.toggle_rail(cx)))
        .child(probe(&self.env.regions, Region::Assistant("today-toggle")))
        .children(badge)
        .into_any_element()
    }

    /// A small ghost text button of the header: no outline, a hover fill.
    fn header_button(
        &self,
        id: &'static str,
        label: &'static str,
        region: &'static str,
        enabled: bool,
        look: &Look,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        Button::new(id, label, &theme, &tokens)
            .compact()
            .ghost()
            .disabled(!enabled)
            .on_click(on_click)
            .child(probe(&self.env.regions, Region::Assistant(region)))
            .into_any_element()
    }

    /// **Stop**: the kill switch, in the error color (text, no outline) and drawn only while
    /// there is something to stop, at any width.
    fn stop_button(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        Button::new("assistant-stop", "Stop", &theme, &tokens)
            .compact()
            .destructive()
            .tooltip("Stop Mawas: deny what it asks and interrupt its turn")
            .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
            .child(probe(&self.env.regions, Region::Assistant("stop")))
            .into_any_element()
    }

    /// The mode chip: what Mawas may do, opening the menu that switches it.
    fn mode_chip(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let label = self.mode.map_or("Mode…", MawasMode::label);
        let help = self.mode.map_or("", MawasMode::help);
        let enabled = self.connected(cx) && self.mode.is_some();
        Button::new("assistant-mode", label, &theme, &tokens)
            .compact()
            .ghost()
            .disabled(!enabled)
            .tooltip(help)
            .child(icon(Icon::ActionExpand, 12.0, theme.muted))
            .on_click(cx.listener(|_, e: &gpui::ClickEvent, window, cx| {
                window.dispatch_action(Box::new(ShowAssistantModeMenu(e.position())), cx);
            }))
            .child(probe(&self.env.regions, Region::Assistant("mode")))
            .into_any_element()
    }

    /// The model id the shown assistant's session runs, when it runs on `choice`; `None` when
    /// the panel shows no session yet, another backend, or a session whose model is not known.
    fn running_model_id(&self, choice: &BackendChoice, cx: &App) -> Option<String> {
        let session = self.session(cx)?;
        let chat = self.env.sessions.read(cx).session(session)?.chat.clone()?;
        let same = match choice {
            BackendChoice::Harness(kind) => chat.agent == kind.as_str(),
            BackendChoice::Provider(id) => {
                chat.agent == "assistant" && chat.provider_id.as_deref() == Some(id.as_str())
            }
        };
        (same && !chat.model.is_empty()).then_some(chat.model)
    }

    /// [`running_model_id`](Self::running_model_id) as a person reads it: by its display name
    /// when the desktop knows the harness's catalog.
    fn running_model(&self, choice: &BackendChoice, cx: &App) -> Option<String> {
        let id = self.running_model_id(choice, cx)?;
        Some(match choice {
            BackendChoice::Harness(kind) => self
                .env
                .sessions
                .read(cx)
                .model_names()
                .label(kind.as_str(), &id),
            BackendChoice::Provider(_) => id,
        })
    }

    /// The header's backend and model: what the shown assistant runs on, opening the chooser.
    /// With nothing chosen (no choice of this window's, none in `daemon.toml`) the daemon picked
    /// the backend, and the label begins "Automatic · " so it does not read like a pinned
    /// choice. A session whose model is unknown (the harness's own default runs) says
    /// "Automatic" for the model.
    pub fn backend_label(&self, cx: &App) -> String {
        let workspace = match self.key(cx) {
            ScopeKey::Global => None,
            ScopeKey::Workspace(id) => Some(id),
        };
        let automatic = self.env.sessions.read(cx).assistant_is_automatic(workspace);
        self.session(cx)
            .and_then(|session| {
                let store = self.env.sessions.read(cx);
                let chat = store.session(session)?.chat.clone()?;
                let backend = if chat.agent == "assistant" {
                    chat.provider_id
                        .clone()
                        .unwrap_or_else(|| "provider".to_owned())
                } else {
                    // `Claude Code (Work)` when it runs as a managed account, as the chat's header.
                    crate::workbench::chat::who_runs(&harnesses::harness_name(&chat.agent), &chat)
                };
                // The model by its display name when the desktop knows the harness's catalog.
                let model = store.model_names().label(&chat.agent, &chat.model);
                Some(match (automatic, chat.model.is_empty()) {
                    (true, true) => format!("Automatic · {backend}"),
                    (true, false) => format!("Automatic · {backend} · {model}"),
                    (false, true) => format!("{backend} · Automatic"),
                    (false, false) => format!("{backend} · {model}"),
                })
            })
            .unwrap_or_else(|| "Choose backend".to_owned())
    }

    /// The header's backend·model label: what the shown assistant runs on, as muted text with
    /// the state dot, cut with "…" and right-aligned in the room the other controls leave (a
    /// page below 640 pt keeps it in ⋯). It opens the menu with Switch model… and the chooser.
    fn backend_chip(
        &self,
        dot: Option<AnyElement>,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let label = self.backend_label(cx);
        let enabled = self.connected(cx);
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(4.0))
            .children(dot)
            .child(
                Button::new(
                    "assistant-backend",
                    SharedString::from(label),
                    &theme,
                    &tokens,
                )
                .compact()
                .ghost()
                .max_width(320.0)
                .disabled(!enabled)
                .child(icon(Icon::ActionExpand, 12.0, theme.muted))
                .on_click(cx.listener(|_, e: &gpui::ClickEvent, window, cx| {
                    window.dispatch_action(Box::new(ShowAssistantModelMenu(e.position())), cx);
                }))
                .child(probe(&self.env.regions, Region::Assistant("backend"))),
            )
            .into_any_element()
    }

    /// The chooser: backends first, then the models of the one picked; a model (or the
    /// backend's default) is the choice.
    fn render_switcher(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let directory = self.env.harnesses.read(cx);
        let backend = self.switcher.as_ref().and_then(|s| s.backend.clone());
        let mut choices: Vec<(String, BackendChoice, String, bool, &'static str)> = Vec::new();
        for harness in HARNESSES {
            let kind = match harness.id {
                harnesses::OMP => AssistantBackend::Omp,
                harnesses::CLAUDE => AssistantBackend::Claude,
                _ => AssistantBackend::Codex,
            };
            let detection = directory.detection(harness.id);
            let detail = match &detection {
                Detection::NotFound(how) => how.clone(),
                _ => harnesses::login_summary(directory, harness.id),
            };
            let usable = !matches!(detection, Detection::NotFound(_));
            choices.push((
                format!("backend-{}", harness.id),
                BackendChoice::Harness(kind),
                detail,
                usable,
                match harness.id {
                    harnesses::OMP => "backend-omp",
                    harnesses::CLAUDE => "backend-claude",
                    _ => "backend-codex",
                },
            ));
        }
        for provider in &self.providers {
            choices.push((
                format!("backend-provider-{}", provider.id),
                BackendChoice::Provider(provider.id.clone()),
                provider.base_url.clone(),
                true,
                "backend-provider",
            ));
        }
        let rows: Vec<AnyElement> = choices
            .into_iter()
            .map(|(id, choice, detail, usable, region)| {
                let selected = backend.as_ref() == Some(&choice);
                let picked = choice.clone();
                div()
                    .id(SharedString::from(id))
                    .flex()
                    .flex_col()
                    .px(px(tokens.spacing * 2.0))
                    .py(px(tokens.spacing))
                    .rounded(px(tokens.radius))
                    .when(selected, |s| s.bg(theme.accent_soft))
                    .when(usable, |s| {
                        s.cursor_pointer()
                            .hover(move |s| s.bg(theme.surface_hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.choose_backend(picked.clone(), cx);
                            }))
                    })
                    .when(!usable, |s| s.opacity(0.6))
                    .child(div().medium().child(SharedString::from(choice.label())))
                    .child(
                        div()
                            .text_color(theme.muted)
                            .text_size(px(tokens.secondary_font_size))
                            .child(SharedString::from(detail)),
                    )
                    .child(probe(&self.env.regions, Region::Assistant(region)))
                    .into_any_element()
            })
            .collect();
        let mut models: Vec<AnyElement> = Vec::new();
        if let Some(chosen) = backend.clone() {
            let default_choice = chosen.clone();
            // No stored model: the daemon picks one. Once the session exists it says which.
            let automatic = match self.running_model(&chosen, cx) {
                Some(model) => format!("Automatic · runs {model}"),
                None => "Automatic".to_owned(),
            };
            models.push(
                Button::new(
                    "assistant-model-default",
                    automatic,
                    &look.theme,
                    &look.tokens,
                )
                .primary()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.use_backend(default_choice.clone(), None, cx);
                }))
                .child(probe(&self.env.regions, Region::Assistant("use-default")))
                .into_any_element(),
            );
            match self.switcher_models(cx) {
                Models::Loading => models.push(
                    div()
                        .text_color(theme.muted)
                        .child("Loading the models…")
                        .into_any_element(),
                ),
                Models::Failed(error) => models.push(
                    div()
                        .text_color(theme.danger)
                        .child(SharedString::from(error))
                        .into_any_element(),
                ),
                Models::Ready(list) => {
                    // In the pickers' order, the dated twin told apart, the running one checked.
                    let (agent, ordered) = match &chosen {
                        BackendChoice::Harness(kind) => (kind.as_str(), true),
                        BackendChoice::Provider(_) => ("", false),
                    };
                    let running = self.running_model_id(&chosen, cx);
                    let rows = picker_rows(agent, &list, "", running.as_deref(), ordered);
                    for (index, row) in rows.into_iter().enumerate() {
                        let chosen = chosen.clone();
                        let id = row.model.id.clone();
                        let mut label = row.model.display_name.clone();
                        if let Some(variant) = &row.variant {
                            label = format!("{label} {variant}");
                        }
                        if row.current {
                            label = format!("✓ {label}");
                        }
                        models.push(
                            Button::new(
                                format!("assistant-model-{index}"),
                                label,
                                &look.theme,
                                &look.tokens,
                            )
                            .ghost()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.use_backend(chosen.clone(), Some(id.clone()), cx);
                            }))
                            .into_any_element(),
                        );
                    }
                }
            }
        }
        hutan_ui::scroll_y(
            "assistant-switcher-scroll",
            div()
                .id("assistant-switcher")
                .size_full()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing * 2.0))
                .p(px(tokens.spacing * 3.0))
                .child(ui::section_label(
                    "What the assistant runs on",
                    &theme,
                    &tokens,
                ))
                .children(rows)
                .child(ui::section_label("Model", &theme, &tokens))
                .children(models),
        )
        .into_any_element()
    }

    fn notice(
        &self,
        mascot: Mascot,
        title: &str,
        detail: Vec<String>,
        buttons: Vec<AnyElement>,
        look: &Look,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 4.0))
            .child(ui::mascot(mascot, crate::theme::mascot_size()))
            .child(
                div()
                    .prose(tokens)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(SharedString::from(title.to_owned())),
            )
            .children(detail.into_iter().map(|line| {
                div()
                    .max_w(px(520.0))
                    .prose(tokens)
                    .text_color(theme.muted)
                    .child(SharedString::from(line))
            }))
            .child(div().flex().gap(px(tokens.spacing * 2.0)).children(buttons))
            .into_any_element()
    }

    fn render_body(&mut self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let key = self.key(cx);
        if matches!(self.state(key), Opened::Failed(Failure::Unconfigured(_)))
            && *self.env.harnesses.read(cx).listing() == Listing::Idle
        {
            self.env
                .harnesses
                .update(cx, |directory, cx| directory.refresh(cx));
        }
        let retry = Button::new("assistant-retry", "Retry", &look.theme, &look.tokens)
            .on_click(cx.listener(|this, _, _, cx| this.retry(cx)))
            .child(probe(&self.env.regions, Region::Assistant("retry")))
            .into_any_element();
        let models = Button::new(
            "assistant-models",
            "Add a provider…",
            &look.theme,
            &look.tokens,
        )
        .primary()
        .on_click(|_, window, cx| window.dispatch_action(Box::new(ManageProviders), cx))
        .child(probe(&self.env.regions, Region::Assistant("models")))
        .into_any_element();
        if self.switcher.is_some() && self.connected(cx) {
            return self.render_switcher(look, cx);
        }
        if !self.connected(cx) {
            return self.notice(
                Mascot::Sleep,
                "Not connected to hutand",
                vec!["The assistant opens as soon as the daemon is back.".to_owned()],
                Vec::new(),
                look,
            );
        }
        match self.state(key) {
            Opened::Idle | Opened::Opening => self.notice(
                Mascot::Thinking,
                "Opening the assistant…",
                Vec::new(),
                Vec::new(),
                look,
            ),
            Opened::Ready(session) => match self.chats.get(&session) {
                Some(chat) => div()
                    .size_full()
                    .child(chat.view.clone())
                    .into_any_element(),
                None => self.notice(
                    Mascot::Thinking,
                    "Opening the assistant…",
                    Vec::new(),
                    Vec::new(),
                    look,
                ),
            },
            Opened::Failed(Failure::Unconfigured(_)) => {
                let directory = self.env.harnesses.read(cx);
                let omp_ready = directory.detection(harnesses::OMP) == Detection::Installed
                    && !directory.logged_in(harnesses::OMP).is_empty();
                let can_choose = self
                    .env
                    .sessions
                    .read(cx)
                    .connection()
                    .supports_assistant_backend();
                let mut buttons: Vec<AnyElement> = Vec::new();
                if omp_ready && can_choose {
                    buttons.push(
                        Button::new("assistant-use-omp", "Use OMP", &look.theme, &look.tokens)
                            .primary()
                            .on_click(cx.listener(|this, _, _, cx| this.use_omp_everywhere(cx)))
                            .child(probe(&self.env.regions, Region::Assistant("use-omp")))
                            .into_any_element(),
                    );
                }
                buttons.push(models);
                buttons.push(retry);
                let detail = if can_choose {
                    "Answer through OMP when you are logged in to it (Settings → Accounts), or add a model provider with your own key. Nothing is sent anywhere until you choose."
                } else {
                    "This hutand is older than the app: restart it (Daemon → Restart) to choose OMP, Claude Code or Codex. A model provider works as before."
                };
                self.notice(
                    Mascot::Empty,
                    "Set up the assistant",
                    vec![detail.to_owned()],
                    buttons,
                    look,
                )
            }
            Opened::Failed(Failure::Unsupported) => self.notice(
                Mascot::Error,
                "This hutand has no assistant",
                vec![
                    "Restart the daemon (Daemon → Restart) to run the version of this app."
                        .to_owned(),
                ],
                vec![retry],
                look,
            ),
            Opened::Failed(Failure::Other(error)) => {
                // The configured backend failed to start: offer another one, not only a
                // provider (a harness backend has nothing to do with provider keys).
                let change = Button::new(
                    "assistant-change-backend",
                    "Change backend…",
                    &look.theme,
                    &look.tokens,
                )
                .primary()
                .on_click(cx.listener(|this, _, _, cx| this.toggle_switcher(cx)))
                .child(probe(
                    &self.env.regions,
                    Region::Assistant("change-backend"),
                ))
                .into_any_element();
                // A harness the daemon could not find: offer to install it from its source.
                let install = crate::harness_install::harness_of_missing(&error).map(|harness| {
                    Button::new("assistant-install", "Install…", &look.theme, &look.tokens)
                        .primary()
                        .on_click(move |_, window, cx| {
                            window.dispatch_action(
                                Box::new(crate::workbench::actions::InstallHarness(
                                    harness.to_owned(),
                                )),
                                cx,
                            );
                        })
                        .child(probe(&self.env.regions, Region::Assistant("install")))
                        .into_any_element()
                });
                let mut buttons: Vec<AnyElement> = install.into_iter().collect();
                buttons.push(change);
                buttons.push(retry);
                self.notice(
                    Mascot::Error,
                    "The assistant could not be opened",
                    vec![error],
                    buttons,
                    look,
                )
            }
        }
    }
}

impl Render for AssistantPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders += 1;
        self.env.regions.rendered(cx.entity_id());
        self.ensure_open(cx);
        self.ensure_policy(cx);
        if self.focus_wanted
            && let Some(chat) = self.session(cx).and_then(|s| self.chats.get(&s))
        {
            self.focus_wanted = false;
            let handle = chat.view.focus_handle(cx);
            window.focus(&handle, cx);
        }
        // `assistant.home_scope` changed: Home opens what the new value says.
        let home_setting = self.env.settings.get::<AssistantSettings>().home_scope;
        if home_setting != self.home_seen {
            self.home_seen = home_setting;
            self.choice = Choice::opened_by(home_setting);
            self.switcher = None;
        }
        // Another window (or the file) changed `assistant.rail_open`: this page follows.
        let rail_setting = self.env.settings.get::<AssistantSettings>().rail_open;
        if rail_setting != self.rail_seen {
            self.rail_seen = rail_setting;
            self.rail_open = rail_setting;
        }
        let look = Look::of(cx);
        let scale = f32::from(window.rem_size()) / 16.0;
        let measured = self.width.get();
        self.folded = if measured > 0.0 {
            measured < FOLD_BELOW * scale
        } else {
            f32::from(window.viewport_size().width) * 0.6 < FOLD_BELOW * scale
        };
        let header = self.render_header(&look, cx);
        let body = self.render_body(&look, cx);
        let (theme, tokens) = (look.theme, look.tokens);
        let notes: Vec<AnyElement> = [
            self.mode_note
                .as_ref()
                .map(|note| (note, theme.muted, "mode-note")),
            self.error
                .as_ref()
                .map(|error| (error, theme.danger, "error")),
        ]
        .into_iter()
        .flatten()
        .map(|(text, color, region)| {
            div()
                .relative()
                .flex_none()
                .px(px(tokens.spacing * 3.0))
                .py(px(tokens.spacing))
                .border_b_1()
                .border_color(theme.border)
                .text_color(color)
                .text_size(px(tokens.secondary_font_size))
                .child(SharedString::from(text.clone()))
                .child(probe(&self.env.regions, Region::Assistant(region)))
                .into_any_element()
        })
        .collect();
        // Below 640 pt the backend leaves the header for ⋯; what Mawas runs on still shows, as
        // one muted line under it.
        let backend_line = self.folded.then(|| {
            div()
                .relative()
                .flex_none()
                .px(px(tokens.spacing * 3.0))
                .py(px(tokens.spacing * 0.5))
                .border_b_1()
                .border_color(theme.border)
                .text_color(theme.muted)
                .text_size(px(tokens.secondary_font_size))
                .truncate()
                .child(SharedString::from(self.backend_label(cx)))
                .child(probe(&self.env.regions, Region::Assistant("backend-line")))
        });
        let connected = self.connected(cx);
        let today_page = self.folded && self.today_only && connected;
        let rem_px = f32::from(window.rem_size());
        self.ensure_memory(cx);
        self.ensure_recent(cx);
        self.sync_catch_up(cx);
        let rail = (!self.folded && self.rail_open && connected)
            .then(|| self.render_rail(&look, false, rem_px, cx));
        let asks = self.render_chat_asks(&look, cx);
        let width_cell = self.width.clone();
        let folded_for_paint = self.folded;
        let column_width = px(self.chat_width_rem() * f32::from(window.rem_size()));
        // A page below 640 pt has no room to drag: no handles, and nothing listens.
        let handles = (!self.folded).then(|| {
            [
                self.chat_handle(-1.0, "chat-edge-left", &look, cx),
                self.chat_handle(1.0, "chat-edge-right", &look, cx),
            ]
        });
        let listeners = (!self.folded).then(|| self.edge_drag_listeners());
        let resizing = self.resizing_chat() || self.resizing_rail();
        let conversation = div()
            .relative()
            .flex()
            .flex_1()
            .min_w_0()
            .justify_center()
            .px(px(GUTTER))
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(column_width)
                    .h_full()
                    .children(asks)
                    .child(div().relative().flex_1().min_h_0().child(body))
                    .children(handles.into_iter().flatten())
                    .child(probe(&self.env.regions, Region::Assistant("column"))),
            )
            .child(probe(&self.env.regions, Region::Assistant("conversation")))
            // While an edge is dragged the pointer is the resize one wherever it goes.
            .when(resizing, |s| {
                s.child(div().absolute().size_full().cursor_col_resize())
            })
            .children(listeners);
        div()
            .id("assistant-panel")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.fg)
            .text_size(px(tokens.font_size))
            .child(header)
            .children(backend_line)
            .children(notes)
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    // Below 640 pt Today is a page of its own (⋯ → Today), never a column that
                    // leaves the composer a few pixels.
                    .when(!today_page, |s| s.child(conversation))
                    .when(today_page, |s| {
                        s.child(self.render_rail(&look, true, rem_px, cx))
                    })
                    .children(rail),
            )
            .child(
                // The width of this frame chooses the fold of the next; a frame on the other
                // side of the line draws once more.
                canvas(
                    |bounds, _, _| bounds,
                    move |_, bounds, window, cx| {
                        let width: f32 = bounds.size.width.into();
                        let scale = f32::from(window.rem_size()) / 16.0;
                        width_cell.set(width);
                        if (width < FOLD_BELOW * scale) != folded_for_paint {
                            cx.notify(window.current_view());
                            // Asking for the frame wakes the platform: a redraw that is only marked is not drawn
                            // until the next input.
                            window.request_animation_frame();
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
