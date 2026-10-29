//! Updates (protocol 0.60, capability `update`, ADR 0024) against an in-process daemon on a
//! temporary `HUTAN_HOME`: the check against a local mock of GitHub's releases API (nothing here
//! ever contacts GitHub), the install method read from fake Homebrew layouts, and the update
//! session that runs a fake `brew` found first on `PATH` (no real `brew`, no `/Applications`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, WAIT, remote_code, wait_event};
use hutan_client::Client;
use hutan_daemon::DaemonConfig;
use hutan_daemon::update::UpdateTuning;
use hutan_gateway::testing::{MockServer, Reply};
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Event, Origin, Request,
};
use hutan_protocol::update::{
    CASK_COMMAND, CheckOutcome, FORMULA_COMMAND, InstallMethod, InstallState, UpdateChannel,
    UpdateCheck, UpdateInstall, UpdateInstallRequest, UpdateStatus,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::{sleep, timeout};

pub(crate) const RELEASES: &str = "/repos/robertt3kuk/hutan/releases";
pub(crate) const RUNNING: &str = "0.3.1";

// ---- fixtures ---------------------------------------------------------------------------------

fn release(tag: &str, prerelease: bool, archive: bool) -> Value {
    let version = tag.trim_start_matches('v');
    let mut assets = vec![json!({"name": format!("Hutan-{version}-source.tar.gz"), "size": 5})];
    if archive {
        assets.push(json!({
            "name": format!("Hutan-{version}-arm64.zip"),
            "size": 1234,
            "digest": format!("sha256:{}", "ab".repeat(32)),
            "state": "uploaded",
        }));
    }
    json!({
        "tag_name": tag,
        "draft": false,
        "prerelease": prerelease,
        "published_at": "2026-10-29T10:00:00Z",
        "html_url": format!("https://github.com/robertt3kuk/hutan/releases/tag/{tag}"),
        "body": format!("## Hutan {version}\n\n- notes"),
        "assets": assets,
    })
}

/// The path of a release's archive on the download host (`<base>/<this>`).
pub(crate) fn archive_path(tag: &str) -> String {
    let version = tag.trim_start_matches('v');
    format!("/robertt3kuk/hutan/releases/download/{tag}/Hutan-{version}-arm64.zip")
}

/// The releases API lists `releases`; the archive of each that has one answers `HEAD` at once
/// (a test that wants it not to calls [`hold_back`]).
fn serve(server: &MockServer, releases: &[Value]) {
    server.set_route(
        "GET",
        RELEASES,
        Reply::json(200, &serde_json::to_string(releases).unwrap()),
    );
    for release in releases {
        let tag = release["tag_name"].as_str().unwrap();
        let version = tag.trim_start_matches('v');
        let wanted = format!("Hutan-{version}-arm64.zip");
        let has_archive = release["assets"]
            .as_array()
            .is_some_and(|assets| assets.iter().any(|a| a["name"] == wanted.as_str()));
        if has_archive {
            server.set_route(
                "HEAD",
                &archive_path(tag),
                Reply::fixed(200, "application/zip", ""),
            );
        }
    }
}

/// A fake program at a stable path named by its content (macOS vets every new executable once,
/// so scripts are written once, not once per test).
pub(crate) fn executable(name: &str, body: &str) -> PathBuf {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let text = format!("#!/bin/sh\n{body}\n");
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let dir = std::env::temp_dir().join("hutan-test-fake-clis");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{:016x}-{name}", hasher.finish()));
    if std::fs::read_to_string(&path).is_ok_and(|have| have == text) {
        return path;
    }
    let staging = dir.join(format!(".{}-{}", std::process::id(), name));
    std::fs::write(&staging, &text).unwrap();
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&staging, &path).unwrap();
    path
}

/// A fake `brew` that records its arguments next to itself (`brew.log` in the folder it is
/// invoked from), waits for a file `go` there when `hold` is set, and exits with `code`.
fn fake_brew(code: i32, hold: bool) -> PathBuf {
    let wait = if hold {
        "while [ ! -e \"$(dirname \"$0\")/go\" ]; do sleep 0.05; done\n"
    } else {
        ""
    };
    executable(
        &format!("brew-{code}-{}", u8::from(hold)),
        &format!("echo \"$@\" >> \"$(dirname \"$0\")/brew.log\"\n{wait}exit {code}"),
    )
}

