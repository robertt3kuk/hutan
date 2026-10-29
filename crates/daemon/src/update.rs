//! Updates (protocol 0.60, capability `update`, ADR 0024; `docs/packaging.md` § Updates): the
//! daemon's check for a newer Hutan, how this Hutan was installed, and the Homebrew update it
//! runs for the user.
//!
//! - **The check** is one unauthenticated `GET <api>/repos/robertt3kuk/hutan/releases` with
//!   `User-Agent: Hutan/<version>`, nothing else sent. It runs at most once every 24 hours while
//!   `updates.check = "auto"` (the first one [`UpdateTuning::startup_delay`] after the daemon
//!   started, and only in an installed Hutan: a development build never checks by itself) and
//!   whenever a client asks (`update.check`, whatever the setting). A check that cannot finish
//!   (offline, rate limited, an answer that is no release list) is silent: it is recorded in
//!   `last_check` and retried after [`UpdateTuning::retry`]; only a check that read an answer
//!   counts for the 24 hours, and the time of the last one survives a restart (`update.json`).
//! - **What is newer** is decided by [`releases`]: the highest semver above the running version,
//!   in the channel, with `Hutan-<version>-arm64.zip` attached.
//! - **How Hutan was installed** is read once, at start, by [`install::detect`].
//! - **`update.install`** (user-only) starts a visible terminal session that runs Homebrew for a
//!   cask or formula install and reports how it ended with `update.install`. Nothing ever
//!   updates by itself, and nothing here runs `brew` except that session.
//! - **A bundle copied by hand** ([`bundle`], UPDATES-2) is updated by the daemon itself:
//!   `update.install` downloads `Hutan-<version>-arm64.zip`, checks its SHA-256 against GitHub's
//!   digest *and* the tap's `Casks/hutan.rb`, unpacks it with `ditto` and verifies it with
//!   `codesign`; once the user asks (`apply`) and the desktop has quit, the old bundle goes to
//!   the Trash, the new one takes its place and is opened.

mod bundle;
mod install;
pub mod releases;

pub use self::bundle::Tools;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hutan_core::SessionId;
use hutan_gateway::{GatewayError, Http, HttpConfig};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{
    Event, SessionCreate, SessionListFilter, SessionSpec, TerminalSpec,
};
use hutan_protocol::update::{
    CASK_COMMAND, CheckOutcome, FORMULA_COMMAND, InstallMethod, InstallState, UpdateCheck,
    UpdateCheckInfo, UpdateInstall, UpdateInstallRequest, UpdateInstalling, UpdateRelease,
    UpdateStatus,
};
use parking_lot::Mutex;
use semver::Version;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

pub use self::install::{Detected, Layout, default_prefixes, detect};
use crate::bus::EventBus;
use crate::config::UpdatesSettings;
use crate::files::trash::{SystemTrash, Trash};
use crate::registry::{RegistryHandle, now_ms};

/// The file next to the database that remembers when the last check read an answer and what it
/// found, so a restart neither asks again at once nor forgets the release.
pub const STATE_FILE: &str = "update.json";
/// GitHub's API.
pub const GITHUB_API: &str = "https://api.github.com";
/// The environment variable that points a `hutand` at another releases API (a mock server for
/// screenshots and demos); it also lets a development build check by itself.
pub const API_ENV: &str = "HUTAN_UPDATE_API";
/// Like [`API_ENV`], where a release's archive is downloaded from (a mock of GitHub's release
/// downloads).
pub const DOWNLOADS_ENV: &str = "HUTAN_UPDATE_DOWNLOADS";
/// Like [`API_ENV`], where the tap's `Casks/hutan.rb` is read from.
pub const TAP_ENV: &str = "HUTAN_UPDATE_TAP";
/// The most of the release list that is read.
const BODY_MAX: usize = 2 * 1024 * 1024;
/// How often a running install session is looked at to see whether it ended.
const WATCH_INTERVAL: Duration = Duration::from_millis(500);
/// The timer re-reads the clock at least this often (a sleeping Mac stops monotonic time).
const MAX_SLEEP: Duration = Duration::from_secs(3600);

