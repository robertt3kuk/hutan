//! Updates (protocol 0.60, capability `update`, ADR 0024): the DTOs of `update.status`,
//! `update.check`, `update.install` and the events `update.available` and `update.install`
//! (`docs/protocol.md` § Updates, `docs/packaging.md` § Updates).
//!
//! The daemon checks, never a client: once every 24 hours (unless `updates.check = "off"` in
//! `daemon.toml`) and when a client asks. It keeps the newest release of the user's channel that
//! is above the running version and carries the app's archive, and says how this Hutan was
//! installed. Nothing updates by itself: `update.install` is the user's alone.

use hutan_core::SessionId;
use serde::{Deserialize, Serialize};

use crate::messages::SessionInfo;

/// Longest [`UpdateRelease::notes`], in bytes (cut at a character boundary).
pub const NOTES_MAX_BYTES: usize = 32 * 1024;
/// Most assets of a release that are kept.
pub const ASSETS_MAX: usize = 16;
/// The Homebrew command that updates the app (cask): what `update.install` runs for
/// [`InstallMethod::Cask`].
pub const CASK_COMMAND: &str = "brew update && brew upgrade --cask robertt3kuk/hutan/hutan";
/// What `update.install` runs for [`InstallMethod::Formula`] (CLI and daemon only).
pub const FORMULA_COMMAND: &str = "brew update && brew upgrade robertt3kuk/hutan/hutan";

/// `updates.check` of `daemon.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateCheck {
    /// At most once every 24 hours, and on request.
    #[default]
    Auto,
    /// Only on request (`update.check`).
    Off,
}

/// `updates.channel` of `daemon.toml`: which releases count as newer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    /// Every release, pre-releases included (the default while every release is a pre-release).
    #[default]
    Prerelease,
    /// Releases that are not marked pre-release.
    Stable,
}

/// How this Hutan was installed, as the daemon found out from its own executable and Homebrew's
/// Caskroom and Cellar (read-only: `brew` is never run to find out).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallMethod {
    /// `Hutan.app` installed by `brew install --cask robertt3kuk/hutan/hutan`.
    Cask,
    /// `hutan` and `hutand` installed by `brew install robertt3kuk/hutan/hutan`.
    Formula,
    /// A `Hutan.app` copied by hand into a folder the user can write.
    Bundle,
    /// A development build, an unwritable location, or anything else: the release is shown with
    /// the manual command, nothing automatic is offered.
    Unknown,
}

/// One file of a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateAsset {
    pub name: String,
    pub size: u64,
    /// The SHA-256 GitHub computed for the asset (`digest: "sha256:…"`), lowercase hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// A release newer than the running Hutan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRelease {
    /// `0.3.2` (the tag without its `v`).
    pub version: String,
    pub tag: String,
    pub prerelease: bool,
    /// When it was published, as GitHub writes it (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    /// The release's page.
    pub notes_url: String,
    /// The release body (Markdown), at most [`NOTES_MAX_BYTES`].
    pub notes: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub notes_truncated: bool,
    pub assets: Vec<UpdateAsset>,
}

/// How the last check ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    /// The answer was read (there may or may not be something newer).
    Checked,
    /// No network or no answer in time.
    Offline,
    /// GitHub's rate limit (the unauthenticated API allows 60 requests an hour per address).
    RateLimited,
    /// Any other failure: a status, an answer that is not a release list, one that is too big.
    Failed,
}

/// The last check, whether it found something or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCheckInfo {
    pub at_ms: u64,
    pub outcome: CheckOutcome,
    /// What went wrong, for the outcomes other than `checked`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// How an `update.install` run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallState {
    /// The terminal session is running Homebrew.
    Running,
    /// Homebrew exited 0: the new version is installed; a relaunch starts it.
    Succeeded,
    /// Homebrew exited non-zero, was stopped, or could not start; the session shows why.
    Failed,
}

