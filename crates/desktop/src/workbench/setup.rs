//! The setup (design.md § First-run setup): one flow, re-runnable at any time, a centered sheet in
//! the v2 style that walks through Welcome, Look, Editing (Vim mode, the send key), Agents,
//! History, Background, Command line and Done. It opens from the first screen, from the
//! reinstall card, from the Hutan menu's *Run Setup Again…*, from ⌘K "Hutan: Run Setup Again…",
//! from Settings → General and from `hutan setup`, and every time shows today's values.
//!
//! Every choice is kept in [`SetupState`] while its step is on screen and written when the step is
//! left (Continue or Back), and only when it differs from what the settings say: a step can be
//! skipped (*Skip this step*) and keeps every value, and finishing resets nothing. *Skip setup*
//! writes `app.setup_done` and nothing else. The Look step previews live: the window changes as
//! the choice does, and goes back to the settings when the step or the setup is skipped. Nothing
//! here talks to the system by itself: the login item, the notification permission, the CLI link
//! and the agent integrations each have their own seam and are only used when the user presses
//! their control.

use std::collections::BTreeSet;

use gpui::{
    AnyElement, ClipboardItem, Context, InteractiveElement, IntoElement, MouseButton,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
    relative,
};
use hutan_protocol::assistant::{AssistantBackend, AssistantConfigure, AssistantScope};
use hutan_settings::{AppearanceMode, Density, SettingsStore};

use super::actions::{InstallHarness, setup as setup_actions};
use super::harnesses::{Detection, HARNESSES, Harness, login_summary};
use super::render::probe;
use super::ui::motion::{self, PresentExt as _};
use super::ui::scroll::ScrollExt as _;
use super::ui::{self, Button, ButtonKind, Mascot, StyledExt as _};
use super::view::{Region, Workbench};
use crate::cli_link::{CliLink, Manual, Plan};
use crate::integrations::IntegrationStatus;
use crate::macos::NotificationAnswer;
use crate::settings::{
    AppSettings, AppearanceSettings, AssistantSettings, ChatSettings, EditorSettings,
    IntegrationsSettings, SendKey,
};
use crate::theme::{DensityExt as _, Theme};

/// Width of the sheet.
const WIDTH: f32 = 600.0;
/// The shortcuts the Background step offers for the system-wide Assistant shortcut (the setting
/// takes any; empty is off).
const SHORTCUTS: [(&str, &str); 3] = [
    ("", "Off"),
    ("alt-space", "⌥ Space"),
    ("cmd-shift-space", "⌘⇧ Space"),
];

/// The steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupStep {
    Welcome,
    Look,
    Editing,
    Agents,
    History,
    Background,
    CommandLine,
    Done,
}

impl SetupStep {
    pub const ALL: [SetupStep; 8] = [
        SetupStep::Welcome,
        SetupStep::Look,
        SetupStep::Editing,
        SetupStep::Agents,
        SetupStep::History,
        SetupStep::Background,
        SetupStep::CommandLine,
        SetupStep::Done,
    ];

    /// Position in [`ALL`](Self::ALL).
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    pub fn next(self) -> Option<Self> {
        Self::ALL.get(self.index() + 1).copied()
    }

    pub fn previous(self) -> Option<Self> {
        self.index()
            .checked_sub(1)
            .and_then(|i| Self::ALL.get(i))
            .copied()
    }

    pub fn title(self) -> &'static str {
        match self {
            SetupStep::Welcome => "Welcome",
            SetupStep::Look => "Look",
            SetupStep::Editing => "Editing",
            SetupStep::Agents => "Agents",
            SetupStep::History => "History",
            SetupStep::Background => "Background",
            SetupStep::CommandLine => "Command line",
            SetupStep::Done => "Done",
        }
    }

    /// Steps that ask something: each can be skipped on its own.
    pub fn skippable(self) -> bool {
        !matches!(self, SetupStep::Welcome | SetupStep::Done)
    }

    fn mascot(self) -> Mascot {
        match self {
            SetupStep::Welcome => Mascot::AngleFront,
            SetupStep::Look => Mascot::Angle34Left,
            SetupStep::Editing => Mascot::Focus,
            SetupStep::Agents => Mascot::Focus,
            SetupStep::History => Mascot::Thinking,
            SetupStep::Background => Mascot::Sleep,
            SetupStep::CommandLine => Mascot::AngleLookDown,
            SetupStep::Done => Mascot::Success,
        }
    }
}

/// Where the notification permission stands in the Background step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationUi {
    /// The button has not been pressed.
    NotAsked,
    /// The system's prompt is up.
    Asking,
    Answered(NotificationAnswer),
}

/// What the user picked so far; written to the settings when the step is left. Every field
/// starts at today's value ([`SetupState::from_settings`]).
#[derive(Debug, Clone)]
pub struct SetupState {
    pub step: SetupStep,
    pub appearance: AppearanceMode,
    pub density: Density,
    /// `editor.vim_mode`.
    pub vim_mode: bool,
    /// `chat.send_key`.
    pub send_key: SendKey,
    pub assistant_enabled: bool,
    /// `None` until picked: the default is the first ready harness (OMP, Claude Code, Codex),
    /// else none ([`Workbench::setup_backend`]).
    pub assistant_backend: Option<Option<AssistantBackend>>,
    pub auto_install: bool,
    /// The harnesses whose hooks the user ticked to install when the Agents step is left.
    pub hooks: BTreeSet<&'static str>,
    /// The History step's sources, in [`HISTORY_SOURCES`] order: what the index reads now (none
    /// while indexing is off or the daemon has not answered). Reading other tools' transcripts
    /// is something the user chooses.
    pub history: [bool; 4],
    /// `history` as the daemon last said it, which leaving the step compares with.
    pub history_now: [bool; 4],
    /// The user ticked or unticked a source: a late answer from the daemon no longer replaces
    /// the ticks.
    pub history_touched: bool,
    pub keep_running: bool,
    pub start_at_login: bool,
    pub global_shortcut: String,
    pub notifications: NotificationUi,
    pub cli_link: bool,
    /// What making the link did, shown on the last step.
    pub cli_outcome: Option<String>,
    /// The setup is hidden while a harness login runs; ⌘K "Hutan: Run Setup Again…" continues it.
    pub suspended: bool,
}

impl SetupState {
    fn from_settings(settings: &SettingsStore) -> Self {
        let look = settings.get::<AppearanceSettings>();
        let app = settings.get::<AppSettings>();
        let assistant = settings.get::<AssistantSettings>();
        Self {
            step: SetupStep::Welcome,
            appearance: look.appearance,
            density: look.density,
            vim_mode: settings.get::<EditorSettings>().vim_mode,
            send_key: settings.get::<ChatSettings>().send_key,
            assistant_enabled: assistant.enabled,
            assistant_backend: None,
            auto_install: settings.get::<IntegrationsSettings>().auto_install,
            hooks: BTreeSet::new(),
            history: [false; 4],
            history_now: [false; 4],
            history_touched: false,
            keep_running: app.keep_running,
            start_at_login: app.start_at_login,
            global_shortcut: assistant.global_shortcut.clone(),
            notifications: NotificationUi::NotAsked,
            cli_link: false,
            cli_outcome: None,
            suspended: false,
        }
    }
}

/// What the History step offers to read: the source's id in `daemon.toml` (`history.<id>`), its
/// name, and where it reads.
pub const HISTORY_SOURCES: [(&str, &str, &str); 4] = [
    ("hutan", "Hutan", "this app's own terminals and chats"),
    ("claude", "Claude Code", "~/.claude/projects"),
    ("codex", "Codex", "~/.codex/sessions"),
    ("omp", "OMP", "~/.omp/agent/sessions"),
];

