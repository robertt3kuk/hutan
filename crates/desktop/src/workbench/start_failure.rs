//! A daemon that cannot start, as the window shows it (architecture.md § Start errors): one
//! line under the title bar with the reason in one sentence and the action that fits it, the
//! same in the daemon popover, the `Daemon` menu and the palette.
//!
//! - data from a newer Hutan (`schema_too_new`): update (*Copy Update Command*), or *Start with
//!   Empty Data…*, which moves the database into `backups/` after a confirmation (nothing is
//!   deleted) and starts the daemon;
//! - another `hutand` holds the lock and does not answer (`already_running`): *Show the
//!   Process* (its pid and executable), *Stop It…* (SIGTERM to that pid after a confirmation);
//! - anything else: the message, *Open Logs*, *Copy Details*.
//!
//! Where the failure comes from: [`StoreState::daemon_start`](hutan_client::StoreState), kept
//! by the connection. These are the user's actions only: nothing else can reach them.

use std::path::PathBuf;

use gpui::{
    AnyElement, App, ClipboardItem, Context, IntoElement, ParentElement, SharedString, Styled,
    Window, div, prelude::*, px,
};
use hutan_client::{DaemonFiles, StartErrorKind, StartFailure, move_data_aside};

use super::actions::daemon;
use super::panels::StatusTone;
use super::render::probe;
use super::ui::{Button, ButtonKind};
use super::view::{Region, Workbench};
use crate::icons::{Icon, glyph};
use crate::theme::DensityExt as _;

/// What a failed start offers, by its kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartFailureKind {
    /// The data was written by a newer Hutan.
    NewerData,
    /// Another `hutand` holds the lock but does not answer.
    Stuck,
    /// Anything else.
    Other,
}

impl StartFailureKind {
    pub fn of(failure: &StartFailure) -> Self {
        match failure.kind() {
            Some(StartErrorKind::SchemaTooNew { .. }) => Self::NewerData,
            Some(StartErrorKind::AlreadyRunning { .. }) => Self::Stuck,
            _ => Self::Other,
        }
    }

    /// The status item's few words.
    pub fn status_text(self) -> &'static str {
        match self {
            Self::NewerData => "hutand: data from a newer Hutan",
            Self::Stuck => "hutand: another one is stuck",
            Self::Other => "hutand could not start",
        }
    }

    /// The `Daemon` menu's (disabled) first line.
    pub fn menu_text(self) -> &'static str {
        match self {
            Self::NewerData => "Not Running: Data From a Newer Hutan",
            Self::Stuck => "Not Running: Another hutand Is Stuck",
            Self::Other => "Not Running: Start Failed",
        }
    }
}

/// The executable of process `pid`, if it runs.
#[allow(unsafe_code)] // one libc call; the block says why it is sound.
fn process_path(pid: u32) -> Option<PathBuf> {
    let pid = i32::try_from(pid).ok()?;
    let mut buffer = vec![0u8; usize::try_from(libc::PROC_PIDPATHINFO_MAXSIZE).ok()?];
    let size = u32::try_from(buffer.len()).ok()?;
    // SAFETY: the buffer is valid for `size` bytes; the call writes at most that many and
    // returns the length written (≤ 0 on failure).
    let written = unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), size) };
    let written = usize::try_from(written).ok().filter(|n| *n > 0)?;
    buffer.truncate(written);
    Some(PathBuf::from(String::from_utf8_lossy(&buffer).into_owned()))
}

