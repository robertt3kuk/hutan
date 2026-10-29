#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! GPUI tests of updates in the window (UPDATES-1, protocol 0.60, ADR 0024): the *Update to x.y.z*
//! status item, the sheet with the release notes and its three answers, *Skip this version*,
//! *Update now* running a fake `brew` and then *Relaunch*, *Check for Updates…*, and the rows in
//! Settings → General. The daemon is a real one in this process that checks a local mock of
//! GitHub's releases API and runs a fake `brew` found first on `PATH`; no test contacts GitHub,
//! runs a real `brew` or opens an app.

use crate::common;

use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{TestDaemon, click_at, draw, open_options, settle, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_daemon::update::UpdateTuning;
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::update::Relauncher;
use hutan_desktop::workbench::{Region, Workbench, WorkbenchOptions};
use hutan_gateway::testing::{MockServer, Reply};
use hutan_protocol::update::{InstallState, UpdateCheck};
use hutan_settings::{SettingsPaths, SettingsStore};
use serde_json::json;
use tempfile::TempDir;

pub(crate) const RELEASES: &str = "/repos/robertt3kuk/hutan/releases";

// ---- fixtures ---------------------------------------------------------------------------------

/// A mock of GitHub's releases API on a runtime of its own (the daemon's is another).
pub(crate) struct Releases {
    _runtime: tokio::runtime::Runtime,
    pub(crate) server: MockServer,
}

impl Releases {
    pub(crate) fn serving(tags: &[&str]) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(MockServer::start());
        let releases = Self {
            _runtime: runtime,
            server,
        };
        releases.serve(tags);
        releases
    }

    pub(crate) fn serve(&self, tags: &[&str]) {
        let list: Vec<_> = tags
            .iter()
            .map(|tag| {
                let version = tag.trim_start_matches('v');
                json!({
                    "tag_name": tag,
                    "prerelease": true,
                    "published_at": "2026-10-29T10:00:00Z",
                    "html_url": format!("https://github.com/robertt3kuk/hutan/releases/tag/{tag}"),
                    "body": format!("## Hutan {version}\n\n- Update check and brew update\n- Another thing"),
                    "assets": [{
                        "name": format!("Hutan-{version}-arm64.zip"),
                        "size": 1234,
                        "digest": format!("sha256:{}", "ab".repeat(32)),
                    }],
                })
            })
            .collect();
        self.server.set_route(
            "GET",
            RELEASES,
            Reply::json(200, &serde_json::to_string(&list).unwrap()),
        );
        // Every archive can be downloaded: the check asks with a `HEAD` before it offers one.
        for tag in tags {
            let version = tag.trim_start_matches('v');
            self.server.set_route(
                "HEAD",
                &format!("/robertt3kuk/hutan/releases/download/{tag}/Hutan-{version}-arm64.zip"),
                Reply::fixed(200, "application/zip", ""),
            );
        }
    }
}

/// A fake program at a stable path named by its content (macOS vets every new executable once).
fn fake_brew(code: i32) -> PathBuf {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let text = format!("#!/bin/sh\necho \"$@\" >> \"$(dirname \"$0\")/brew.log\"\nexit {code}\n");
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let dir = std::env::temp_dir().join("hutan-test-fake-clis");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{:016x}-gpui-brew-{code}", hasher.finish()));
    if std::fs::read_to_string(&path).is_ok_and(|have| have == text) {
        return path;
    }
    let staging = dir.join(format!(".{}-gpui-brew-{code}", std::process::id()));
    std::fs::write(&staging, &text).unwrap();
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&staging, &path).unwrap();
    path
}

/// A Homebrew cask layout: `Caskroom/hutan/0.3.1` links to `Applications/Hutan.app`, and the
/// prefix's `bin` holds the fake `brew`.
struct Cask {
    _dir: TempDir,
    prefix: PathBuf,
    bundle: PathBuf,
}