/// What the Done step's buttons do after the setup closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupFinish {
    /// ⌘O.
    OpenFolder,
    NewTerminal,
}

/// The setup inside the workbench.
pub(crate) struct SetupControl {
    state: Option<SetupState>,
    /// The state of a setup that just closed, while its sheet fades out.
    exit: motion::Leaving<SetupState>,
    pub(crate) focus: gpui::FocusHandle,
    cli_link: Option<CliLink>,
}

impl SetupControl {
    pub(crate) fn new(focus: gpui::FocusHandle, cli_link: Option<CliLink>) -> Self {
        Self {
            state: None,
            exit: motion::Leaving::default(),
            focus,
            cli_link,
        }
    }
}

/// Where each harness explains how to install it.
fn install_guide(harness: &str) -> &'static str {
    match harness {
        "omp" => "https://www.npmjs.com/package/@oh-my-pi/pi-coding-agent",
        "claude" => "https://docs.claude.com/en/docs/claude-code/setup",
        _ => "https://github.com/openai/codex",
    }
}

fn appearance_key(mode: AppearanceMode) -> &'static str {
    match mode {
        AppearanceMode::System => "system",
        AppearanceMode::Light => "light",
        AppearanceMode::Dark => "dark",
    }
}

fn density_key(density: Density) -> &'static str {
    match density {
        Density::Compact => "compact",
        Density::Comfortable => "comfortable",
    }
}

impl Workbench {
    // ---- read access ------------------------------------------------------------------------

    /// The setup's state while it is open or paused.
    pub fn setup(&self) -> Option<&SetupState> {
        self.setup.state.as_ref()
    }

    /// The setup is on screen.
    pub fn setup_open(&self) -> bool {
        self.setup.state.as_ref().is_some_and(|s| !s.suspended)
    }

    /// The appearance the window shows now: the settings', or the Look step's pending choice.
    pub fn shown_appearance(&self) -> AppearanceMode {
        self.chrome
    }

    /// The backend the Agents step shows for the assistant: the user's pick, else the first
    /// harness that is ready, in the daemon's order for an assistant with nothing configured
    /// (OMP, Claude Code, Codex): logged in and, for Claude Code, with its adapter found as far
    /// as the directory knows. None when no harness is ready.
    pub fn setup_backend(&self, cx: &gpui::App) -> Option<AssistantBackend> {
        let state = self.setup.state.as_ref()?;
        match state.assistant_backend {
            Some(picked) => picked,
            None => {
                let directory = self.env.harnesses.read(cx);
                [
                    (super::harnesses::OMP, AssistantBackend::Omp),
                    (super::harnesses::CLAUDE, AssistantBackend::Claude),
                    (super::harnesses::CODEX, AssistantBackend::Codex),
                ]
                .into_iter()
                .find(|(harness, _)| {
                    !directory.logged_in(harness).is_empty() && directory.startable(harness)
                })
                .map(|(_, backend)| backend)
            }
        }
    }

    // ---- opening and closing ---------------------------------------------------------------

