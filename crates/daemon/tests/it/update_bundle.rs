//! The update of a hand-copied `Hutan.app` (UPDATES-2, protocol 0.60 `update.install` method
//! `bundle`, ADR 0024 decision 3) against an in-process daemon: a mock of GitHub (the release
//! list, the archive, the tap's cask), a fake bundle in a temporary folder, a fake `codesign`
//! and `open`, a folder as the Trash. Nothing here touches `/Applications`, the user's Trash,
//! GitHub or the user's Hutan.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;
use crate::update::{MANUAL, RELEASES, RUNNING, executable, status, status_where};

use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{TestDaemon, remote_code, wait_event};
use hutan_client::Client;
use hutan_daemon::files::confine::FsError;
use hutan_daemon::files::trash::{FolderTrash, Trash};
use hutan_daemon::update::{Tools, UpdateTuning};
use hutan_gateway::testing::{MockServer, Reply, Step};
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::Event;
use hutan_protocol::update::{InstallMethod, InstallStage, InstallState, UpdateInstall};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

const TAG: &str = "v0.3.2";
const VERSION: &str = "0.3.2";
const ARCHIVE: &str = "/robertt3kuk/hutan/releases/download/v0.3.2/Hutan-0.3.2-arm64.zip";
const CASK_AT_TAG: &str = "/robertt3kuk/homebrew-hutan/v0.3.2/Casks/hutan.rb";
const CASK_ON_MAIN: &str = "/robertt3kuk/homebrew-hutan/main/Casks/hutan.rb";

// ---- fixtures ---------------------------------------------------------------------------------

fn info_plist(id: &str, version: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>{id}</string><key>CFBundleShortVersionString</key><string>{version}</string></dict></plist>\n"
    )
}

/// A fake `Hutan.app` at `dir/Hutan.app`; `marker` says which one it is.
fn make_bundle(dir: &Path, id: &str, version: &str, marker: &str) -> PathBuf {
    let bundle = dir.join("Hutan.app");
    std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
    std::fs::create_dir_all(bundle.join("Contents/Resources")).unwrap();
    std::fs::write(bundle.join("Contents/MacOS/hutand"), b"#!/bin/sh\n").unwrap();
    std::fs::write(bundle.join("Contents/Info.plist"), info_plist(id, version)).unwrap();
    std::fs::write(bundle.join("Contents/Resources/marker.txt"), marker).unwrap();
    bundle
}