/// What a test (or `hutand`'s environment) changes about the update service.
#[derive(Debug, Clone)]
pub struct UpdateTuning {
    /// Where the releases come from; `None`: this daemon has no source, so it never checks and
    /// a check on request says so. `hutand` sets GitHub ([`UpdateTuning::github`]); a test sets
    /// a mock server, so no test ever contacts GitHub.
    pub api_base: Option<String>,
    /// Check by itself even when this is not an installed Hutan (a source that was set by hand).
    pub auto_anywhere: bool,
    /// The running version; defaults to this build's.
    pub version: String,
    /// The executable the install method is read from; defaults to this process's.
    pub exe: Option<PathBuf>,
    /// Where Homebrew's `Caskroom` and `Cellar` are looked for.
    pub brew_prefixes: Vec<PathBuf>,
    /// Between two checks that read an answer (24 hours).
    pub interval: Duration,
    /// After a check that could not finish (one hour).
    pub retry: Duration,
    /// After a check that found a release whose archive cannot be downloaded yet (five
    /// minutes): GitHub serves a new release's files some time after it lists them.
    pub unready_retry: Duration,
    /// Before the first automatic check of a daemon that has not checked recently (30 seconds).
    pub startup_delay: Duration,
    /// The whole request.
    pub request_timeout: Duration,
    /// Where a release's files are downloaded from (`<base>/robertt3kuk/hutan/releases/download/
    /// <tag>/<name>`): GitHub, or a mock server.
    pub download_base: String,
    /// Where the tap's files are read from (`<base>/robertt3kuk/homebrew-hutan/<ref>/Casks/
    /// hutan.rb`): `raw.githubusercontent.com`, or a mock server.
    pub tap_base: String,
    /// The most an archive may be, in bytes; a bigger download is refused (512 MiB).
    pub max_archive: u64,
    /// The longest silence while an archive downloads (30 seconds).
    pub download_idle: Duration,
    /// The longest a whole archive may take to download (15 minutes).
    pub download_timeout: Duration,
    /// How long the swap waits for the desktop that asked for it to quit (60 seconds).
    pub quit_timeout: Duration,
    /// The programs a bundle update runs (`ditto`, `codesign`, `open`).
    pub tools: Tools,
    /// Where the old bundle goes: the user's Trash; a test's folder.
    pub trash: Arc<dyn Trash>,
}

impl Default for UpdateTuning {
    fn default() -> Self {
        Self {
            api_base: None,
            auto_anywhere: false,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            exe: None,
            brew_prefixes: default_prefixes(std::env::var("HOMEBREW_PREFIX").ok().as_deref()),
            interval: Duration::from_secs(24 * 3600),
            retry: Duration::from_secs(3600),
            unready_retry: Duration::from_secs(300),
            startup_delay: Duration::from_secs(30),
            request_timeout: Duration::from_secs(15),
            download_base: bundle::DOWNLOAD_BASE.to_owned(),
            tap_base: bundle::TAP_BASE.to_owned(),
            max_archive: bundle::ARCHIVE_MAX_BYTES,
            download_idle: Duration::from_secs(30),
            download_timeout: Duration::from_secs(15 * 60),
            quit_timeout: Duration::from_secs(60),
            tools: Tools::default(),
            trash: Arc::new(SystemTrash),
        }
    }
}

impl UpdateTuning {
    /// What `hutand` runs with: GitHub, or the API `HUTAN_UPDATE_API` names (and, for what a
    /// mock server of a release also serves, `HUTAN_UPDATE_DOWNLOADS` and `HUTAN_UPDATE_TAP`).
    pub fn github() -> Self {
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let over = env(API_ENV);
        let default = Self::default();
        // A mock of the releases API also answers the question whether a release's archive can
        // be downloaded yet, unless another server is named for the downloads: a source set by
        // hand never leads to GitHub's own download URLs.
        let download_base = env(DOWNLOADS_ENV)
            .or_else(|| over.clone())
            .unwrap_or(default.download_base.clone());
        Self {
            auto_anywhere: over.is_some(),
            api_base: Some(over.unwrap_or_else(|| GITHUB_API.to_owned())),
            download_base,
            tap_base: env(TAP_ENV).unwrap_or(default.tap_base.clone()),
            ..default
        }
    }
}

