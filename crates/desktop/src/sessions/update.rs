//! What the window says about updates (UPDATES-1, UPDATES-2, protocol 0.60, ADR 0024): the
//! words of the *Update to x.y.z* status item and of the sheet — what will happen, how far it
//! is, what the buttons are — derived from the daemon's [`UpdateStatus`]. Pure data, no GPUI.
//!
//! The daemon checks and installs; the window only words it. Nothing here decides whether an
//! update happens: every step is a button the user presses.

use hutan_protocol::update::{
    CheckOutcome, InstallMethod, InstallStage, InstallState, UpdateCheck, UpdateInstall,
    UpdateRelease, UpdateStatus,
};

/// How the status item looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemTone {
    /// An update waits: the item is a link-like invitation, never a warning.
    Offer,
    /// Homebrew is running, or a bundle is being downloaded and checked.
    Busy,
    /// Homebrew finished (relaunch), or a bundle is ready (quit and install).
    Done,
    /// The update did not finish.
    Failed,
}

/// The `update` status item: its words and its tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemView {
    pub text: String,
    pub tooltip: String,
    pub tone: ItemTone,
}

/// The status item, or `None` when there is nothing to say: no newer release, or one the user
/// skipped (a check the user asks for still shows it, in the sheet).
pub fn item(status: &UpdateStatus) -> Option<ItemView> {
    if let Some(install) = &status.install {
        if install.method == InstallMethod::Bundle {
            return bundle_item(install);
        }
        let version = &install.version;
        match install.state {
            InstallState::Running => {
                return Some(ItemView {
                    text: format!("Updating to {version}…"),
                    tooltip: "Homebrew is updating Hutan in a terminal · click for details"
                        .to_owned(),
                    tone: ItemTone::Busy,
                });
            }
            InstallState::Succeeded => {
                return Some(ItemView {
                    text: format!("Relaunch for {version}"),
                    tooltip: format!("Hutan {version} is installed · click to relaunch"),
                    tone: ItemTone::Done,
                });
            }
            InstallState::Failed => {
                return Some(ItemView {
                    text: "Update failed".to_owned(),
                    tooltip: "Homebrew did not finish · click for details".to_owned(),
                    tone: ItemTone::Failed,
                });
            }
        }
    }
    let release = status.available.as_ref()?;
    if status.skipped.as_deref() == Some(release.version.as_str()) {
        return None;
    }
    Some(ItemView {
        text: format!("Update to {}", release.version),
        tooltip: format!(
            "Hutan {} is available (this is {}) · click for what's new",
            release.version, status.version
        ),
        tone: ItemTone::Offer,
    })
}

/// The item of a hand-copied bundle's update: it follows the daemon's stage. Once the new app
/// is in place (`Succeeded`) there is nothing left to say: the app that runs is the new one.
fn bundle_item(install: &UpdateInstall) -> Option<ItemView> {
    let version = &install.version;
    let busy = |text: String, tooltip: &str| ItemView {
        text,
        tooltip: tooltip.to_owned(),
        tone: ItemTone::Busy,
    };
    match (install.state, install.stage) {
        (InstallState::Succeeded, _) => None,
        (InstallState::Failed, _) => Some(ItemView {
            text: "Update failed".to_owned(),
            tooltip: "Hutan did not update itself · click for details".to_owned(),
            tone: ItemTone::Failed,
        }),
        (InstallState::Running, Some(InstallStage::Ready)) => Some(ItemView {
            text: format!("Quit and Install {version}"),
            tooltip: format!(
                "Hutan {version} is downloaded and verified · click to quit and install it"
            ),
            tone: ItemTone::Done,
        }),
        (InstallState::Running, Some(InstallStage::Installing)) => Some(busy(
            format!("Installing {version}…"),
            "Hutan is replacing the app · it opens again in a moment",
        )),
        (InstallState::Running, Some(InstallStage::Downloading)) => Some(busy(
            match percent(install) {
                Some(percent) => format!("Downloading {version} · {percent}%"),
                None => format!("Downloading {version}…"),
            },
            "Hutan is downloading the update · click for details",
        )),
        (InstallState::Running, _) => Some(busy(
            format!("Checking {version}…"),
            "Hutan is checking the download · click for details",
        )),
    }
}