/// The archive of `bundle`, made the way `xtask release` does (`ditto -c -k --keepParent`).
fn zip_of(bundle: &Path, out: &Path) -> Vec<u8> {
    let status = std::process::Command::new("/usr/bin/ditto")
        .args(["-c", "-k", "--keepParent"])
        .arg(bundle)
        .arg(out)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(out).unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn bytes_reply(bytes: &[u8]) -> Reply {
    let mut reply = Reply::fixed(200, "application/zip", "");
    reply.steps = vec![Step::Send(bytes.to_vec())];
    reply
}

fn cask(version: &str, digest: &str) -> String {
    format!(
        "cask \"hutan\" do\n  version \"{version}\"\n  sha256 \"{digest}\"\n\n  app \"Hutan.app\"\nend\n"
    )
}

fn marker(bundle: &Path) -> String {
    std::fs::read_to_string(bundle.join("Contents/Resources/marker.txt")).unwrap_or_default()
}

/// A Trash that cannot be written to.
#[derive(Debug)]
struct FullTrash;

impl Trash for FullTrash {
    fn trash(&self, path: &Path) -> Result<PathBuf, FsError> {
        Err(FsError::unsupported(
            "no_trash",
            format!(
                "{} cannot be moved to the Trash: the Trash is full",
                path.display()
            ),
        ))
    }
}

/// Gives a folder its permissions back when a test ends, whatever happens.
struct Restore(PathBuf);

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// What the daemon is given to work with.
struct Env {
    _dir: TempDir,
    /// The folder the old `Hutan.app` lives in.
    apps: PathBuf,
    /// The old bundle, the one that gets replaced.
    old: PathBuf,
    trash: PathBuf,
    /// `codesign` and `open` (symlinks to fakes) and their logs.
    bin: PathBuf,
    server: MockServer,
    archive: Vec<u8>,
}

impl Env {
    /// A mock of GitHub that serves release 0.3.2 with its archive and the tap's cask at its
    /// tag, all consistent; a test breaks what it wants to break.
    async fn new() -> Self {
        Self::with_archive(make_bundle_for(VERSION, "dev.hutan.Hutan")).await
    }

    async fn with_archive(archive: Vec<u8>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let apps = root.join("Copied");
        let old = make_bundle(&apps, "dev.hutan.Hutan", RUNNING, "old");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        symlink(
            executable(
                "codesign-ok",
                "echo \"$@\" >> \"$(dirname \"$0\")/codesign.log\"\nexit 0",
            ),
            bin.join("codesign"),
        )
        .unwrap();
        symlink(
            executable(
                "open-ok",
                "echo \"$@\" >> \"$(dirname \"$0\")/open.log\"\nexit 0",
            ),
            bin.join("open"),
        )
        .unwrap();
        let env = Self {
            trash: root.join("trash"),
            _dir: dir,
            apps,
            old,
            bin,
            server: MockServer::start().await,
            archive,
        };
        env.serve_release(Some(sha256(&env.archive)), env.archive.len());
        env.server
            .set_route("GET", ARCHIVE, bytes_reply(&env.archive));
        env.serve_cask(CASK_AT_TAG, VERSION, &sha256(&env.archive));
        env
    }

    fn serve_release(&self, digest: Option<String>, size: usize) {
        let mut asset = json!({
            "name": format!("Hutan-{VERSION}-arm64.zip"),
            "size": size,
            "state": "uploaded",
        });
        if let Some(digest) = digest {
            asset["digest"] = json!(format!("sha256:{digest}"));
        }
        let release = json!([{
            "tag_name": TAG,
            "draft": false,
            "prerelease": true,
            "published_at": "2026-10-29T10:00:00Z",
            "html_url": format!("https://github.com/robertt3kuk/hutan/releases/tag/{TAG}"),
            "body": "## Hutan 0.3.2",
            "assets": [asset],
        }]);
        self.server.set_route(
            "GET",
            RELEASES,
            Reply::json(200, &serde_json::to_string(&release).unwrap()),
        );
    }

    fn serve_cask(&self, path: &str, version: &str, digest: &str) {
        self.server.set_route(
            "GET",
            path,
            Reply::fixed(200, "text/plain", &cask(version, digest)),
        );
    }

    fn logged(&self, name: &str) -> String {
        std::fs::read_to_string(self.bin.join(name)).unwrap_or_default()
    }

    fn tuning(&self, change: impl FnOnce(&mut UpdateTuning)) -> UpdateTuning {
        let mut tuning = UpdateTuning {
            api_base: Some(self.server.url()),
            version: RUNNING.to_owned(),
            exe: Some(self.old.join("Contents/MacOS/hutand")),
            brew_prefixes: vec![self.apps.join("no-brew-here")],
            startup_delay: Duration::ZERO,
            download_base: self.server.url(),
            tap_base: self.server.url(),
            tools: Tools {
                ditto: PathBuf::from("/usr/bin/ditto"),
                codesign: self.bin.join("codesign"),
                open: self.bin.join("open"),
            },
            trash: Arc::new(FolderTrash::new(&self.trash)),
            quit_timeout: Duration::from_secs(10),
            ..UpdateTuning::default()
        };
        change(&mut tuning);
        tuning
    }

    async fn start(&self) -> TestDaemon {
        self.start_tuned(|_| {}).await
    }

    async fn start_tuned(&self, change: impl FnOnce(&mut UpdateTuning)) -> TestDaemon {
        let tuning = self.tuning(change);
        TestDaemon::start_with(MANUAL, move |config| config.updates = tuning).await
    }
}

/// The archive of a bundle that says it is Hutan `version` with identifier `id`.
fn make_bundle_for(version: &str, id: &str) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let bundle = make_bundle(dir.path(), id, version, "new");
    zip_of(&bundle, &dir.path().join("Hutan.zip"))
}

