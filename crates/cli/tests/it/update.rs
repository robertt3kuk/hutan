//! `hutan update status|check|install` (protocol 0.60, ADR 0024) against an in-process daemon
//! that checks a local mock of GitHub's releases API and runs a fake `brew`: nothing here
//! contacts GitHub, runs a real `brew` or touches `/Applications`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::PathBuf;
use std::time::Duration;

use common::TestDaemon;
use hutan_daemon::update::UpdateTuning;
use hutan_gateway::testing::{MockServer, Reply};
use serde_json::json;
use tempfile::TempDir;

const RELEASES: &str = "/repos/robertt3kuk/hutan/releases";

fn serve(server: &MockServer, tags: &[&str]) {
    let releases: Vec<_> = tags
        .iter()
        .map(|tag| {
            let version = tag.trim_start_matches('v');
            json!({
                "tag_name": tag,
                "prerelease": true,
                "html_url": format!("https://github.com/robertt3kuk/hutan/releases/tag/{tag}"),
                "body": "notes",
                "assets": [{"name": format!("Hutan-{version}-arm64.zip"), "size": 9}],
            })
        })
        .collect();
    server.set_route(
        "GET",
        RELEASES,
        Reply::json(200, &serde_json::to_string(&releases).unwrap()),
    );
}

/// A fake `brew` at a stable path named by its content (macOS vets every new executable once).
fn fake_brew(code: i32) -> PathBuf {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let text = format!("#!/bin/sh\necho \"$@\" >> \"$(dirname \"$0\")/brew.log\"\nexit {code}\n");
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let dir = std::env::temp_dir().join("hutan-test-fake-clis");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{:016x}-cli-brew-{code}", hasher.finish()));
    if std::fs::read_to_string(&path).is_ok_and(|have| have == text) {
        return path;
    }
    let staging = dir.join(format!(".{}-cli-brew-{code}", std::process::id()));
    std::fs::write(&staging, &text).unwrap();
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&staging, &path).unwrap();
    path
}

/// A cask layout (`Caskroom` links to an `Applications/Hutan.app`) with the fake `brew` in its
/// prefix; `None` is a development build.
struct Layout {
    _dir: TempDir,
    prefix: PathBuf,
    exe: PathBuf,
}

fn layout(brew_code: i32, cask: bool) -> Layout {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let prefix = root.join("prefix");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    symlink(fake_brew(brew_code), prefix.join("bin/brew")).unwrap();
    let exe = if cask {
        let bundle = root.join("Applications/Hutan.app");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        let staged = prefix.join("Caskroom/hutan/0.3.1");
        std::fs::create_dir_all(&staged).unwrap();
        symlink(&bundle, staged.join("Hutan.app")).unwrap();
        bundle.join("Contents/MacOS/hutand")
    } else {
        root.join("hutand")
    };
    std::fs::write(&exe, b"").unwrap();
    Layout {
        _dir: dir,
        prefix,
        exe,
    }
}