/// How much of the download is in, in whole percent.
fn percent(install: &UpdateInstall) -> Option<u64> {
    let (done, total) = (
        install.downloaded?,
        install.total.filter(|total| *total > 0)?,
    );
    Some((done.min(total) * 100) / total)
}

/// The main button of the sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Primary {
    /// *Update now*: the daemon runs Homebrew, or downloads and checks the new bundle.
    UpdateNow,
    /// Homebrew is running, or the bundle is being downloaded and checked; nothing to press.
    Running,
    /// *Relaunch*: Homebrew finished.
    Relaunch,
    /// *Quit and Install*: the new bundle is downloaded and verified.
    QuitAndInstall,
    /// *Try again*: Homebrew failed, or a download was refused.
    TryAgain,
    /// Hutan cannot update itself here: *Open release page* and the words that say why.
    Manual,
}

/// One of the three steps of a hand-copied bundle's update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Waiting,
    Current,
    Done,
}

/// How far a hand-copied bundle's update is: the three steps (download, verify, ready), what
/// the current one is doing, and how much of the download is in.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub steps: [(&'static str, StepState); 3],
    /// What is happening now, one line.
    pub label: String,
    /// `0.0..=1.0` while the length is known, else no bar.
    pub fraction: Option<f32>,
}

/// Everything the sheet shows.
#[derive(Debug, Clone, PartialEq)]
pub struct SheetView {
    pub title: String,
    /// `You have 0.3.1 · pre-release · published 2026-10-29`.
    pub subtitle: String,
    /// The release notes (Markdown), as the daemon bounded them.
    pub notes: String,
    pub notes_truncated: bool,
    pub notes_url: String,
    /// What will happen, one short paragraph each (what runs, what a running session meets).
    pub will_happen: Vec<String>,
    pub primary: Primary,
    /// The install's outcome, when one ran: `Homebrew finished` / `Homebrew exited with 1`, or
    /// why the bundle's update stopped.
    pub outcome: Option<String>,
    /// The command to run by hand where Hutan runs nothing itself.
    pub manual_command: Option<String>,
    /// A hand-copied bundle's update in progress.
    pub progress: Option<Progress>,
}

/// The sheet for the status, or `None` when there is no release to show.
pub fn sheet(status: &UpdateStatus, live_sessions: usize) -> Option<SheetView> {
    let release = status.available.as_ref()?;
    let primary = primary_of(status);
    let bundle = status
        .install
        .as_ref()
        .filter(|install| install.method == InstallMethod::Bundle);
    Some(SheetView {
        title: format!("Update to Hutan {}", release.version),
        subtitle: subtitle(status, release),
        notes: release.notes.clone(),
        notes_truncated: release.notes_truncated,
        notes_url: release.notes_url.clone(),
        will_happen: will_happen(status, release, live_sessions, primary == Primary::Relaunch),
        primary,
        outcome: status.install.as_ref().and_then(outcome),
        manual_command: match (status.method, bundle) {
            (InstallMethod::Cask | InstallMethod::Formula, _) => None,
            (InstallMethod::Bundle, install) => install.and_then(|i| i.manual_command.clone()),
            (InstallMethod::Unknown, _) => Some(hutan_protocol::update::CASK_COMMAND.to_owned()),
        },
        progress: bundle.and_then(|install| progress(install, release)),
    })
}

fn primary_of(status: &UpdateStatus) -> Primary {
    match (&status.install, status.method) {
        (Some(install), InstallMethod::Bundle) => match install.state {
            InstallState::Running if install.stage == Some(InstallStage::Ready) => {
                Primary::QuitAndInstall
            }
            InstallState::Running | InstallState::Succeeded => Primary::Running,
            InstallState::Failed if install.manual_command.is_some() => Primary::Manual,
            InstallState::Failed => Primary::TryAgain,
        },
        (None, InstallMethod::Bundle) => Primary::UpdateNow,
        (Some(install), _) if install.state == InstallState::Running => Primary::Running,
        (Some(install), _) if install.state == InstallState::Succeeded => Primary::Relaunch,
        (Some(_), InstallMethod::Cask | InstallMethod::Formula) => Primary::TryAgain,
        (None, InstallMethod::Cask | InstallMethod::Formula) => Primary::UpdateNow,
        _ => Primary::Manual,
    }
}

