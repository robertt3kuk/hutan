//! The update of a `Hutan.app` that was copied by hand (UPDATES-2, ADR 0024 decision 3,
//! `docs/packaging.md` § Updates). Three steps, the first two without touching the old app:
//!
//! 1. **Prepare** (`update.install {method: bundle}`): `Hutan-<version>-arm64.zip` is downloaded
//!    to a folder of its own in the data folder (bounded, redirects followed), its SHA-256 must
//!    equal **both** the digest GitHub lists for the asset and the one in the tap's
//!    `Casks/hutan.rb` (read at the release's tag in the tap, else on its `main`, and for the
//!    same version), `ditto -x -k` unpacks it, and `codesign --verify --deep --strict` plus the
//!    bundle's identifier and version must pass. Any mismatch stops here, says why, and leaves
//!    everything but the staging folder as it was. The stage is reported (`update.install`
//!    events) so the window shows the progress.
//! 2. **Apply** (`update.install {method: bundle, apply: true, after_pid}`): once the desktop
//!    that asked has quit, the new bundle is copied next to the old one, the old one goes to
//!    the Trash, the copy is renamed into place and the new app is opened (`open -n`, the
//!    user's own app by design). A step that fails puts the old bundle back and reopens it.
//! 3. **Not writable**: a folder the daemon cannot write to is never attempted; the verified new
//!    bundle stays staged and the install says what to run by hand (`manual_command`).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hutan_gateway::GatewayError;
use hutan_protocol::control::ErrorBody;
use hutan_protocol::messages::Event;
use hutan_protocol::update::{
    InstallMethod, InstallStage, InstallState, UpdateAsset, UpdateInstall, UpdateInstallRequest,
    UpdateInstalling, UpdateRelease,
};
use hutan_terminal::procs::{self, ProcState};
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;
use tracing::{info, warn};

use super::install::writable;
use super::releases::REPOSITORY;
use super::{UpdateService, bad_request, invalid_state, quote};
use crate::files::trash::Trash;

/// Where GitHub's release downloads are.
pub const DOWNLOAD_BASE: &str = "https://github.com";
/// Where the tap's files are served raw.
pub const TAP_BASE: &str = "https://raw.githubusercontent.com";
/// The tap that carries the cask.
const TAP_REPOSITORY: &str = "robertt3kuk/homebrew-hutan";
/// The most an archive may be, by default.
pub const ARCHIVE_MAX_BYTES: u64 = 512 * 1024 * 1024;
/// The most of `Casks/hutan.rb` that is read.
const CASK_MAX: usize = 64 * 1024;
/// What the archive holds at its top.
const BUNDLE_NAME: &str = "Hutan.app";
/// The bundle identifier of Hutan (`assets/macos/Info.plist`).
const BUNDLE_ID: &str = "dev.hutan.Hutan";
/// The longest `ditto`, `codesign` or `open` may take.
const TOOL_TIMEOUT: Duration = Duration::from_secs(300);
/// How often the swap looks whether the desktop has quit.
const QUIT_POLL: Duration = Duration::from_millis(50);

/// The programs a bundle update runs; a test points them at fakes.
#[derive(Debug, Clone)]
pub struct Tools {
    /// `ditto`: unpacks the archive (`-x -k`) and copies the new bundle.
    pub ditto: PathBuf,
    /// `codesign`: `--verify --deep --strict`.
    pub codesign: PathBuf,
    /// `open`: `-n <bundle>` starts the new app.
    pub open: PathBuf,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            ditto: PathBuf::from("/usr/bin/ditto"),
            codesign: PathBuf::from("/usr/bin/codesign"),
            open: PathBuf::from("/usr/bin/open"),
        }
    }
}

/// A new bundle that passed every check and waits to be put in place.
#[derive(Debug, Clone)]
pub(super) struct Staged {
    /// `<staging>/extracted/Hutan.app`.
    pub bundle: PathBuf,
}