impl Cask {
    fn new(brew_code: i32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let prefix = root.join("prefix");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        symlink(fake_brew(brew_code), prefix.join("bin/brew")).unwrap();
        let bundle = root.join("Applications/Hutan.app");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        std::fs::write(bundle.join("Contents/MacOS/hutand"), b"").unwrap();
        let staged = prefix.join("Caskroom/hutan/0.3.1");
        std::fs::create_dir_all(&staged).unwrap();
        symlink(&bundle, staged.join("Hutan.app")).unwrap();
        Self {
            _dir: dir,
            prefix,
            bundle,
        }
    }

    fn brew_log(&self) -> String {
        std::fs::read_to_string(self.prefix.join("bin/brew.log")).unwrap_or_default()
    }
}

/// A daemon that runs as `cask` and checks `releases` (by itself, at once, unless `toml` says
/// otherwise).
fn daemon(releases: &Releases, cask: &Cask, toml: &str) -> TestDaemon {
    let (api, exe, prefix) = (
        releases.server.url(),
        cask.bundle.join("Contents/MacOS/hutand"),
        cask.prefix.clone(),
    );
    TestDaemon::start_tweaked(toml, move |config| {
        config.updates = UpdateTuning {
            api_base: Some(api.clone()),
            version: "0.3.1".to_owned(),
            exe: Some(exe),
            brew_prefixes: vec![prefix],
            startup_delay: Duration::ZERO,
            // The mock also answers whether an archive can be downloaded: nothing here asks
            // GitHub.
            download_base: api,
            ..UpdateTuning::default()
        };
    })
}

/// What *Relaunch* was asked to open.
#[derive(Debug, Default)]
struct Recorder(Mutex<Vec<PathBuf>>);

impl Relauncher for Recorder {
    fn relaunch_after_exit(&self, bundle: &Path) -> std::io::Result<()> {
        self.0.lock().unwrap().push(bundle.to_owned());
        Ok(())
    }
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    recorder: Arc<Recorder>,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let options =
        WorkbenchOptions::builtin(settings, daemon.connection(), None).with_relauncher(recorder);
    let (view, cx) = open_options(cx, options);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    (view, cx)
}

pub(crate) fn item_text(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, cx| {
        wb.update_directory().read(cx).item().map(|item| item.text)
    })
}

pub(crate) fn item_drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::StatusItem("update"))
            .is_some_and(|bounds| bounds.size.width > gpui::px(8.0))
    })
}

pub(crate) fn drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
}

/// Clicks `region` as the frame drawn last shows it.
pub(crate) fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        drawn(view, cx, region)
    });
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap();
    click_at(cx, bounds.center());
    cx.run_until_parked();
}

pub(crate) fn sheet_open(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| wb.update_sheet_open())
}

pub(crate) fn wait_for_item(view: &Entity<Workbench>, cx: &mut VisualTestContext, text: &str) {
    wait_until(cx, &format!("the update item to say {text:?}"), |cx| {
        item_text(view, cx).as_deref() == Some(text) && item_drawn(view, cx)
    });
}

pub(crate) fn daemon_toml(daemon: &TestDaemon) -> String {
    std::fs::read_to_string(daemon.home().join("daemon.toml")).unwrap_or_default()
}

// ---- the item and the sheet -------------------------------------------------------------------

#[gpui::test]
fn a_newer_release_shows_the_status_item_and_the_sheet_with_its_notes(cx: &mut TestAppContext) {
    let releases = Releases::serving(&["v0.3.0", "v0.3.2"]);
    let cask = Cask::new(0);
    let daemon = daemon(&releases, &cask, "");
    let (view, cx) = open(cx, &daemon, Arc::default());

    // The daemon checked by itself; nothing was asked for.
    wait_for_item(&view, cx, "Update to 0.3.2");
    assert!(
        !sheet_open(&view, cx),
        "the item invites, it never opens the sheet"
    );
    view.read_with(cx, |wb, cx| {
        let status = wb.update_directory().read(cx).status().cloned().unwrap();
        assert_eq!(status.version, "0.3.1");
        assert_eq!(status.available.unwrap().version, "0.3.2");
    });

    click(&view, cx, Region::StatusItem("update"));
    wait_until(cx, "the sheet", |cx| {
        sheet_open(&view, cx) && drawn(&view, cx, Region::Update("sheet"))
    });
    settle(cx);
    for part in ["title", "notes", "notes-link", "skip", "later", "now"] {
        assert!(
            drawn(&view, cx, Region::Update(part)),
            "{part} is part of the sheet"
        );
    }
    assert!(!drawn(&view, cx, Region::Update("relaunch")));
    assert!(!drawn(&view, cx, Region::Update("outcome")));

    // Later: the sheet goes, the item stays.
    click(&view, cx, Region::Update("later"));
    wait_until(cx, "the sheet to close", |cx| !sheet_open(&view, cx));
    settle(cx);
    assert!(!drawn(&view, cx, Region::Update("sheet")));
    assert_eq!(item_text(&view, cx).as_deref(), Some("Update to 0.3.2"));
    assert!(item_drawn(&view, cx));
    assert_eq!(cask.brew_log(), "", "nothing ran");
}

