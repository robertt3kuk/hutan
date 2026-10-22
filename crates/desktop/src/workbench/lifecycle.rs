//! Which daemon the app runs, and a stop the user asked for (`docs/architecture.md` § Daemon
//! lifecycle).
//!
//! On every connect to a new daemon (a new epoch) the app compares the build that answers
//! (`Welcome.daemon_build`, protocol 0.42) with the `hutand` it starts itself
//! ([`WorkbenchOptions::own_daemon`](super::WorkbenchOptions)):
//!
//! - the same build: nothing;
//! - another build of this install, no live session: restarted onto the app's `hutand` (a
//!   graceful `daemon.shutdown {restart}`, then a start), with one notice "Updated the
//!   background service to 0.2.0";
//! - another build with live sessions: a line under the title bar asks first (*Restart Now*,
//!   *Later*: asked again at the next launch);
//! - a newer build than the app: the line says to update the app;
//! - a daemon from another executable (another install, a development build): the line says
//!   where it runs from (*Use This App's Service*, *Keep*).
//!
//! Nothing here signals a process: every restart is the daemon's own shutdown.
//!
//! A stop someone asked for (`daemon.stopping`, from the CLI or the daemon's menu bar item)
//! puts the window in the stopped state its own *Stop* uses: "Stopped by you · Start", and no
//! daemon is started by itself until the user starts it — or relaunches the app, which needs
//! one.

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px,
};
use hutan_client::identity::{self, DaemonIdentity, Mismatch};
use hutan_core::Epoch;

use super::actions::daemon;
use super::render::probe;
use super::ui::{Button, ButtonKind};
use super::view::{Region, Workbench};
use crate::icons::{Icon, glyph};
use crate::theme::DensityExt as _;

/// The identity check of the daemon controls.
#[derive(Debug)]
pub(crate) struct IdentityCheck {
    /// The `hutand` this app starts; `None` checks nothing.
    own: Option<DaemonIdentity>,
    /// The daemon (epoch) last compared: each one is looked at once.
    checked: Option<Epoch>,
    /// What the line under the title bar says about it.
    shown: Option<DaemonMismatch>,
    /// An idle daemon of another build was restarted onto the app's `hutand` in this run. A
    /// daemon there that still reports another build is then taken as it is (the `hutand`
    /// beside the app is not of the app's build, as in a development checkout), instead of
    /// being restarted again and again.
    restarted: bool,
}

impl IdentityCheck {
    pub(crate) fn new(own: Option<DaemonIdentity>) -> Self {
        Self {
            own,
            checked: None,
            shown: None,
            restarted: false,
        }
    }
}

/// How the daemon that answers differs from the one this app starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonMismatch {
    pub kind: Mismatch,
    pub running: DaemonIdentity,
    pub own: DaemonIdentity,
}

impl DaemonMismatch {
    /// The line under the title bar.
    pub fn sentence(&self) -> String {
        let from = |identity: &DaemonIdentity| {
            identity.exe_path.as_ref().map_or_else(
                || "an older install".to_owned(),
                |p| p.display().to_string(),
            )
        };
        match &self.kind {
            Mismatch::Different => format!(
                "The background service still runs Hutan {} (from {}). Restart it with {}? \
                 Running terminals are restored; agent turns in progress are interrupted.",
                self.running.version,
                from(&self.running),
                self.own.version
            ),
            Mismatch::Newer => format!(
                "The background service runs Hutan {}, which is newer than this app ({}): \
                 update the app.",
                self.running.version, self.own.version
            ),
            Mismatch::Foreign { path } => {
                format!("Connected to a Hutan service from {}.", path.display())
            }
        }
    }
}

impl Workbench {
    /// How the daemon that answers differs from the `hutand` this app starts, while the line
    /// under the title bar says so.
    pub fn daemon_mismatch(&self) -> Option<&DaemonMismatch> {
        self.daemon.identity.shown.as_ref()
    }

    /// A daemon of a new epoch answers: compare its build with the app's (once per daemon).
    pub(crate) fn check_daemon_identity(&mut self, cx: &mut Context<Self>) {
        if self.daemon.busy.is_some() || self.daemon.quitting || !self.daemon_connected(cx) {
            return;
        }
        let Some(own) = self.daemon.identity.own.clone() else {
            return;
        };
        let state = self.sessions.read(cx).state();
        let (Some(epoch), Some(running)) = (state.epoch, state.daemon.clone()) else {
            return;
        };
        if self.daemon.identity.checked == Some(epoch) {
            return;
        }
        self.daemon.identity.checked = Some(epoch);
        self.daemon.identity.shown = None;
        let Some(kind) = identity::compare(&own, &running) else {
            cx.notify();
            return;
        };
        match kind {
            Mismatch::Different if self.daemon.identity.restarted => {
                tracing::warn!(
                    %running,
                    %own,
                    "this app's hutand runs another build than the app; keeping it"
                );
            }
            Mismatch::Different if self.live_sessions(cx) == 0 => {
                tracing::info!(
                    %running,
                    %own,
                    "an idle daemon of another build answers; restarting it onto this app's \
                     hutand"
                );
                self.daemon.identity.restarted = true;
                self.daemon.start_notice =
                    Some(format!("Updated the background service to {}", own.version));
                self.begin_stop(true, cx);
            }
            kind => {
                tracing::info!(%running, %own, ?kind, "the daemon is not this app's build");
                self.daemon.identity.shown = Some(DaemonMismatch { kind, running, own });
            }
        }
        cx.notify();
    }