/// Why the preparation stopped: the step, and the words for the user.
struct Stop {
    stage: InstallStage,
    message: String,
}

fn stop(stage: InstallStage, message: impl Into<String>) -> Stop {
    Stop {
        stage,
        message: message.into(),
    }
}

/// What the tap's `Casks/hutan.rb` says about the release.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct CaskEntry {
    pub version: String,
    pub sha256: String,
}

/// The `version` and `sha256` lines of a cask (the first of each; `sha256 "…"` or
/// `sha256 arm: "…"`).
pub(super) fn parse_cask(text: &str) -> Result<CaskEntry, String> {
    let (mut version, mut sha256) = (None, None);
    for line in text.lines().map(str::trim) {
        if version.is_none()
            && let Some(rest) = line.strip_prefix("version ")
        {
            version = quoted(rest);
        } else if sha256.is_none()
            && let Some(rest) = line.strip_prefix("sha256 ")
        {
            sha256 = quoted(rest)
                .map(|hex| hex.to_ascii_lowercase())
                .filter(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }
    match (version, sha256) {
        (Some(version), Some(sha256)) => Ok(CaskEntry { version, sha256 }),
        (None, _) => Err("it has no version line".to_owned()),
        (_, None) => Err("it has no sha256 line with a SHA-256".to_owned()),
    }
}

/// The first `"…"` of `text`.
fn quoted(text: &str) -> Option<String> {
    let start = text.find('"')? + 1;
    let len = text[start..].find('"')?;
    Some(text[start..start + len].to_owned())
}

fn short(digest: &str) -> &str {
    digest.get(..12).unwrap_or(digest)
}

/// Runs `program args`; `Err` is what to tell the user (the first line it printed to stderr, or
/// how it ended).
async fn run(program: &Path, args: &[&OsStr]) -> Result<(), String> {
    let output = tokio::time::timeout(
        TOOL_TIMEOUT,
        tokio::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| format!("{} took longer than 5 minutes", program.display()))?
    .map_err(|error| format!("{} could not run: {error}", program.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map_or_else(|| output.status.to_string(), str::to_owned))
}

fn describe(error: &GatewayError) -> String {
    match error {
        GatewayError::Connect { .. } | GatewayError::Timeout | GatewayError::Stream { .. } => {
            format!("GitHub could not be reached ({error})")
        }
        other => other.to_string(),
    }
}

/// The command to run by hand when the daemon cannot write to the bundle's folder: the verified
/// new bundle replaces the old one, which goes to the Trash.
pub(super) fn manual_command(staged: &Path, target: &Path) -> String {
    let (staged, target) = (
        quote(&staged.to_string_lossy()),
        quote(&target.to_string_lossy()),
    );
    format!(
        "sudo mv {target} \"$HOME/.Trash/Hutan (old).app\" && sudo ditto {staged} {target} && open -n {target}"
    )
}

/// Puts the staged bundle where `target` is: copies it beside the old one, moves the old one
/// to `trash`, renames the copy into place. A failure after the old bundle went to the Trash
/// puts it back. Blocking.
pub(super) fn swap(
    target: &Path,
    staged: &Path,
    trash: &dyn Trash,
    ditto: &Path,
) -> Result<(), String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("{} has no folder", target.display()))?;
    let fresh = parent.join(format!(".Hutan-update-{}", std::process::id()));
    let discard = |path: &Path| {
        if let Err(error) = std::fs::remove_dir_all(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!(%error, path = %path.display(), "a half-copied bundle was not removed");
        }
    };
    discard(&fresh);
    let copied = std::process::Command::new(ditto)
        .arg(staged)
        .arg(&fresh)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("ditto could not run: {error}"))?;
    if !copied.status.success() {
        discard(&fresh);
        let stderr = String::from_utf8_lossy(&copied.stderr);
        return Err(format!(
            "the new Hutan.app could not be copied into {}: {}",
            parent.display(),
            stderr.lines().next().unwrap_or("ditto failed")
        ));
    }
    let trashed = match trash.trash(target) {
        Ok(trashed) => trashed,
        Err(error) => {
            discard(&fresh);
            return Err(format!(
                "the old Hutan.app could not be moved to the Trash: {}",
                ErrorBody::from(error).message
            ));
        }
    };
    if let Err(error) = std::fs::rename(&fresh, target) {
        discard(&fresh);
        return Err(match std::fs::rename(&trashed, target) {
            Ok(()) => format!(
                "the new Hutan.app could not be put in place ({error}); the old one is back"
            ),
            Err(restore) => format!(
                "the new Hutan.app could not be put in place ({error}) and the old one could not be moved back ({restore}); it is in the Trash at {}",
                trashed.display()
            ),
        });
    }
    Ok(())
}