fn subtitle(status: &UpdateStatus, release: &UpdateRelease) -> String {
    let mut parts = vec![format!("You have {}", status.version)];
    if release.prerelease {
        parts.push("pre-release".to_owned());
    }
    if let Some(published) = release.published.as_deref().and_then(|p| p.get(..10)) {
        parts.push(format!("published {published}"));
    }
    parts.join(" · ")
}

/// `text` with its first letter in capitals and a full stop at its end: the daemon's messages
/// are clauses (`the download's SHA-256 … is not the one GitHub lists`), the sheet's are
/// sentences.
fn sentence(text: &str) -> String {
    let mut chars = text.trim().chars();
    let mut sentence: String = chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    if !sentence.ends_with(['.', '!', '?', ':']) {
        sentence.push('.');
    }
    sentence
}

fn outcome(install: &UpdateInstall) -> Option<String> {
    if install.method == InstallMethod::Bundle {
        return match install.state {
            InstallState::Running => None,
            InstallState::Succeeded => Some(install.message.as_deref().map_or_else(
                || format!("Hutan {} is installed.", install.version),
                sentence,
            )),
            InstallState::Failed => Some(
                install
                    .message
                    .as_deref()
                    .map_or_else(|| "The update did not finish.".to_owned(), sentence),
            ),
        };
    }
    match install.state {
        InstallState::Running => None,
        InstallState::Succeeded => Some(format!("Hutan {} is installed.", install.version)),
        InstallState::Failed => Some(match install.exit_code {
            Some(code) => format!("Homebrew exited with {code}. Its terminal shows why."),
            None => "Homebrew was stopped before it finished.".to_owned(),
        }),
    }
}

/// `36.7 MB` (decimal, as Finder counts).
pub fn megabytes(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)] // a size shown to one decimal
    let megabytes = bytes as f64 / 1_000_000.0;
    format!("{megabytes:.1} MB")
}

/// The three steps and what the current one is doing; `None` when there is nothing to show
/// (a bundle update that ended).
fn progress(install: &UpdateInstall, release: &UpdateRelease) -> Option<Progress> {
    if install.state != InstallState::Running {
        return None;
    }
    use StepState::{Current, Done, Waiting};
    let stage = install.stage?;
    let steps =
        |download, verify, ready| [("Download", download), ("Verify", verify), ("Ready", ready)];
    let version = &release.version;
    Some(match stage {
        InstallStage::Downloading => {
            let fraction = install
                .total
                .filter(|total| *total > 0)
                .zip(install.downloaded)
                .map(|(total, done)| {
                    #[allow(clippy::cast_precision_loss)] // a fraction for a bar
                    let fraction = done.min(total) as f32 / total as f32;
                    fraction
                });
            let label = match (install.downloaded, install.total) {
                (Some(done), Some(total)) if total > 0 => format!(
                    "Downloading Hutan {version} · {} of {}",
                    megabytes(done.min(total)),
                    megabytes(total)
                ),
                _ => format!("Downloading Hutan {version}…"),
            };
            Progress {
                steps: steps(Current, Waiting, Waiting),
                label,
                fraction,
            }
        }
        InstallStage::Verifying => Progress {
            steps: steps(Done, Current, Waiting),
            label: "Checking the download against GitHub's checksum and the tap's…".to_owned(),
            fraction: None,
        },
        InstallStage::Extracting => Progress {
            steps: steps(Done, Current, Waiting),
            label: "Unpacking the app…".to_owned(),
            fraction: None,
        },
        InstallStage::Checking => Progress {
            steps: steps(Done, Current, Waiting),
            label: "Checking the app's signature…".to_owned(),
            fraction: None,
        },
        InstallStage::Ready => Progress {
            steps: steps(Done, Done, Done),
            label: format!("Hutan {version} is downloaded and verified."),
            fraction: Some(1.0),
        },
        InstallStage::Installing => Progress {
            steps: steps(Done, Done, Done),
            label: "Replacing the app…".to_owned(),
            fraction: None,
        },
        InstallStage::Installed => return None,
    })
}