/// What the service is made from.
#[derive(Debug)]
pub struct UpdateInputs {
    pub settings: UpdatesSettings,
    pub tuning: UpdateTuning,
    /// `update.json`.
    pub state_file: PathBuf,
    pub bus: Arc<EventBus>,
    pub registry: RegistryHandle,
}

/// `update.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    last_checked_ms: Option<u64>,
    #[serde(default)]
    available: Option<UpdateRelease>,
    /// When a check last found a release whose archive could not be downloaded yet; cleared by
    /// the next check that finds it can (or finds nothing).
    #[serde(default)]
    unready_ms: Option<u64>,
}

#[derive(Debug, Default)]
struct State {
    available: Option<UpdateRelease>,
    /// The version `update.available` was sent for.
    announced: Option<String>,
    last_check: Option<UpdateCheckInfo>,
    /// When a check last read an answer (persisted).
    last_ok_ms: Option<u64>,
    /// When a check last failed, while it is later than `last_ok_ms`.
    last_failed_ms: Option<u64>,
    /// When the last check found a release it did not offer, because its archive was not
    /// downloadable yet (persisted).
    unready_ms: Option<u64>,
    install: Option<UpdateInstall>,
    /// The verified new bundle a hand-copied install waits to put in place.
    staged: Option<bundle::Staged>,
}

/// The update service of one daemon.
#[derive(Debug)]
pub struct UpdateService {
    bus: Arc<EventBus>,
    registry: RegistryHandle,
    tuning: UpdateTuning,
    running: Version,
    http: Option<Http>,
    /// The client that downloads archives (follows redirects, no whole-request limit).
    downloader: Option<Http>,
    detected: Detected,
    state_file: PathBuf,
    settings: Mutex<UpdatesSettings>,
    state: Mutex<State>,
    /// One request to GitHub at a time.
    checking: tokio::sync::Mutex<()>,
    /// One `update.install` at a time.
    installing: tokio::sync::Mutex<()>,
    /// The settings changed: the timer looks again.
    changed: Notify,
}

fn bad_request(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::BadRequest, message)
}

fn invalid_state(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::InvalidState, message)
}

impl UpdateService {
    pub fn new(inputs: UpdateInputs) -> Arc<Self> {
        let UpdateInputs {
            settings,
            tuning,
            state_file,
            bus,
            registry,
        } = inputs;
        let running = Version::parse(&tuning.version).unwrap_or_else(|error| {
            warn!(version = %tuning.version, %error, "the running version is not semver; no update is newer");
            Version::new(u64::MAX, 0, 0)
        });
        let exe = tuning
            .exe
            .clone()
            .or_else(|| std::env::current_exe().ok())
            .unwrap_or_default();
        let detected = detect(&Layout {
            exe,
            prefixes: tuning.brew_prefixes.clone(),
        });
        let http = tuning.api_base.as_ref().and_then(|base| {
            let config = HttpConfig {
                request_timeout: tuning.request_timeout,
                connect_retries: 0,
                // A source set by hand (a mock on loopback) is never reached through a proxy.
                no_proxy: !base.starts_with("https://"),
                ..HttpConfig::default()
            };
            Http::new(config)
                .map_err(|error| warn!(%error, "the update check cannot set up its HTTP client"))
                .ok()
        });
        let downloader = tuning.api_base.as_ref().and_then(|_| {
            let config = HttpConfig {
                read_timeout: tuning.download_idle,
                connect_retries: 0,
                no_proxy: !tuning.download_base.starts_with("https://"),
                ..HttpConfig::default()
            };
            Http::new(config)
                .map_err(|error| warn!(%error, "the update download cannot set up its HTTP client"))
                .ok()
        });
        let persisted = load(&state_file);
        let available = persisted
            .available
            .filter(|release| fits(release, &running, &settings));
        let state = State {
            announced: available.as_ref().map(|r| r.version.clone()),
            available,
            last_ok_ms: persisted.last_checked_ms,
            unready_ms: persisted.unready_ms,
            ..State::default()
        };
        Arc::new(Self {
            bus,
            registry,
            tuning,
            running,
            http,
            downloader,
            detected,
            state_file,
            settings: Mutex::new(settings),
            state: Mutex::new(state),
            checking: tokio::sync::Mutex::new(()),
            installing: tokio::sync::Mutex::new(()),
            changed: Notify::new(),
        })
    }