fn staging(t: &TestDaemon) -> PathBuf {
    t.paths.base.join("update-staging")
}

/// The install the stream shows next, whatever its state.
async fn next_install(
    events: &mut tokio::sync::broadcast::Receiver<(hutan_core::Seq, Event)>,
) -> UpdateInstall {
    wait_event(events, "update.install", |event| match event {
        Event::UpdateInstallChanged { install } => Some((**install).clone()),
        _ => None,
    })
    .await
}

/// Reads install events until the stage is `until` or the install failed; the stages in the
/// order they were first seen, and the last event.
async fn stages_until(
    events: &mut tokio::sync::broadcast::Receiver<(hutan_core::Seq, Event)>,
    until: InstallStage,
) -> (Vec<InstallStage>, UpdateInstall) {
    let mut stages = Vec::new();
    loop {
        let install = next_install(events).await;
        if let Some(stage) = install.stage
            && stages.last() != Some(&stage)
        {
            stages.push(stage);
        }
        if install.stage == Some(until) || install.state == InstallState::Failed {
            return (stages, install);
        }
    }
}

/// A daemon that found the release and was asked to update a bundle; returns the failed install.
async fn refused(env: &Env) -> (TestDaemon, UpdateInstall) {
    let t = env.start().await;
    let (client, mut events) = t.connect_subscribed().await;
    client.update_check().await.unwrap();
    client.update_install(InstallMethod::Bundle).await.unwrap();
    let (_, last) = stages_until(&mut events, InstallStage::Ready).await;
    assert_eq!(last.state, InstallState::Failed, "{last:?}");
    let status = status(&client).await;
    assert_eq!(status.install.as_ref(), Some(&last));
    (t, last)
}

fn assert_old_app_untouched(env: &Env) {
    assert_eq!(marker(&env.old), "old", "the old bundle was not touched");
    assert!(
        !env.trash.exists() || std::fs::read_dir(&env.trash).unwrap().next().is_none(),
        "nothing went to the Trash"
    );
    assert_eq!(env.logged("open.log"), "", "nothing was opened");
}