/// What the user is told before pressing *Update now*, and after it ran.
fn will_happen(
    status: &UpdateStatus,
    release: &UpdateRelease,
    live_sessions: usize,
    installed: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    match status.method {
        InstallMethod::Cask | InstallMethod::Formula => {
            if !installed {
                let command = status.command.as_deref().unwrap_or_default();
                lines.push(format!(
                    "Hutan runs `{command}` in a terminal you can watch. Homebrew may ask for \
                     your password, and macOS may ask you to approve Hutan {} the first time you \
                     open it.",
                    release.version
                ));
            }
            if status.method == InstallMethod::Formula {
                lines.push(
                    "This install is the command line and the daemon only; the app is not part \
                     of it."
                        .to_owned(),
                );
            }
            lines.push(running_sessions(live_sessions, installed));
        }
        InstallMethod::Bundle => bundle_will_happen(status, release, live_sessions, &mut lines),
        InstallMethod::Unknown => lines.push(
            "Hutan does not know how it was installed (a development build, or a folder it \
             cannot write to). Update it the way you installed it."
                .to_owned(),
        ),
    }
    lines
}

fn bundle_will_happen(
    status: &UpdateStatus,
    release: &UpdateRelease,
    live_sessions: usize,
    lines: &mut Vec<String>,
) {
    let place = status
        .install_path
        .as_deref()
        .unwrap_or("this copy of Hutan.app");
    let install = status
        .install
        .as_ref()
        .filter(|install| install.method == InstallMethod::Bundle);
    match install.map(|i| (i.state, i.stage, i.manual_command.is_some())) {
        None | Some((InstallState::Failed, _, false)) => {
            let size = release
                .assets
                .iter()
                .find(|asset| asset.name == format!("Hutan-{}-arm64.zip", release.version))
                .filter(|asset| asset.size > 0)
                .map(|asset| format!(" ({})", megabytes(asset.size)))
                .unwrap_or_default();
            lines.push(format!(
                "Hutan downloads Hutan {}{size}, checks it against the checksums of GitHub and \
                 of the Homebrew tap, and verifies its signature. Nothing changes until you \
                 choose Quit and Install.",
                release.version
            ));
            lines.push(format!(
                "Then Hutan quits, moves {place} to the Trash, puts the new app in its place and \
                 opens it."
            ));
            lines.push(bundle_sessions(live_sessions));
        }
        Some((InstallState::Running, Some(InstallStage::Ready), _)) => {
            lines.push(format!(
                "Quit and Install closes this window, moves {place} to the Trash (you can put it \
                 back), puts Hutan {} in its place and opens it.",
                release.version
            ));
            lines.push(bundle_sessions(live_sessions));
        }
        Some((InstallState::Running, _, _)) => {
            lines.push(format!(
                "Nothing has changed yet: {place} stays as it is until the download is checked \
                 and you choose Quit and Install."
            ));
        }
        Some((InstallState::Failed, _, true)) => lines.push(
            "Hutan cannot replace the app itself here. The new app is downloaded and verified; \
             this command installs it:"
                .to_owned(),
        ),
        Some((InstallState::Succeeded, _, _)) => {
            lines.push(format!("Hutan {} is installed.", release.version));
        }
    }
}

fn bundle_sessions(live: usize) -> String {
    let sessions = match live {
        0 => "Nothing is running now.".to_owned(),
        1 => "1 session is running and keeps running while the app is replaced.".to_owned(),
        n => format!("{n} sessions are running and keep running while the app is replaced."),
    };
    format!("{sessions} {}", restart_note(live))
}

fn restart_note(live: usize) -> &'static str {
    if live == 0 {
        "A service with nothing running moves onto the new build by itself."
    } else {
        "A service with sessions running asks first (Restart now or Later): terminals are \
         restored, and agent turns in progress are interrupted."
    }
}

fn running_sessions(live: usize, installed: bool) -> String {
    let sessions = match live {
        0 => "Nothing is running now.".to_owned(),
        1 => "1 session is running and keeps running while Homebrew works.".to_owned(),
        n => format!("{n} sessions are running and keep running while Homebrew works."),
    };
    let after = if installed {
        "Relaunching quits the app and opens the new one; the background service stays up."
    } else {
        "Nothing changes until you relaunch."
    };
    format!("{sessions} {after} {}", restart_note(live))
}

/// What *Check for Updates…* tells the user when it found nothing to offer: up to date, or why
/// the check could not finish. `None` when the sheet has something to show.
pub fn check_note(status: &UpdateStatus) -> Option<String> {
    if status.available.is_some() {
        return None;
    }
    match status.last_check.as_ref() {
        Some(check) if check.outcome != CheckOutcome::Checked => Some(format!(
            "Could not check for updates: {}",
            check.message.as_deref().unwrap_or("no answer")
        )),
        // Published, not downloadable yet: said instead of "up to date" (the daemon's words).
        Some(check) if check.message.is_some() => check.message.clone(),
        _ => Some(format!("Hutan {} is up to date", status.version)),
    }
}