    /// Starts the timer that checks by itself; it ends with `shutdown`.
    pub fn start(self: &Arc<Self>, shutdown: CancellationToken) {
        let service = self.clone();
        tokio::spawn(async move { service.timer(shutdown).await });
    }

    /// `[updates]` as the service uses it now.
    pub fn settings(&self) -> UpdatesSettings {
        self.settings.lock().clone()
    }

    /// A change of `[updates]` from `daemon.settings.set`: takes effect at once. Another channel
    /// drops a release it no longer allows and has the timer look again soon.
    pub fn apply_settings(&self, settings: UpdatesSettings) {
        let channel_changed = self.settings.lock().channel != settings.channel;
        *self.settings.lock() = settings.clone();
        if channel_changed {
            let mut state = self.state.lock();
            if state
                .available
                .as_ref()
                .is_some_and(|release| !fits(release, &self.running, &settings))
            {
                state.available = None;
                state.announced = None;
            }
            state.last_ok_ms = None;
            self.persist(&state);
        }
        self.changed.notify_one();
    }

    /// `update.status`: what is known, without contacting anything.
    pub fn status(&self) -> UpdateStatus {
        let settings = self.settings.lock().clone();
        let state = self.state.lock();
        let command = match self.detected.method {
            InstallMethod::Cask => Some(CASK_COMMAND.to_owned()),
            InstallMethod::Formula => Some(FORMULA_COMMAND.to_owned()),
            InstallMethod::Bundle | InstallMethod::Unknown => None,
        };
        UpdateStatus {
            version: self.running.to_string(),
            check: settings.check,
            channel: settings.channel,
            method: self.detected.method,
            install_path: self
                .detected
                .path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            command,
            available: state.available.clone(),
            skipped: Some(settings.skip_version).filter(|v| !v.is_empty()),
            last_check: state.last_check.clone(),
            install: state.install.clone(),
        }
    }

    /// `update.check`: asks the releases now, whatever `updates.check` says. A check that could
    /// not finish is in `last_check` of the answer, not an error.
    pub async fn check(&self) -> UpdateStatus {
        self.run_check().await;
        self.status()
    }

    // The timer -----------------------------------------------------------------------------

