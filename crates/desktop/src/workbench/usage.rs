//! Usage and limits in the window (USAGE-LIMITS, protocol 0.45; `docs/agents.md` § Usage): the
//! one model of what the daemon says about the harnesses' limit windows and the turns' tokens
//! and cost, and what the window does with it — the `usage` status item, the Usage section of
//! the Agents page (`panels/agents_page.rs`) and the warning toast when a limit passes 80 % and
//! 95 %.
//!
//! The daemon owns the numbers (`usage.summary`, `usage.changed`); this keeps the last summary,
//! asks again when the connection is new, and asks the harnesses to report (`refresh`) when the
//! user looks: the Agents page is shown, or the Refresh button is pressed. Nothing here is a
//! credential: a limit is a label, a percentage and a reset time.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, Entity, InteractiveElement, IntoElement, ParentElement, SharedString,
    Styled, Subscription, div, px,
};
use hutan_client::ConnectionState;
use hutan_protocol::usage::UsageSummary;

use super::panels::common::now_ms;
use super::render::probe;
use super::ui::motion;
use super::ui::{Button, IconButton, StyledExt as _};
use super::view::{Region, Workbench};
use crate::sessions::usage::{self, StatusText, UsageWarning, UsageWarnings};
use crate::sessions::{SessionsStore, StoreChanged, UsageSummaryChanged};
use crate::settings::{NotificationsSettings, StatusSettings};
use crate::theme::DensityExt as _;

/// How long a warning toast stays.
pub const USAGE_TOAST_TIME: Duration = Duration::from_secs(12);
/// Refreshes closer together than this are one: the harnesses answer over the network.
const REFRESH_EVERY: Duration = Duration::from_secs(30);

/// What the daemon says about usage; see the module documentation.
pub struct UsageDirectory {
    sessions: Entity<SessionsStore>,
    summary: Option<UsageSummary>,
    warnings: UsageWarnings,
    /// Warnings earned and not yet shown: the window takes them.
    due: Vec<UsageWarning>,
    /// The connection the summary was asked on (`Connection::generation`).
    loaded_on: Option<u64>,
    /// The first answer of the connection is still on its way.
    loading: bool,
    /// A refresh (the harnesses are asked) is in flight.
    refreshing: bool,
    refreshed_at: Option<Instant>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for UsageDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsageDirectory")
            .field("known", &self.summary.is_some())
            .field("refreshing", &self.refreshing)
            .finish_non_exhaustive()
    }
}