/// Whether the daemon checks by itself (`updates.check = "auto"`).
pub fn checks_automatically(status: &UpdateStatus) -> bool {
    status.check == UpdateCheck::Auto
}

#[cfg(test)]
mod tests {
    use hutan_core::SessionId;
    use hutan_protocol::update::{
        CASK_COMMAND, UpdateAsset, UpdateChannel, UpdateCheckInfo, UpdateInstall,
    };

    use super::*;

    fn release() -> UpdateRelease {
        UpdateRelease {
            version: "0.3.2".into(),
            tag: "v0.3.2".into(),
            prerelease: true,
            published: Some("2026-10-29T10:15:00Z".into()),
            notes_url: "https://github.com/robertt3kuk/hutan/releases/tag/v0.3.2".into(),
            notes: "## Hutan 0.3.2\n\n- Update check".into(),
            notes_truncated: false,
            assets: vec![UpdateAsset {
                name: "Hutan-0.3.2-arm64.zip".into(),
                size: 36_700_160,
                sha256: None,
            }],
        }
    }

    fn status(method: InstallMethod) -> UpdateStatus {
        UpdateStatus {
            version: "0.3.1".into(),
            check: UpdateCheck::Auto,
            channel: UpdateChannel::Prerelease,
            method,
            install_path: Some("/Applications/Hutan.app".into()),
            command: matches!(method, InstallMethod::Cask).then(|| CASK_COMMAND.to_owned()),
            available: Some(release()),
            skipped: None,
            last_check: Some(UpdateCheckInfo {
                at_ms: 1,
                outcome: CheckOutcome::Checked,
                message: None,
            }),
            install: None,
        }
    }

    fn install(state: InstallState, exit_code: Option<i32>) -> UpdateInstall {
        UpdateInstall {
            method: InstallMethod::Cask,
            session_id: Some(SessionId::new()),
            version: "0.3.2".into(),
            command: CASK_COMMAND.into(),
            state,
            exit_code,
            stage: None,
            downloaded: None,
            total: None,
            message: None,
            manual_command: None,
        }
    }

    fn bundle_install(stage: InstallStage) -> UpdateInstall {
        UpdateInstall {
            method: InstallMethod::Bundle,
            session_id: None,
            version: "0.3.2".into(),
            command: String::new(),
            state: InstallState::Running,
            exit_code: None,
            stage: Some(stage),
            downloaded: Some(9_175_040),
            total: Some(36_700_160),
            message: None,
            manual_command: None,
        }
    }

    #[test]
    fn the_item_offers_the_version_and_goes_when_it_is_skipped_or_there_is_none() {
        let mut s = status(InstallMethod::Cask);
        let shown = item(&s).unwrap();
        assert_eq!(shown.text, "Update to 0.3.2");
        assert_eq!(shown.tone, ItemTone::Offer);
        assert!(shown.tooltip.contains("0.3.1"));
        s.skipped = Some("0.3.2".into());
        assert_eq!(item(&s), None);
        s.skipped = Some("0.3.1".into());
        assert!(item(&s).is_some(), "another version's skip does not count");
        s.available = None;
        assert_eq!(item(&s), None);
    }

    #[test]
    fn the_item_follows_the_install() {
        let mut s = status(InstallMethod::Cask);
        s.install = Some(install(InstallState::Running, None));
        assert_eq!(item(&s).unwrap().text, "Updating to 0.3.2…");
        s.install = Some(install(InstallState::Succeeded, Some(0)));
        let done = item(&s).unwrap();
        assert_eq!(done.text, "Relaunch for 0.3.2");
        assert_eq!(done.tone, ItemTone::Done);
        s.install = Some(install(InstallState::Failed, Some(1)));
        assert_eq!(item(&s).unwrap().tone, ItemTone::Failed);
        // A skipped version still shows its install: the user pressed it.
        s.skipped = Some("0.3.2".into());
        assert!(item(&s).is_some());
    }