    async fn timer(self: Arc<Self>, shutdown: CancellationToken) {
        let started = Instant::now();
        let auto = self.tuning.api_base.is_some()
            && (self.tuning.auto_anywhere || self.detected.method != InstallMethod::Unknown);
        loop {
            let wait = self.due_in(auto, now_ms(), started.elapsed());
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = self.changed.notified() => continue,
                () = async {
                    match wait {
                        Some(wait) => tokio::time::sleep(wait.min(MAX_SLEEP)).await,
                        None => std::future::pending().await,
                    }
                } => {}
            }
            if self.due_in(auto, now_ms(), started.elapsed()) == Some(Duration::ZERO) {
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    () = self.run_check() => {}
                }
            }
        }
    }

    fn due_in(&self, auto: bool, now: u64, since_start: Duration) -> Option<Duration> {
        let check = self.settings.lock().check;
        let state = self.state.lock();
        due_in(
            auto && check == UpdateCheck::Auto,
            &self.tuning,
            now,
            since_start,
            state.last_ok_ms,
            state.last_failed_ms,
            state.unready_ms,
        )
    }

    // The check -----------------------------------------------------------------------------

    async fn run_check(&self) {
        let _one = self.checking.lock().await;
        let at_ms = now_ms();
        let (channel, running) = (self.settings.lock().channel, self.running.clone());
        let result = self
            .fetch()
            .await
            .map(|list| releases::newest(list, &running, channel));
        // A listed release is an update once its archive answers: GitHub lists the assets (and
        // their digests) before it serves them. One `HEAD` per new release; a release that was
        // offered before is not asked about again.
        let mut waiting = None;
        let result = match result {
            Ok(Some(release)) => {
                let offered = self
                    .state
                    .lock()
                    .available
                    .as_ref()
                    .is_some_and(|known| known.version == release.version);
                if offered || self.archive_downloadable(&release).await {
                    Ok(Some(release))
                } else {
                    waiting = Some(release.version);
                    Ok(None)
                }
            }
            other => other,
        };
        let mut announce = None;
        {
            let mut state = self.state.lock();
            match result {
                Ok(found) => {
                    debug!(found = ?found.as_ref().map(|r| &r.version), "update check read an answer");
                    state.last_check = Some(UpdateCheckInfo {
                        at_ms,
                        outcome: CheckOutcome::Checked,
                        message: None,
                    });
                    state.last_ok_ms = Some(at_ms);
                    state.last_failed_ms = None;
                    if let Some(version) = waiting {
                        // Neither offered nor forgotten: what was offered before stays, and
                        // the check looks again soon.
                        debug!(%version, "a newer release is listed, but its archive is not downloadable yet");
                        state.unready_ms = Some(at_ms);
                    } else {
                        state.unready_ms = None;
                        match &found {
                            Some(release) if state.announced.as_ref() != Some(&release.version) => {
                                announce = Some(release.clone());
                                state.announced = Some(release.version.clone());
                            }
                            Some(_) => {}
                            None => state.announced = None,
                        }
                        state.available = found;
                    }
                    self.persist(&state);
                }
                Err((outcome, message)) => {
                    debug!(?outcome, %message, "update check did not finish");
                    state.last_check = Some(UpdateCheckInfo {
                        at_ms,
                        outcome,
                        message: Some(message),
                    });
                    state.last_failed_ms = Some(at_ms);
                }
            }
        }
        if let Some(release) = announce {
            info!(version = %release.version, "a newer Hutan is available");
            self.publish(&Event::UpdateAvailable {
                release: Box::new(release),
            });
        }
    }

    /// The release list, or why it could not be read.
    async fn fetch(&self) -> Result<Vec<releases::Release>, (CheckOutcome, String)> {
        let failed = |message: &str| (CheckOutcome::Failed, message.to_owned());
        let Some(base) = &self.tuning.api_base else {
            return Err(failed("this Hutan has no update source"));
        };
        let Some(http) = &self.http else {
            return Err(failed("the update check could not set up its HTTP client"));
        };
        let url = format!(
            "{}/repos/{}/releases?per_page=30",
            base.trim_end_matches('/'),
            releases::REPOSITORY
        );
        let agent = format!("Hutan/{}", self.running);
        match http
            .get_bounded(&url, &agent, "application/vnd.github+json", BODY_MAX)
            .await
        {
            Ok(answer) if answer.status == 200 => releases::parse_list(&answer.body)
                .map_err(|message| (CheckOutcome::Failed, message)),
            Ok(answer)
                if answer.status == 429
                    || (answer.status == 403 && answer.rate_limit_exhausted) =>
            {
                Err((
                    CheckOutcome::RateLimited,
                    "GitHub's rate limit for this address is reached".to_owned(),
                ))
            }
            Ok(answer) => Err((
                CheckOutcome::Failed,
                format!("GitHub answered HTTP {}", answer.status),
            )),
            Err(
                GatewayError::Connect { .. } | GatewayError::Timeout | GatewayError::Stream { .. },
            ) => Err((
                CheckOutcome::Offline,
                "GitHub could not be reached".to_owned(),
            )),
            Err(error) => Err((CheckOutcome::Failed, error.to_string())),
        }
    }

    /// Writes `update.json`; a failure is only logged (the check works without it).
    fn persist(&self, state: &State) {
        let persisted = Persisted {
            last_checked_ms: state.last_ok_ms,
            available: state.available.clone(),
            unready_ms: state.unready_ms,
        };
        if let Err(error) = save(&self.state_file, &persisted) {
            debug!(%error, file = %self.state_file.display(), "update state was not saved");
        }
    }

    fn publish(&self, event: &Event) {
        if let Err(error) = self.bus.publish(event) {
            warn!(%error, "failed to publish an update event");
        }
    }

    // The install ---------------------------------------------------------------------------

    /// `update.install`.
    pub async fn install(
        self: &Arc<Self>,
        req: UpdateInstallRequest,
    ) -> Result<UpdateInstalling, ErrorBody> {
        if req.method == InstallMethod::Bundle {
            self.check_method(req.method)?;
            return self.install_bundle(req).await;
        }
        if req.apply || req.after_pid.is_some() {
            return Err(bad_request(
                "`apply` is for a bundle that was copied by hand; a Homebrew install runs in its terminal",
            ));
        }
        let (command, prefix) = match req.method {
            InstallMethod::Cask => (CASK_COMMAND, self.detected.prefix.clone()),
            InstallMethod::Formula => (FORMULA_COMMAND, self.detected.prefix.clone()),
            method => {
                self.check_method(method)?;
                return Err(invalid_state("this Hutan cannot be updated that way"));
            }
        };
        self.check_method(req.method)?;
        let _one = self.installing.lock().await;
        let release = {
            let state = self.state.lock();
            if state
                .install
                .as_ref()
                .is_some_and(|i| i.state == InstallState::Running)
            {
                return Err(invalid_state("an update is already running"));
            }
            state.available.clone()
        };
        let Some(release) = release else {
            return Err(invalid_state(
                "no newer release is known; check for updates first",
            ));
        };
        let session = self
            .registry
            .create_session(SessionCreate {
                workspace_id: None,
                spec: SessionSpec::Terminal(TerminalSpec {
                    command: vec![
                        "/bin/sh".to_owned(),
                        "-c".to_owned(),
                        script(prefix.as_deref(), command),
                    ],
                    title: Some(format!("Update Hutan to {}", release.version)),
                    ..TerminalSpec::default()
                }),
            })
            .await?;
        let install = UpdateInstall {
            method: req.method,
            session_id: Some(session.id),
            version: release.version,
            command: command.to_owned(),
            state: InstallState::Running,
            exit_code: None,
            stage: None,
            downloaded: None,
            total: None,
            message: None,
            manual_command: None,
        };
        self.state.lock().install = Some(install.clone());
        self.watch(install.clone(), session.id);
        Ok(UpdateInstalling {
            session: Some(session),
            install,
        })
    }

    /// `bad_request` for a method that cannot be installed, `invalid_state` when it is not the
    /// one this Hutan was installed with.
    fn check_method(&self, method: InstallMethod) -> Result<(), ErrorBody> {
        match method {
            InstallMethod::Unknown => Err(bad_request(
                "Hutan was not installed in a way it can update; update it the way you installed it",
            )),
            method if method != self.detected.method => Err(invalid_state(format!(
                "this Hutan is installed as {:?}, not {method:?}; ask `update.status` first",
                self.detected.method
            ))),
            _ => Ok(()),
        }
    }

    /// Looks at the install's session until it ends, then records and announces how.
    fn watch(self: &Arc<Self>, install: UpdateInstall, session_id: SessionId) {
        let service = self.clone();
        tokio::spawn(async move {
            let (state, exit_code) = loop {
                tokio::time::sleep(WATCH_INTERVAL).await;
                match service.session_end(session_id).await {
                    Ok(Some(end)) => break end,
                    Ok(None) => {}
                    // The registry is gone: the daemon is stopping.
                    Err(()) => return,
                }
            };
            let done = UpdateInstall {
                state,
                exit_code,
                ..install
            };
            service.state.lock().install = Some(done.clone());
            service.publish(&Event::UpdateInstallChanged {
                install: Box::new(done),
            });
        });
    }

    /// `Some` once the session ended; `Err` when the registry no longer answers.
    async fn session_end(&self, id: SessionId) -> Result<Option<(InstallState, Option<i32>)>, ()> {
        use hutan_core::SessionState;
        let list = self
            .registry
            .list_sessions(SessionListFilter::default())
            .await
            .map_err(|_| ())?;
        let Some(session) = list.sessions.iter().find(|s| s.id == id) else {
            // Closed before anyone saw how it ended.
            return Ok(Some((InstallState::Failed, None)));
        };
        Ok(match &session.state {
            SessionState::Exited {
                code: Some(0),
                signal: None,
            } => Some((InstallState::Succeeded, Some(0))),
            SessionState::Exited { code, .. } => Some((InstallState::Failed, *code)),
            SessionState::Failed { .. } | SessionState::Stopped => {
                Some((InstallState::Failed, None))
            }
            _ => None,
        })
    }
}