impl UsageDirectory {
    pub fn new(sessions: Entity<SessionsStore>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe(&sessions, |this, _, event: &UsageSummaryChanged, cx| {
                this.apply((*event.summary).clone(), cx);
            }),
            cx.subscribe(&sessions, |this, _, change: &StoreChanged, cx| {
                if change.intersects(StoreChanged::CONNECTION | StoreChanged::SESSIONS) {
                    this.load(cx);
                }
            }),
        ];
        let mut directory = Self {
            sessions,
            summary: None,
            warnings: UsageWarnings::default(),
            due: Vec::new(),
            loaded_on: None,
            loading: false,
            refreshing: false,
            refreshed_at: None,
            _subscriptions: subscriptions,
        };
        directory.load(cx);
        directory
    }

    // ---- reading ---------------------------------------------------------------------------

    /// The last summary the daemon sent; `None` until it has.
    pub fn summary(&self) -> Option<&UsageSummary> {
        self.summary.as_ref()
    }

    /// Whether the harnesses are being asked what is left.
    pub fn refreshing(&self) -> bool {
        self.refreshing
    }

    /// Whether a request to the daemon (the summary of a new connection, a refresh) is still
    /// unanswered. Its answer replaces whatever summary was applied meanwhile, so a test that
    /// applies its own waits for this to be `false` first.
    pub fn pending(&self) -> bool {
        self.loading || self.refreshing
    }

    /// The words of the status item now.
    pub fn status(&self) -> Option<StatusText> {
        usage::status_text(self.summary.as_ref()?, now_ms())
    }

    /// The rings of the `usage.meters` item now: one per harness with a running limit window.
    pub fn meters(&self) -> Vec<usage::MeterView> {
        self.summary
            .as_ref()
            .map(|summary| usage::meters(summary, now_ms()))
            .unwrap_or_default()
    }

    /// The warnings earned since the last call.
    pub fn take_warnings(&mut self) -> Vec<UsageWarning> {
        std::mem::take(&mut self.due)
    }

    // ---- folding in what the daemon says ---------------------------------------------------

    /// Takes `summary` as the state of things; its limits may earn warnings. The daemon's
    /// `usage.changed` lands here, and so do the answers to the requests below (and tests').
    pub fn apply(&mut self, summary: UsageSummary, cx: &mut Context<Self>) {
        let earned = self.warnings.observe(&summary, now_ms());
        self.due.extend(earned);
        self.summary = Some(summary);
        cx.notify();
    }

    /// Asks for the summary once per connection (nothing asks the harnesses).
    fn load(&mut self, cx: &mut Context<Self>) {
        let store = self.sessions.read(cx);
        if store.connection_state() != ConnectionState::Connected {
            return;
        }
        let connection = store.connection().clone();
        let generation = connection.generation();
        if self.loaded_on == Some(generation) || !connection.supports_usage() {
            return;
        }
        self.loaded_on = Some(generation);
        self.loading = true;
        cx.spawn(async move |this, cx| {
            let answer = connection.usage_summary(false).await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match answer {
                    Ok(summary) => this.apply(summary, cx),
                    Err(error) => {
                        tracing::debug!(%error, "no usage summary");
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Asks the harnesses what is left (`usage.summary` with `refresh`): OMP's own report, and
    /// Codex's for a Codex that ran through Hutan. At most once every 30 s unless `force`.
    pub fn refresh(&mut self, force: bool, cx: &mut Context<Self>) {
        let store = self.sessions.read(cx);
        if store.connection_state() != ConnectionState::Connected
            || !store.connection().supports_usage()
            || self.refreshing
        {
            return;
        }
        if !force
            && self
                .refreshed_at
                .is_some_and(|at| at.elapsed() < REFRESH_EVERY)
        {
            return;
        }
        let connection = store.connection().clone();
        self.refreshing = true;
        self.refreshed_at = Some(Instant::now());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let answer = connection.usage_summary(true).await;
            this.update(cx, |this, cx| {
                this.refreshing = false;
                match answer {
                    Ok(summary) => this.apply(summary, cx),
                    Err(error) => {
                        tracing::debug!(%error, "no usage summary");
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }
}

/// The warning toast: one line, a link to the Agents page, a dismiss button.
#[derive(Debug, Clone)]
pub struct UsageToast {
    pub text: SharedString,
    rev: u64,
}

impl Workbench {
    /// What the daemon says about usage (tests, inspection).
    pub fn usage_directory(&self) -> &Entity<UsageDirectory> {
        &self.env.usage
    }

    /// The warning toast, while it shows.
    pub fn usage_toast(&self) -> Option<&UsageToast> {
        self.usage_toast.as_ref()
    }

    /// The usage model changed: the warnings it earned become a toast (unless
    /// `notifications.usage_warnings` is off), and the bar and the page redraw.
    pub(crate) fn on_usage_changed(&mut self, cx: &mut Context<Self>) {
        let due = self.env.usage.update(cx, |usage, _| usage.take_warnings());
        if !due.is_empty() && self.settings.get::<NotificationsSettings>().usage_warnings {
            let now = now_ms();
            let mut text = due[0].text(now);
            if due.len() > 1 {
                text.push_str(&format!(" (+{} more)", due.len() - 1));
            }
            self.show_usage_toast(text, cx);
        }
        cx.notify();
    }

    fn show_usage_toast(&mut self, text: String, cx: &mut Context<Self>) {
        self.toast_rev += 1;
        let rev = self.toast_rev;
        self.usage_toast = Some(UsageToast {
            text: text.into(),
            rev,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(USAGE_TOAST_TIME).await;
            this.update(cx, |this, cx| {
                if this.usage_toast.as_ref().is_some_and(|t| t.rev == rev) {
                    this.usage_toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Takes the warning toast off.
    pub fn dismiss_usage_toast(&mut self, cx: &mut Context<Self>) {
        if self.usage_toast.take().is_some() {
            cx.notify();
        }
    }

    /// The status bar as it is drawn: the current preset's bar with the usage items as
    /// `status.usage.style` says (Settings → Status bar).
    pub fn status_bar_drawn(&self) -> super::registry::StatusBarLayout {
        let mut bar = self.status_bar();
        bar.with_usage_style(self.settings.get::<StatusSettings>().usage.style);
        bar
    }

    pub(crate) fn render_usage_toast(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let toast =
            self.usage_toast_exit
                .follow(self.usage_toast.as_ref(), motion::Kind::Toast, cx)?;
        let (theme, tokens) = (self.theme, self.density.ui());
        let (wanted, toast) = (toast.wanted, toast.value);
        Some(
            motion::present(
                "usage-toast-motion",
                motion::Kind::Toast,
                wanted,
                motion::Slide::Rise,
                div()
                    .id("usage-toast")
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 3.0))
                    .toast(&theme, &tokens)
                    .px(px(tokens.spacing * 3.0))
                    .py(px(tokens.spacing * 2.0))
                    .child(toast.text.clone())
                    .child(
                        Button::new("usage-toast-open", "Usage", &theme, &tokens)
                            .link()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dismiss_usage_toast(cx);
                                this.open_agents_page(
                                    super::panels::AgentsRequest::default(),
                                    window,
                                    cx,
                                );
                            }))
                            .child(probe(&self.regions, Region::UsageToast("open"))),
                    )
                    .child(
                        IconButton::new(
                            "usage-toast-dismiss",
                            crate::icons::Icon::ActionClose,
                            "Dismiss",
                            &theme,
                            &tokens,
                        )
                        .compact()
                        .on_click(cx.listener(|this, _, _, cx| this.dismiss_usage_toast(cx)))
                        .child(probe(&self.regions, Region::UsageToast("dismiss"))),
                    )
                    .child(probe(&self.regions, Region::UsageToast("toast"))),
            )
            .into_any_element(),
        )
    }
}