/// Whether the process is gone (or only a zombie).
fn gone(pid: u32) -> bool {
    procs::inspect(pid).is_none_or(|process| process.state == ProcState::Zombie)
}

impl UpdateService {
    /// Where the archive and the unpacked bundle of the update in progress live.
    fn staging_dir(&self) -> PathBuf {
        self.state_file.with_file_name("update-staging")
    }

    /// Changes the install in progress and tells the clients. `None` when there is none.
    fn change_install(&self, change: impl FnOnce(&mut UpdateInstall)) -> Option<UpdateInstall> {
        let install = {
            let mut state = self.state.lock();
            let install = state
                .install
                .as_mut()
                .filter(|install| install.method == InstallMethod::Bundle)?;
            change(install);
            install.clone()
        };
        self.publish(&Event::UpdateInstallChanged {
            install: Box::new(install.clone()),
        });
        Some(install)
    }

    fn stage(&self, stage: InstallStage) {
        self.change_install(|install| install.stage = Some(stage));
    }

    fn fail_install(&self, stage: InstallStage, message: String, manual: Option<String>) {
        warn!(%message, ?stage, "the update of the bundle stopped");
        self.change_install(|install| {
            install.state = InstallState::Failed;
            install.stage = Some(stage);
            install.message = Some(message);
            install.manual_command = manual;
        });
    }