// ---- preparing --------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_archive_is_downloaded_verified_unpacked_and_checked_before_anything_changes() {
    let env = Env::new().await;
    let t = env.start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let checked = client.update_check().await.unwrap();
    assert_eq!(checked.method, InstallMethod::Bundle);
    assert_eq!(checked.install_path.as_deref(), env.old.to_str());

    let started = client.update_install(InstallMethod::Bundle).await.unwrap();
    assert!(started.session.is_none(), "no terminal runs");
    assert_eq!(started.install.method, InstallMethod::Bundle);
    assert_eq!(started.install.state, InstallState::Running);
    assert_eq!(started.install.stage, Some(InstallStage::Downloading));
    assert_eq!(started.install.total, Some(env.archive.len() as u64));
    assert_eq!(started.install.command, "");

    let (stages, ready) = stages_until(&mut events, InstallStage::Ready).await;
    assert_eq!(
        stages,
        [
            InstallStage::Downloading,
            InstallStage::Verifying,
            InstallStage::Extracting,
            InstallStage::Checking,
            InstallStage::Ready,
        ]
    );
    assert_eq!(ready.state, InstallState::Running);
    assert_eq!(ready.downloaded, Some(env.archive.len() as u64));
    assert_eq!(ready.message, None);
    assert_eq!(ready.manual_command, None);
    assert_eq!(status(&client).await.install, Some(ready));

    // codesign was asked to verify the unpacked bundle, deeply and strictly.
    let unpacked = staging(&t).join("extracted/Hutan.app");
    assert_eq!(
        env.logged("codesign.log"),
        format!("--verify --deep --strict {}\n", unpacked.display())
    );
    assert_eq!(marker(&unpacked), "new");
    assert!(
        !staging(&t).join("Hutan-0.3.2-arm64.zip").exists(),
        "the archive is not kept once unpacked"
    );
    // Nothing in place changed.
    assert_old_app_untouched(&env);

    // Only a User-Agent went to the download and the tap.
    let download = &env.server.requests_to(ARCHIVE)[0];
    assert_eq!(download.header("user-agent"), Some("Hutan/0.3.1"));
    let tap = &env.server.requests_to(CASK_AT_TAG)[0];
    assert_eq!(tap.header("user-agent"), Some("Hutan/0.3.1"));
    for request in [download, tap] {
        for secret in ["authorization", "cookie"] {
            assert_eq!(request.header(secret), None);
        }
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_download_is_reported_while_it_arrives() {
    let env = Env::new().await;
    // Half of the archive, then the server holds the connection: the stage and the count are
    // what the daemon has right now.
    let half = env.archive.len() / 2;
    let mut reply = Reply::ndjson(Vec::<String>::new());
    reply.headers = vec![("content-type".into(), "application/zip".into())];
    reply.steps = vec![Step::Send(env.archive[..half].to_vec()), Step::Hold];
    env.server.set_route("GET", ARCHIVE, reply);
    let t = env.start().await;
    let client = t.connect().await;
    client.update_check().await.unwrap();
    client.update_install(InstallMethod::Bundle).await.unwrap();
    let partial = status_where(&client, "half of the archive", |s| {
        s.install.as_ref().and_then(|i| i.downloaded) == Some(half as u64)
    })
    .await
    .install
    .unwrap();
    assert_eq!(partial.stage, Some(InstallStage::Downloading));
    assert_eq!(partial.state, InstallState::Running);
    assert_eq!(partial.total, Some(env.archive.len() as u64));
    // A second request while it runs is refused; the daemon stops with the download going.
    assert_eq!(
        remote_code(client.update_install(InstallMethod::Bundle).await),
        ErrorCode::InvalidState
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_digest_that_github_does_not_list_refuses() {
    let env = Env::new().await;
    env.serve_release(Some("cd".repeat(32)), env.archive.len());
    let (t, failed) = refused(&env).await;
    assert_eq!(failed.stage, Some(InstallStage::Verifying));
    let message = failed.message.unwrap();
    assert!(message.contains("not the one GitHub lists"), "{message}");
    assert!(message.contains("nothing was installed"), "{message}");
    assert_eq!(
        env.logged("codesign.log"),
        "",
        "nothing was unpacked or checked"
    );
    assert!(!staging(&t).exists(), "the staging folder is removed");
    assert_old_app_untouched(&env);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_release_without_a_github_digest_refuses() {
    let env = Env::new().await;
    env.serve_release(None, env.archive.len());
    let (t, failed) = refused(&env).await;
    assert_eq!(failed.stage, Some(InstallStage::Verifying));
    assert!(failed.message.unwrap().contains("GitHub lists no SHA-256"),);
    assert_old_app_untouched(&env);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_digest_that_the_tap_does_not_list_refuses() {
    let env = Env::new().await;
    // GitHub's digest is right; the tap lists another.
    env.serve_cask(CASK_AT_TAG, VERSION, &"ef".repeat(32));
    let (t, failed) = refused(&env).await;
    assert_eq!(failed.stage, Some(InstallStage::Verifying));
    let message = failed.message.unwrap();
    assert!(message.contains("tap's Casks/hutan.rb"), "{message}");
    assert_eq!(env.logged("codesign.log"), "");
    assert!(!staging(&t).exists());
    assert_old_app_untouched(&env);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tap_that_has_no_cask_or_one_for_another_version_refuses() {
    // No cask at the tag, none on main.
    let env = Env::new().await;
    env.server
        .set_route("GET", CASK_AT_TAG, Reply::json(404, "{}"));
    let (t, failed) = refused(&env).await;
    assert_eq!(failed.stage, Some(InstallStage::Verifying));
    let message = failed.message.unwrap();
    assert!(
        message.contains("neither at its tag nor on main"),
        "{message}"
    );
    assert_old_app_untouched(&env);
    t.stop().await;

    // The tap moved on: its main is another version.
    let env = Env::new().await;
    env.server
        .set_route("GET", CASK_AT_TAG, Reply::json(404, "{}"));
    env.serve_cask(CASK_ON_MAIN, "0.3.3", &sha256(&env.archive));
    let (t, failed) = refused(&env).await;
    let message = failed.message.unwrap();
    assert!(
        message.contains("is for Hutan 0.3.3, not 0.3.2"),
        "{message}"
    );
    assert_old_app_untouched(&env);
    t.stop().await;

    // The tap cannot be reached at all: the download cannot be checked, so it is refused.
    let env = Env::new().await;
    env.server
        .set_route("GET", CASK_AT_TAG, Reply::json(503, "{}"));
    let (t, failed) = refused(&env).await;
    assert!(failed.message.unwrap().contains("HTTP 503"));
    assert_old_app_untouched(&env);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tap_without_a_tag_is_read_on_main() {
    let env = Env::new().await;
    env.server
        .set_route("GET", CASK_AT_TAG, Reply::json(404, "{}"));
    env.serve_cask(CASK_ON_MAIN, VERSION, &sha256(&env.archive));
    let t = env.start().await;
    let (client, mut events) = t.connect_subscribed().await;
    client.update_check().await.unwrap();
    client.update_install(InstallMethod::Bundle).await.unwrap();
    let (_, ready) = stages_until(&mut events, InstallStage::Ready).await;
    assert_eq!(ready.state, InstallState::Running, "{ready:?}");
    assert_eq!(
        env.server.requests_to(CASK_AT_TAG).len(),
        1,
        "the tag was tried first"
    );
    assert_eq!(env.server.requests_to(CASK_ON_MAIN).len(), 1);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signature_macos_rejects_refuses() {
    let env = Env::new().await;
    std::fs::remove_file(env.bin.join("codesign")).unwrap();
    symlink(
        executable(
            "codesign-bad",
            "echo \"$@\" >> \"$(dirname \"$0\")/codesign.log\"\necho 'a sealed resource is missing or invalid' >&2\nexit 1",
        ),
        env.bin.join("codesign"),
    )
    .unwrap();
    let (t, failed) = refused(&env).await;
    assert_eq!(failed.stage, Some(InstallStage::Checking));
    let message = failed.message.unwrap();
    assert!(message.contains("signature"), "{message}");
    assert!(
        message.contains("a sealed resource is missing or invalid"),
        "{message}"
    );
    assert!(
        env.logged("codesign.log")
            .contains("--verify --deep --strict")
    );
    assert!(!staging(&t).exists());
    assert_old_app_untouched(&env);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_archive_that_is_not_this_release_of_hutan_refuses() {
    // The archive says 0.3.1 (a downgrade) although the release says 0.3.2.
    let env = Env::with_archive(make_bundle_for("0.3.1", "dev.hutan.Hutan")).await;
    let (t, failed) = refused(&env).await;
    assert_eq!(failed.stage, Some(InstallStage::Checking));
    let message = failed.message.unwrap();
    assert!(
        message.contains("holds Hutan 0.3.1, not the 0.3.2"),
        "{message}"
    );
    assert_old_app_untouched(&env);
    t.stop().await;

    // Another app altogether.
    let env = Env::with_archive(make_bundle_for(VERSION, "com.example.Other")).await;
    let (t, failed) = refused(&env).await;
    assert!(failed.message.unwrap().contains("not Hutan"));
    assert_old_app_untouched(&env);
    t.stop().await;

    // Not an archive at all (its digests are right).
    let env = Env::with_archive(b"this is not a zip".to_vec()).await;
    let (t, failed) = refused(&env).await;
    assert_eq!(failed.stage, Some(InstallStage::Extracting));
    assert!(failed.message.unwrap().contains("could not be unpacked"));
    assert_old_app_untouched(&env);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_over_the_limit_is_refused() {
    let env = Env::new().await;
    let limit = env.archive.len() as u64 - 1;
    // The release lists the size: refused before a byte is asked for.
    let t = env.start_tuned(|tuning| tuning.max_archive = limit).await;
    let (client, mut events) = t.connect_subscribed().await;
    client.update_check().await.unwrap();
    client.update_install(InstallMethod::Bundle).await.unwrap();
    let (_, failed) = stages_until(&mut events, InstallStage::Ready).await;
    assert_eq!(failed.state, InstallState::Failed);
    assert_eq!(failed.stage, Some(InstallStage::Downloading));
    assert!(failed.message.unwrap().contains("more than the"), "bounded");
    assert_eq!(env.server.requests_to(ARCHIVE).len(), 0);
    t.stop().await;

    // The release lists no size: the server's own length is what is refused.
    let env = Env::new().await;
    env.serve_release(Some(sha256(&env.archive)), 0);
    let limit = env.archive.len() as u64 - 1;
    let t = env.start_tuned(|tuning| tuning.max_archive = limit).await;
    let (client, mut events) = t.connect_subscribed().await;
    client.update_check().await.unwrap();
    client.update_install(InstallMethod::Bundle).await.unwrap();
    let (_, failed) = stages_until(&mut events, InstallStage::Ready).await;
    assert_eq!(failed.state, InstallState::Failed);
    assert!(failed.message.unwrap().contains("more than the"));
    assert!(!staging(&t).join("Hutan-0.3.2-arm64.zip").exists());
    assert_old_app_untouched(&env);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_the_server_refuses_says_so_and_can_be_tried_again() {
    let env = Env::new().await;
    env.server.set_route("GET", ARCHIVE, Reply::json(404, "{}"));
    let (t, failed) = refused(&env).await;
    assert_eq!(failed.stage, Some(InstallStage::Downloading));
    assert!(failed.message.unwrap().contains("HTTP 404"));
    // Fixed upstream: the same daemon tries again.
    env.server
        .set_route("GET", ARCHIVE, bytes_reply(&env.archive));
    let (client, mut events) = t.connect_subscribed().await;
    client.update_install(InstallMethod::Bundle).await.unwrap();
    let (_, ready) = stages_until(&mut events, InstallStage::Ready).await;
    assert_eq!(ready.state, InstallState::Running);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redirected_download_is_followed() {
    let env = Env::new().await;
    let moved = "/objects/Hutan-0.3.2-arm64.zip";
    env.server.set_route(
        "GET",
        ARCHIVE,
        Reply::json(302, "").header("location", moved),
    );
    env.server
        .set_route("GET", moved, bytes_reply(&env.archive));
    let t = env.start().await;
    let (client, mut events) = t.connect_subscribed().await;
    client.update_check().await.unwrap();
    client.update_install(InstallMethod::Bundle).await.unwrap();
    let (_, ready) = stages_until(&mut events, InstallStage::Ready).await;
    assert_eq!(ready.state, InstallState::Running, "{ready:?}");
    assert_eq!(env.server.requests_to(moved).len(), 1);
    t.stop().await;
}

// ---- installing -------------------------------------------------------------------------------

/// A daemon whose bundle update is ready.
async fn ready(
    env: &Env,
    change: impl FnOnce(&mut UpdateTuning),
) -> (
    TestDaemon,
    Client,
    tokio::sync::broadcast::Receiver<(hutan_core::Seq, Event)>,
) {
    let t = env.start_tuned(change).await;
    let (client, mut events) = t.connect_subscribed().await;
    client.update_check().await.unwrap();
    client.update_install(InstallMethod::Bundle).await.unwrap();
    let (_, last) = stages_until(&mut events, InstallStage::Ready).await;
    assert_eq!(last.stage, Some(InstallStage::Ready), "{last:?}");
    assert_eq!(last.state, InstallState::Running);
    (t, client, events)
}

async fn until_state(
    events: &mut tokio::sync::broadcast::Receiver<(hutan_core::Seq, Event)>,
    state: InstallState,
) -> UpdateInstall {
    wait_event(events, "the install to end", |event| match event {
        Event::UpdateInstallChanged { install } if install.state == state => {
            Some((**install).clone())
        }
        _ => None,
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn after_the_app_quits_the_old_bundle_goes_to_the_trash_and_the_new_one_opens() {
    let env = Env::new().await;
    let (t, client, mut events) = ready(&env, |_| {}).await;

    // The desktop is a process that is still running.
    let mut app = std::process::Command::new("/bin/sleep")
        .arg("600")
        .spawn()
        .unwrap();
    let applying = client.update_install_apply(Some(app.id())).await.unwrap();
    assert_eq!(applying.install.stage, Some(InstallStage::Installing));
    assert_eq!(applying.install.state, InstallState::Running);
    let now = status(&client).await.install.unwrap();
    assert_eq!(now.stage, Some(InstallStage::Installing));
    assert_eq!(marker(&env.old), "old", "nothing moves while the app runs");
    assert_eq!(env.logged("open.log"), "");

    // It quits: the swap follows.
    app.kill().unwrap();
    app.wait().unwrap();
    let done = until_state(&mut events, InstallState::Succeeded).await;
    assert_eq!(done.stage, Some(InstallStage::Installed));
    assert_eq!(done.message, None);

    assert_eq!(marker(&env.old), "new", "the new bundle is in place");
    assert_eq!(
        marker(&env.trash.join("0/Hutan.app")),
        "old",
        "the old one is in the Trash"
    );
    assert_eq!(
        env.logged("open.log"),
        format!("-n {}\n", env.old.display()),
        "the new app was opened"
    );
    assert!(!staging(&t).exists(), "the staging folder is gone");
    let leftovers: Vec<_> = std::fs::read_dir(&env.apps)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        leftovers,
        ["Hutan.app"],
        "no half-made copy is left beside it"
    );
    // That release is installed: nothing newer is offered by the old daemon.
    let after = status(&client).await;
    assert_eq!(after.available, None);
    assert_eq!(after.install.unwrap().state, InstallState::Succeeded);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_process_to_wait_for_the_swap_is_made_at_once_and_opens_nothing() {
    let env = Env::new().await;
    let (t, client, mut events) = ready(&env, |_| {}).await;
    client.update_install_apply(None).await.unwrap();
    let done = until_state(&mut events, InstallState::Succeeded).await;
    assert_eq!(done.stage, Some(InstallStage::Installed));
    assert_eq!(marker(&env.old), "new");
    assert_eq!(
        env.logged("open.log"),
        "",
        "a second window is never opened beside a running app"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn installing_asks_for_a_ready_update_and_nothing_else() {
    let env = Env::new().await;
    let t = env.start().await;
    let (client, mut events) = t.connect_subscribed().await;
    // Nothing prepared.
    assert_eq!(
        remote_code(client.update_install_apply(None).await),
        ErrorCode::InvalidState
    );
    client.update_check().await.unwrap();
    assert_eq!(
        remote_code(client.update_install_apply(None).await),
        ErrorCode::InvalidState
    );
    // The daemon's own pid is never something to wait for.
    assert_eq!(
        remote_code(client.update_install_apply(Some(std::process::id())).await),
        ErrorCode::BadRequest
    );
    // Prepared and applied: a second apply finds nothing ready.
    client.update_install(InstallMethod::Bundle).await.unwrap();
    stages_until(&mut events, InstallStage::Ready).await;
    client.update_install_apply(None).await.unwrap();
    until_state(&mut events, InstallState::Succeeded).await;
    assert_eq!(
        remote_code(client.update_install_apply(None).await),
        ErrorCode::InvalidState
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_that_cannot_be_written_shows_the_manual_command_and_changes_nothing() {
    let env = Env::new().await;
    let (t, client, _events) = ready(&env, |_| {}).await;
    // The folder became read-only after the daemon started.
    let _restore = Restore(env.apps.clone());
    std::fs::set_permissions(&env.apps, std::fs::Permissions::from_mode(0o555)).unwrap();

    let error = client.update_install_apply(None).await.unwrap_err();
    assert_eq!(remote_code::<()>(Err(error)), ErrorCode::InvalidState);
    let install = status(&client).await.install.unwrap();
    assert_eq!(install.state, InstallState::Failed);
    assert_eq!(install.stage, Some(InstallStage::Ready));
    assert!(install.message.unwrap().contains("cannot write to"));
    let command = install.manual_command.unwrap();
    let unpacked = staging(&t).join("extracted/Hutan.app");
    assert!(
        command.contains(&unpacked.to_string_lossy().into_owned()),
        "{command}"
    );
    assert!(
        command.contains(&env.old.to_string_lossy().into_owned()),
        "{command}"
    );
    assert!(command.starts_with("sudo mv "), "{command}");
    assert!(
        unpacked.exists(),
        "the verified bundle is kept for the command"
    );
    assert_old_app_untouched(&env);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_that_is_not_writable_when_the_update_is_ready_offers_the_command_instead() {
    let env = Env::new().await;
    let t = env.start().await;
    let (client, mut events) = t.connect_subscribed().await;
    client.update_check().await.unwrap();
    let _restore = Restore(env.apps.clone());
    std::fs::set_permissions(&env.apps, std::fs::Permissions::from_mode(0o555)).unwrap();
    client.update_install(InstallMethod::Bundle).await.unwrap();
    let (_, last) = stages_until(&mut events, InstallStage::Ready).await;
    assert_eq!(last.state, InstallState::Failed);
    assert_eq!(last.stage, Some(InstallStage::Ready));
    assert!(last.message.unwrap().contains("verified"));
    assert!(last.manual_command.unwrap().starts_with("sudo mv "));
    assert!(staging(&t).join("extracted/Hutan.app").exists());
    assert_old_app_untouched(&env);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_swap_that_fails_leaves_the_old_bundle_in_place_and_reopens_it() {
    let env = Env::new().await;
    let (t, client, mut events) = ready(&env, |tuning| tuning.trash = Arc::new(FullTrash)).await;
    let mut app = std::process::Command::new("/bin/sleep")
        .arg("600")
        .spawn()
        .unwrap();
    client.update_install_apply(Some(app.id())).await.unwrap();
    app.kill().unwrap();
    app.wait().unwrap();
    let failed = until_state(&mut events, InstallState::Failed).await;
    assert_eq!(failed.stage, Some(InstallStage::Installing));
    let message = failed.message.unwrap();
    assert!(
        message.contains("could not be moved to the Trash"),
        "{message}"
    );
    assert!(failed.manual_command.is_some());
    assert_eq!(marker(&env.old), "old", "the old bundle is where it was");
    let leftovers: Vec<_> = std::fs::read_dir(&env.apps)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(leftovers, ["Hutan.app"], "the half-made copy is removed");
    // The window the user closed for the update comes back, as the old app (the daemon says
    // the update failed first, then opens it).
    let expected = format!("-n {}\n", env.old.display());
    tokio::time::timeout(common::WAIT, async {
        while env.logged("open.log") != expected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the old app is opened again");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_that_does_not_quit_in_time_stops_the_update_without_a_change() {
    let env = Env::new().await;
    let (t, client, mut events) = ready(&env, |tuning| {
        tuning.quit_timeout = Duration::from_millis(200);
    })
    .await;
    let mut app = std::process::Command::new("/bin/sleep")
        .arg("600")
        .spawn()
        .unwrap();
    client.update_install_apply(Some(app.id())).await.unwrap();
    let failed = until_state(&mut events, InstallState::Failed).await;
    app.kill().unwrap();
    app.wait().unwrap();
    assert!(failed.message.unwrap().contains("did not quit in time"));
    assert_old_app_untouched(&env);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_can_never_update_a_bundle() {
    use hutan_protocol::messages::{AgentApplied, AgentReportRequest, AgentState, Origin, Request};

    let env = Env::new().await;
    let tuning = env.tuning(|_| {});
    let t = TestDaemon::start_with(
        "[updates]\ncheck = \"off\"\n[control]\nagents = \"allow\"\n",
        move |config| config.updates = tuning,
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
    assert_eq!(
        remote_code(agent.update_install(InstallMethod::Bundle).await),
        ErrorCode::Forbidden
    );
    assert_eq!(
        remote_code(agent.update_install_apply(None).await),
        ErrorCode::Forbidden
    );
    assert_eq!(
        env.server.requests_to(ARCHIVE).len(),
        0,
        "nothing was downloaded"
    );
    t.stop().await;
}