    /// *Run Setup Again…* (the Hutan menu, ⌘K, Settings → General, the first screen, the
    /// reinstall card, `hutan setup`): opens the setup at its first step with today's values, or
    /// brings a paused one (a login was running) back where it was.
    pub fn open_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self.picker = None;
        self.context_menu = None;
        self.sheet = None;
        self.daemon.popover = false;
        match &mut self.setup.state {
            Some(state) => state.suspended = false,
            None => {
                self.setup.state = Some(SetupState::from_settings(&self.settings));
                // What the history index reads now: the daemon answers, the step shows it.
                self.setup_adopt_history();
                self.refresh_history_index(cx);
            }
        }
        window.focus(&self.setup.focus, cx);
        self.setup_entered_step(cx);
        cx.notify();
    }

    /// The History step starts at what the index reads now (`history.status`), unless the user
    /// already ticked something. Called when the setup opens and when the daemon answers.
    pub(crate) fn setup_adopt_history(&mut self) {
        let Some(status) = &self.history_index.status else {
            return;
        };
        let now = HISTORY_SOURCES.map(|(id, _, _)| {
            status.enabled && status.sources.iter().any(|source| source.name == id)
        });
        if let Some(state) = &mut self.setup.state {
            state.history_now = now;
            if !state.history_touched {
                state.history = now;
            }
        }
    }

    /// The first-run setup is on screen: it is modal, so the workbench's shortcuts (⌘T, ⌘B, …)
    /// do nothing until it is finished or skipped (Escape skips).
    pub(crate) fn setup_blocks_shortcuts(&self) -> bool {
        self.setup.state.as_ref().is_some_and(|s| !s.suspended)
    }

    /// The harness login of the Agents step started: the window shows its terminal, so the
    /// setup steps aside until it is opened again.
    pub(crate) fn suspend_setup(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &mut self.setup.state else {
            return;
        };
        state.suspended = true;
        self.set_notice(
            "Setup is paused while you log in. Choose “Hutan: Run Setup Again…” in the command palette to continue.",
            cx,
        );
    }

    fn close_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(old) = self.setup.state.take() {
            self.setup.exit.start(old, motion::Kind::Sheet, cx);
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    /// Skip setup: nothing more is written, the look goes back to the settings, and the setup
    /// does not come back by itself.
    pub fn skip_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.setup.state.is_none() {
            return;
        }
        self.apply_look_from_settings(cx);
        self.mark_setup_done(cx);
        self.close_setup(window, cx);
    }

    /// The last step's buttons: the setup is done; `then` is what the user chose to do next.
    pub fn finish_setup(
        &mut self,
        then: Option<SetupFinish>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.setup.state.is_none() {
            return;
        }
        self.mark_setup_done(cx);
        self.close_setup(window, cx);
        match then {
            Some(SetupFinish::OpenFolder) => self.open_folder_dialog(window, cx),
            Some(SetupFinish::NewTerminal) => self.new_terminal(window, cx),
            None => {}
        }
    }

    pub(super) fn mark_setup_done(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self
            .settings
            .write_user("app.setup_done", toml::Value::Boolean(true))
        {
            tracing::warn!("cannot save that the setup is done: {error}");
            self.set_notice(format!("Cannot save app.setup_done: {error}"), cx);
        }
    }

    // ---- moving between steps -------------------------------------------------------------

    /// Continue: writes this step's choices and shows the next one; on the last step, finishes.
    pub fn setup_continue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(step) = self.setup.state.as_ref().map(|s| s.step) else {
            return;
        };
        self.setup_leave_step(step, cx);
        match step.next() {
            Some(next) => self.setup_show_step(next, cx),
            None => self.finish_setup(None, window, cx),
        }
    }

    /// Back: writes this step's choices (leaving a step writes them) and shows the one before.
    pub fn setup_back(&mut self, cx: &mut Context<Self>) {
        let Some(step) = self.setup.state.as_ref().map(|s| s.step) else {
            return;
        };
        if let Some(previous) = step.previous() {
            self.setup_leave_step(step, cx);
            self.setup_show_step(previous, cx);
        }
    }

    fn setup_show_step(&mut self, step: SetupStep, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.setup.state {
            state.step = step;
        }
        self.setup_entered_step(cx);
        cx.notify();
    }

    fn setup_entered_step(&mut self, cx: &mut Context<Self>) {
        if self.setup.state.as_ref().map(|s| s.step) == Some(SetupStep::Agents) {
            self.refresh_accounts(cx);
            self.refresh_integration_statuses(cx);
        }
    }

    /// Writes what `step` chose to the settings (and to the system where the choice is an
    /// action: the login item, the CLI link, the assistant's backend), and only what differs from
    /// what is there now: a step the user passed without touching writes nothing.
    fn setup_leave_step(&mut self, step: SetupStep, cx: &mut Context<Self>) {
        let Some(state) = self.setup.state.clone() else {
            return;
        };
        match step {
            SetupStep::Welcome | SetupStep::Done => {}
            SetupStep::History => {
                if state.history != state.history_now {
                    self.setup_enable_history(state.history, cx);
                    if let Some(now) = &mut self.setup.state {
                        now.history_now = state.history;
                    }
                }
            }
            SetupStep::Look => {
                let look = self.settings.get::<AppearanceSettings>();
                if state.appearance != look.appearance {
                    self.setup_write(
                        "appearance.appearance",
                        appearance_key(state.appearance),
                        cx,
                    );
                }
                if state.density != look.density {
                    self.setup_write("appearance.density", density_key(state.density), cx);
                }
            }
            SetupStep::Editing => {
                if state.vim_mode != self.settings.get::<EditorSettings>().vim_mode {
                    self.setup_write_bool("editor.vim_mode", state.vim_mode, cx);
                }
                // The send key was offered on this page: the composer need not ask again.
                let written = if state.send_key != self.send_key() {
                    super::send_key::write_send_key(&self.settings, state.send_key)
                } else {
                    super::send_key::stop_asking(&self.settings)
                };
                if let Err(error) = written {
                    tracing::warn!("cannot save chat.send_key: {error}");
                    self.set_notice(format!("Cannot save chat.send_key: {error}"), cx);
                }
            }
            SetupStep::Agents => {
                // Mawas is on by default: nothing is written unless the choice changed.
                let turned_on = state.assistant_enabled && !self.assistant_enabled();
                if state.assistant_enabled != self.assistant_enabled() {
                    self.setup_write_bool("assistant.enabled", state.assistant_enabled, cx);
                }
                if state.auto_install != self.settings.get::<IntegrationsSettings>().auto_install {
                    self.setup_write_bool("integrations.auto_install", state.auto_install, cx);
                }
                let ticked: Vec<&'static str> = state.hooks.iter().copied().collect();
                if !ticked.is_empty() {
                    self.install_integrations(ticked, cx);
                }
                if state.assistant_enabled && (turned_on || state.assistant_backend.is_some()) {
                    self.setup_configure_assistant(cx);
                }
            }
            SetupStep::Background => {
                let app = self.settings.get::<AppSettings>();
                if state.keep_running != app.keep_running {
                    self.set_keep_running(state.keep_running, cx);
                }
                // Only where it can be registered: outside Hutan.app there is nothing to ask.
                if self.start_at_login_available() && state.start_at_login != app.start_at_login {
                    self.set_start_at_login(state.start_at_login, cx);
                }
                if state.global_shortcut != self.settings.get::<AssistantSettings>().global_shortcut
                {
                    self.setup_write("assistant.global_shortcut", &state.global_shortcut, cx);
                }
            }
            SetupStep::CommandLine => {
                if state.cli_link {
                    self.setup_make_cli_link(cx);
                }
            }
        }
    }

    /// *Skip this step*: what the step offered is dropped (every value of it goes back to what
    /// the settings say, the Look preview with it) and the next step shows.
    pub fn setup_skip_step(&mut self, cx: &mut Context<Self>) {
        let Some(step) = self.setup.state.as_ref().map(|s| s.step) else {
            return;
        };
        if !step.skippable() {
            return;
        }
        let today = SetupState::from_settings(&self.settings);
        if let Some(state) = &mut self.setup.state {
            match step {
                SetupStep::Look => {
                    state.appearance = today.appearance;
                    state.density = today.density;
                }
                SetupStep::Editing => {
                    state.vim_mode = today.vim_mode;
                    state.send_key = today.send_key;
                }
                SetupStep::Agents => {
                    state.assistant_enabled = today.assistant_enabled;
                    state.assistant_backend = None;
                    state.auto_install = today.auto_install;
                    state.hooks.clear();
                }
                SetupStep::History => {
                    state.history = state.history_now;
                    state.history_touched = false;
                }
                SetupStep::Background => {
                    state.keep_running = today.keep_running;
                    state.start_at_login = today.start_at_login;
                    state.global_shortcut = today.global_shortcut;
                }
                SetupStep::CommandLine => state.cli_link = false,
                SetupStep::Welcome | SetupStep::Done => {}
            }
        }
        if step == SetupStep::Look {
            self.apply_look_from_settings(cx);
        }
        if let Some(next) = step.next() {
            self.setup_show_step(next, cx);
        }
    }

    fn setup_write(&mut self, key: &str, value: &str, cx: &mut Context<Self>) {
        if let Err(error) = self
            .settings
            .write_user(key, toml::Value::String(value.to_owned()))
        {
            tracing::warn!("cannot save {key}: {error}");
            self.set_notice(format!("Cannot save {key}: {error}"), cx);
        }
    }

    fn setup_write_bool(&mut self, key: &str, value: bool, cx: &mut Context<Self>) {
        if let Err(error) = self.settings.write_user(key, toml::Value::Boolean(value)) {
            tracing::warn!("cannot save {key}: {error}");
            self.set_notice(format!("Cannot save {key}: {error}"), cx);
        }
    }

    /// The History step's sources reach `daemon.toml` (`history.<source>`, all four, so an
    /// unticked one stays unread whatever the file had) and indexing is on while any is ticked.
    fn setup_enable_history(&mut self, sources: [bool; 4], cx: &mut Context<Self>) {
        let connection = self.connection(cx);
        if !connection.supports_history() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result: Result<(), hutan_client::ClientError> = async {
                if connection.supports_daemon_settings() {
                    for ((id, _, _), on) in HISTORY_SOURCES.iter().zip(sources) {
                        connection
                            .daemon_setting_set(
                                &format!("history.{id}"),
                                Some(serde_json::Value::Bool(on)),
                            )
                            .await?;
                    }
                }
                connection
                    .history_set_enabled(sources.iter().any(|on| *on))
                    .await
                    .map(drop)
            }
            .await;
            if let Err(error) = result {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Cannot turn on history: {error}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// The Agents step's backend choice reaches the daemon (`assistant.configure`, global scope).
    fn setup_configure_assistant(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.setup_backend(cx) else {
            return;
        };
        let connection = self.connection(cx);
        if !connection.supports_assistant_backend() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let stored = connection
                .assistant_configure(AssistantConfigure {
                    scope: AssistantScope::Global,
                    workspace_id: None,
                    backend,
                    model: None,
                    provider: None,
                })
                .await;
            if let Err(error) = stored {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Cannot choose Mawas's backend: {error}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn setup_make_cli_link(&mut self, cx: &mut Context<Self>) {
        let Some(link) = self.setup.cli_link.clone() else {
            return;
        };
        let outcome = match link.apply() {
            Ok(Plan::Create) => format!("Linked {} to this Hutan", link.link().display()),
            Ok(Plan::AlreadyLinked) => format!("{} already points here", link.link().display()),
            Ok(Plan::Manual { command, .. }) => {
                self.set_notice(
                    format!(
                        "Hutan did not touch {}. Run: {command}",
                        link.link().display()
                    ),
                    cx,
                );
                format!("Not linked. Run: {command}")
            }
            Err(error) => {
                self.set_notice(
                    format!("Cannot link hutan: {error}. Run: {}", link.command()),
                    cx,
                );
                format!("Not linked: {error}")
            }
        };
        if let Some(state) = &mut self.setup.state {
            state.cli_outcome = Some(outcome);
        }
    }

    // ---- choices ----------------------------------------------------------------------------

    pub fn setup_set_appearance(&mut self, mode: AppearanceMode, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.setup.state {
            state.appearance = mode;
        }
        self.setup_preview_look(cx);
    }

    pub fn setup_set_density(&mut self, density: Density, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.setup.state {
            state.density = density;
        }
        self.setup_preview_look(cx);
    }

    /// The window shows the pending look; the settings are written when the step is left.
    fn setup_preview_look(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.setup.state else {
            return;
        };
        self.chrome = state.appearance;
        self.density = state.density;
        self.refresh_theme(cx);
        self.publish_style(cx);
        cx.notify();
    }

    /// The window as the settings say (a preview is dropped).
    fn apply_look_from_settings(&mut self, cx: &mut Context<Self>) {
        let look = self.settings.get::<AppearanceSettings>();
        if look.appearance == self.chrome && look.density == self.density {
            return;
        }
        self.chrome = look.appearance;
        self.density = look.density;
        self.refresh_theme(cx);
        self.publish_style(cx);
        cx.notify();
    }

    pub fn setup_set_assistant_enabled(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.setup.state {
            state.assistant_enabled = on;
            cx.notify();
        }
    }

    pub fn setup_set_assistant_backend(
        &mut self,
        backend: Option<AssistantBackend>,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = &mut self.setup.state {
            state.assistant_backend = Some(backend);
            cx.notify();
        }
    }

    pub fn setup_set_auto_install(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.setup.state {
            state.auto_install = on;
            cx.notify();
        }
    }

    /// Ticks or unticks "Install hooks for …" of one harness; nothing is installed until the
    /// step is left.
    pub fn setup_set_hook(&mut self, harness: &'static str, on: bool, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.setup.state {
            if on {
                state.hooks.insert(harness);
            } else {
                state.hooks.remove(harness);
            }
            cx.notify();
        }
    }

    /// Ticks or unticks a source of the History step (index into [`HISTORY_SOURCES`]).
    pub fn setup_set_history_source(&mut self, index: usize, on: bool, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.setup.state
            && let Some(slot) = state.history.get_mut(index)
        {
            *slot = on;
            state.history_touched = true;
            cx.notify();
        }
    }

    /// The Editing step's Vim switch; nothing is written until the step is left.
    pub fn setup_set_vim_mode(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.setup.state {
            state.vim_mode = on;
            cx.notify();
        }
    }

    /// The Editing step's send key; nothing is written until the step is left.
    pub fn setup_set_send_key(&mut self, key: SendKey, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.setup.state {
            state.send_key = key;
            cx.notify();
        }
    }

    pub fn setup_set_keep_running(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.setup.state {
            state.keep_running = on;
            cx.notify();
        }
    }

    pub fn setup_set_start_at_login(&mut self, on: bool, cx: &mut Context<Self>) {
        if !self.start_at_login_available() {
            return;
        }
        if let Some(state) = &mut self.setup.state {
            state.start_at_login = on;
            cx.notify();
        }
    }

    pub fn setup_set_global_shortcut(&mut self, shortcut: &str, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.setup.state {
            state.global_shortcut = shortcut.to_owned();
            cx.notify();
        }
    }

    pub fn setup_set_cli_link(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.setup.cli_link.is_none() {
            return;
        }
        if let Some(state) = &mut self.setup.state {
            state.cli_link = on;
            cx.notify();
        }
    }

    /// What the Command line step would do now (`None` outside `Hutan.app`).
    pub fn setup_cli_plan(&self) -> Option<Plan> {
        self.setup.cli_link.as_ref().map(CliLink::plan)
    }

    /// "Copy command" of the Command line step.
    pub fn setup_copy_cli_command(&mut self, cx: &mut Context<Self>) {
        if let Some(link) = &self.setup.cli_link {
            cx.write_to_clipboard(ClipboardItem::new_string(link.command()));
            self.set_notice("Copied the command", cx);
        }
    }

    /// "Allow notifications": the only place the permission is asked for, and only when pressed.
    pub fn setup_request_notifications(&mut self, cx: &mut Context<Self>) {
        let permission = self.mac.notifications();
        let Some(state) = &mut self.setup.state else {
            return;
        };
        if state.notifications == NotificationUi::Asking {
            return;
        }
        if !permission.available() {
            state.notifications = NotificationUi::Answered(NotificationAnswer::Failed(
                "Notifications are available in Hutan.app".to_owned(),
            ));
            cx.notify();
            return;
        }
        state.notifications = NotificationUi::Asking;
        let answer = permission.request();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let answer = answer.await.unwrap_or_else(|_| {
                NotificationAnswer::Failed("the system did not answer".to_owned())
            });
            this.update(cx, |this, cx| {
                if let Some(state) = &mut this.setup.state {
                    state.notifications = NotificationUi::Answered(answer);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// "Log in…" of a harness: its own login flow in a terminal session of the daemon. The
    /// setup is paused until the user comes back to it.
    pub fn setup_log_in(
        &mut self,
        harness: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.account_flow(harness, None, true, window, cx);
    }

    /// "Install guide": the harness's install page in the browser.
    pub fn setup_open_install_guide(&mut self, harness: &str, cx: &mut Context<Self>) {
        cx.open_url(install_guide(harness));
    }

    // ---- rendering --------------------------------------------------------------------------

    /// The Settings → app row that opens the setup again.
    pub(crate) fn render_run_setup_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let button = Button::new("run-setup", "Run setup again…", &theme, &tokens)
            .min_width(ui::CONTROL_WIDTH)
            .on_click(cx.listener(|this, _, window, cx| this.open_setup(window, cx)))
            .child(probe(&self.regions, Region::Setup("run-setup")));
        super::settings_rows::sheet_row(
            "field-run-setup",
            "Run setup again…",
            "Look, editing, agents, history, the daemon in the background, the command line. \
             Every answer starts at today's value; nothing changes unless you change it.",
            Vec::new(),
            button.into_any_element(),
            &theme,
            &tokens,
        )
    }

    pub(crate) fn render_setup(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.setup.state.as_ref().is_some_and(|s| !s.suspended) {
            self.setup.exit.cancel();
            return self.build_setup(true, window, cx);
        }
        let now = cx.background_executor().now();
        motion::while_leaving(
            self,
            now,
            |this| &mut this.setup.state,
            |this| &mut this.setup.exit,
            |this| this.build_setup(false, window, cx),
        )
        .flatten()
    }

    /// `wanted` is false while the sheet leaves: it takes no input and no focus.
    fn build_setup(
        &mut self,
        wanted: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let state = self.setup.state.clone().filter(|s| !s.suspended)?;
        // The sheet is modal: the keyboard stays in it (a terminal that starts or a daemon that
        // connects behind it must not take the focus and the keys).
        if wanted && !self.setup.focus.contains_focused(window, cx) {
            window.focus(&self.setup.focus, cx);
        }
        let theme = self.theme;
        let tokens = self.density.ui();
        let step = state.step;
        let body = match step {
            SetupStep::Welcome => self.setup_welcome(&theme, &tokens),
            SetupStep::Look => self.setup_look(&state, window, cx),
            SetupStep::Editing => self.setup_editing(&state, cx),
            SetupStep::Agents => self.setup_agents(&state, cx),
            SetupStep::History => self.setup_history(&state, cx),
            SetupStep::Background => self.setup_background(&state, cx),
            SetupStep::CommandLine => self.setup_command_line(&state, cx),
            SetupStep::Done => self.setup_done(&state, cx),
        };
        let button = |id: &'static str, label: &'static str, kind: ButtonKind| {
            Button::new(id, label, &theme, &tokens)
                .kind(kind)
                .child(probe(&self.regions, Region::Setup(id)))
        };
        let footer = div()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 3.0))
            .px(px(tokens.spacing * 6.0))
            .py(px(tokens.spacing * 3.0))
            .border_t_1()
            .border_color(theme.border)
            .child(
                div().flex_1().flex().child(
                    button("skip", "Skip setup", ButtonKind::Ghost)
                        .on_click(cx.listener(|this, _, window, cx| this.skip_setup(window, cx))),
                ),
            )
            .child(ui::step_dots(step.index(), SetupStep::ALL.len(), &theme))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .justify_end()
                    .gap(px(tokens.spacing * 2.0))
                    .when(step.skippable(), |s| {
                        s.child(
                            button("skip-step", "Skip this step", ButtonKind::Ghost)
                                .on_click(cx.listener(|this, _, _, cx| this.setup_skip_step(cx))),
                        )
                    })
                    .when(step.previous().is_some(), |s| {
                        s.child(
                            button("back", "Back", ButtonKind::Secondary)
                                .on_click(cx.listener(|this, _, _, cx| this.setup_back(cx))),
                        )
                    })
                    .child(
                        button(
                            "continue",
                            match step {
                                SetupStep::Welcome => "Get started",
                                SetupStep::Done => "Finish",
                                _ => "Continue",
                            },
                            ButtonKind::Primary,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.setup_continue(window, cx);
                        })),
                    ),
            );

        Some(
            div()
                .id("setup-backdrop")
                .absolute()
                .inset_0()
                .shield_behind()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.scrim())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .id("setup")
                        .key_context("Setup")
                        .when(wanted, |s| s.track_focus(&self.setup.focus))
                        .relative()
                        .w(px(WIDTH))
                        .max_h(relative(0.94))
                        .flex()
                        .flex_col()
                        .popover(&theme, &tokens)
                        .p(px(0.0))
                        .overflow_hidden()
                        .text_size(px(tokens.font_size))
                        .text_color(theme.fg)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_action(
                            cx.listener(|this, _: &setup_actions::Continue, window, cx| {
                                this.setup_continue(window, cx);
                            }),
                        )
                        .on_action(cx.listener(|this, _: &setup_actions::Skip, window, cx| {
                            this.skip_setup(window, cx);
                        }))
                        .child(hutan_ui::scroll_y(
                            "setup-body-scroll",
                            div()
                                .id("setup-body")
                                .flex_1()
                                .min_h_0()
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap(px(tokens.spacing * 4.0))
                                .px(px(tokens.spacing * 10.0))
                                .pt(px(tokens.spacing * 8.0))
                                .pb(px(tokens.spacing * 6.0))
                                .child(ui::mascot(step.mascot(), 104.0))
                                .child(body),
                        ))
                        .child(footer)
                        .child(probe(&self.regions, Region::Setup("sheet")))
                        .presented(
                            "setup-motion",
                            motion::Kind::Sheet,
                            wanted,
                            motion::Slide::Rise,
                        ),
                )
                .presented(
                    "setup-scrim",
                    motion::Kind::Sheet,
                    wanted,
                    motion::Slide::None,
                )
                .inert(!wanted)
                .into_any_element(),
        )
    }

    /// Title, one paragraph, and the step's own content, centered in a column.
    fn setup_page(
        &self,
        title: &'static str,
        text: &'static str,
        content: Option<AnyElement>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(tokens.spacing * 4.0))
            .child(ui::headline(title, &theme).text_center())
            .child(
                div()
                    .max_w(px(440.0))
                    .text_center()
                    .text_color(theme.muted)
                    .prose(&tokens)
                    .child(text),
            )
            .when_some(content, |s, content| s.child(content))
            .into_any_element()
    }

    fn setup_welcome(&self, _theme: &Theme, _tokens: &crate::theme::Tokens) -> AnyElement {
        self.setup_page(
            "Code from the canopy",
            "Hutan is a workbench for your terminals and your coding agents: sessions that \
             keep running, approvals in one place, all on your Mac. A minute of setup, and \
             you can skip any of it.",
            None,
        )
    }

    /// A setting: its name and explanation on the left, the control on the right.
    fn setup_row(
        &self,
        title: &'static str,
        text: impl IntoElement,
        control: AnyElement,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 4.0))
            .py(px(tokens.spacing * 3.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(div().medium().child(title))
                    .child(
                        div()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child(text),
                    ),
            )
            .child(div().flex_none().child(control))
            .into_any_element()
    }

    fn setup_look(
        &self,
        state: &SetupState,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let cards = [
            (AppearanceMode::System, "System", "appearance-system"),
            (AppearanceMode::Light, "Light", "appearance-light"),
            (AppearanceMode::Dark, "Dark", "appearance-dark"),
        ]
        .map(|(mode, label, id)| {
            let preview = Theme::for_chrome(mode, window.appearance());
            let selected = state.appearance == mode;
            div()
                .id(id)
                .relative()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                // The chosen mode is a fill behind its thumbnail and label, not an outline.
                .p(px(tokens.spacing))
                .rounded(px(tokens.card_radius + tokens.spacing))
                .when(selected, |s| s.bg(theme.accent_soft))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.setup_set_appearance(mode, cx)))
                .child(
                    div()
                        .flex()
                        .w(px(120.0))
                        .h(px(76.0))
                        .rounded(px(tokens.card_radius))
                        .border_1()
                        .border_color(theme.border)
                        .bg(preview.bg)
                        .overflow_hidden()
                        .child(div().w(px(30.0)).h_full().bg(preview.sidebar))
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap(px(5.0))
                                .p(px(9.0))
                                .child(
                                    div()
                                        .h(px(8.0))
                                        .w(px(44.0))
                                        .rounded(px(4.0))
                                        .bg(preview.accent),
                                )
                                .child(div().h(px(6.0)).rounded(px(3.0)).bg(preview.border))
                                .child(
                                    div()
                                        .h(px(6.0))
                                        .w(px(52.0))
                                        .rounded(px(3.0))
                                        .bg(preview.border),
                                ),
                        ),
                )
                .child(
                    div()
                        .text_color(if selected { theme.fg } else { theme.muted })
                        .when(selected, |s| s.medium())
                        .child(label),
                )
                .child(probe(&self.regions, Region::Setup(id)))
        });
        let density = [
            (Density::Compact, "Compact", "density-compact"),
            (Density::Comfortable, "Comfortable", "density-comfortable"),
        ]
        .map(|(value, label, id)| {
            ui::choice(id, label, state.density == value, &theme, &tokens)
                .relative()
                .on_click(cx.listener(move |this, _, _, cx| this.setup_set_density(value, cx)))
                .child(probe(&self.regions, Region::Setup(id)))
        });
        let content = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 4.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing * 2.0))
                    .child(ui::section_label("Appearance", &theme, &tokens))
                    .child(
                        div()
                            .flex()
                            .justify_center()
                            .gap(px(tokens.spacing * 4.0))
                            .children(cards),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing * 2.0))
                    .child(ui::section_label("Density", &theme, &tokens))
                    .child(
                        div()
                            .flex()
                            .justify_center()
                            .gap(px(tokens.spacing * 2.0))
                            .children(density),
                    ),
            )
            .into_any_element();
        self.setup_page(
            "Make it yours",
            "Follow macOS or pick a side; the window changes as you choose.",
            Some(content),
        )
    }

    fn setup_agents(&self, state: &SetupState, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let backend = self.setup_backend(cx);
        // What the directory says, copied out so the rows can use `cx` for their listeners.
        let (infos, logged, problem) = {
            let directory = self.env.harnesses.read(cx);
            let infos: Vec<_> = HARNESSES
                .iter()
                .map(|h| {
                    (
                        *h,
                        directory.detection(h.id),
                        login_summary(directory, h.id),
                        self.integration_status(h.id),
                    )
                })
                .collect();
            let logged: Vec<bool> = HARNESSES
                .iter()
                .map(|h| !directory.logged_in(h.id).is_empty())
                .collect();
            (infos, logged, directory.problem().map(str::to_owned))
        };
        let rows: Vec<AnyElement> = infos
            .iter()
            .map(|(harness, detection, summary, hooks)| {
                self.setup_harness_row(*harness, detection, summary, *hooks, cx)
            })
            .collect();
        let auto = ui::switch("auto-install", state.auto_install, true, &theme)
            .relative()
            .on_click(cx.listener(|this, _, _, cx| {
                let on = this.setup.state.as_ref().is_some_and(|s| !s.auto_install);
                this.setup_set_auto_install(on, cx);
            }))
            .child(probe(&self.regions, Region::Setup("auto-install")));
        let assistant = ui::switch("assistant", state.assistant_enabled, true, &theme)
            .relative()
            .on_click(cx.listener(|this, _, _, cx| {
                let on = this
                    .setup
                    .state
                    .as_ref()
                    .is_some_and(|s| !s.assistant_enabled);
                this.setup_set_assistant_enabled(on, cx);
            }))
            .child(probe(&self.regions, Region::Setup("assistant")));
        let mut choices: Vec<(&'static str, &'static str, Option<AssistantBackend>)> =
            vec![("backend-none", "None", None)];
        for (index, (id, kind)) in [
            ("backend-omp", AssistantBackend::Omp),
            ("backend-claude", AssistantBackend::Claude),
            ("backend-codex", AssistantBackend::Codex),
        ]
        .into_iter()
        .enumerate()
        {
            if logged[index] {
                choices.push((id, HARNESSES[index].name, Some(kind)));
            }
        }
        let backends = div()
            .flex()
            .flex_wrap()
            .justify_end()
            .gap(px(tokens.spacing * 2.0))
            .children(choices.into_iter().map(|(id, label, kind)| {
                ui::choice(id, label, backend == kind, &theme, &tokens)
                    .relative()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.setup_set_assistant_backend(kind, cx);
                    }))
                    .child(probe(&self.regions, Region::Setup(id)))
            }));
        let content = div()
            .w_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing * 2.0))
                    .children(rows),
            )
            .when_some(problem, |s, problem| {
                s.child(
                    div()
                        .pt(px(tokens.spacing * 2.0))
                        .text_color(theme.danger)
                        .child(SharedString::from(format!("Could not start: {problem}"))),
                )
            })
            .child(self.setup_hooks(infos.as_slice(), state, cx))
            .child(self.setup_row(
                "Keep hooks installed on every start",
                "Off unless you turn it on: then each time Hutan starts it adds its hooks \
                 to every agent above that is installed and lacks them. Without it, only \
                 the agents ticked above get hooks, once.",
                auto.into_any_element(),
            ))
            .child(self.setup_row(
                "Mawas",
                "On by default. Your assistant: answers questions about your work through one of your logged-in agents, and acts on your behalf only after you approve each change.",
                assistant.into_any_element(),
            ))
            .when(state.assistant_enabled, |s| {
                s.child(
                    div()
                        .py(px(tokens.spacing * 3.0))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(tokens.spacing * 3.0))
                        .child(div().text_color(theme.muted).child("Backend"))
                        .child(backends),
                )
            })
            .into_any_element();
        self.setup_page(
            "Bring your agents",
            "Hutan works with the logins you already made in OMP, Claude Code and Codex. It \
             never sees a token.",
            Some(content),
        )
    }

    /// The Editing step: Vim mode and the send key, each a pending choice written when the step
    /// is left.
    fn setup_editing(&self, state: &SetupState, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let vim = ui::switch("vim-mode", state.vim_mode, true, &theme)
            .relative()
            .on_click(cx.listener(|this, _, _, cx| {
                let on = this.setup.state.as_ref().is_some_and(|s| !s.vim_mode);
                this.setup_set_vim_mode(on, cx);
            }))
            .child(probe(&self.regions, Region::Setup("vim-mode")));
        let content = div()
            .w_full()
            .flex()
            .flex_col()
            .child(self.setup_row(
                "Vim mode",
                "Modal editing (Normal, Insert, Visual) in file tabs and text fields. \
                 Escape goes to Normal; off keeps every field a plain text box.",
                vim.into_any_element(),
            ))
            .child(self.setup_send_key_row(state, cx))
            .into_any_element();
        self.setup_page(
            "How you type",
            "Both can be changed later in Settings.",
            Some(content),
        )
    }

    /// The Editing step's one-line send-key choice: ⌘Return or Return sends a message. It is
    /// written (as the same setting the composers' quick switch writes) when the step is left.
    fn setup_send_key_row(&self, state: &SetupState, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let current = state.send_key;
        let choices = div()
            .flex()
            .gap(px(tokens.spacing * 2.0))
            .children(SendKey::ALL.map(|key| {
                let (id, region) = match key {
                    SendKey::CmdEnter => ("send-key-cmd-enter", "send-key-cmd-enter"),
                    SendKey::Enter => ("send-key-enter", "send-key-enter"),
                };
                ui::choice(id, key.short_label(), current == key, &theme, &tokens)
                    .relative()
                    .on_click(cx.listener(move |this, _, _, cx| this.setup_set_send_key(key, cx)))
                    .child(probe(&self.regions, Region::Setup(region)))
            }));
        self.setup_row(
            "Send messages with",
            "In chats, Mawas, the Start sheet, review comments and answers to an agent's \
             questions. ⌘Return always sends too; ⌘K “Chat: Toggle Send Key” switches it later.",
            choices.into_any_element(),
        )
    }

    fn setup_harness_row(
        &self,
        harness: Harness,
        detection: &Detection,
        summary: &str,
        hooks: Option<IntegrationStatus>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let id = harness.id;
        let found = !matches!(detection, Detection::NotFound(_));
        let (state_text, state_color) = match detection {
            Detection::Installed => ("installed", theme.success),
            Detection::NotFound(_) => ("not found", theme.warning),
            Detection::Checking => ("checking…", theme.muted),
            Detection::Unknown => ("unknown", theme.muted),
        };
        let subtitle = match detection {
            Detection::Installed => match hooks {
                Some(status) => format!("{summary} · {}", status.label()),
                None => summary.to_owned(),
            },
            Detection::NotFound(_) => "Not found on this Mac".to_owned(),
            Detection::Checking => "Looking…".to_owned(),
            Detection::Unknown => "Hutan could not ask the daemon".to_owned(),
        };
        let action = |kind_id: &'static str, label: &'static str, kind: ButtonKind| {
            Button::new(
                SharedString::from(format!("setup-{kind_id}-{id}")),
                label,
                &theme,
                &tokens,
            )
            .kind(kind)
            .child(probe(&self.regions, Region::SetupHarness(id, kind_id)))
        };
        let mut buttons = div().flex().items_center().gap(px(tokens.spacing * 2.0));
        if found {
            buttons = buttons.child(action("login", "Log in…", ButtonKind::Secondary).on_click(
                cx.listener(move |this, _, window, cx| this.setup_log_in(id, window, cx)),
            ));
        } else {
            if self.harness_install_available() {
                buttons = buttons.child(
                    action("install", "Install…", ButtonKind::Secondary).on_click(
                        |_, window, cx| {
                            window.dispatch_action(Box::new(InstallHarness(id.to_owned())), cx);
                        },
                    ),
                );
            }
            buttons = buttons.child(
                action("install-guide", "Install guide", ButtonKind::Secondary).on_click(
                    cx.listener(move |this, _, _, cx| this.setup_open_install_guide(id, cx)),
                ),
            );
        }
        if found
            && matches!(
                hooks,
                Some(IntegrationStatus::Installed | IntegrationStatus::Partial)
            )
        {
            buttons = buttons.child(
                action("uninstall", "Uninstall hooks", ButtonKind::Ghost).on_click(
                    cx.listener(move |this, _, _, cx| this.uninstall_integration(id, cx)),
                ),
            );
        }
        div()
            .id(SharedString::from(format!("setup-harness-{id}")))
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 3.0))
            .p(px(tokens.spacing * 3.0))
            .card(&theme, &tokens)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(div().medium().child(harness.name))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child(SharedString::from(subtitle)),
                    ),
            )
            .child(ui::state_label(
                ui::state_glyph(state_color, &theme),
                state_text,
                state_color,
                false,
                &theme,
                &tokens,
            ))
            .child(buttons)
            .into_any_element()
    }

    /// "Install hooks for OMP / Claude Code / Codex": one unticked checkbox per installed agent,
    /// each saying what it writes and where. Nothing is written until the step is left.
    fn setup_hooks(
        &self,
        infos: &[(Harness, Detection, String, Option<IntegrationStatus>)],
        state: &SetupState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        if !self.integrations_available() {
            return div().into_any_element();
        }
        let mut rows: Vec<AnyElement> = Vec::new();
        for (harness, detection, _, hooks) in infos {
            if *detection != Detection::Installed {
                continue;
            }
            let id = harness.id;
            let ticked = state.hooks.contains(id);
            let present = matches!(hooks, Some(IntegrationStatus::Installed));
            let (writes, place) = super::integrations::hook_files(id);
            let text = if present {
                "Already installed.".to_owned()
            } else {
                format!(
                    "Writes {writes}: {place}. Only Hutan's own entries; changed files are backed up first."
                )
            };
            rows.push(
                div()
                    .id(SharedString::from(format!("setup-hook-{id}")))
                    .relative()
                    .flex()
                    .items_start()
                    .gap(px(tokens.spacing * 3.0))
                    .py(px(tokens.spacing * 2.0))
                    .cursor_pointer()
                    .when(!present, |s| {
                        s.on_click(cx.listener(move |this, _, _, cx| {
                            let on = this
                                .setup
                                .state
                                .as_ref()
                                .is_some_and(|s| !s.hooks.contains(id));
                            this.setup_set_hook(id, on, cx);
                        }))
                    })
                    .child(
                        div()
                            .flex_none()
                            .child(crate::icons::glyph(if present || ticked {
                                crate::icons::Icon::StateCompleted
                            } else {
                                crate::icons::Icon::StateTodo
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.0))
                            .child(div().medium().child(SharedString::from(format!(
                                "Install hooks for {}",
                                harness.name
                            ))))
                            .child(
                                div()
                                    .text_size(px(tokens.secondary_font_size))
                                    .text_color(theme.muted)
                                    .child(SharedString::from(text)),
                            ),
                    )
                    .child(probe(&self.regions, Region::SetupHarness(id, "hooks")))
                    .into_any_element(),
            );
        }
        if rows.is_empty() {
            return div().into_any_element();
        }
        div()
            .w_full()
            .flex()
            .flex_col()
            .pt(px(tokens.spacing * 2.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(if self.integrations_custom_home() {
                        "Hooks let a terminal session show what its agent is doing. Hutan runs \
                         with its own HUTAN_HOME, but these hooks go into the agents' real \
                         configuration folders. Nothing is written unless you tick it."
                    } else {
                        "Hooks let a terminal session show what its agent is doing. They go \
                         into the agents' own configuration folders. Nothing is written \
                         unless you tick it."
                    }),
            )
            .children(rows)
            .into_any_element()
    }

    /// Optional: the sources History may read, unticked, each with the folder it reads. Leaving
    /// the step with none ticked changes nothing.
    fn setup_history(&self, state: &SetupState, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let mut rows: Vec<AnyElement> = Vec::new();
        for (index, (id, name, place)) in HISTORY_SOURCES.iter().enumerate() {
            let ticked = state.history[index];
            rows.push(
                div()
                    .id(SharedString::from(format!("setup-history-{id}")))
                    .relative()
                    .flex()
                    .items_start()
                    .gap(px(tokens.spacing * 3.0))
                    .py(px(tokens.spacing * 2.0))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let on = this.setup.state.as_ref().is_some_and(|s| !s.history[index]);
                        this.setup_set_history_source(index, on, cx);
                    }))
                    .child(div().flex_none().child(crate::icons::glyph(if ticked {
                        crate::icons::Icon::StateCompleted
                    } else {
                        crate::icons::Icon::StateTodo
                    })))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.0))
                            .child(div().medium().child(*name))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(tokens.secondary_font_size))
                                    .text_color(theme.muted)
                                    .child(*place),
                            ),
                    )
                    .child(probe(&self.regions, Region::SetupHarness(id, "history")))
                    .into_any_element(),
            );
        }
        let content = div()
            .w_full()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(theme.border)
            .children(rows)
            .child(
                div()
                    .pt(px(tokens.spacing * 2.0))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(
                        "Hutan reads a source only when you tick it, into one index file on \
                         this Mac, with secrets masked as far as possible. Nothing leaves \
                         the machine. Leave everything unticked to keep history off; the \
                         History panel can set it up later.",
                    ),
            )
            .into_any_element();
        self.setup_page(
            "Search your agent sessions",
            "Optional. History lists and searches the sessions of your agents in one place. \
             These are the folders it would read.",
            Some(content),
        )
    }

    fn setup_background(&self, state: &SetupState, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let keep = ui::switch("keep-running", state.keep_running, true, &theme)
            .relative()
            .on_click(cx.listener(|this, _, _, cx| {
                let on = this.setup.state.as_ref().is_some_and(|s| !s.keep_running);
                this.setup_set_keep_running(on, cx);
            }))
            .child(probe(&self.regions, Region::Setup("keep-running")));
        let available = self.start_at_login_available();
        let usable = self.start_at_login_usable();
        let login_note =
            crate::macos::status_item::start_at_login_note(available, self.start_at_login_status());
        let login = ui::switch("start-at-login", state.start_at_login, usable, &theme)
            .relative()
            .when(usable, |s| {
                s.on_click(cx.listener(|this, _, _, cx| {
                    let on = this.setup.state.as_ref().is_some_and(|s| !s.start_at_login);
                    this.setup_set_start_at_login(on, cx);
                }))
            })
            .child(probe(&self.regions, Region::Setup("start-at-login")));
        let notifications_available = self.mac.notifications().available();
        let (note_text, note_color) = match &state.notifications {
            NotificationUi::NotAsked => (
                if notifications_available {
                    "Macs ask once, when you press the button."
                } else {
                    "Available when Hutan runs from Hutan.app."
                },
                theme.muted,
            ),
            NotificationUi::Asking => ("Waiting for your answer…", theme.muted),
            NotificationUi::Answered(NotificationAnswer::Allowed) => ("Allowed", theme.success),
            NotificationUi::Answered(NotificationAnswer::Denied) => (
                "Not allowed. Change it in System Settings → Notifications.",
                theme.warning,
            ),
            NotificationUi::Answered(NotificationAnswer::Failed(_)) => {
                ("Could not ask the system.", theme.danger)
            }
        };
        let asking = state.notifications == NotificationUi::Asking;
        let notifications = div()
            .flex()
            .items_center()
            .child(
                Button::new(
                    "allow-notifications",
                    "Allow notifications",
                    &theme,
                    &tokens,
                )
                .disabled(!(notifications_available && !asking))
                .when(notifications_available && !asking, |s| {
                    s.on_click(cx.listener(|this, _, _, cx| this.setup_request_notifications(cx)))
                })
                .child(probe(&self.regions, Region::Setup("notifications"))),
            )
            .into_any_element();
        let shortcuts = div()
            .flex()
            .gap(px(tokens.spacing * 2.0))
            .children(SHORTCUTS.map(|(value, label)| {
                let id = match value {
                    "" => "shortcut-off",
                    "alt-space" => "shortcut-alt-space",
                    _ => "shortcut-cmd-shift-space",
                };
                ui::choice(id, label, state.global_shortcut == value, &theme, &tokens)
                    .relative()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.setup_set_global_shortcut(value, cx);
                    }))
                    .child(probe(&self.regions, Region::Setup(id)))
            }))
            .into_any_element();
        let content = div()
            .w_full()
            .flex()
            .flex_col()
            .child(self.setup_row(
                "Keep the daemon running after Hutan quits",
                "Quit leaves hutand and your sessions running, without asking. Stop it from \
                 the Daemon menu or `hutan daemon stop`.",
                keep.into_any_element(),
            ))
            .child(self.setup_row(
                "Start Hutan's daemon when you log in",
                login_note,
                login.into_any_element(),
            ))
            .child(self.setup_row(
                "Notifications",
                div().text_color(note_color).child(note_text),
                notifications,
            ))
            .child(self.setup_row(
                "Global shortcut for Mawas",
                "Opens Mawas from any app. Off unless you choose one.",
                shortcuts,
            ))
            .into_any_element();
        self.setup_page(
            "Running in the background",
            "Like Orca and Paseo, Hutan's daemon runs while the app runs. Nothing starts at \
             login unless you ask.",
            Some(content),
        )
    }

    fn setup_command_line(&self, state: &SetupState, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let plan = self.setup_cli_plan();
        let available = plan.is_some();
        let (note, command) = match &plan {
            None => ("Available when Hutan runs from Hutan.app.".to_owned(), None),
            Some(Plan::Create) => (
                "Makes ~/.local/bin/hutan point into this Hutan.".to_owned(),
                None,
            ),
            Some(Plan::AlreadyLinked) => ("Already linked to this Hutan.".to_owned(), None),
            Some(Plan::Manual { why, command }) => (
                match why {
                    Manual::NoFolder => "~/.local/bin does not exist. Run this in a terminal:",
                    Manual::Taken => "Something else is called hutan there. To replace it, run:",
                    Manual::NoTarget => "This app has no hutan to link to. Run:",
                }
                .to_owned(),
                Some(command.clone()),
            ),
        };
        let link = ui::switch("cli-link", state.cli_link, available, &theme)
            .relative()
            .when(available, |s| {
                s.on_click(cx.listener(|this, _, _, cx| {
                    let on = this.setup.state.as_ref().is_some_and(|s| !s.cli_link);
                    this.setup_set_cli_link(on, cx);
                }))
            })
            .child(probe(&self.regions, Region::Setup("cli-link")));
        let content = div()
            .w_full()
            .flex()
            .flex_col()
            .child(self.setup_row("Add hutan to your PATH", note, link.into_any_element()))
            .when_some(command, |s, command| {
                s.child(
                    div()
                        .mt(px(tokens.spacing * 3.0))
                        .flex()
                        .items_center()
                        .gap(px(tokens.spacing * 3.0))
                        .p(px(tokens.spacing * 3.0))
                        .card(&theme, &tokens)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_family("Menlo")
                                .text_size(px(tokens.secondary_font_size))
                                .child(SharedString::from(command)),
                        )
                        .child(
                            Button::new("copy-command", "Copy", &theme, &tokens)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.setup_copy_cli_command(cx)),
                                )
                                .child(probe(&self.regions, Region::Setup("copy-command"))),
                        ),
                )
            })
            .child(
                div()
                    .pt(px(tokens.spacing * 4.0))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(
                        "Terminals Hutan starts already know your prompts and folders (zsh, \
                         bash and fish): nothing is added to your shell's startup files.",
                    ),
            )
            .into_any_element();
        self.setup_page(
            "Use Hutan from your terminal",
            "The hutan command lists, starts and attaches to sessions from any terminal.",
            Some(content),
        )
    }

    fn setup_done(&self, state: &SetupState, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let daemon = if state.start_at_login {
            "The daemon starts when you log in."
        } else if state.keep_running {
            "The daemon keeps running after Hutan quits."
        } else {
            "The daemon runs while Hutan runs."
        };
        let mut lines = vec![daemon.to_owned()];
        if let NotificationUi::Answered(NotificationAnswer::Allowed) = state.notifications {
            lines.push("Notifications are on.".to_owned());
        }
        if let Some(outcome) = &state.cli_outcome {
            lines.push(outcome.clone());
        }
        let content = div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(tokens.spacing * 4.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(2.0))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .children(lines.into_iter().map(SharedString::from)),
            )
            .child(
                div()
                    .flex()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        Button::new("open-folder", "Open a folder", &theme, &tokens)
                            .shortcut("⌘O")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.finish_setup(Some(SetupFinish::OpenFolder), window, cx);
                            }))
                            .child(probe(&self.regions, Region::Setup("open-folder"))),
                    )
                    .child(
                        Button::new("new-terminal", "New terminal", &theme, &tokens)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.finish_setup(Some(SetupFinish::NewTerminal), window, cx);
                            }))
                            .child(probe(&self.regions, Region::Setup("new-terminal"))),
                    ),
            )
            .into_any_element();
        self.setup_page(
            "You're all set",
            "Open a project folder to start, or begin with a terminal. Run the setup again any \
             time from the command palette or Settings.",
            Some(content),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_run_welcome_to_done_in_order() {
        let mut step = SetupStep::Welcome;
        let mut seen = vec![step];
        while let Some(next) = step.next() {
            assert_eq!(next.previous(), Some(step));
            seen.push(next);
            step = next;
        }
        assert_eq!(seen, SetupStep::ALL);
        assert_eq!(SetupStep::Welcome.previous(), None);
        assert_eq!(SetupStep::Done.next(), None);
        assert_eq!(
            SetupStep::ALL.map(SetupStep::title),
            [
                "Welcome",
                "Look",
                "Editing",
                "Agents",
                "History",
                "Background",
                "Command line",
                "Done"
            ]
        );
    }
}