    async fn clear_staging(&self) {
        let dir = self.staging_dir();
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!(%error, dir = %dir.display(), "the update's staging folder was not removed");
        }
    }

    /// `update.install` for a bundle: starts the preparation, or with `apply` the swap.
    pub(super) async fn install_bundle(
        self: &Arc<Self>,
        req: UpdateInstallRequest,
    ) -> Result<UpdateInstalling, ErrorBody> {
        let _one = self.installing.lock().await;
        if req.apply {
            return self.apply(req.after_pid);
        }
        if req.after_pid.is_some() {
            return Err(bad_request("`after_pid` goes with `apply`"));
        }
        let release = {
            let mut state = self.state.lock();
            if state
                .install
                .as_ref()
                .is_some_and(|install| install.state == InstallState::Running)
            {
                return Err(invalid_state("an update is already running"));
            }
            let Some(release) = state.available.clone() else {
                return Err(invalid_state(
                    "no newer release is known; check for updates first",
                ));
            };
            state.staged = None;
            release
        };
        if self.detected.path.is_none() || self.downloader.is_none() || self.http.is_none() {
            return Err(invalid_state("this Hutan has no update source"));
        }
        let asset_name = archive_name(&release);
        let install = UpdateInstall {
            method: InstallMethod::Bundle,
            session_id: None,
            version: release.version.clone(),
            command: String::new(),
            state: InstallState::Running,
            exit_code: None,
            stage: Some(InstallStage::Downloading),
            downloaded: Some(0),
            total: release
                .assets
                .iter()
                .find(|asset| asset.name == asset_name)
                .map(|asset| asset.size)
                .filter(|size| *size > 0),
            message: None,
            manual_command: None,
        };
        self.state.lock().install = Some(install.clone());
        self.publish(&Event::UpdateInstallChanged {
            install: Box::new(install.clone()),
        });
        let service = self.clone();
        tokio::spawn(async move { service.prepare(release).await });
        Ok(UpdateInstalling {
            session: None,
            install,
        })
    }

    /// Download, verify, unpack, check; ends in `ready` or in a failed install.
    async fn prepare(self: Arc<Self>, release: UpdateRelease) {
        match self.prepare_steps(&release).await {
            Ok(staged) => self.ready(staged).await,
            Err(stop) => {
                self.clear_staging().await;
                self.fail_install(stop.stage, stop.message, None);
            }
        }
    }

    async fn prepare_steps(&self, release: &UpdateRelease) -> Result<Staged, Stop> {
        use InstallStage::{Checking, Downloading, Extracting, Verifying};
        let name = archive_name(release);
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == name)
            .ok_or_else(|| stop(Downloading, format!("the release has no {name}")))?;
        self.clear_staging().await;
        let work = self.staging_dir();
        tokio::fs::create_dir_all(&work).await.map_err(|error| {
            stop(
                Downloading,
                format!(
                    "the staging folder {} cannot be made: {error}",
                    work.display()
                ),
            )
        })?;
        let zip = work.join(&name);
        let url = format!(
            "{}/{}/releases/download/{}/{}",
            self.tuning.download_base.trim_end_matches('/'),
            REPOSITORY,
            release.tag,
            name
        );
        let (digest, _) = self.fetch_archive(&url, &zip, asset).await.map_err(|why| {
            stop(
                Downloading,
                format!("{name} could not be downloaded: {why}"),
            )
        })?;

        self.stage(Verifying);
        let listed = asset.sha256.as_deref().map(str::to_ascii_lowercase);
        let Some(listed) = listed else {
            return Err(stop(
                Verifying,
                format!("GitHub lists no SHA-256 for {name}, so the download cannot be checked"),
            ));
        };
        if digest != listed {
            return Err(stop(
                Verifying,
                format!(
                    "the download's SHA-256 ({}…) is not the one GitHub lists for {name} ({}…); nothing was installed",
                    short(&digest),
                    short(&listed)
                ),
            ));
        }
        let tap = self
            .tap_digest(release)
            .await
            .map_err(|why| stop(Verifying, why))?;
        if digest != tap {
            return Err(stop(
                Verifying,
                format!(
                    "the download's SHA-256 ({}…) is not the one in the tap's Casks/hutan.rb ({}…); nothing was installed",
                    short(&digest),
                    short(&tap)
                ),
            ));
        }

        self.stage(Extracting);
        let extracted = work.join("extracted");
        run(
            &self.tuning.tools.ditto,
            &[
                OsStr::new("-x"),
                OsStr::new("-k"),
                zip.as_os_str(),
                extracted.as_os_str(),
            ],
        )
        .await
        .map_err(|why| {
            stop(
                Extracting,
                format!("the archive could not be unpacked: {why}"),
            )
        })?;
        let bundle = extracted.join(BUNDLE_NAME);
        if !tokio::fs::metadata(bundle.join("Contents/MacOS"))
            .await
            .is_ok_and(|meta| meta.is_dir())
        {
            return Err(stop(
                Extracting,
                format!("the archive holds no {BUNDLE_NAME}"),
            ));
        }
        // The archive is not needed again.
        if let Err(error) = tokio::fs::remove_file(&zip).await {
            warn!(%error, "the downloaded archive was not removed");
        }

        self.stage(Checking);
        run(
            &self.tuning.tools.codesign,
            &[
                OsStr::new("--verify"),
                OsStr::new("--deep"),
                OsStr::new("--strict"),
                bundle.as_os_str(),
            ],
        )
        .await
        .map_err(|why| {
            stop(
                Checking,
                format!("macOS does not accept the new {BUNDLE_NAME}'s signature: {why}"),
            )
        })?;
        let version = release.version.clone();
        let info = bundle.join("Contents/Info.plist");
        tokio::task::spawn_blocking(move || check_identity(&info, &version))
            .await
            .map_err(|error| {
                stop(
                    Checking,
                    format!("the bundle could not be inspected: {error}"),
                )
            })?
            .map_err(|why| stop(Checking, why))?;
        Ok(Staged { bundle })
    }

    /// The archive is downloaded, checked and unpacked: waits for the user, unless the folder of
    /// the old bundle cannot be written (then the manual command is the way).
    async fn ready(&self, staged: Staged) {
        let target = self.detected.path.clone().unwrap_or_default();
        let folder_ok = target.parent().is_some_and(writable);
        let manual = manual_command(&staged.bundle, &target);
        if folder_ok {
            self.state.lock().staged = Some(staged);
            self.stage(InstallStage::Ready);
            info!("the new Hutan.app is verified and waits to be installed");
        } else {
            self.fail_install(
                InstallStage::Ready,
                format!(
                    "the new Hutan.app is verified, but Hutan cannot write to {}; run the command to install it yourself",
                    target.parent().unwrap_or(&target).display()
                ),
                Some(manual),
            );
        }
    }

    /// `apply`: the swap, once the desktop that asked has quit.
    fn apply(self: &Arc<Self>, after_pid: Option<u32>) -> Result<UpdateInstalling, ErrorBody> {
        if after_pid == Some(std::process::id()) {
            return Err(bad_request("`after_pid` is the daemon itself"));
        }
        let staged = {
            let state = self.state.lock();
            match (&state.install, &state.staged) {
                (Some(install), Some(staged))
                    if install.method == InstallMethod::Bundle
                        && install.state == InstallState::Running
                        && install.stage == Some(InstallStage::Ready) =>
                {
                    staged.clone()
                }
                _ => {
                    return Err(invalid_state(
                        "no update is ready to install; ask for the update first and wait until it is ready",
                    ));
                }
            }
        };
        let Some(target) = self.detected.path.clone() else {
            return Err(invalid_state(
                "this Hutan does not know where it is installed",
            ));
        };
        if !target.parent().is_some_and(writable) {
            let message = format!(
                "Hutan cannot write to {}; run the command to install the update yourself",
                target.parent().unwrap_or(&target).display()
            );
            self.fail_install(
                InstallStage::Ready,
                message.clone(),
                Some(manual_command(&staged.bundle, &target)),
            );
            return Err(invalid_state(message));
        }
        let Some(install) = self.change_install(|install| {
            install.stage = Some(InstallStage::Installing);
        }) else {
            return Err(invalid_state("no update is ready to install"));
        };
        let service = self.clone();
        tokio::spawn(async move { service.finish(staged, target, after_pid).await });
        Ok(UpdateInstalling {
            session: None,
            install,
        })
    }

    /// Waits for the desktop to quit, swaps, opens the app.
    async fn finish(self: Arc<Self>, staged: Staged, target: PathBuf, after_pid: Option<u32>) {
        if let Some(pid) = after_pid
            && !self.wait_for_exit(pid).await
        {
            self.fail_install(
                InstallStage::Installing,
                "Hutan did not quit in time, so nothing was changed".to_owned(),
                None,
            );
            return;
        }
        let (trash, ditto) = (self.tuning.trash.clone(), self.tuning.tools.ditto.clone());
        let (from, to) = (staged.bundle.clone(), target.clone());
        let swapped = tokio::task::spawn_blocking(move || swap(&to, &from, trash.as_ref(), &ditto))
            .await
            .unwrap_or_else(|error| Err(format!("the swap did not finish: {error}")));
        match swapped {
            Ok(()) => {
                info!(bundle = %target.display(), "the new Hutan.app is in place");
                {
                    let mut state = self.state.lock();
                    state.available = None;
                    state.announced = None;
                    state.staged = None;
                    self.persist(&state);
                }
                self.clear_staging().await;
                // The window is gone only when it was asked to quit; opening a second one
                // beside a running app is never what the user wants.
                let opened = match after_pid {
                    Some(_) => self.open_app(&target).await,
                    None => Ok(()),
                };
                self.change_install(|install| {
                    install.state = InstallState::Succeeded;
                    install.stage = Some(InstallStage::Installed);
                    install.message = opened.err().map(|why| {
                        format!(
                            "Hutan {} is installed but could not be opened: {why}",
                            install.version
                        )
                    });
                });
            }
            Err(message) => {
                let manual = manual_command(&staged.bundle, &target);
                self.fail_install(InstallStage::Installing, message, Some(manual));
                // The old app is where it was: bring the window back.
                if after_pid.is_some()
                    && let Err(why) = self.open_app(&target).await
                {
                    warn!(%why, "the old Hutan.app was not reopened");
                }
            }
        }
    }

    async fn open_app(&self, bundle: &Path) -> Result<(), String> {
        run(
            &self.tuning.tools.open,
            &[OsStr::new("-n"), bundle.as_os_str()],
        )
        .await
    }

    /// Whether `pid` exited within the quit timeout.
    async fn wait_for_exit(&self, pid: u32) -> bool {
        let deadline = Instant::now() + self.tuning.quit_timeout;
        loop {
            if gone(pid) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(QUIT_POLL).await;
        }
    }

    /// Downloads `url` to `dest`, bounded; returns the SHA-256 (lowercase hex) and the size.
    async fn fetch_archive(
        &self,
        url: &str,
        dest: &Path,
        asset: &UpdateAsset,
    ) -> Result<(String, u64), String> {
        let Some(http) = &self.downloader else {
            return Err("the update download could not set up its HTTP client".to_owned());
        };
        let limit = self.tuning.max_archive;
        if asset.size > limit {
            return Err(format!(
                "it is {} bytes, more than the {limit} allowed",
                asset.size
            ));
        }
        let agent = format!("Hutan/{}", self.running);
        let all = async {
            let mut download = http
                .get_stream(url, &agent, "application/octet-stream")
                .await
                .map_err(|error| describe(&error))?;
            if download.status != 200 {
                return Err(format!("GitHub answered HTTP {}", download.status));
            }
            let total = download
                .content_length
                .or_else(|| (asset.size > 0).then_some(asset.size));
            if total.is_some_and(|total| total > limit) {
                return Err(format!("it is more than the {limit} bytes allowed"));
            }
            self.change_install(|install| install.total = total);
            let mut file = tokio::fs::File::create(dest)
                .await
                .map_err(|error| format!("{} cannot be written: {error}", dest.display()))?;
            let mut hasher = Sha256::new();
            let (mut got, mut announced) = (0_u64, 0_u64);
            let step = (total.unwrap_or(limit) / 100).max(1);
            while let Some(chunk) = download.chunk().await.map_err(|error| describe(&error))? {
                got += chunk.len() as u64;
                if got > limit {
                    return Err(format!("it is more than the {limit} bytes allowed"));
                }
                hasher.update(&chunk);
                file.write_all(&chunk)
                    .await
                    .map_err(|error| format!("{} cannot be written: {error}", dest.display()))?;
                if got - announced >= step {
                    announced = got;
                    self.change_install(|install| install.downloaded = Some(got));
                }
            }
            file.flush()
                .await
                .map_err(|error| format!("{} cannot be written: {error}", dest.display()))?;
            self.change_install(|install| install.downloaded = Some(got));
            if asset.size > 0 && got != asset.size {
                return Err(format!(
                    "{got} bytes arrived, but GitHub lists {} for it",
                    asset.size
                ));
            }
            Ok((format!("{:x}", hasher.finalize()), got))
        };
        tokio::time::timeout(self.tuning.download_timeout, all)
            .await
            .map_err(|_| "it took too long".to_owned())?
    }

    /// The SHA-256 the tap's `Casks/hutan.rb` lists for this release: read at the release's tag
    /// of the tap, else on its `main`; it must be the cask of the same version.
    async fn tap_digest(&self, release: &UpdateRelease) -> Result<String, String> {
        let Some(http) = &self.http else {
            return Err("the tap cannot be read: no HTTP client".to_owned());
        };
        let agent = format!("Hutan/{}", self.running);
        for reference in [release.tag.as_str(), "main"] {
            let url = format!(
                "{}/{TAP_REPOSITORY}/{reference}/Casks/hutan.rb",
                self.tuning.tap_base.trim_end_matches('/')
            );
            let answer = http
                .get_bounded(&url, &agent, "text/plain", CASK_MAX)
                .await
                .map_err(|error| {
                    format!("the tap's Casks/hutan.rb could not be read ({}), so the download cannot be checked against it", describe(&error))
                })?;
            match answer.status {
                200 => {
                    let cask = String::from_utf8_lossy(&answer.body);
                    let entry = parse_cask(&cask).map_err(|why| {
                        format!("the tap's Casks/hutan.rb at {reference} cannot be used: {why}")
                    })?;
                    if entry.version != release.version {
                        return Err(format!(
                            "the tap's Casks/hutan.rb at {reference} is for Hutan {}, not {}",
                            entry.version, release.version
                        ));
                    }
                    return Ok(entry.sha256);
                }
                404 => {}
                status => {
                    return Err(format!(
                        "the tap answered HTTP {status} for Casks/hutan.rb, so the download cannot be checked against it"
                    ));
                }
            }
        }
        Err(format!(
            "the tap has no Casks/hutan.rb for {} (neither at its tag nor on main), so the download cannot be checked against it",
            release.tag
        ))
    }
}