#[gpui::test]
fn skip_this_version_removes_the_item_and_is_remembered_by_the_daemon(cx: &mut TestAppContext) {
    let releases = Releases::serving(&["v0.3.2"]);
    let cask = Cask::new(0);
    let daemon = daemon(&releases, &cask, "");
    let (view, cx) = open(cx, &daemon, Arc::default());
    wait_for_item(&view, cx, "Update to 0.3.2");

    click(&view, cx, Region::StatusItem("update"));
    click(&view, cx, Region::Update("skip"));
    wait_until(cx, "the item to go", |cx| {
        item_text(&view, cx).is_none() && !item_drawn(&view, cx)
    });
    assert!(!sheet_open(&view, cx));
    wait_until(cx, "daemon.toml to remember the skip", |_| {
        daemon_toml(&daemon).contains("skip_version = \"0.3.2\"")
    });

    // A check the user asks for still shows the release they skipped.
    cx.dispatch_action(actions::CheckForUpdates);
    wait_until(cx, "the sheet of the skipped release", |cx| {
        sheet_open(&view, cx) && drawn(&view, cx, Region::Update("sheet"))
    });
    assert_eq!(item_text(&view, cx), None, "the item stays away");

    // A newer release is announced again.
    releases.serve(&["v0.3.3"]);
    click(&view, cx, Region::Update("later"));
    cx.dispatch_action(actions::CheckForUpdates);
    wait_for_item(&view, cx, "Update to 0.3.3");
}

#[gpui::test]
fn update_now_runs_brew_in_a_terminal_then_offers_relaunch(cx: &mut TestAppContext) {
    let releases = Releases::serving(&["v0.3.2"]);
    let cask = Cask::new(0);
    let daemon = daemon(&releases, &cask, "[updates]\ncheck = \"off\"\n");
    let recorder = Arc::new(Recorder::default());
    let (view, cx) = open(cx, &daemon, recorder.clone());

    // Off: nothing came by itself. The user asks.
    assert_eq!(item_text(&view, cx), None);
    assert!(!item_drawn(&view, cx));
    cx.dispatch_action(actions::CheckForUpdates);
    wait_until(cx, "the sheet", |cx| {
        sheet_open(&view, cx) && drawn(&view, cx, Region::Update("now"))
    });

    click(&view, cx, Region::Update("now"));
    wait_until(cx, "Homebrew to finish", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.update_directory()
                .read(cx)
                .status()
                .and_then(|s| s.install.as_ref())
                .is_some_and(|i| i.state == InstallState::Succeeded)
        })
    });
    assert_eq!(
        cask.brew_log().lines().collect::<Vec<_>>(),
        ["update", "upgrade --cask robertt3kuk/hutan/hutan"]
    );
    // The terminal of the update is a tab of the window.
    view.read_with(cx, |wb, cx| {
        let install = wb
            .update_directory()
            .read(cx)
            .status()
            .and_then(|s| s.install.clone())
            .unwrap();
        let id = install
            .session_id
            .expect("a Homebrew update has a session")
            .to_string();
        assert!(
            wb.state().find_session_tab(&id).is_some(),
            "the window shows the terminal Homebrew runs in"
        );
    });

    wait_for_item(&view, cx, "Relaunch for 0.3.2");
    click(&view, cx, Region::StatusItem("update"));
    wait_until(cx, "the sheet to offer Relaunch", |cx| {
        sheet_open(&view, cx) && drawn(&view, cx, Region::Update("relaunch"))
    });
    assert!(!drawn(&view, cx, Region::Update("now")));
    assert!(drawn(&view, cx, Region::Update("outcome")));

    click(&view, cx, Region::Update("relaunch"));
    wait_until(cx, "the window to quit", |cx| {
        view.read_with(cx, |wb, _| wb.quitting())
    });
    assert_eq!(
        *recorder.0.lock().unwrap(),
        [cask.bundle.canonicalize().unwrap()],
        "the installed app is what opens again"
    );
}