/// A Homebrew prefix in a temporary folder, with the fake `brew` in its `bin`.
struct Layout {
    dir: TempDir,
    prefix: PathBuf,
    /// The executable the daemon reads its install method from.
    exe: PathBuf,
}

impl Layout {
    fn new(brew: &Path) -> Self {
        let dir = tempfile::tempdir().unwrap();
        // Canonical: the daemon resolves symlinks, and /var is one on macOS.
        let root = dir.path().canonicalize().unwrap();
        let prefix = root.join("prefix");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        symlink(brew, prefix.join("bin/brew")).unwrap();
        Self {
            exe: root.join("hutand"),
            prefix,
            dir,
        }
    }

    /// `Hutan.app` in `Applications` that `Caskroom/hutan/0.3.1` links to.
    fn cask(brew: &Path) -> Self {
        let mut layout = Self::new(brew);
        let root = layout.dir.path().canonicalize().unwrap();
        let bundle = root.join("Applications/Hutan.app");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        layout.exe = bundle.join("Contents/MacOS/hutand");
        std::fs::write(&layout.exe, b"").unwrap();
        let staged = layout.prefix.join("Caskroom/hutan/0.3.1");
        std::fs::create_dir_all(&staged).unwrap();
        symlink(&bundle, staged.join("Hutan.app")).unwrap();
        layout
    }

    /// `hutand` in `Cellar/hutan/0.3.1/bin`.
    fn formula(brew: &Path) -> Self {
        let mut layout = Self::new(brew);
        let bin = layout.prefix.join("Cellar/hutan/0.3.1/bin");
        std::fs::create_dir_all(&bin).unwrap();
        layout.exe = bin.join("hutand");
        std::fs::write(&layout.exe, b"").unwrap();
        layout
    }

    /// A `Hutan.app` copied into a folder the user can write; no Caskroom knows it.
    fn bundle(brew: &Path) -> Self {
        let mut layout = Self::new(brew);
        let root = layout.dir.path().canonicalize().unwrap();
        let bundle = root.join("Copied/Hutan.app");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        layout.exe = bundle.join("Contents/MacOS/hutand");
        std::fs::write(&layout.exe, b"").unwrap();
        layout
    }

    /// An executable that is neither: a development build.
    fn development(brew: &Path) -> Self {
        let layout = Self::new(brew);
        std::fs::write(&layout.exe, b"").unwrap();
        layout
    }

    fn brew_log(&self) -> String {
        std::fs::read_to_string(self.prefix.join("bin/brew.log")).unwrap_or_default()
    }

    fn release_brew(&self) {
        std::fs::write(self.prefix.join("bin/go"), b"").unwrap();
    }

    fn tune(&self, api: Option<String>) -> impl FnOnce(&mut DaemonConfig) + use<> {
        let (exe, prefix) = (self.exe.clone(), self.prefix.clone());
        move |config: &mut DaemonConfig| {
            let mut tuning = UpdateTuning {
                api_base: api.clone(),
                version: RUNNING.to_owned(),
                exe: Some(exe),
                brew_prefixes: vec![prefix],
                startup_delay: Duration::ZERO,
                ..UpdateTuning::default()
            };
            // The mock also serves the downloads: no test ever asks GitHub whether an archive
            // can be had.
            if let Some(api) = api {
                tuning.download_base = api;
            }
            config.updates = tuning;
        }
    }
}

pub(crate) async fn status(client: &Client) -> UpdateStatus {
    client.update_status().await.unwrap()
}