fn archive_name(release: &UpdateRelease) -> String {
    format!("Hutan-{}-arm64.zip", release.version)
}

/// The unpacked bundle is Hutan, at the version the release says.
fn check_identity(info: &Path, version: &str) -> Result<(), String> {
    let value = plist::Value::from_file(info)
        .map_err(|error| format!("the new {BUNDLE_NAME} has no readable Info.plist: {error}"))?;
    let text = |key: &str| {
        value
            .as_dictionary()
            .and_then(|dictionary| dictionary.get(key))
            .and_then(plist::Value::as_string)
            .map(str::to_owned)
    };
    match text("CFBundleIdentifier").as_deref() {
        Some(BUNDLE_ID) => {}
        other => {
            return Err(format!(
                "the archive's app is {}, not Hutan ({BUNDLE_ID})",
                other.unwrap_or("unnamed")
            ));
        }
    }
    match text("CFBundleShortVersionString").as_deref() {
        Some(found) if found == version => Ok(()),
        other => Err(format!(
            "the archive holds Hutan {}, not the {version} the release says",
            other.unwrap_or("of no version")
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_cask_gives_its_version_and_digest() {
        let digest = "ab".repeat(32);
        let cask = format!(
            "cask \"hutan\" do\n  version \"0.3.2\"\n  sha256 \"{digest}\"\n\n  url \"https://example.invalid\"\nend\n"
        );
        assert_eq!(
            parse_cask(&cask),
            Ok(CaskEntry {
                version: "0.3.2".into(),
                sha256: digest.clone()
            })
        );
        let arm = format!(
            "version \"1.0\"\nsha256 arm: \"{}\", intel: \"x\"\n",
            digest.to_uppercase()
        );
        assert_eq!(parse_cask(&arm).unwrap().sha256, digest);
        assert!(parse_cask("version \"1.0\"\nsha256 \"nothex\"\n").is_err());
        assert!(parse_cask("sha256 \"abc\"\n").is_err());
        assert!(parse_cask("").is_err());
    }

    #[test]
    fn the_manual_command_quotes_the_paths() {
        let command = manual_command(
            Path::new("/data/update-staging/extracted/Hutan.app"),
            Path::new("/Apps/O'Brien/Hutan.app"),
        );
        assert!(
            command.contains("'/Apps/O'\\''Brien/Hutan.app'"),
            "{command}"
        );
        assert!(command.starts_with("sudo mv "), "{command}");
    }
}