/// SIGTERM to `pid`.
#[allow(unsafe_code)] // one libc call; the block says why it is sound.
fn terminate(pid: u32) -> std::io::Result<()> {
    let pid = i32::try_from(pid).map_err(std::io::Error::other)?;
    // SAFETY: a plain syscall on a pid; no memory is passed.
    if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

impl Workbench {
    /// Why the daemon is not there, while it is not (`None` while connected).
    pub fn daemon_start_failure(&self, cx: &App) -> Option<StartFailure> {
        if self.daemon_connected(cx) {
            return None;
        }
        self.sessions.read(cx).state().daemon_start.clone()
    }

    pub(crate) fn start_failure_kind(&self, cx: &App) -> Option<StartFailureKind> {
        self.daemon_start_failure(cx)
            .as_ref()
            .map(StartFailureKind::of)
    }

    /// The status item's line while the start failed.
    pub(crate) fn start_failure_line(&self, cx: &App) -> Option<(String, StatusTone)> {
        let kind = self.start_failure_kind(cx)?;
        Some((kind.status_text().to_owned(), StatusTone::Error))
    }

    /// What *Show the Process* found, shown under the sentence.
    pub fn stuck_daemon_info(&self) -> Option<&str> {
        self.daemon.stuck_info.as_deref()
    }

    /// `daemon::CopyStartError`: everything known about the failure, on the clipboard.
    pub fn copy_start_error(&mut self, cx: &mut Context<Self>) {
        match self.daemon_start_failure(cx) {
            Some(failure) => {
                cx.write_to_clipboard(ClipboardItem::new_string(failure.details()));
                self.set_notice("Copied the start error details", cx);
            }
            None => self.set_notice("The daemon has no start error", cx),
        }
    }

    /// `daemon::CopyUpdateCommand`: `brew upgrade --cask hutan`, on the clipboard.
    pub fn copy_update_command(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(
            hutan_client::start_failure::UPDATE_COMMAND.to_owned(),
        ));
        self.set_notice(
            format!(
                "Copied `{}`: run it in Terminal, then start Hutan again",
                hutan_client::start_failure::UPDATE_COMMAND
            ),
            cx,
        );
    }

    /// `daemon::StartWithEmptyData`: after a confirmation, moves the database a newer Hutan
    /// wrote into `backups/` and starts the daemon.
    pub fn start_with_empty_data(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(failure) = self.daemon_start_failure(cx) else {
            self.set_notice("The daemon has no start error", cx);
            return;
        };
        let Some((db, found, _)) = failure.schema_too_new() else {
            self.set_notice(
                "Start with empty data is for data from a newer Hutan; this start failed \
                 for another reason",
                cx,
            );
            return;
        };
        let backups = db
            .parent()
            .map(|dir| dir.join("backups"))
            .unwrap_or_else(|| PathBuf::from("backups"));
        let message = format!(
            "Hutan moves your data ({}, with its -wal and -shm files) into {} as \
             hutan-…-v{found}-newer.db and starts the daemon with empty data: no workspaces, \
             sessions or chats. Nothing is deleted; a newer Hutan can open the moved data again.",
            db.display(),
            backups.display()
        );
        self.open_confirm(
            "Start with empty data?",
            message,
            "Move data and start",
            move |this, _, cx| this.move_data_and_start(failure.clone(), cx),
            window,
            cx,
        );
    }

    fn move_data_and_start(&mut self, failure: StartFailure, cx: &mut Context<Self>) {
        let files = DaemonFiles::for_socket(&failure.socket);
        let record = failure.record.clone();
        let moving = cx
            .background_executor()
            .spawn(async move { move_data_aside(&files, record.as_ref()) });
        self.daemon.task = Some(cx.spawn(async move |this, cx| {
            let moved = moving.await;
            this.update(cx, |this, cx| match moved {
                Ok(moved) => {
                    tracing::info!(backup = %moved.backup.display(), "moved the newer data aside");
                    this.set_notice(
                        format!("Your data was moved to {}", moved.backup.display()),
                        cx,
                    );
                    this.begin_start(false, cx);
                }
                Err(error) => {
                    tracing::warn!("cannot move the data aside: {error}");
                    this.set_notice(format!("Cannot start with empty data: {error}"), cx);
                }
            })
            .ok();
        }));
    }

    /// `daemon::ShowStuckDaemon`: the pid and executable of the daemon that holds the lock.
    pub fn show_stuck_daemon(&mut self, cx: &mut Context<Self>) {
        let Some(pid) = self.daemon_start_failure(cx).and_then(|f| f.lock_holder()) else {
            self.set_notice("No stuck daemon is known", cx);
            return;
        };
        self.daemon.stuck_info = Some(match process_path(pid) {
            Some(path) => format!("pid {pid}: {}", path.display()),
            None => format!("pid {pid} is no longer running"),
        });
        cx.notify();
    }

    /// `daemon::StopStuckDaemon`: after a confirmation, SIGTERM to exactly the pid that holds
    /// the lock (only if it is a `hutand`), then a new start.
    pub fn stop_stuck_daemon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pid) = self.daemon_start_failure(cx).and_then(|f| f.lock_holder()) else {
            self.set_notice("No stuck daemon is known", cx);
            return;
        };
        self.open_destructive(
            format!("Stop hutand (pid {pid})?"),
            "It holds the daemon's lock but does not answer. Hutan asks it to stop (SIGTERM) \
             and starts a new daemon. Its sessions come back with the new one.",
            "Stop it",
            move |this, _, cx| this.terminate_stuck_daemon(pid, cx),
            window,
            cx,
        );
    }

    fn terminate_stuck_daemon(&mut self, pid: u32, cx: &mut Context<Self>) {
        let is_hutand = process_path(pid)
            .and_then(|path| path.file_name().map(|name| name == "hutand"))
            .unwrap_or(false);
        if !is_hutand {
            self.set_notice(
                format!("pid {pid} is not a running hutand; nothing was stopped"),
                cx,
            );
            self.begin_start(false, cx);
            return;
        }
        match terminate(pid) {
            Ok(()) => {
                self.daemon.stuck_info = None;
                self.set_notice(format!("Asked hutand (pid {pid}) to stop"), cx);
                self.begin_start(false, cx);
            }
            Err(error) => {
                self.set_notice(format!("Cannot stop hutand (pid {pid}): {error}"), cx);
            }
        }
    }

    /// The buttons a failure of `kind` offers, with the `region` each is probed as; *Retry*
    /// first when the connection gave up starting the daemon (`autostart_halted`).
    fn start_failure_buttons(
        &self,
        kind: StartFailureKind,
        region: fn(&'static str) -> Region,
        halted: bool,
    ) -> Vec<AnyElement> {
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
                .child(probe(&regions, region(id)))
                .into_any_element()
        };
        let retry = halted.then(|| {
            button(
                "daemon-retry",
                "Retry",
                ButtonKind::Secondary,
                Box::new(daemon::Start),
            )
        });
        let offered = match kind {
            StartFailureKind::NewerData => vec![
                button(
                    "daemon-copy-update",
                    "Copy update command",
                    ButtonKind::Secondary,
                    Box::new(daemon::CopyUpdateCommand),
                ),
                button(
                    "daemon-empty-data",
                    "Start with empty data…",
                    ButtonKind::Ghost,
                    Box::new(daemon::StartWithEmptyData),
                ),
            ],
            StartFailureKind::Stuck => vec![
                button(
                    "daemon-show-stuck",
                    "Show the process",
                    ButtonKind::Secondary,
                    Box::new(daemon::ShowStuckDaemon),
                ),
                button(
                    "daemon-stop-stuck",
                    "Stop it…",
                    ButtonKind::Ghost,
                    Box::new(daemon::StopStuckDaemon),
                ),
            ],
            StartFailureKind::Other => vec![
                button(
                    "daemon-failure-logs",
                    "Open logs",
                    ButtonKind::Secondary,
                    Box::new(daemon::OpenLogs),
                ),
                button(
                    "daemon-copy-details",
                    "Copy details",
                    ButtonKind::Ghost,
                    Box::new(daemon::CopyStartError),
                ),
            ],
        };
        retry.into_iter().chain(offered).collect()
    }

    /// The reason and its actions, for the line under the title bar and the popover.
    pub(crate) fn render_start_failure_body(
        &self,
        failure: &StartFailure,
        region: fn(&'static str) -> Region,
        halted: bool,
    ) -> gpui::Div {
        let theme = self.theme;
        let tokens = self.density.ui();
        let kind = StartFailureKind::of(failure);
        let busy = self.daemon.busy.is_some();
        let mut text = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(px(2.0))
            .child(
                div()
                    .text_color(theme.fg)
                    .child(SharedString::from(failure.sentence())),
            );
        if let Some(info) = self
            .daemon
            .stuck_info
            .clone()
            .filter(|_| kind == StartFailureKind::Stuck)
        {
            text = text.child(
                div()
                    .text_color(theme.muted)
                    .child(SharedString::from(info)),
            );
        }
        div()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .child(
                div()
                    .flex_none()
                    .text_color(theme.danger)
                    .child(glyph(Icon::StateFailed)),
            )
            .child(text)
            .when(!busy, |s| {
                s.children(self.start_failure_buttons(kind, region, halted))
            })
    }

    /// The line under the title bar while the daemon could not start.
    pub(crate) fn render_start_failure_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let failure = self.daemon_start_failure(cx)?;
        let halted = self.sessions.read(cx).state().autostart_halted;
        let theme = self.theme;
        let tokens = self.density.ui();
        Some(
            div()
                .id("daemon-start-failure")
                .relative()
                .flex_none()
                .px(px(tokens.spacing * 4.0))
                .py(px(tokens.spacing))
                .bg(theme.surface)
                .border_b_1()
                .border_color(theme.danger)
                .text_size(px(tokens.secondary_font_size))
                .child(self.render_start_failure_body(&failure, Region::Offer, halted))
                .child(probe(&self.regions, Region::Offer("daemon-start-failure")))
                .into_any_element(),
        )
    }
}