/// Polls `update.status` until `done` holds (the daemon's state, not the clock).
pub(crate) async fn status_where(
    client: &Client,
    what: &str,
    done: impl Fn(&UpdateStatus) -> bool,
) -> UpdateStatus {
    timeout(WAIT, async {
        loop {
            let status = status(client).await;
            if done(&status) {
                return status;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

async fn start(server: &MockServer, layout: &Layout, toml: &str) -> TestDaemon {
    TestDaemon::start_with(toml, layout.tune(Some(server.url()))).await
}

/// A daemon that never checks by itself: the check waits for the user.
pub(crate) const MANUAL: &str = "[updates]\ncheck = \"off\"\n";

// ---- the check --------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_newer_prerelease_is_found_announced_once_and_sends_only_a_user_agent() {
    let server = MockServer::start().await;
    serve(
        &server,
        &[
            release("v0.3.0", true, true),
            release("v0.3.1", true, true),
            release("v0.3.2", true, true),
            release("v0.3.3", true, false),
        ],
    );
    let layout = Layout::cask(&fake_brew(0, false));
    let t = start(&server, &layout, MANUAL).await;
    let (client, mut events) = t.connect_subscribed().await;
    assert!(client.supports_update());

    let before = status(&client).await;
    assert_eq!(before.version, RUNNING);
    assert_eq!(before.available, None);
    assert_eq!(before.last_check, None);
    assert_eq!(server.requests().len(), 0, "status contacts nothing");

    let checked = client.update_check().await.unwrap();
    let found = checked
        .available
        .expect("0.3.2 is newer and has its archive");
    assert_eq!(found.version, "0.3.2");
    assert!(found.prerelease);
    assert_eq!(found.notes, "## Hutan 0.3.2\n\n- notes");
    assert_eq!(
        found.notes_url,
        "https://github.com/robertt3kuk/hutan/releases/tag/v0.3.2"
    );
    let archive = found
        .assets
        .iter()
        .find(|a| a.name == "Hutan-0.3.2-arm64.zip")
        .unwrap();
    assert_eq!(archive.sha256.as_deref(), Some("ab".repeat(32).as_str()));
    let last = checked.last_check.unwrap();
    assert_eq!(last.outcome, CheckOutcome::Checked);

    let announced = wait_event(&mut events, "update.available", |event| match event {
        Event::UpdateAvailable { release } => Some(release.version.clone()),
        _ => None,
    })
    .await;
    assert_eq!(announced, "0.3.2");

    // The list, and one `HEAD` for the new release's archive, each carrying the User-Agent and
    // nothing identifying.
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        (requests[0].method.as_str(), requests[0].path()),
        ("GET", RELEASES)
    );
    assert_eq!(
        (requests[1].method.as_str(), requests[1].path()),
        ("HEAD", archive_path("v0.3.2").as_str())
    );
    for request in &requests {
        assert_eq!(request.header("user-agent"), Some("Hutan/0.3.1"));
        for secret in ["authorization", "cookie", "x-github-token"] {
            assert_eq!(request.header(secret), None, "{secret} is never sent");
        }
    }

    // A second check finds the same release and does not announce it again, nor ask about its
    // archive again: the list is the only request. The next announcement is the newer one.
    client.update_check().await.unwrap();
    assert_eq!(
        server.requests().len(),
        3,
        "an offered release is not asked about twice"
    );
    serve(
        &server,
        &[release("v0.3.4", true, true), release("v0.3.2", true, true)],
    );
    let third = client.update_check().await.unwrap();
    assert_eq!(third.available.unwrap().version, "0.3.4");
    let announced = wait_event(&mut events, "update.available again", |event| match event {
        Event::UpdateAvailable { release } => Some(release.version.clone()),
        _ => None,
    })
    .await;
    assert_eq!(announced, "0.3.4", "0.3.2 was announced once");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn older_equal_and_archiveless_releases_find_nothing() {
    let server = MockServer::start().await;
    serve(
        &server,
        &[
            release("v0.3.0", true, true),
            release("v0.3.1", true, true),
            release("v0.3.9", true, false),
        ],
    );
    let layout = Layout::cask(&fake_brew(0, false));
    let t = start(&server, &layout, MANUAL).await;
    let client = t.connect().await;
    let checked = client.update_check().await.unwrap();
    assert_eq!(checked.available, None);
    assert_eq!(checked.last_check.unwrap().outcome, CheckOutcome::Checked);
    t.stop().await;
}

/// Requests the mock got with `method`.
fn count(server: &MockServer, method: &str) -> usize {
    server
        .requests()
        .iter()
        .filter(|request| request.method == method)
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_release_is_not_offered_until_its_archive_answers_a_head_with_2xx() {
    let server = MockServer::start().await;
    let release = release("v0.3.2", true, true);
    serve(&server, std::slice::from_ref(&release));
    // Right after `gh release create`: the API lists the asset and its digest, the download URL
    // says `Not Found` (9 bytes, HTTP 404) for a while, then the file.
    let archive = archive_path("v0.3.2");
    server.set_route(
        "HEAD",
        &archive,
        Reply::fixed(404, "text/plain", "Not Found"),
    );
    server.route("HEAD", &archive, Reply::fixed(200, "application/zip", ""));
    let layout = Layout::cask(&fake_brew(0, false));
    let t = start(&server, &layout, MANUAL).await;
    let (client, mut events) = t.connect_subscribed().await;

    let early = client.update_check().await.unwrap();
    assert_eq!(early.available, None, "listed, but not downloadable yet");
    let last = early.last_check.unwrap();
    assert_eq!(
        last.outcome,
        CheckOutcome::Checked,
        "quiet: no failure to show"
    );
    assert_eq!(
        last.message.as_deref(),
        Some("Hutan 0.3.2 was just published; it can be installed in a minute"),
        "what a manual check says instead of \"up to date\""
    );
    assert_eq!(count(&server, "HEAD"), 1);
    assert!(events.try_recv().is_err(), "nothing was announced");

    let later = client.update_check().await.unwrap();
    assert_eq!(
        later.last_check.unwrap().message,
        None,
        "the words go with the wait"
    );
    assert_eq!(later.available.unwrap().version, "0.3.2");
    assert_eq!(count(&server, "HEAD"), 2);
    let announced = wait_event(&mut events, "update.available", |event| match event {
        Event::UpdateAvailable { release } => Some(release.version.clone()),
        _ => None,
    })
    .await;
    assert_eq!(announced, "0.3.2");
    let head = &server.requests_to(&archive)[0];
    assert_eq!(head.method, "HEAD");
    assert_eq!(head.header("user-agent"), Some("Hutan/0.3.1"));
    for secret in ["authorization", "cookie"] {
        assert_eq!(head.header(secret), None);
    }

    // Offered once, not asked about again: one extra request per new release, none after.
    client.update_check().await.unwrap();
    client.update_check().await.unwrap();
    assert_eq!(count(&server, "HEAD"), 2);
    assert_eq!(count(&server, "GET"), 4);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_head_follows_the_redirect_to_the_asset_host() {
    let server = MockServer::start().await;
    serve(&server, &[release("v0.3.2", true, true)]);
    // GitHub answers 302 to its asset host, which is what says whether the file is there.
    let moved = "/objects/Hutan-0.3.2-arm64.zip";
    server.set_route(
        "HEAD",
        &archive_path("v0.3.2"),
        Reply::json(302, "").header("location", moved),
    );
    server.set_route("HEAD", moved, Reply::fixed(404, "text/plain", "Not Found"));
    let layout = Layout::cask(&fake_brew(0, false));
    let t = start(&server, &layout, MANUAL).await;
    let client = t.connect().await;
    assert_eq!(client.update_check().await.unwrap().available, None);
    assert_eq!(
        server.requests_to(moved).len(),
        1,
        "the redirect was followed"
    );

    server.set_route("HEAD", moved, Reply::fixed(200, "application/zip", ""));
    assert!(client.update_check().await.unwrap().available.is_some());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_release_that_is_not_downloadable_yet_leaves_what_was_offered_before() {
    let server = MockServer::start().await;
    serve(&server, &[release("v0.3.2", true, true)]);
    let layout = Layout::cask(&fake_brew(0, false));
    let t = start(&server, &layout, MANUAL).await;
    let client = t.connect().await;
    assert_eq!(
        client
            .update_check()
            .await
            .unwrap()
            .available
            .unwrap()
            .version,
        "0.3.2"
    );
    // 0.3.3 is listed, its archive is not served yet: 0.3.2 stays the update.
    serve(
        &server,
        &[release("v0.3.2", true, true), release("v0.3.3", true, true)],
    );
    server.set_route(
        "HEAD",
        &archive_path("v0.3.3"),
        Reply::fixed(404, "text/plain", "Not Found"),
    );
    let checked = client.update_check().await.unwrap();
    assert_eq!(checked.available.unwrap().version, "0.3.2");
    assert_eq!(checked.last_check.unwrap().outcome, CheckOutcome::Checked);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_that_checks_by_itself_looks_again_soon_for_a_release_that_is_not_there_yet() {
    let server = MockServer::start().await;
    serve(&server, &[release("v0.3.2", true, true)]);
    let archive = archive_path("v0.3.2");
    server.set_route(
        "HEAD",
        &archive,
        Reply::fixed(404, "text/plain", "Not Found"),
    );
    server.route("HEAD", &archive, Reply::fixed(200, "application/zip", ""));
    let layout = Layout::cask(&fake_brew(0, false));
    let tune = layout.tune(Some(server.url()));
    // `check = "auto"`, no startup delay, and a short wait for the second look: the daemon is
    // not asked, it finds the release by itself once its archive is served.
    let t = TestDaemon::start_with("", move |config| {
        tune(config);
        config.updates.unready_retry = Duration::from_millis(20);
    })
    .await;
    let client = t.connect().await;
    let found = status_where(&client, "the release", |s| s.available.is_some()).await;
    assert_eq!(found.available.unwrap().version, "0.3.2");
    assert_eq!(
        count(&server, "HEAD"),
        2,
        "not downloadable, then downloadable"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_stable_channel_ignores_prereleases_until_the_setting_changes() {
    let server = MockServer::start().await;
    serve(&server, &[release("v0.3.2", true, true)]);
    let layout = Layout::cask(&fake_brew(0, false));
    let t = start(
        &server,
        &layout,
        "[updates]\ncheck = \"off\"\nchannel = \"stable\"\n",
    )
    .await;
    let client = t.connect().await;
    let first = client.update_check().await.unwrap();
    assert_eq!(first.channel, UpdateChannel::Stable);
    assert_eq!(
        first.available, None,
        "a pre-release is not on the stable channel"
    );

    client
        .daemon_setting_set("updates.channel", Some(json!("prerelease")))
        .await
        .unwrap();
    assert_eq!(status(&client).await.channel, UpdateChannel::Prerelease);
    let second = client.update_check().await.unwrap();
    assert_eq!(second.available.unwrap().version, "0.3.2");

    // Back to stable: the release it no longer allows goes at once.
    client
        .daemon_setting_set("updates.channel", Some(json!("stable")))
        .await
        .unwrap();
    assert_eq!(status(&client).await.available, None);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn trouble_with_the_network_is_recorded_and_never_an_error_or_an_event() {
    let layout = Layout::cask(&fake_brew(0, false));

    // Offline: nothing listens.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let t = TestDaemon::start_with(MANUAL, layout.tune(Some(closed))).await;
    let client = t.connect().await;
    let offline = client.update_check().await.unwrap();
    let last = offline.last_check.unwrap();
    assert_eq!(last.outcome, CheckOutcome::Offline);
    assert!(last.message.is_some());
    assert_eq!(offline.available, None);
    t.stop().await;

    // Rate limited, a failing server, and an answer that is no release list.
    let server = MockServer::start().await;
    let t = start(&server, &layout, MANUAL).await;
    let (client, mut events) = t.connect_subscribed().await;
    let cases = [
        (
            Reply::json(403, r#"{"message":"API rate limit exceeded"}"#)
                .header("x-ratelimit-remaining", "0"),
            CheckOutcome::RateLimited,
        ),
        (Reply::json(429, "{}"), CheckOutcome::RateLimited),
        (
            Reply::json(403, r#"{"message":"forbidden"}"#),
            CheckOutcome::Failed,
        ),
        (Reply::json(500, "{}"), CheckOutcome::Failed),
        (
            Reply::json(200, r#"{"message":"Not Found"}"#),
            CheckOutcome::Failed,
        ),
        (
            Reply::fixed(200, "text/html", "<html>"),
            CheckOutcome::Failed,
        ),
    ];
    for (reply, expected) in cases {
        server.set_route("GET", RELEASES, reply);
        let checked = client.update_check().await.unwrap();
        assert_eq!(checked.last_check.unwrap().outcome, expected);
        assert_eq!(checked.available, None);
    }
    // A too long answer is refused, not buffered.
    server.set_route(
        "GET",
        RELEASES,
        Reply::json(200, &format!("[\"{}\"]", "x".repeat(2 * 1024 * 1024 + 10))),
    );
    let big = client.update_check().await.unwrap();
    assert_eq!(big.last_check.unwrap().outcome, CheckOutcome::Failed);
    assert!(
        events.try_recv().is_err(),
        "nothing was announced for a failed check"
    );
    // The check is still good once the network is.
    serve(&server, &[release("v0.3.2", true, true)]);
    assert!(client.update_check().await.unwrap().available.is_some());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_check_on_request_works_when_checking_is_off_and_without_a_source() {
    let server = MockServer::start().await;
    serve(&server, &[release("v0.3.2", true, true)]);
    let layout = Layout::cask(&fake_brew(0, false));
    let t = start(&server, &layout, MANUAL).await;
    let client = t.connect().await;
    let off = status(&client).await;
    assert_eq!(off.check, UpdateCheck::Off);
    assert!(client.update_check().await.unwrap().available.is_some());
    assert_eq!(
        server.requests().len(),
        2,
        "off means: only when asked (the list, and the new release's archive)"
    );
    t.stop().await;

    // A daemon with no update source says so instead of failing.
    let t = TestDaemon::start_with(MANUAL, layout.tune(None)).await;
    let client = t.connect().await;
    let none = client.update_check().await.unwrap();
    assert_eq!(none.last_check.unwrap().outcome, CheckOutcome::Failed);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_installed_hutan_checks_by_itself_and_remembers_it_across_a_restart() {
    let server = MockServer::start().await;
    serve(&server, &[release("v0.3.2", true, true)]);
    let layout = Layout::cask(&fake_brew(0, false));
    // `check = "auto"` (the default) and no startup delay: the daemon asks without being asked.
    let t = start(&server, &layout, "").await;
    // The daemon may have asked before this connection exists, so the state is what is waited
    // for, not the event.
    let client = t.connect().await;
    let found = status_where(&client, "the release", |s| s.available.is_some()).await;
    assert_eq!(found.available.unwrap().version, "0.3.2");
    assert_eq!(
        server.requests().len(),
        2,
        "the list, and the archive's HEAD"
    );

    // A restart on the same home neither forgets the release nor asks again at once: it checked
    // less than 24 hours ago.
    let t = t.restart_with(layout.tune(Some(server.url()))).await;
    let client = t.connect().await;
    let remembered = status(&client).await;
    assert_eq!(remembered.available.unwrap().version, "0.3.2");
    assert_eq!(remembered.last_check, None, "no check since the restart");
    // A check on request is the sync point: had the daemon asked by itself, it would have by now.
    client.update_check().await.unwrap();
    assert_eq!(
        server.requests().len(),
        3,
        "the 24 hour throttle held: only the list, the release was offered before"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_development_build_does_not_check_by_itself() {
    let server = MockServer::start().await;
    serve(&server, &[release("v0.3.2", true, true)]);
    let layout = Layout::development(&fake_brew(0, false));
    let t = start(&server, &layout, "").await;
    let client = t.connect().await;
    let status = status(&client).await;
    assert_eq!(status.method, InstallMethod::Unknown);
    assert_eq!(status.command, None);
    client.update_check().await.unwrap();
    assert_eq!(
        server.requests().len(),
        2,
        "only the request that was asked for (the list, and the new release's archive)"
    );
    t.stop().await;
}

// ---- how it was installed ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_install_method_is_read_from_the_layout() {
    let brew = fake_brew(0, false);
    let cases = [
        (Layout::cask(&brew), InstallMethod::Cask, Some(CASK_COMMAND)),
        (
            Layout::formula(&brew),
            InstallMethod::Formula,
            Some(FORMULA_COMMAND),
        ),
        (Layout::bundle(&brew), InstallMethod::Bundle, None),
        (Layout::development(&brew), InstallMethod::Unknown, None),
    ];
    for (layout, method, command) in cases {
        let t = TestDaemon::start_with(MANUAL, layout.tune(None)).await;
        let client = t.connect().await;
        let status = status(&client).await;
        assert_eq!(status.method, method);
        assert_eq!(status.command.as_deref(), command);
        assert_eq!(
            status.install_path.is_some(),
            method != InstallMethod::Unknown,
            "{method:?}"
        );
        t.stop().await;
    }
}

// ---- the install ------------------------------------------------------------------------------

async fn install_ended(
    events: &mut tokio::sync::broadcast::Receiver<(hutan_core::Seq, Event)>,
    state: InstallState,
) -> UpdateInstall {
    wait_event(events, "update.install ended", |event| match event {
        Event::UpdateInstallChanged { install } if install.state == state => {
            Some((**install).clone())
        }
        _ => None,
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn update_now_runs_brew_for_a_cask_in_a_visible_session_and_reports_success() {
    let server = MockServer::start().await;
    serve(&server, &[release("v0.3.2", true, true)]);
    let layout = Layout::cask(&fake_brew(0, false));
    let t = start(&server, &layout, MANUAL).await;
    let (client, mut events) = t.connect_subscribed().await;
    client.update_check().await.unwrap();

    let started = client.update_install(InstallMethod::Cask).await.unwrap();
    assert_eq!(started.install.state, InstallState::Running);
    assert_eq!(started.install.version, "0.3.2");
    assert_eq!(started.install.command, CASK_COMMAND);
    let session = started.session.clone().expect("Homebrew runs in a session");
    assert_eq!(session.title, "Update Hutan to 0.3.2");
    assert!(
        session.terminal_id.is_some(),
        "a terminal the user can watch"
    );
    assert_eq!(&session.command[..2], ["/bin/sh", "-c"]);
    assert!(
        session.command[2].contains("brew update && brew upgrade --cask robertt3kuk/hutan/hutan"),
        "{:?}",
        session.command
    );

    let done = install_ended(&mut events, InstallState::Succeeded).await;
    assert_eq!(done.exit_code, Some(0));
    assert_eq!(done.session_id, Some(session.id));
    // The fake brew saw both steps, with Homebrew's own words.
    assert_eq!(
        layout.brew_log().lines().collect::<Vec<_>>(),
        ["update", "upgrade --cask robertt3kuk/hutan/hutan"]
    );
    let after = status(&client).await;
    assert_eq!(after.install.unwrap().state, InstallState::Succeeded);
    // Hutan has not changed under itself: the running version is still the old one.
    assert_eq!(after.version, RUNNING);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_brew_is_reported_with_its_exit_code_and_the_formula_runs_its_own_command() {
    let server = MockServer::start().await;
    serve(&server, &[release("v0.3.2", true, true)]);

    let layout = Layout::cask(&fake_brew(3, false));
    let t = start(&server, &layout, MANUAL).await;
    let (client, mut events) = t.connect_subscribed().await;
    client.update_check().await.unwrap();
    client.update_install(InstallMethod::Cask).await.unwrap();
    let failed = install_ended(&mut events, InstallState::Failed).await;
    assert_eq!(failed.exit_code, Some(3));
    // A failed install can be tried again.
    client.update_install(InstallMethod::Cask).await.unwrap();
    install_ended(&mut events, InstallState::Failed).await;
    t.stop().await;

    let layout = Layout::formula(&fake_brew(0, false));
    let t = start(&server, &layout, MANUAL).await;
    let (client, mut events) = t.connect_subscribed().await;
    client.update_check().await.unwrap();
    let started = client.update_install(InstallMethod::Formula).await.unwrap();
    assert_eq!(started.install.command, FORMULA_COMMAND);
    install_ended(&mut events, InstallState::Succeeded).await;
    assert_eq!(
        layout.brew_log().lines().collect::<Vec<_>>(),
        ["update", "upgrade robertt3kuk/hutan/hutan"]
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_install_that_cannot_run_is_refused_with_its_reason() {
    let server = MockServer::start().await;
    serve(&server, &[release("v0.3.2", true, true)]);

    // A development build has no method; a method other than the detected one is not run
    // (a hand-copied bundle is updated by the daemon itself: `update_bundle.rs`).
    let layout = Layout::bundle(&fake_brew(0, false));
    let t = start(&server, &layout, MANUAL).await;
    let client = t.connect().await;
    client.update_check().await.unwrap();
    assert_eq!(
        remote_code(client.update_install(InstallMethod::Unknown).await),
        ErrorCode::BadRequest
    );
    // A method other than the detected one is not run.
    assert_eq!(
        remote_code(client.update_install(InstallMethod::Cask).await),
        ErrorCode::InvalidState
    );
    t.stop().await;

    // Nothing newer is known: nothing to install.
    let layout = Layout::cask(&fake_brew(0, false));
    let t = start(&server, &layout, MANUAL).await;
    let client = t.connect().await;
    assert_eq!(
        remote_code(client.update_install(InstallMethod::Cask).await),
        ErrorCode::InvalidState
    );
    // `apply` is for a bundle copied by hand; a Homebrew install runs in its terminal.
    assert_eq!(
        remote_code(
            client
                .call::<hutan_protocol::update::UpdateInstalling>(Request::UpdateInstall(
                    UpdateInstallRequest {
                        method: InstallMethod::Cask,
                        apply: true,
                        after_pid: None,
                    }
                ))
                .await
        ),
        ErrorCode::BadRequest
    );
    assert_eq!(layout.brew_log(), "", "brew was not run");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn one_install_at_a_time() {
    let server = MockServer::start().await;
    serve(&server, &[release("v0.3.2", true, true)]);
    let layout = Layout::cask(&fake_brew(0, true));
    let t = start(&server, &layout, MANUAL).await;
    let (client, mut events) = t.connect_subscribed().await;
    client.update_check().await.unwrap();
    client.update_install(InstallMethod::Cask).await.unwrap();
    assert_eq!(
        remote_code(client.update_install(InstallMethod::Cask).await),
        ErrorCode::InvalidState
    );
    let running = status(&client).await;
    assert_eq!(running.install.unwrap().state, InstallState::Running);
    layout.release_brew();
    install_ended(&mut events, InstallState::Succeeded).await;
    t.stop().await;
}

// ---- who may ask ------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_can_read_the_status_but_never_install() {
    let server = MockServer::start().await;
    serve(&server, &[release("v0.3.2", true, true)]);
    let layout = Layout::cask(&fake_brew(0, false));
    // The most permissive agent policy there is.
    let t = start(
        &server,
        &layout,
        "[updates]\ncheck = \"off\"\n[control]\nagents = \"allow\"\n",
    )
    .await;
    let user = t.connect().await;
    user.update_check().await.unwrap();

    let session = common::create(&user, common::sh("sleep 60")).await;
    let reporter = t.connect().await;
    let applied: AgentApplied = reporter
        .call(Request::AgentReport(AgentReportRequest {
            session_id: session.id,
            source: "test-hooks".into(),
            agent: "fake".into(),
            state: Some(AgentState::Working),
            message: None,
            seq: 1,
            agent_session_id: None,
            resume_argv: None,
            pid: None,
            node: None,
        }))
        .await
        .unwrap();
    assert!(applied.applied);
    let mut options = t.options();
    options.client.origin = Some(Origin {
        session_id: session.id,
    });
    let agent = Client::connect(options).await.unwrap();

    assert!(
        status(&agent).await.available.is_some(),
        "reading is allowed"
    );
    assert_eq!(
        remote_code(agent.update_install(InstallMethod::Cask).await),
        ErrorCode::Forbidden
    );
    assert_eq!(layout.brew_log(), "", "brew was not run");
    // The user can.
    user.update_install(InstallMethod::Cask).await.unwrap();
    t.stop().await;
}

// ---- the settings -----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_settings_are_listed_validated_and_skip_version_shows_in_the_status() {
    let layout = Layout::cask(&fake_brew(0, false));
    let t = TestDaemon::start_with("", layout.tune(None)).await;
    let client = t.connect().await;
    let listing = client.daemon_settings().await.unwrap();
    let value = |key: &str| {
        listing
            .entries
            .iter()
            .find(|e| e.key == key)
            .unwrap_or_else(|| panic!("no setting {key}"))
            .value
            .clone()
    };
    assert_eq!(value("updates.check"), json!("auto"));
    assert_eq!(value("updates.channel"), json!("prerelease"));
    assert_eq!(value("updates.skip_version"), json!(""));

    client
        .daemon_setting_set("updates.check", Some(json!("off")))
        .await
        .unwrap();
    client
        .daemon_setting_set("updates.skip_version", Some(json!("0.3.2")))
        .await
        .unwrap();
    let after = status(&client).await;
    assert_eq!(after.check, UpdateCheck::Off);
    assert_eq!(after.skipped.as_deref(), Some("0.3.2"));
    assert!(
        std::fs::read_to_string(&t.paths.config_file)
            .unwrap()
            .contains("skip_version")
    );
    client
        .daemon_setting_set("updates.skip_version", None)
        .await
        .unwrap();
    assert_eq!(status(&client).await.skipped, None);

    for bad in ["weekly", "Auto"] {
        assert_eq!(
            remote_code(
                client
                    .daemon_setting_set("updates.check", Some(json!(bad)))
                    .await
            ),
            ErrorCode::BadRequest
        );
    }
    t.stop().await;
}