async fn daemon(server: &MockServer, layout: &Layout) -> TestDaemon {
    let (api, exe, prefix) = (server.url(), layout.exe.clone(), layout.prefix.clone());
    // Checking is only on request: the commands are the sync points.
    TestDaemon::start_toml("[updates]\ncheck = \"off\"\n", move |config| {
        config.updates = UpdateTuning {
            api_base: Some(api),
            version: "0.3.1".to_owned(),
            exe: Some(exe),
            brew_prefixes: vec![prefix],
            startup_delay: Duration::ZERO,
            ..UpdateTuning::default()
        };
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn status_contacts_nothing_and_says_how_hutan_was_installed() {
    let server = MockServer::start().await;
    let layout = layout(0, false);
    let t = daemon(&server, &layout).await;
    let text = t.hutan(&["update", "status"]).await.ok().stdout;
    assert!(text.contains("Hutan 0.3.1"), "{text}");
    assert!(text.contains("not a known install"), "{text}");
    assert!(text.contains("only when asked"), "{text}");
    assert!(text.contains("no check yet"), "{text}");
    let json = t.hutan(&["update", "status", "--json"]).await.ok().json();
    assert_eq!(json["version"], "0.3.1");
    assert_eq!(json["method"], "unknown");
    assert_eq!(json["check"], "off");
    assert_eq!(server.requests().len(), 0);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn check_finds_the_newer_release_and_says_how_to_get_it() {
    let server = MockServer::start().await;
    serve(&server, &["v0.3.0", "v0.3.2"]);
    let layout = layout(0, true);
    let t = daemon(&server, &layout).await;
    let text = t.hutan(&["update", "check"]).await.ok().stdout;
    assert!(text.contains("Homebrew cask"), "{text}");
    assert!(
        text.contains("update available: Hutan 0.3.2 (pre-release)"),
        "{text}"
    );
    assert!(text.contains("releases/tag/v0.3.2"), "{text}");
    assert!(text.contains("hutan update install"), "{text}");
    let json = t.hutan(&["update", "check", "--json"]).await.ok().json();
    assert_eq!(json["available"]["version"], "0.3.2");
    assert_eq!(json["last_check"]["outcome"], "checked");
    assert_eq!(server.requests().len(), 2);

    // Nothing newer: up to date. A server that fails is said, not an error.
    serve(&server, &["v0.3.1"]);
    let text = t.hutan(&["update", "check"]).await.ok().stdout;
    assert!(text.contains("up to date"), "{text}");
    server.set_route("GET", RELEASES, Reply::json(500, "{}"));
    let text = t.hutan(&["update", "check"]).await.ok().stdout;
    assert!(
        text.contains("could not finish: GitHub answered HTTP 500"),
        "{text}"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn install_runs_brew_and_wait_reports_how_it_ended() {
    let server = MockServer::start().await;
    serve(&server, &["v0.3.2"]);

    let ok = layout(0, true);
    let t = daemon(&server, &ok).await;
    t.hutan(&["update", "check"]).await.ok();
    let out = t.hutan(&["update", "install", "--wait"]).await.ok();
    assert!(
        out.stdout
            .contains("running `brew update && brew upgrade --cask"),
        "{out:?}"
    );
    assert!(out.stdout.contains("Hutan 0.3.2 is installed"), "{out:?}");
    let log = std::fs::read_to_string(ok.prefix.join("bin/brew.log")).unwrap();
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        ["update", "upgrade --cask robertt3kuk/hutan/hutan"]
    );
    let status = t.hutan(&["update", "status", "--json"]).await.ok().json();
    assert_eq!(status["install"]["state"], "succeeded");
    t.stop().await;

    let failing = layout(3, true);
    let t = daemon(&server, &failing).await;
    t.hutan(&["update", "check"]).await.ok();
    let out = t.hutan(&["update", "install", "--wait", "--json"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    let last: serde_json::Value = serde_json::from_str(out.stdout.lines().last().unwrap()).unwrap();
    assert_eq!(last["state"], "failed");
    assert_eq!(last["exit_code"], 3);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn install_without_wait_names_the_session_and_a_hand_made_install_is_refused() {
    let server = MockServer::start().await;
    serve(&server, &["v0.3.2"]);
    let cask = layout(0, true);
    let t = daemon(&server, &cask).await;
    t.hutan(&["update", "check"]).await.ok();
    let out = t.hutan(&["update", "install"]).await.ok();
    assert!(out.stdout.contains("hutan attach"), "{out:?}");
    t.stop().await;

    let dev = layout(0, false);
    let t = daemon(&server, &dev).await;
    t.hutan(&["update", "check"]).await.ok();
    let out = t.hutan(&["update", "install"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stderr.contains("not installed in a way it can update"),
        "{out:?}"
    );
    t.stop().await;
}
