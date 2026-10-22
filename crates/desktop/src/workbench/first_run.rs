//! The first run (PR-FIRST-RUN, `docs/design.md` § First run): not a modal wizard but a first
//! screen in the centre and two steps — see which agents are ready, open a folder — after which
//! the centre becomes the start composer of that project (the Start sheet's rows, inline).
//!
//! Everything the old walk-through asked up front is asked where it matters, once, and
//! writes nothing unless the user chooses:
//!
//! - **Notifications** — one line under the title bar the first time something needs the user
//!   (an approval, a finished run) while Hutan is not the frontmost app ([`NotifyOffer`]).
//! - **Hooks** — *Install hooks…* on the strip under a terminal where a known agent runs
//!   without having reported (the *Track as* strip).
//! - **Send key** — one line above the first composer: the chat's, or this start composer's.
//!
//! The setup (the walk-through of `setup.rs`) stays reachable as *Run Setup Again…*, and the
//! first screen links to it.

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use hutan_settings::SettingsStore;

use super::actions::InstallHarness;
use super::harnesses::harness_name;
use super::launcher::{HarnessChoice, LauncherPreset, Readiness};
use super::model::{ItemRef, PaneId};
use super::render::probe;
use super::ui::{self, ButtonKind, Mascot, StyledExt as _};
use super::view::{Region, Workbench};
use crate::icons::{Icon, glyph};
use crate::integrations::IntegrationStatus;
use crate::macos::NotificationAnswer;
use crate::sessions::launch::LaunchKind;
use crate::settings::{AppSettings, ChatSettings, SendKey};
use crate::theme::DensityExt as _;

/// Where the notification offer stands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NotifyOffer {
    /// Nothing needed the user yet (or it was answered before).
    #[default]
    Hidden,
    /// The line is shown: *Notify me* / *No thanks*.
    Offered,
    /// The system's prompt is up.
    Asking,
    /// The system answered; the line says so until it is closed.
    Answered(NotificationAnswer),
}

/// The first run's state inside the workbench.
#[derive(Default)]
pub(crate) struct FirstRunControl {
    /// The first screen is wanted: this launch started with `app.setup_done` unset. It ends
    /// when a folder is opened or a terminal is started, and the screen only shows while the
    /// centre holds nothing but the *Welcome* placeholder and no project exists.
    pub(crate) first_screen: bool,
    pub(crate) notify: NotifyOffer,
    /// The agent whose *Install hooks…* strip is unfolded (what it writes, and where).
    pub(crate) hooks_open: Option<&'static str>,
    /// The first screen asked the daemon which agents are ready (once per launch, and again
    /// when the window comes back from a login terminal).
    pub(crate) accounts_asked: bool,
}

/// What the *Track as* strip adds for an agent: the button, and the unfolded explanation.
pub(crate) struct HooksOffer {
    pub(crate) buttons: Vec<AnyElement>,
    pub(crate) detail: Option<AnyElement>,
}

/// `chat.ask_send_key` and the like: writes `key = false` once the question is answered.
fn stop(settings: &SettingsStore, key: &str) -> Result<(), hutan_settings::SettingsError> {
    settings.write_user(key, toml::Value::Boolean(false))
}

impl Workbench {
    // ---- the first screen ------------------------------------------------------------------

    /// Starts the first run on this launch (`app::run` asks for it with `app.setup_done` unset).
    pub(crate) fn begin_first_run(&mut self) {
        self.first_run.first_screen = true;
    }

    /// The first screen is what the centre shows: it is the first run, no project exists yet,
    /// and nothing but the *Welcome* placeholder is open.
    pub fn first_screen_open(&self, cx: &gpui::App) -> bool {
        self.first_run.first_screen
            && !self
                .sessions
                .read(cx)
                .workspaces()
                .iter()
                .any(|w| w.kind == hutan_protocol::messages::WorkspaceKind::Project)
    }

