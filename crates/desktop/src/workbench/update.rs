//! Updates in the window (UPDATES-1, UPDATES-2, protocol 0.60, ADR 0024; `docs/design.md`
//! § Updates): the *Update to x.y.z* status item, the sheet with the release notes and the
//! three answers, *Check for Updates…* (the Hutan menu and ⌘K), and what happens after a
//! successful Homebrew run: *Relaunch*, or for a hand-copied bundle *Quit and Install*.
//!
//! The daemon checks and installs (`update.status`, `update.check`, `update.install`); this keeps
//! its last answer ([`UpdateDirectory`]), words it ([`crate::sessions::update`]) and asks the
//! user. Nothing here ever updates by itself: the sheet opens when the user clicks the item or
//! asks for a check, and every step is a button.
//!
//! - **Update now** (a Homebrew install) has the daemon run Homebrew in a terminal session; the
//!   window opens that session so the user sees Homebrew's output and prompts. When it ends the
//!   daemon says how, the item turns into *Relaunch for x.y.z*, and the sheet offers *Relaunch*.
//! - **Update now** (a bundle copied by hand) has the daemon download the release, check it
//!   against two checksums and `codesign`, and unpack it; the sheet shows download, verify,
//!   ready as the daemon's `update.install` events arrive, then *Quit and Install*: the daemon
//!   is asked to replace the bundle once this process has exited, and the window quits.
//! - **Later** closes the sheet; the item stays.
//! - **Skip this version** writes `updates.skip_version` to `daemon.toml`; the item goes until a
//!   higher version comes. *Check for Updates…* still shows a skipped release.
//! - **Relaunch** quits the app (never asking, never stopping the daemon: its sessions go on) and
//!   opens the installed bundle again through a [`Relauncher`].

use gpui::{
    AnyElement, ClipboardItem, Context, Entity, InteractiveElement, IntoElement, MouseButton,
    ParentElement, SharedString, Styled, Subscription, Window, div, px,
};
use hutan_client::ConnectionState;
use hutan_protocol::update::{
    InstallMethod, InstallState, UpdateInstall, UpdateRelease, UpdateStatus,
};
use std::path::{Path, PathBuf};

use super::chat::{Ids, Look, blocks};
use super::render::probe;
use super::ui::motion::{self, PresentExt as _};
use super::ui::{Button, StyledExt as _};
use super::view::{Region, Workbench};
use crate::sessions::update::{self as words, ItemView, Primary, SheetView};
use crate::sessions::{SessionsStore, StoreChanged, UpdateChanged};
use crate::theme::DensityExt as _;

/// Width of the sheet.
const WIDTH: f32 = 540.0;
/// How tall the release notes get before they scroll.
const NOTES_MAX_HEIGHT: f32 = 240.0;

/// Opens the installed bundle again once this process has left (*Relaunch*). Not a test's
/// business: tests record the bundle they are asked to open.
pub trait Relauncher: std::fmt::Debug + Send + Sync {
    /// Arranges for `bundle` to be opened after this process exits. Returns before it does.
    fn relaunch_after_exit(&self, bundle: &Path) -> std::io::Result<()>;
}

/// The real one: a detached shell that waits for this process to end, then `open`s the bundle.
/// (`LSMultipleInstancesProhibited` would hand the `open` to the running app if it were sooner.)
#[derive(Debug, Default)]
pub struct OpenRelauncher;

impl Relauncher for OpenRelauncher {
    fn relaunch_after_exit(&self, bundle: &Path) -> std::io::Result<()> {
        use std::os::unix::process::CommandExt as _;
        use std::process::{Command, Stdio};
        let script = format!(
            "while kill -0 {} 2>/dev/null; do sleep 0.2; done; exec /usr/bin/open \"$1\"",
            std::process::id()
        );
        Command::new("/bin/sh")
            .args(["-c", &script, "hutan-relaunch"])
            .arg(bundle)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Its own group: it must outlive this process and its terminal.
            .process_group(0)
            .spawn()
            .map(drop)
    }
}

/// What the daemon says about updates: the last [`UpdateStatus`], asked once per connection and
/// kept current by `update.available` and `update.install`.
pub struct UpdateDirectory {
    sessions: Entity<SessionsStore>,
    status: Option<UpdateStatus>,
    /// The connection the status was asked on (`Connection::generation`).
    loaded_on: Option<u64>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for UpdateDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateDirectory")
            .field("known", &self.status.is_some())
            .finish_non_exhaustive()
    }
}