/// How long until the next automatic check: `None` when none will run, `Some(ZERO)` when one is
/// due. Pure, so the schedule is tested without a clock.
fn due_in(
    auto: bool,
    tuning: &UpdateTuning,
    now_ms: u64,
    since_start: Duration,
    last_ok_ms: Option<u64>,
    last_failed_ms: Option<u64>,
    unready_ms: Option<u64>,
) -> Option<Duration> {
    if !auto {
        return None;
    }
    let ms = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
    // A clock that went backwards must not postpone a check for longer than the interval.
    let after =
        |time: Option<u64>, gap: Duration| time.map(|t| t.min(now_ms).saturating_add(ms(gap)));
    // A release that is listed but not downloadable yet is looked at again soon, not in a day.
    let due_ms = after(unready_ms, tuning.unready_retry).unwrap_or_else(|| {
        after(last_ok_ms, tuning.interval)
            .into_iter()
            .chain(after(last_failed_ms, tuning.retry))
            .max()
            .unwrap_or(0)
    });
    let wait = Duration::from_millis(due_ms.saturating_sub(now_ms));
    Some(wait.max(tuning.startup_delay.saturating_sub(since_start)))
}

/// Whether `release` is still an update for `running` in the settings' channel.
fn fits(release: &UpdateRelease, running: &Version, settings: &UpdatesSettings) -> bool {
    releases::parse_version(&release.tag).is_some_and(|version| version > *running)
        && (settings.channel == hutan_protocol::update::UpdateChannel::Prerelease
            || !release.prerelease)
}