    /// `daemon::UseOwnDaemon`: *Restart Now* (another build with live sessions) and *Use This
    /// App's Service* (a daemon from elsewhere): the daemon shuts down (its live sessions with
    /// it, restored by the new one) and the app starts its own `hutand`.
    pub fn use_own_daemon(&mut self, cx: &mut Context<Self>) {
        let Some(mismatch) = self.daemon.identity.shown.take() else {
            return;
        };
        if self.daemon.busy.is_some() {
            self.daemon.identity.shown = Some(mismatch);
            return;
        }
        self.daemon.identity.restarted = true;
        self.sync_menus(cx);
        self.daemon.start_notice = Some(match mismatch.kind {
            Mismatch::Foreign { .. } => "Using this app's background service".to_owned(),
            _ => format!("Updated the background service to {}", mismatch.own.version),
        });
        tracing::info!(running = %mismatch.running, "restarting the daemon onto this app's hutand");
        self.begin_stop(true, cx);
    }

    /// `daemon::DismissMismatch`: *Later* and *Keep*: the line goes until the next launch (or
    /// the next daemon).
    pub fn dismiss_daemon_mismatch(&mut self, cx: &mut Context<Self>) {
        if self.daemon.identity.shown.take().is_some() {
            self.sync_menus(cx);
            cx.notify();
        }
    }

    /// The daemon announced a stop someone asked for (`daemon.stopping`): the window shows it
    /// stopped and starts nothing by itself (the connection already turned autostart off).
    pub(crate) fn follow_announced_stop(&mut self, cx: &mut Context<Self>) {
        let by = self.sessions.read(cx).state().stopped_by;
        if let Some(by) = by
            && !self.daemon.stopped
            && self.daemon.busy.is_none()
            && !self.daemon.quitting
        {
            tracing::info!(
                ?by,
                "the daemon was stopped on request; it stays down until started"
            );
            self.set_daemon_stopped(true, cx);
        }
    }

    /// The line under the title bar while the daemon is not the app's build.
    pub(crate) fn render_daemon_mismatch_banner(&self) -> Option<AnyElement> {
        let mismatch = self.daemon.identity.shown.as_ref()?;
        let theme = self.theme;
        let tokens = self.density.ui();
        let regions = self.regions.clone();
        let button = |id: &'static str,
                      label: &'static str,
                      style: ButtonKind,
                      action: Box<dyn gpui::Action>| {
            Button::new(id, label, &theme, &tokens)
                .kind(style)
                .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
                .child(probe(&regions, Region::Offer(id)))
                .into_any_element()
        };
        let buttons = match mismatch.kind {
            Mismatch::Different => vec![
                button(
                    "daemon-mismatch-restart",
                    "Restart now",
                    ButtonKind::Secondary,
                    Box::new(daemon::UseOwnDaemon),
                ),
                button(
                    "daemon-mismatch-dismiss",
                    "Later",
                    ButtonKind::Ghost,
                    Box::new(daemon::DismissMismatch),
                ),
            ],
            Mismatch::Newer => vec![
                button(
                    "daemon-mismatch-update",
                    "Copy update command",
                    ButtonKind::Secondary,
                    Box::new(daemon::CopyUpdateCommand),
                ),
                button(
                    "daemon-mismatch-dismiss",
                    "Later",
                    ButtonKind::Ghost,
                    Box::new(daemon::DismissMismatch),
                ),
            ],
            Mismatch::Foreign { .. } => vec![
                button(
                    "daemon-mismatch-restart",
                    "Use this app's service",
                    ButtonKind::Secondary,
                    Box::new(daemon::UseOwnDaemon),
                ),
                button(
                    "daemon-mismatch-dismiss",
                    "Keep",
                    ButtonKind::Ghost,
                    Box::new(daemon::DismissMismatch),
                ),
            ],
        };
        let busy = self.daemon.busy.is_some();
        Some(
            div()
                .id("daemon-mismatch")
                .relative()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .px(px(tokens.spacing * 4.0))
                .py(px(tokens.spacing))
                .bg(theme.surface)
                .border_b_1()
                .border_color(theme.warning)
                .text_size(px(tokens.secondary_font_size))
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.warning)
                        .child(glyph(Icon::StateWarning)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(theme.fg)
                        .child(SharedString::from(mismatch.sentence())),
                )
                .children((!busy).then_some(buttons).into_iter().flatten())
                .child(probe(&self.regions, Region::Offer("daemon-mismatch")))
                .into_any_element(),
        )
    }
}