    #[test]
    fn the_sheet_says_what_will_happen_to_running_sessions() {
        let s = status(InstallMethod::Cask);
        let none = sheet(&s, 0).unwrap();
        assert_eq!(none.title, "Update to Hutan 0.3.2");
        assert_eq!(
            none.subtitle,
            "You have 0.3.1 · pre-release · published 2026-10-29"
        );
        assert_eq!(none.primary, Primary::UpdateNow);
        let text = none.will_happen.join(" ");
        assert!(text.contains(CASK_COMMAND), "{text}");
        assert!(text.contains("Nothing is running now"), "{text}");
        assert!(
            text.contains("moves onto the new build by itself"),
            "{text}"
        );

        let busy = sheet(&s, 3).unwrap().will_happen.join(" ");
        assert!(
            busy.contains("3 sessions are running and keep running"),
            "{busy}"
        );
        assert!(busy.contains("asks first (Restart now or Later)"), "{busy}");
        let one = sheet(&s, 1).unwrap().will_happen.join(" ");
        assert!(one.contains("1 session is running"), "{one}");
    }

    #[test]
    fn the_primary_button_follows_the_method_and_the_install() {
        let mut cask = status(InstallMethod::Cask);
        assert_eq!(sheet(&cask, 0).unwrap().primary, Primary::UpdateNow);
        cask.install = Some(install(InstallState::Running, None));
        assert_eq!(sheet(&cask, 0).unwrap().primary, Primary::Running);
        cask.install = Some(install(InstallState::Failed, Some(2)));
        let failed = sheet(&cask, 0).unwrap();
        assert_eq!(failed.primary, Primary::TryAgain);
        assert_eq!(
            failed.outcome.as_deref(),
            Some("Homebrew exited with 2. Its terminal shows why.")
        );
        cask.install = Some(install(InstallState::Succeeded, Some(0)));
        let done = sheet(&cask, 2).unwrap();
        assert_eq!(done.primary, Primary::Relaunch);
        assert!(
            done.will_happen
                .join(" ")
                .contains("Relaunching quits the app")
        );
        assert!(!done.will_happen.join(" ").contains("Hutan runs `"));

        let manual = sheet(&status(InstallMethod::Unknown), 0).unwrap();
        assert_eq!(manual.primary, Primary::Manual);
        assert_eq!(manual.manual_command.as_deref(), Some(CASK_COMMAND));
        assert_eq!(
            sheet(&status(InstallMethod::Formula), 0).unwrap().primary,
            Primary::UpdateNow
        );
    }

    #[test]
    fn a_hand_copied_bundle_is_updated_by_hutan_and_offers_update_now() {
        let s = status(InstallMethod::Bundle);
        let view = sheet(&s, 0).unwrap();
        assert_eq!(view.primary, Primary::UpdateNow);
        assert_eq!(view.manual_command, None, "nothing to run by hand");
        assert_eq!(view.progress, None);
        let text = view.will_happen.join(" ");
        assert!(text.contains("downloads Hutan 0.3.2 (36.7 MB)"), "{text}");
        assert!(
            text.contains("checksums of GitHub and of the Homebrew tap"),
            "{text}"
        );
        assert!(
            text.contains("moves /Applications/Hutan.app to the Trash"),
            "{text}"
        );
        assert!(
            text.contains("Nothing changes until you choose Quit and Install"),
            "{text}"
        );
        let busy = sheet(&s, 2).unwrap().will_happen.join(" ");
        assert!(
            busy.contains("2 sessions are running and keep running"),
            "{busy}"
        );
    }