#[gpui::test]
fn a_failed_brew_says_so_and_offers_to_try_again(cx: &mut TestAppContext) {
    let releases = Releases::serving(&["v0.3.2"]);
    let cask = Cask::new(3);
    let daemon = daemon(&releases, &cask, "");
    let (view, cx) = open(cx, &daemon, Arc::default());
    wait_for_item(&view, cx, "Update to 0.3.2");
    click(&view, cx, Region::StatusItem("update"));
    click(&view, cx, Region::Update("now"));
    wait_for_item(&view, cx, "Update failed");
    click(&view, cx, Region::StatusItem("update"));
    wait_until(cx, "the sheet to say it failed", |cx| {
        sheet_open(&view, cx) && drawn(&view, cx, Region::Update("outcome"))
    });
    // *Update now* is back, as *Try again*; Relaunch is not offered.
    assert!(drawn(&view, cx, Region::Update("now")));
    assert!(!drawn(&view, cx, Region::Update("relaunch")));
}

#[gpui::test]
fn check_for_updates_says_when_there_is_nothing_newer(cx: &mut TestAppContext) {
    let releases = Releases::serving(&["v0.3.1"]);
    let cask = Cask::new(0);
    let daemon = daemon(&releases, &cask, "[updates]\ncheck = \"off\"\n");
    let (view, cx) = open(cx, &daemon, Arc::default());
    cx.dispatch_action(actions::CheckForUpdates);
    wait_until(cx, "the answer", |cx| {
        view.read_with(cx, |wb, _| {
            wb.notice()
                .is_some_and(|n| n.as_ref() == "Hutan 0.3.1 is up to date")
        })
    });
    assert!(!sheet_open(&view, cx));
    assert_eq!(item_text(&view, cx), None);
}

// ---- the settings -----------------------------------------------------------------------------

#[gpui::test]
fn the_update_settings_are_rows_of_general_and_follow_the_daemon(cx: &mut TestAppContext) {
    let releases = Releases::serving(&["v0.3.1"]);
    let cask = Cask::new(0);
    let daemon = daemon(&releases, &cask, "");
    let (view, cx) = open(cx, &daemon, Arc::default());
    cx.dispatch_action(actions::OpenSettingsFor("updates".into()));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some("updates".to_owned())
    );
    wait_until(cx, "the daemon settings", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_settings().settings().is_some())
    });
    // The section is on the General page, with the other daemon settings of it.
    view.read_with(cx, |wb, _| {
        assert!(wb.sheet_pages().contains(&"app"));
        let settings = wb.daemon_settings();
        assert_eq!(settings.value("updates.check"), Some(&json!("auto")));
        assert_eq!(
            settings.value("updates.channel"),
            Some(&json!("prerelease"))
        );
        assert_eq!(settings.value("updates.skip_version"), Some(&json!("")));
    });

    view.update(cx, |wb, cx| wb.daemon_choose("updates.check", "off", cx));
    wait_until(cx, "daemon.toml", |_| {
        daemon_toml(&daemon).contains("check = \"off\"")
    });
    // The window's own model follows (the setup's row and the item read it).
    wait_until(cx, "the window to know checking is off", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.update_directory()
                .read(cx)
                .status()
                .is_some_and(|s| s.check == UpdateCheck::Off)
        })
    });
}
