//! "Install…" for a harness the daemon could not find (`crate::harness_install`): checks the
//! package against the fixed table, asks the user to confirm with the version and integrity in
//! front of them, and runs the install in an ordinary terminal tab. When that tab exits with 0
//! the harness detection asks again, so a failed chat or assistant offers Retry for a harness
//! that is there now.
//!
//! The buttons of the setup, the Accounts page, the new-agent dialog and the assistant's failure
//! card all dispatch [`InstallHarness`](super::actions::InstallHarness).

use std::sync::Arc;

use gpui::{Context, Window};
use hutan_core::SessionId;
use hutan_core::session::SessionState;

use super::harnesses::harness_name;
use super::modal::Modal;
use super::view::Workbench;
use crate::harness_install::{self, PackageRunner, Verified};

/// An install running in a terminal tab.
struct Running {
    session: SessionId,
    verified: Verified,
}

/// State of the installs inside the workbench.
#[derive(Default)]
pub(crate) struct HarnessInstallControl {
    runner: Option<Arc<dyn PackageRunner>>,
    /// The registry is being asked.
    checking: bool,
    running: Vec<Running>,
}

impl HarnessInstallControl {
    pub(crate) fn new(runner: Option<Arc<dyn PackageRunner>>) -> Self {
        Self {
            runner,
            ..Self::default()
        }
    }
}

impl Workbench {
    /// Whether "Install…" can be offered at all (the desktop was given a [`PackageRunner`]).
    pub fn harness_install_available(&self) -> bool {
        self.harness_install.runner.is_some()
    }

    /// The registry is being asked about a package.
    pub fn harness_install_checking(&self) -> bool {
        self.harness_install.checking
    }

    /// Installs running in a terminal tab, as `(harness, session)`.
    pub fn harness_installs_running(&self) -> Vec<(&'static str, SessionId)> {
        self.harness_install
            .running
            .iter()
            .map(|r| (r.verified.source.harness, r.session))
            .collect()
    }

    /// "Install…" of `harness`: verify, then ask. Nothing is installed before the user confirms.
    pub fn install_harness(&mut self, harness: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(runner) = self.harness_install.runner.clone() else {
            self.set_notice("Installing harnesses is not available here", cx);
            return;
        };
        let Some(source) = harness_install::source(harness) else {
            self.set_notice(
                format!("Hutan does not know where to install {harness} from"),
                cx,
            );
            return;
        };
        if self.harness_install.checking {
            return;
        }
        self.harness_install.checking = true;
        self.set_notice(format!("Checking {} on npm…", source.package), cx);
        let work = cx
            .background_executor()
            .spawn(async move { harness_install::verify(runner.as_ref(), source) });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            this.update_in(cx, |this, window, cx| {
                this.harness_install.checking = false;
                this.clear_notice(cx);
                match result {
                    Ok(verified) => this.confirm_harness_install(verified, window, cx),
                    Err(refusal) => {
                        tracing::warn!("not installing {}: {refusal}", source.package);
                        // Without npm or bun this is the command to run by hand and the source.
                        this.set_notice(refusal.to_string(), cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn confirm_harness_install(
        &mut self,
        verified: Verified,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source = verified.source;
        let title = format!(
            "Install {} {} from npm ({})?",
            source.package, verified.version, source.repository
        );
        let message = format!(
            "{} from {}. The registry lists it with the integrity {}. Hutan opens a terminal \
             tab running `{}` so you see what it does; nothing is installed before you \
             confirm.",
            capitalized(source.what),
            source.link(),
            verified.integrity,
            verified.command()
        );
        let modal = Modal {
            title: title.into(),
            message: message.into(),
            rows: Vec::new(),
            confirm_label: "Install".into(),
            input: None,
            secure_input: false,
            destructive: false,
            choice: None,
            checkbox: None,
            on_confirm: std::rc::Rc::new(move |this, _, window, cx| {
                this.run_harness_install(verified.clone(), window, cx);
            }),
        };
        self.open_modal(modal, window, cx);
    }

    /// The confirmed install: a terminal tab running the command.
    fn run_harness_install(
        &mut self,
        verified: Verified,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(runner) = self.harness_install.runner.clone() else {
            return;
        };
        let (workspace, cwd) = {
            let store = self.sessions.read(cx);
            let workspace = self.target_workspace(store);
            let cwd = workspace
                .and_then(|w| w.roots.first().cloned())
                .or_else(std::env::home_dir);
            (workspace.map(|w| w.id), cwd)
        };
        let mut spec = self.new_terminal_spec(workspace, cwd, cx);
        spec.command = runner.install_argv(verified.source.tool, &verified.spec());
        spec.title = Some(format!("Install {}", verified.source.package));
        let task = self
            .sessions
            .update(cx, |store, cx| store.create_session(workspace, spec, cx));
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(info) => {
                this.update_in(cx, |this, window, cx| {
                    this.harness_install.running.push(Running {
                        session: info.id,
                        verified,
                    });
                    this.open_session(info.id, window, cx);
                    this.check_harness_installs(cx);
                })
                .ok();
            }
            Err(error) => {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Cannot start the install: {error}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// A session changed: an install tab that has ended is settled. Exit 0 makes the harness
    /// detection ask again.
    pub(crate) fn check_harness_installs(&mut self, cx: &mut Context<Self>) {
        if self.harness_install.running.is_empty() {
            return;
        }
        let mut settled: Vec<(Verified, Option<i32>)> = Vec::new();
        let store = self.sessions.read(cx);
        self.harness_install.running.retain(|running| {
            let outcome = match store.session(running.session).map(|s| &s.state) {
                None => Some(None),
                Some(SessionState::Exited { code, .. }) => Some(*code),
                Some(SessionState::Failed { .. } | SessionState::Stopped) => Some(None),
                Some(_) => None,
            };
            match outcome {
                Some(code) => {
                    settled.push((running.verified.clone(), code));
                    false
                }
                None => true,
            }
        });
        for (verified, code) in settled {
            let harness = verified.source.harness;
            if code == Some(0) {
                tracing::info!("installed {}", verified.spec());
                self.set_notice(
                    format!(
                        "Installed {} {}. Looking for {} again…",
                        verified.source.package,
                        verified.version,
                        harness_name(harness)
                    ),
                    cx,
                );
                self.env.harnesses.update(cx, |directory, cx| {
                    directory.refresh(cx);
                    directory.load_models(harness, cx);
                });
            } else {
                let how = code.map_or("did not finish".to_owned(), |c| format!("exited with {c}"));
                self.set_notice(
                    format!(
                        "The install of {} {how}; its terminal tab has the output. By hand: {}",
                        verified.source.package,
                        verified.command()
                    ),
                    cx,
                );
            }
        }
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