    #[test]
    fn the_bundle_update_shows_download_verify_ready_and_then_quit_and_install() {
        let mut s = status(InstallMethod::Bundle);
        s.install = Some(bundle_install(InstallStage::Downloading));
        let item_now = item(&s).unwrap();
        assert_eq!(item_now.text, "Downloading 0.3.2 · 25%");
        assert_eq!(item_now.tone, ItemTone::Busy);
        let view = sheet(&s, 0).unwrap();
        assert_eq!(view.primary, Primary::Running);
        let progress = view.progress.unwrap();
        assert_eq!(
            progress.label,
            "Downloading Hutan 0.3.2 · 9.2 MB of 36.7 MB"
        );
        assert_eq!(progress.fraction, Some(0.25));
        assert_eq!(
            progress.steps.map(|(_, state)| state),
            [StepState::Current, StepState::Waiting, StepState::Waiting]
        );

        for stage in [
            InstallStage::Verifying,
            InstallStage::Extracting,
            InstallStage::Checking,
        ] {
            s.install = Some(bundle_install(stage));
            assert_eq!(item(&s).unwrap().text, "Checking 0.3.2…");
            let view = sheet(&s, 0).unwrap();
            assert_eq!(view.primary, Primary::Running);
            assert_eq!(
                view.progress.unwrap().steps.map(|(_, state)| state),
                [StepState::Done, StepState::Current, StepState::Waiting]
            );
        }

        s.install = Some(bundle_install(InstallStage::Ready));
        let ready = item(&s).unwrap();
        assert_eq!(ready.text, "Quit and Install 0.3.2");
        assert_eq!(ready.tone, ItemTone::Done);
        let view = sheet(&s, 1).unwrap();
        assert_eq!(view.primary, Primary::QuitAndInstall);
        let progress = view.progress.unwrap();
        assert_eq!(
            progress.steps.map(|(name, _)| name),
            ["Download", "Verify", "Ready"]
        );
        assert_eq!(progress.fraction, Some(1.0));
        let text = view.will_happen.join(" ");
        assert!(
            text.contains("Quit and Install closes this window"),
            "{text}"
        );
        assert!(text.contains("1 session is running"), "{text}");

        s.install = Some(bundle_install(InstallStage::Installing));
        assert_eq!(item(&s).unwrap().text, "Installing 0.3.2…");
        assert_eq!(sheet(&s, 0).unwrap().primary, Primary::Running);
    }

    #[test]
    fn a_bundle_update_that_stopped_says_why_and_a_folder_it_cannot_write_offers_the_command() {
        let mut s = status(InstallMethod::Bundle);
        let mut failed = bundle_install(InstallStage::Verifying);
        failed.state = InstallState::Failed;
        failed.message = Some("the download's SHA-256 is not the one GitHub lists".into());
        s.install = Some(failed.clone());
        let view = sheet(&s, 0).unwrap();
        assert_eq!(view.primary, Primary::TryAgain);
        assert_eq!(
            view.outcome.as_deref(),
            Some("The download's SHA-256 is not the one GitHub lists.")
        );
        assert_eq!(view.progress, None);
        assert_eq!(item(&s).unwrap().tone, ItemTone::Failed);

        failed.stage = Some(InstallStage::Ready);
        failed.manual_command = Some("sudo mv a b".into());
        s.install = Some(failed);
        let view = sheet(&s, 0).unwrap();
        assert_eq!(view.primary, Primary::Manual);
        assert_eq!(view.manual_command.as_deref(), Some("sudo mv a b"));
        assert!(
            view.will_happen
                .join(" ")
                .contains("this command installs it")
        );

        // Once it is in place the app that runs is the new one: no item.
        let mut done = bundle_install(InstallStage::Installed);
        done.state = InstallState::Succeeded;
        s.install = Some(done);
        assert_eq!(item(&s), None);
    }

    #[test]
    fn the_notes_come_through_and_a_status_without_a_release_has_no_sheet() {
        let mut s = status(InstallMethod::Cask);
        let view = sheet(&s, 0).unwrap();
        assert!(view.notes.starts_with("## Hutan 0.3.2"));
        assert!(!view.notes_truncated);
        s.available = None;
        assert_eq!(sheet(&s, 0), None);
    }

    #[test]
    fn a_check_the_user_asked_for_says_up_to_date_or_why_not() {
        let mut s = status(InstallMethod::Cask);
        assert_eq!(check_note(&s), None, "a release has the sheet");
        s.available = None;
        assert_eq!(check_note(&s).unwrap(), "Hutan 0.3.1 is up to date");
        s.last_check = Some(UpdateCheckInfo {
            at_ms: 1,
            outcome: CheckOutcome::Offline,
            message: Some("GitHub could not be reached".into()),
        });
        assert_eq!(
            check_note(&s).unwrap(),
            "Could not check for updates: GitHub could not be reached"
        );
        // A newer release that is not downloadable yet is neither "up to date" nor a failure.
        s.last_check = Some(UpdateCheckInfo {
            at_ms: 1,
            outcome: CheckOutcome::Checked,
            message: Some("Hutan 0.3.2 was just published; it can be installed in a minute".into()),
        });
        assert_eq!(
            check_note(&s).unwrap(),
            "Hutan 0.3.2 was just published; it can be installed in a minute"
        );
        assert!(checks_automatically(&s));
        s.check = UpdateCheck::Off;
        assert!(!checks_automatically(&s));
    }
}