    /// The first run is over: the first screen goes away and `app.setup_done` is written.
    pub(crate) fn finish_first_run(&mut self, cx: &mut Context<Self>) {
        if !self.first_run.first_screen {
            return;
        }
        self.first_run.first_screen = false;
        if !self.settings.get::<AppSettings>().setup_done {
            self.mark_setup_done(cx);
        }
        cx.notify();
    }

    /// *New terminal* of the first screen.
    fn first_screen_new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_first_run(cx);
        self.new_terminal(window, cx);
    }

    /// Every pane of the centre holds nothing but the *Welcome* placeholder.
    fn centre_is_empty(&self) -> bool {
        self.state.panes().iter().all(|pane| {
            pane.items
                .iter()
                .all(|item| matches!(item, ItemRef::Placeholder { .. }))
        })
    }

    /// A folder was opened (⌘O, the first screen, the sidebar): when the centre of its project
    /// is empty it becomes the start composer of that project.
    pub(crate) fn project_opened(
        &mut self,
        project: hutan_core::WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_first_run(cx);
        if !self.centre_is_empty() {
            return;
        }
        self.open_launcher(
            LauncherPreset {
                project: Some(project),
                kind: Some(LaunchKind::Chat),
                inline: true,
                ..LauncherPreset::default()
            },
            window,
            cx,
        );
    }

    /// An inline composer whose *Welcome* pane went away (a session opened, the project
    /// changed) has nothing to show in: it goes with it.
    pub(crate) fn drop_stale_composer(&mut self) {
        if self.launcher.as_ref().is_some_and(|l| l.inline) && !self.centre_is_empty() {
            self.launcher = None;
        }
    }

    /// What the *Welcome* placeholder of `pane` shows: the start composer while one is open (in
    /// the pane that has the keyboard), else the first screen, else the plain empty state.
    pub(crate) fn render_welcome(
        &mut self,
        pane: PaneId,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if pane == self.state.active_pane_id()
            && let Some(composer) = self.render_launcher_inline(cx)
        {
            return Some(composer);
        }
        self.first_screen_open(cx)
            .then(|| self.render_first_screen(cx))
    }

    /// The agents of the first screen come from the daemon's account listing; it is asked for
    /// once the daemon answers, and again when the window comes back to the front (a login
    /// ran in a terminal meanwhile).
    pub(crate) fn refresh_first_screen_agents(&mut self, again: bool, cx: &mut Context<Self>) {
        if (self.first_run.accounts_asked && !again) || !self.daemon_connected(cx) {
            return;
        }
        self.first_run.accounts_asked = true;
        self.refresh_accounts(cx);
    }

    fn render_first_screen(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.refresh_first_screen_agents(false, cx);
        let theme = self.theme;
        let tokens = self.density.ui();
        let choices = self.launcher_harness_choices(cx);
        let agents = div()
            .flex()
            .flex_wrap()
            .justify_center()
            .gap_x(px(tokens.spacing * 5.0))
            .gap_y(px(tokens.spacing * 2.0))
            .children(
                choices
                    .into_iter()
                    .map(|choice| self.first_screen_agent(choice, cx)),
            );
        div()
            .id("first-screen")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(tokens.spacing * 4.0))
            .p(px(tokens.spacing * 6.0))
            .child(ui::mascot(Mascot::AngleFront, 96.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(tokens.spacing))
                    .child(ui::headline("Code from the canopy", &theme).text_center())
                    .child(
                        div()
                            .max_w(px(440.0))
                            .text_center()
                            .text_color(theme.muted)
                            .prose(&tokens)
                            .child(
                                "Your terminals and coding agents in one window. Open a \
                                 project folder and tell an agent what to do.",
                            ),
                    ),
            )
            .child(agents)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        ui::button(
                            "first-open-folder",
                            "Open a folder  ⌘O",
                            ButtonKind::Primary,
                            true,
                            &theme,
                            &tokens,
                        )
                        .relative()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_folder_dialog(window, cx);
                        }))
                        .child(probe(&self.regions, Region::FirstRun("open-folder"))),
                    )
                    .child(
                        ui::button(
                            "first-new-terminal",
                            "New terminal",
                            ButtonKind::Secondary,
                            true,
                            &theme,
                            &tokens,
                        )
                        .relative()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.first_screen_new_terminal(window, cx);
                        }))
                        .child(probe(&self.regions, Region::FirstRun("new-terminal"))),
                    )
                    .child(
                        ui::button(
                            "first-setup",
                            "Set up Hutan…",
                            ButtonKind::Ghost,
                            true,
                            &theme,
                            &tokens,
                        )
                        .relative()
                        .on_click(cx.listener(|this, _, window, cx| this.open_setup(window, cx)))
                        .child(probe(&self.regions, Region::FirstRun("setup"))),
                    ),
            )
            .child(
                div()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.faint)
                    .child("Local only — no account"),
            )
            .child(probe(&self.regions, Region::FirstRun("screen")))
            .into_any_element()
    }

    /// One detected agent: a glyph and a word for its state, and the one thing that gets it
    /// ready (Log in…, Install…).
    fn first_screen_agent(&self, choice: HarnessChoice, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let id = choice.id;
        let (icon, color, word) = match &choice.readiness {
            Readiness::Ready => (Icon::StateCompleted, theme.success, "ready"),
            Readiness::NeedsLogin => (Icon::StateTodo, theme.warning, "not logged in"),
            Readiness::NeedsInstall(_) => (Icon::StateTodo, theme.warning, "not found"),
            Readiness::Checking => (Icon::StateIdle, theme.muted, "checking…"),
            Readiness::NeedsNewerDaemon => (Icon::StateTodo, theme.warning, "restart hutand"),
        };
        let action: Option<(&'static str, &'static str)> = match &choice.readiness {
            Readiness::NeedsLogin => Some(("login", "Log in…")),
            Readiness::NeedsInstall(_) if self.harness_install_available() => {
                Some(("install", "Install…"))
            }
            Readiness::NeedsInstall(_) => Some(("guide", "Install guide")),
            _ => None,
        };
        div()
            .id(SharedString::from(format!("first-agent-{id}")))
            .relative()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .child(div().flex_none().text_color(color).child(glyph(icon)))
            .child(div().medium().child(choice.name))
            .child(
                div()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(color)
                    .child(word),
            )
            .when_some(action, |s, (key, label)| {
                s.child(
                    ui::button(
                        SharedString::from(format!("first-agent-{key}-{id}")),
                        label,
                        ButtonKind::Ghost,
                        true,
                        &theme,
                        &tokens,
                    )
                    .relative()
                    .on_click(cx.listener(move |this, _, window, cx| match key {
                        "login" => this.account_flow(id, None, true, window, cx),
                        "install" => {
                            window.dispatch_action(Box::new(InstallHarness(id.to_owned())), cx);
                        }
                        _ => this.setup_open_install_guide(id, cx),
                    }))
                    .child(probe(&self.regions, Region::FirstRunAgent(id, key))),
                )
            })
            .into_any_element()
    }

    // ---- the send key, above the start composer ----------------------------------------------

    /// The one-line question about the send key above the first composer, for the start
    /// composer: the same line and the same setting as the chat's (`chat.ask_send_key`).
    pub(crate) fn render_start_send_key_offer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.settings.get::<ChatSettings>().ask_send_key {
            return None;
        }
        let theme = self.theme;
        let tokens = self.density.ui();
        let current = self.send_key();
        let choices = SendKey::ALL.map(|key| {
            let (id, region) = match key {
                SendKey::CmdEnter => ("launch-ask-cmd-enter", "ask-cmd-enter"),
                SendKey::Enter => ("launch-ask-enter", "ask-enter"),
            };
            ui::choice(id, key.short_label(), key == current, &theme, &tokens)
                .relative()
                .on_click(cx.listener(move |this, _, _, cx| this.set_send_key(key, cx)))
                .child(probe(&self.regions, Region::Launcher(region)))
        });
        let hover = theme.surface_hover;
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .child("Which key sends a message?")
                .children(choices)
                .child(
                    div()
                        .id("launch-ask-dismiss")
                        .relative()
                        .px(px(tokens.spacing))
                        .rounded(px(tokens.radius - 2.0))
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Err(error) = super::send_key::stop_asking(&this.settings) {
                                this.set_notice(format!("Could not save the setting: {error}"), cx);
                            }
                            cx.notify();
                        }))
                        .child(glyph(Icon::ActionClose))
                        .child(probe(&self.regions, Region::Launcher("ask-dismiss"))),
                )
                .into_any_element(),
        )
    }

    // ---- notifications, when something first needs the user ---------------------------------

    /// The sessions store changed: the first time something needs the user while Hutan is not
    /// the frontmost app, offer notifications (once; the permission is asked only when the
    /// user says yes). Nothing is offered where the system cannot ask (outside `Hutan.app`).
    pub(crate) fn maybe_offer_notifications(&mut self, cx: &mut Context<Self>) {
        if self.window_active
            || self.first_run.notify != NotifyOffer::Hidden
            || !self.settings.get::<AppSettings>().ask_notifications
            || !self.mac.notifications().available()
        {
            return;
        }
        let needs_you = {
            let store = self.sessions.read(cx);
            store.synced()
                && (!store.needs_you().is_empty()
                    || !store.approvals().is_empty()
                    || !store.questions().is_empty())
        };
        if needs_you {
            self.first_run.notify = NotifyOffer::Offered;
            cx.notify();
        }
    }

    /// *Notify me*: the answer counts as the choice (the offer is not made again); the system's
    /// prompt is the one that asks.
    pub fn notify_offer_accept(&mut self, cx: &mut Context<Self>) {
        if self.first_run.notify != NotifyOffer::Offered {
            return;
        }
        if let Err(error) = stop(&self.settings, "app.ask_notifications") {
            tracing::warn!("cannot save app.ask_notifications: {error}");
        }
        let permission = self.mac.notifications();
        self.first_run.notify = NotifyOffer::Asking;
        let answer = permission.request();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let answer = answer.await.unwrap_or_else(|_| {
                NotificationAnswer::Failed("the system did not answer".to_owned())
            });
            this.update(cx, |this, cx| {
                this.first_run.notify = NotifyOffer::Answered(answer);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// *No thanks*: asked once, never again.
    pub fn notify_offer_decline(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = stop(&self.settings, "app.ask_notifications") {
            tracing::warn!("cannot save app.ask_notifications: {error}");
        }
        self.first_run.notify = NotifyOffer::Hidden;
        cx.notify();
    }

    /// Where the notification offer stands.
    pub fn notify_offer(&self) -> &NotifyOffer {
        &self.first_run.notify
    }

    /// The offer's line, under the title bar while there is one.
    pub(crate) fn render_notify_offer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = self.theme;
        let tokens = self.density.ui();
        let button = |id: &'static str, label: &'static str, kind: ButtonKind| {
            ui::button(id, label, kind, true, &theme, &tokens)
                .relative()
                .child(probe(&self.regions, Region::Offer(id)))
        };
        let (text, color, buttons): (&str, _, Vec<AnyElement>) = match &self.first_run.notify {
            NotifyOffer::Hidden => return None,
            NotifyOffer::Offered => (
                "Notify me when an agent needs me? Hutan posts a notification only while it is \
                 in the background. Asked once.",
                theme.fg,
                vec![
                    button("notify-yes", "Notify me", ButtonKind::Secondary)
                        .on_click(cx.listener(|this, _, _, cx| this.notify_offer_accept(cx)))
                        .into_any_element(),
                    button("notify-no", "No thanks", ButtonKind::Ghost)
                        .on_click(cx.listener(|this, _, _, cx| this.notify_offer_decline(cx)))
                        .into_any_element(),
                ],
            ),
            NotifyOffer::Asking => ("Waiting for your answer…", theme.muted, Vec::new()),
            NotifyOffer::Answered(answer) => {
                let (text, color) = match answer {
                    NotificationAnswer::Allowed => ("Notifications are on.", theme.success),
                    NotificationAnswer::Denied => (
                        "Not allowed. Change it in System Settings → Notifications.",
                        theme.warning,
                    ),
                    NotificationAnswer::Failed(_) => ("Could not ask the system.", theme.danger),
                };
                (
                    text,
                    color,
                    vec![
                        button("notify-done", "Done", ButtonKind::Ghost)
                            .on_click(cx.listener(|this, _, _, cx| this.notify_offer_decline(cx)))
                            .into_any_element(),
                    ],
                )
            }
        };
        let icon = match &self.first_run.notify {
            NotifyOffer::Answered(NotificationAnswer::Allowed) => Icon::StateCompleted,
            _ => Icon::StateInfo,
        };
        Some(
            div()
                .id("notify-offer")
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .px(px(tokens.spacing * 4.0))
                .py(px(tokens.spacing))
                .bg(theme.surface)
                .text_size(px(tokens.secondary_font_size))
                .child(div().flex_none().text_color(color).child(glyph(icon)))
                .child(div().flex_1().min_w_0().text_color(color).child(text))
                .children(buttons)
                .child(probe(&self.regions, Region::Offer("notify")))
                .into_any_element(),
        )
    }

    // ---- hooks, when an agent runs in a terminal without having reported --------------------

    /// What the *Track as* strip of a terminal offers for `agent` beyond tracking it: install
    /// that agent's hooks, once (`integrations.ask_hooks`). Nothing is offered while the hooks
    /// are installed, the offer was answered, or the installer is missing.
    pub(crate) fn hooks_offer(
        &mut self,
        agent: &'static str,
        cx: &mut Context<Self>,
    ) -> Option<HooksOffer> {
        if !self.integrations_available() || !self.settings.get::<AppSettings>().ask_hooks {
            return None;
        }
        self.integration_status_wanted(agent, cx);
        match self.integration_status(agent) {
            None | Some(IntegrationStatus::Installed) => return None,
            Some(_) => {}
        }
        let theme = self.theme;
        let tokens = self.density.ui();
        let name = harness_name(agent);
        let open = self.first_run.hooks_open == Some(agent);
        let button = |id: &'static str, label: SharedString, kind: ButtonKind| {
            ui::button(id, label, kind, !self.integration_busy(), &theme, &tokens)
                .relative()
                .child(probe(&self.regions, Region::Offer(id)))
        };
        if !open {
            return Some(HooksOffer {
                buttons: vec![
                    button(
                        "hooks-offer",
                        SharedString::from("Install hooks…"),
                        ButtonKind::Ghost,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.first_run.hooks_open = Some(agent);
                        cx.notify();
                    }))
                    .into_any_element(),
                ],
                detail: None,
            });
        }
        let (writes, place) = super::integrations::hook_files(agent);
        let home = if self.integrations_custom_home() {
            " Hutan runs with its own HUTAN_HOME, but hooks go into the agent's real configuration folder."
        } else {
            ""
        };
        let detail = div()
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.muted)
            .child(SharedString::from(format!(
                "Hooks let Hutan see what {name} does in a terminal, so it can tell you when it \
                 needs you. Writes {writes}: {place}. Only Hutan's own entries; changed files \
                 are backed up first.{home}"
            )))
            .into_any_element();
        Some(HooksOffer {
            buttons: vec![
                button(
                    "hooks-install",
                    SharedString::from(format!("Install hooks for {name}")),
                    ButtonKind::Secondary,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.first_run.hooks_open = None;
                    this.stop_asking_hooks();
                    this.install_integration(agent, cx);
                    cx.notify();
                }))
                .into_any_element(),
                button(
                    "hooks-not-now",
                    SharedString::from("No thanks"),
                    ButtonKind::Ghost,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.first_run.hooks_open = None;
                    this.stop_asking_hooks();
                    cx.notify();
                }))
                .into_any_element(),
            ],
            detail: Some(detail),
        })
    }

    /// The hooks were accepted or declined: they are not offered again.
    fn stop_asking_hooks(&mut self) {
        if let Err(error) = stop(&self.settings, "app.ask_hooks") {
            tracing::warn!("cannot save integrations.ask_hooks: {error}");
        }
    }
}