/// The shell script of the install session: Homebrew's own `bin` first on `PATH` (the daemon
/// and its sessions may not have the user's login `PATH`), the command shown, then run.
fn script(prefix: Option<&Path>, command: &str) -> String {
    let path = prefix.map_or_else(String::new, |prefix| {
        format!(
            "PATH={}:\"$PATH\"; export PATH; ",
            quote(&prefix.join("bin").to_string_lossy())
        )
    });
    format!("{path}echo {}; {command}", quote(&format!("$ {command}")))
}

/// `text` as one single-quoted shell word.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn load(path: &Path) -> Persisted {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(path: &Path, persisted: &Persisted) -> std::io::Result<()> {
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(persisted)?)?;
    std::fs::rename(temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tuning() -> UpdateTuning {
        UpdateTuning {
            interval: Duration::from_secs(24 * 3600),
            retry: Duration::from_secs(3600),
            startup_delay: Duration::from_secs(30),
            ..UpdateTuning::default()
        }
    }

    const HOUR_MS: u64 = 3_600_000;
    const NOW: u64 = 1_000_000_000_000;

    #[test]
    fn a_daemon_that_never_checked_waits_only_the_startup_delay() {
        let tuning = tuning();
        assert_eq!(
            due_in(true, &tuning, NOW, Duration::from_secs(5), None, None, None),
            Some(Duration::from_secs(25))
        );
        assert_eq!(
            due_in(
                true,
                &tuning,
                NOW,
                Duration::from_secs(31),
                None,
                None,
                None
            ),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn a_check_that_read_an_answer_holds_the_next_for_24_hours() {
        let tuning = tuning();
        let since = Duration::from_secs(60);
        assert_eq!(
            due_in(true, &tuning, NOW, since, Some(NOW - HOUR_MS), None, None),
            Some(Duration::from_secs(23 * 3600))
        );
        assert_eq!(
            due_in(
                true,
                &tuning,
                NOW,
                since,
                Some(NOW - 25 * HOUR_MS),
                None,
                None
            ),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn a_failed_check_is_retried_after_the_retry_gap_not_after_a_day() {
        let tuning = tuning();
        let since = Duration::from_secs(60);
        // Read an answer 25 h ago, failed 10 minutes ago: back in 50 minutes.
        assert_eq!(
            due_in(
                true,
                &tuning,
                NOW,
                since,
                Some(NOW - 25 * HOUR_MS),
                Some(NOW - 600_000),
                None
            ),
            Some(Duration::from_secs(50 * 60))
        );
    }

    #[test]
    fn a_release_that_is_not_downloadable_yet_is_looked_at_again_in_minutes_not_a_day() {
        let tuning = tuning();
        let since = Duration::from_secs(60);
        // The check read the list a minute ago and found a release it could not offer.
        assert_eq!(
            due_in(
                true,
                &tuning,
                NOW,
                since,
                Some(NOW - 60_000),
                None,
                Some(NOW - 60_000)
            ),
            Some(Duration::from_secs(4 * 60))
        );
        assert_eq!(
            due_in(
                true,
                &tuning,
                NOW,
                since,
                Some(NOW - 6 * 60_000),
                None,
                Some(NOW - 6 * 60_000)
            ),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn a_clock_that_went_backwards_does_not_postpone_past_the_interval() {
        let tuning = tuning();
        assert_eq!(
            due_in(
                true,
                &tuning,
                NOW,
                Duration::from_secs(60),
                Some(NOW + 100 * HOUR_MS),
                None,
                None
            ),
            Some(Duration::from_secs(24 * 3600))
        );
    }

    #[test]
    fn nothing_is_scheduled_when_checking_is_off() {
        assert_eq!(
            due_in(
                false,
                &tuning(),
                NOW,
                Duration::from_secs(60),
                None,
                None,
                None
            ),
            None
        );
    }

    #[test]
    fn the_install_script_puts_brews_bin_first_and_shows_the_command() {
        let text = script(Some(Path::new("/opt/home brew")), "brew update");
        assert_eq!(
            text,
            "PATH='/opt/home brew/bin':\"$PATH\"; export PATH; echo '$ brew update'; brew update"
        );
        assert_eq!(quote("it's"), "'it'\\''s'");
        assert_eq!(script(None, "x"), "echo '$ x'; x");
    }

    #[test]
    fn a_stored_release_that_is_no_longer_an_update_is_dropped() {
        let release = UpdateRelease {
            version: "0.3.2".into(),
            tag: "v0.3.2".into(),
            prerelease: true,
            published: None,
            notes_url: "https://example.test".into(),
            notes: String::new(),
            notes_truncated: false,
            assets: Vec::new(),
        };
        let settings = UpdatesSettings::default();
        let stable = UpdatesSettings {
            channel: hutan_protocol::update::UpdateChannel::Stable,
            ..UpdatesSettings::default()
        };
        assert!(fits(&release, &Version::new(0, 3, 1), &settings));
        assert!(
            !fits(&release, &Version::new(0, 3, 2), &settings),
            "now running it"
        );
        assert!(!fits(&release, &Version::new(0, 4, 0), &settings));
        assert!(!fits(&release, &Version::new(0, 3, 1), &stable));
    }
}