/// Where the update of a hand-copied bundle ([`InstallMethod::Bundle`]) stands (UPDATES-2). The
/// daemon downloads the release's archive, checks it, and only then waits for the user: the swap
/// happens after the desktop has quit and was asked for (`update.install` with `apply`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallStage {
    /// `Hutan-<version>-arm64.zip` is being downloaded (`downloaded` of `total` bytes).
    Downloading,
    /// Its SHA-256 is compared with the digest GitHub lists for the asset and with the one in
    /// the tap's `Casks/hutan.rb`.
    Verifying,
    /// `ditto -x -k` unpacks it.
    Extracting,
    /// `codesign --verify --deep --strict` and the bundle's identity and version.
    Checking,
    /// Downloaded, verified and unpacked: nothing changes until the user asks to install, which
    /// is after the desktop quits.
    Ready,
    /// The desktop was asked to quit; the daemon moves the old bundle to the Trash, puts the new
    /// one in its place and opens it.
    Installing,
    /// The new bundle is in place and was opened.
    Installed,
}

/// The install the daemon started, and how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateInstall {
    pub method: InstallMethod,
    /// The visible terminal session running the command (cask and formula; a hand-copied
    /// bundle runs no terminal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// The version being installed.
    pub version: String,
    /// What runs (cask and formula); empty for a bundle, which the daemon replaces itself.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    pub state: InstallState,
    /// The session's exit code, once it ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Bundle only: the step the update is at (also when it failed: the step that refused).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<InstallStage>,
    /// Bundle only: bytes of the archive received so far.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloaded: Option<u64>,
    /// Bundle only: the archive's size, as the release lists it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// Bundle only: why the update stopped, in words for the user (a digest that does not
    /// match, a signature that does not verify, a folder that cannot be written). The old app is
    /// untouched unless the message says otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Bundle only: what to run by hand when the daemon cannot replace the bundle itself (its
    /// folder is not writable). The verified new bundle is kept for it until the next update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_command: Option<String>,
}

/// Result of `update.status` and `update.check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateStatus {
    /// The version of the running daemon.
    pub version: String,
    pub check: UpdateCheck,
    pub channel: UpdateChannel,
    pub method: InstallMethod,
    /// The app bundle (cask, bundle) or the executable (formula) the method was read from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_path: Option<String>,
    /// What `update.install` runs for this method; absent for `bundle` and `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The newest release above `version` in the channel, whether or not it was skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available: Option<UpdateRelease>,
    /// `updates.skip_version` of `daemon.toml`: the version the user chose to skip; clients do
    /// not announce it unprompted (a check the user asks for still shows it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<UpdateCheckInfo>,
    /// The install in progress or the last one this daemon ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<UpdateInstall>,
}

/// `update.install`: updates Hutan the way `method` says. `method` must be what `update.status`
/// reported (`bad_request` for `unknown`; `invalid_state` when it differs, when nothing newer is
/// known, or when an install is already running). Answered with [`UpdateInstalling`].
/// User-only: an agent-originated connection gets `forbidden`, always.
///
/// - `cask`, `formula`: runs Homebrew in a visible terminal session.
/// - `bundle` (UPDATES-2): starts the download, which the daemon verifies and unpacks while it
///   sends `update.install` events with the [`InstallStage`]; nothing in place changes. When the
///   stage is `ready`, the same request with `apply` replaces the bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateInstallRequest {
    pub method: InstallMethod,
    /// `bundle` only: install the prepared update (`invalid_state` unless the stage is `ready`).
    /// The daemon waits until the process `after_pid` has exited (the desktop that asked, which
    /// quits right after the answer), moves the old bundle to the Trash, puts the new one in
    /// place and opens it. A folder the daemon cannot write to is answered with the install's
    /// `manual_command` and nothing is changed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub apply: bool,
    /// `bundle` with `apply`: the process to wait for before replacing the bundle; absent: do it
    /// now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_pid: Option<u32>,
}

/// Result of `update.install`: for a Homebrew install, the terminal session that runs it in the
/// Home workspace and what it runs; for a bundle, no session, only the install and its stage.
/// When it ends the daemon sends `update.install` with the outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateInstalling {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionInfo>,
    pub install: UpdateInstall,
}