impl UpdateDirectory {
    pub fn new(sessions: Entity<SessionsStore>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe(&sessions, |this, _, event: &UpdateChanged, cx| {
                this.apply_event(event, cx);
            }),
            cx.subscribe(&sessions, |this, _, change: &StoreChanged, cx| {
                if change.intersects(StoreChanged::CONNECTION | StoreChanged::SESSIONS) {
                    this.load(cx);
                }
            }),
        ];
        let mut directory = Self {
            sessions,
            status: None,
            loaded_on: None,
            _subscriptions: subscriptions,
        };
        directory.load(cx);
        directory
    }

    /// The last status the daemon sent; `None` until it has.
    pub fn status(&self) -> Option<&UpdateStatus> {
        self.status.as_ref()
    }

    /// The status item now, if there is something to say.
    pub fn item(&self) -> Option<ItemView> {
        words::item(self.status.as_ref()?)
    }

    /// Takes `status` as the state of things (the answers to `update.status` and
    /// `update.check`, and tests').
    pub fn apply(&mut self, status: UpdateStatus, cx: &mut Context<Self>) {
        self.status = Some(status);
        cx.notify();
    }

    /// Records that the user skipped `version` (the daemon was told).
    fn mark_skipped(&mut self, version: &str, cx: &mut Context<Self>) {
        if let Some(status) = &mut self.status {
            status.skipped = Some(version.to_owned());
            cx.notify();
        }
    }

    fn apply_event(&mut self, event: &UpdateChanged, cx: &mut Context<Self>) {
        match (event, &mut self.status) {
            (UpdateChanged::Available(release), Some(status)) => {
                status.available = Some((**release).clone());
                cx.notify();
            }
            (UpdateChanged::Install(install), Some(status)) => {
                status.install = Some((**install).clone());
                cx.notify();
            }
            // Nothing to fold into yet: the answer to `update.status` has it all.
            (_, None) => self.load(cx),
        }
    }

    /// Asks for the status again (a setting of `[updates]` changed).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.loaded_on = None;
        self.load(cx);
    }

    /// Asks for the status once per connection.
    fn load(&mut self, cx: &mut Context<Self>) {
        let store = self.sessions.read(cx);
        if store.connection_state() != ConnectionState::Connected {
            return;
        }
        let connection = store.connection().clone();
        let generation = connection.generation();
        if self.loaded_on == Some(generation) || !connection.supports_update() {
            return;
        }
        self.loaded_on = Some(generation);
        cx.spawn(
            async move |this, cx| match connection.update_status().await {
                Ok(status) => {
                    this.update(cx, |this, cx| this.apply(status, cx)).ok();
                }
                Err(error) => tracing::debug!(%error, "no update status"),
            },
        )
        .detach();
    }
}

impl Workbench {
    /// What the daemon says about updates (tests, inspection).
    pub fn update_directory(&self) -> &Entity<UpdateDirectory> {
        &self.updates
    }

    /// The sheet with the release notes is open.
    pub fn update_sheet_open(&self) -> bool {
        self.update_open
    }

    /// The window's update model changed: the item and the sheet redraw, and a sheet whose
    /// release is gone closes.
    pub(crate) fn on_update_changed(&mut self, cx: &mut Context<Self>) {
        if self.update_open
            && self
                .updates
                .read(cx)
                .status()
                .is_none_or(|status| status.available.is_none())
        {
            self.update_open = false;
        }
        cx.notify();
    }

    /// Opens the sheet: the status item's click and `workbench::ShowUpdate`.
    pub fn show_update(&mut self, cx: &mut Context<Self>) {
        if self
            .updates
            .read(cx)
            .status()
            .is_some_and(|status| status.available.is_some())
        {
            self.update_open = true;
            cx.notify();
        }
    }

    /// *Later*, or the sheet's close: the item stays.
    pub fn close_update_sheet(&mut self, cx: &mut Context<Self>) {
        if self.update_open {
            self.update_open = false;
            cx.notify();
        }
    }

    /// *Check for Updates…*: asks the daemon to check now (`update.check`, whatever
    /// `updates.check` says). A release opens the sheet, even one the user skipped; nothing
    /// newer, or a check that could not finish, is said in the status bar.
    pub fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        let connection = self.connection(cx);
        if !connection.supports_update() {
            self.set_notice(
                "This daemon cannot check for updates (it is older than protocol 0.60). Restart \
                 it from the Daemon menu.",
                cx,
            );
            return;
        }
        self.set_notice("Checking for updates…", cx);
        cx.spawn(async move |this, cx| {
            let answer = connection.update_check().await;
            this.update(cx, |this, cx| match answer {
                Ok(status) => {
                    let note = words::check_note(&status);
                    this.updates
                        .update(cx, |updates, cx| updates.apply(status, cx));
                    match note {
                        None => {
                            this.notice = None;
                            this.show_update(cx);
                        }
                        Some(note) => this.set_notice(note, cx),
                    }
                }
                Err(error) => {
                    this.set_notice(format!("Could not check for updates: {error}"), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// *Update now*: the daemon runs Homebrew in a terminal session, which opens here; for a
    /// bundle copied by hand it downloads, verifies and unpacks the new app (the sheet shows
    /// how far it is) and then waits for *Quit and Install*.
    pub fn update_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(method) = self
            .updates
            .read(cx)
            .status()
            .map(|status| status.method)
            .filter(|method| {
                matches!(
                    method,
                    InstallMethod::Cask | InstallMethod::Formula | InstallMethod::Bundle
                )
            })
        else {
            return;
        };
        let task = self
            .sessions
            .update(cx, |store, cx| store.update_install(method, cx));
        cx.spawn_in(window, async move |this, cx| {
            let answer = task.await;
            this.update_in(cx, |this, window, cx| match answer {
                Ok(started) => {
                    this.updates.update(cx, |updates, cx| {
                        if let Some(status) = &mut updates.status {
                            // A bundle's update runs in the daemon and says every step in
                            // `update.install` events, sent before this answer: one that
                            // already arrived (a download refused at once) is newer than the
                            // answer, so the answer only fills an empty place.
                            let events_say_more = started.install.method == InstallMethod::Bundle
                                && status.install.is_some();
                            if !events_say_more {
                                status.install = Some(started.install.clone());
                                cx.notify();
                            }
                        }
                    });
                    if let Some(session) = &started.session {
                        this.open_session(session.id, window, cx);
                    }
                }
                Err(error) => this.set_notice(format!("Cannot update: {error}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// *Quit and Install* (a bundle copied by hand whose new app is verified): asks the daemon
    /// to replace the bundle once this process has exited, then quits — never asking and never
    /// stopping the daemon, so its sessions go on. The daemon moves the old app to the Trash,
    /// puts the new one in place and opens it. A refusal (a folder that cannot be written, an
    /// update that is no longer ready) is said and the window stays.
    pub fn update_quit_and_install(&mut self, cx: &mut Context<Self>) {
        let connection = self.connection(cx);
        let pid = self.update_wait_pid.unwrap_or_else(std::process::id);
        cx.spawn(async move |this, cx| {
            let answer = connection.update_install_apply(Some(pid)).await;
            this.update(cx, |this, cx| match answer {
                Ok(applying) => {
                    this.updates.update(cx, |updates, cx| {
                        if let Some(status) = &mut updates.status {
                            status.install = Some(applying.install);
                            cx.notify();
                        }
                    });
                    this.update_open = false;
                    this.quit_leaving_daemon(cx);
                }
                Err(error) => this.set_notice(format!("Cannot install the update: {error}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// *Skip this version*: `updates.skip_version` in `daemon.toml`; the item goes.
    pub fn update_skip(&mut self, cx: &mut Context<Self>) {
        let Some(version) = self
            .updates
            .read(cx)
            .status()
            .and_then(|status| status.available.as_ref())
            .map(|release| release.version.clone())
        else {
            return;
        };
        self.update_open = false;
        let connection = self.connection(cx);
        let skipped = version.clone();
        self.updates
            .update(cx, |updates, cx| updates.mark_skipped(&skipped, cx));
        cx.spawn(async move |this, cx| {
            let answer = connection
                .daemon_setting_set(
                    "updates.skip_version",
                    Some(serde_json::Value::String(version)),
                )
                .await;
            if let Err(error) = answer {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Cannot skip this version: {error}"), cx);
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    /// *Relaunch*: opens the installed app again once this one has quit. The quit never asks and
    /// never stops the daemon; the new app moves it onto the new build (asking first while
    /// sessions run).
    pub fn update_relaunch(&mut self, cx: &mut Context<Self>) {
        let bundle = self.relaunch_bundle(cx);
        match (&self.relauncher, bundle) {
            (Some(relauncher), Some(bundle)) => match relauncher.relaunch_after_exit(&bundle) {
                Ok(()) => {
                    self.update_open = false;
                    self.quit_leaving_daemon(cx);
                }
                Err(error) => self.set_notice(format!("Cannot relaunch Hutan: {error}"), cx),
            },
            _ => {
                self.update_open = false;
                self.set_notice("Quit Hutan and open it again to use the new version", cx);
            }
        }
    }

    /// The bundle *Relaunch* opens: where the daemon found the install (a cask's
    /// `/Applications/Hutan.app`), else the app this window runs from.
    fn relaunch_bundle(&self, cx: &gpui::App) -> Option<PathBuf> {
        let status = self.updates.read(cx).status()?;
        match status.method {
            InstallMethod::Cask | InstallMethod::Bundle => {
                status.install_path.as_ref().map(PathBuf::from)
            }
            _ => None,
        }
        .or_else(|| self.install.bundle())
    }

    /// Sessions that run now, not counting the one that runs Homebrew.
    fn sessions_besides_update(&self, install: Option<&UpdateInstall>, cx: &gpui::App) -> usize {
        self.sessions
            .read(cx)
            .sessions()
            .iter()
            .filter(|s| s.state.is_live())
            .filter(|s| install.is_none_or(|install| install.session_id != Some(s.id)))
            .count()
    }

    /// The `update` status item's words, if it has any.
    pub(crate) fn update_item(&self, cx: &gpui::App) -> Option<ItemView> {
        self.updates.read(cx).item()
    }

    pub(crate) fn render_update_sheet(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let wanted =
            self.overlays
                .track("update-sheet", self.update_open, motion::Kind::Popover, cx)?;
        let status = self.updates.read(cx).status().cloned()?;
        let live = self.sessions_besides_update(status.install.as_ref(), cx);
        let view = words::sheet(&status, live)?;
        let theme = self.theme;
        let tokens = self.density.ui();
        let regions = self.regions.clone();
        let look = Look::of(cx);
        let release = status.available.as_ref()?;

        let notes = {
            let mut ids = Ids::new("update-notes");
            let parsed = hutan_chat_view::markdown::parse(&view.notes);
            div()
                .id("update-notes")
                .flex()
                .flex_col()
                .gap(px(tokens.spacing * 2.0))
                .child(hutan_ui::scroll_y(
                    "update-notes-scroll",
                    div()
                        .id("update-notes-body")
                        .max_h(px(NOTES_MAX_HEIGHT))
                        .pr(px(tokens.spacing * 2.0))
                        .child(if parsed.is_empty() {
                            div()
                                .text_color(theme.muted)
                                .child("This release has no notes.")
                                .into_any_element()
                        } else {
                            blocks(&parsed, &look, &mut ids)
                        }),
                ))
                .child(probe(&regions, Region::Update("notes")))
        };
        let notes_link = {
            let url = view.notes_url.clone();
            Button::new(
                "update-notes-link",
                if view.notes_truncated {
                    "These notes are cut. Read them on GitHub"
                } else {
                    "Read on GitHub"
                },
                &theme,
                &tokens,
            )
            .link()
            .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url)))
            .child(probe(&regions, Region::Update("notes-link")))
        };
        let paragraphs = view.will_happen.iter().map(|text| {
            div()
                .text_color(theme.muted)
                .text_size(px(tokens.secondary_font_size))
                .child(SharedString::from(text.clone()))
        });
        let outcome = view.outcome.clone().map(|text| {
            let failed = status
                .install
                .as_ref()
                .is_some_and(|i| i.state == InstallState::Failed);
            div()
                .id("update-outcome")
                .text_color(if failed { theme.danger } else { theme.success })
                .child(SharedString::from(text))
                .child(probe(&regions, Region::Update("outcome")))
        });
        let progress = view.progress.clone().map(|progress| {
            let steps = progress.steps.iter().map(|(name, state)| {
                let (mark, color) = match state {
                    words::StepState::Done => ("✓", theme.success),
                    words::StepState::Current => ("●", theme.fg),
                    words::StepState::Waiting => ("○", theme.muted),
                };
                div()
                    .text_color(color)
                    .child(SharedString::from(format!("{mark} {name}")))
            });
            let bar = progress.fraction.map(|fraction| {
                div()
                    .id("update-progress-bar")
                    .h(px(4.0))
                    .w_full()
                    .rounded(px(2.0))
                    .bg(theme.border)
                    .child(
                        div()
                            .h_full()
                            .w(gpui::relative(fraction.clamp(0.0, 1.0)))
                            .rounded(px(2.0))
                            .bg(theme.accent),
                    )
            });
            div()
                .id("update-progress")
                .flex()
                .flex_col()
                .gap(px(tokens.spacing * 2.0))
                .child(
                    div()
                        .flex()
                        .gap(px(tokens.spacing * 4.0))
                        .text_size(px(tokens.secondary_font_size))
                        .children(steps),
                )
                .children(bar)
                .child(
                    div()
                        .text_color(theme.muted)
                        .text_size(px(tokens.secondary_font_size))
                        .child(SharedString::from(progress.label)),
                )
                .child(probe(&regions, Region::Update("progress")))
        });
        let manual = view.manual_command.clone().map(|command| {
            let copy = command.clone();
            div()
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(look.mono.clone())
                        .text_size(px(tokens.secondary_font_size))
                        .child(SharedString::from(command)),
                )
                .child(
                    Button::new("update-copy-command", "Copy command", &theme, &tokens)
                        .compact()
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                        }))
                        .child(probe(&regions, Region::Update("copy-command"))),
                )
        });
        let show_terminal = status
            .install
            .as_ref()
            .and_then(|install| install.session_id)
            .map(|session| {
                Button::new("update-terminal", "Show terminal", &theme, &tokens)
                    .ghost()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_session(session, window, cx);
                    }))
                    .child(probe(&regions, Region::Update("terminal")))
            });
        let primary = self.update_primary_button(&view, release, cx);

        Some(
            div()
                .id("update-sheet")
                .key_context("UpdateSheet")
                .absolute()
                .right(px(12.0))
                .bottom(px(tokens.status_bar + 8.0))
                .w(px(WIDTH))
                .flex()
                .flex_col()
                .gap(px(tokens.spacing * 3.0))
                .popover(&theme, &tokens)
                .p(px(tokens.spacing * 4.0))
                .text_size(px(tokens.font_size))
                .text_color(theme.fg)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .id("update-title")
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(SharedString::from(view.title.clone()))
                                .child(probe(&regions, Region::Update("title"))),
                        )
                        .child(
                            div()
                                .text_color(theme.muted)
                                .text_size(px(tokens.secondary_font_size))
                                .child(SharedString::from(view.subtitle.clone())),
                        ),
                )
                .child(notes)
                .child(div().flex().child(notes_link))
                .children(paragraphs)
                .children(progress)
                .children(outcome)
                .children(manual)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(tokens.spacing * 2.0))
                        .child(
                            Button::new("update-skip", "Skip this version", &theme, &tokens)
                                .ghost()
                                .on_click(cx.listener(|this, _, _, cx| this.update_skip(cx)))
                                .child(probe(&regions, Region::Update("skip"))),
                        )
                        .child(div().flex_1())
                        .children(show_terminal)
                        .child(
                            Button::new("update-later", "Later", &theme, &tokens)
                                .on_click(cx.listener(|this, _, _, cx| this.close_update_sheet(cx)))
                                .child(probe(&regions, Region::Update("later"))),
                        )
                        .child(primary),
                )
                .child(probe(&regions, Region::Update("sheet")))
                .presented(
                    "update-sheet-motion",
                    motion::Kind::Popover,
                    wanted,
                    motion::Slide::Rise,
                )
                .into_any_element(),
        )
    }

    fn update_primary_button(
        &self,
        view: &SheetView,
        release: &UpdateRelease,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (self.theme, self.density.ui());
        let regions = self.regions.clone();
        match view.primary {
            Primary::UpdateNow | Primary::TryAgain => {
                let label = if view.primary == Primary::UpdateNow {
                    "Update now"
                } else {
                    "Try again"
                };
                Button::new("update-now", label, &theme, &tokens)
                    .primary()
                    .on_click(cx.listener(|this, _, window, cx| this.update_now(window, cx)))
                    .child(probe(&regions, Region::Update("now")))
                    .into_any_element()
            }
            Primary::QuitAndInstall => {
                Button::new("update-quit-install", "Quit and Install", &theme, &tokens)
                    .primary()
                    .on_click(cx.listener(|this, _, _, cx| this.update_quit_and_install(cx)))
                    .child(probe(&regions, Region::Update("quit-install")))
                    .into_any_element()
            }
            Primary::Running => Button::new("update-running", "Updating…", &theme, &tokens)
                .disabled(true)
                .child(probe(&regions, Region::Update("running")))
                .into_any_element(),
            Primary::Relaunch => Button::new("update-relaunch", "Relaunch", &theme, &tokens)
                .primary()
                .on_click(cx.listener(|this, _, _, cx| this.update_relaunch(cx)))
                .child(probe(&regions, Region::Update("relaunch")))
                .into_any_element(),
            Primary::Manual => {
                let url = release.notes_url.clone();
                Button::new("update-open-release", "Open release page", &theme, &tokens)
                    .primary()
                    .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url)))
                    .child(probe(&regions, Region::Update("open-release")))
                    .into_any_element()
            }
        }
    }
}
