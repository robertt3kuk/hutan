#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! GPUI tests of the update of a hand-copied `Hutan.app` in the window (UPDATES-2, protocol
//! 0.60, ADR 0024): the sheet's progress while the daemon downloads, verifies and unpacks the
//! release, *Quit and Install*, a refusal with its reason, and the command shown for a folder
//! Hutan cannot write. The daemon is a real one in this process: a mock of GitHub serves the
//! release list, a `ditto`-made archive of a fake `Hutan.app` and the tap's cask, `codesign`
//! and `open` are fakes, the Trash is a folder. Nothing touches `/Applications`, the user's
//! Trash, GitHub or the user's Hutan.

use crate::common;
use crate::update_gpui::{RELEASES, Releases, click, drawn, sheet_open, wait_for_item};

use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{TestDaemon, draw, open_options, settle, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_daemon::files::trash::FolderTrash;
use hutan_daemon::update::{Tools, UpdateTuning};
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::{Region, Workbench, WorkbenchOptions};
use hutan_gateway::testing::{Reply, Step};
use hutan_protocol::update::{InstallStage, InstallState};
use hutan_settings::{SettingsPaths, SettingsStore};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

const ARCHIVE: &str = "/robertt3kuk/hutan/releases/download/v0.3.2/Hutan-0.3.2-arm64.zip";
const CASK: &str = "/robertt3kuk/homebrew-hutan/v0.3.2/Casks/hutan.rb";

// ---- fixtures ---------------------------------------------------------------------------------

fn make_bundle(dir: &Path, version: &str, marker: &str) -> PathBuf {
    let bundle = dir.join("Hutan.app");
    std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
    std::fs::create_dir_all(bundle.join("Contents/Resources")).unwrap();
    std::fs::write(bundle.join("Contents/MacOS/hutand"), b"#!/bin/sh\n").unwrap();
    std::fs::write(
        bundle.join("Contents/Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>dev.hutan.Hutan</string><key>CFBundleShortVersionString</key><string>{version}</string></dict></plist>\n"
        ),
    )
    .unwrap();
    std::fs::write(bundle.join("Contents/Resources/marker.txt"), marker).unwrap();
    bundle
}

fn marker(bundle: &Path) -> String {
    std::fs::read_to_string(bundle.join("Contents/Resources/marker.txt")).unwrap_or_default()
}

/// A fake program at a stable path named by its content (macOS vets every new executable once).
fn executable(name: &str, body: &str) -> PathBuf {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let text = format!("#!/bin/sh\n{body}\n");
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let dir = std::env::temp_dir().join("hutan-test-fake-clis");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{:016x}-gpui-{name}", hasher.finish()));
    if std::fs::read_to_string(&path).is_ok_and(|have| have == text) {
        return path;
    }
    let staging = dir.join(format!(".{}-gpui-{name}", std::process::id()));
    std::fs::write(&staging, &text).unwrap();
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&staging, &path).unwrap();
    path
}

/// The old app, the folder around it, the Trash, the fake tools and the release's archive.
struct Fixture {
    _dir: TempDir,
    apps: PathBuf,
    old: PathBuf,
    trash: PathBuf,
    bin: PathBuf,
    archive: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let apps = root.join("Copied");
        let old = make_bundle(&apps, "0.3.1", "old");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        symlink(
            executable(
                "codesign",
                "echo \"$@\" >> \"$(dirname \"$0\")/codesign.log\"\nexit 0",
            ),
            bin.join("codesign"),
        )
        .unwrap();
        symlink(
            executable(
                "open",
                "echo \"$@\" >> \"$(dirname \"$0\")/open.log\"\nexit 0",
            ),
            bin.join("open"),
        )
        .unwrap();
        let new = tempfile::tempdir().unwrap();
        let bundle = make_bundle(new.path(), "0.3.2", "new");
        let zip = new.path().join("Hutan.zip");
        let made = std::process::Command::new("/usr/bin/ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(&bundle)
            .arg(&zip)
            .status()
            .unwrap();
        assert!(made.success());
        Self {
            trash: root.join("trash"),
            _dir: dir,
            apps,
            old,
            bin,
            archive: std::fs::read(&zip).unwrap(),
        }
    }

    fn digest(&self) -> String {
        format!("{:x}", Sha256::digest(&self.archive))
    }

    /// Serves release 0.3.2 (whose archive GitHub lists with `listed`), the archive and the
    /// tap's cask (which lists `tap`).
    fn serve(&self, releases: &Releases, listed: &str, tap: &str) {
        let release = json!([{
            "tag_name": "v0.3.2",
            "prerelease": true,
            "published_at": "2026-10-29T10:00:00Z",
            "html_url": "https://github.com/robertt3kuk/hutan/releases/tag/v0.3.2",
            "body": "## Hutan 0.3.2\n\n- Updates itself",
            "assets": [{
                "name": "Hutan-0.3.2-arm64.zip",
                "size": self.archive.len(),
                "digest": format!("sha256:{listed}"),
            }],
        }]);
        releases.server.set_route(
            "GET",
            RELEASES,
            Reply::json(200, &serde_json::to_string(&release).unwrap()),
        );
        self.serve_archive(releases, &self.archive);
        releases.server.set_route(
            "GET",
            CASK,
            Reply::fixed(
                200,
                "text/plain",
                &format!("cask \"hutan\" do\n  version \"0.3.2\"\n  sha256 \"{tap}\"\nend\n"),
            ),
        );
    }

    fn serve_archive(&self, releases: &Releases, bytes: &[u8]) {
        let mut reply = Reply::fixed(200, "application/zip", "");
        reply.steps = vec![Step::Send(bytes.to_vec())];
        releases.server.set_route("GET", ARCHIVE, reply);
    }

    fn daemon(&self, releases: &Releases) -> TestDaemon {
        let tuning = UpdateTuning {
            api_base: Some(releases.server.url()),
            version: "0.3.1".to_owned(),
            exe: Some(self.old.join("Contents/MacOS/hutand")),
            brew_prefixes: vec![self.apps.join("no-brew-here")],
            startup_delay: Duration::ZERO,
            download_base: releases.server.url(),
            tap_base: releases.server.url(),
            tools: Tools {
                ditto: PathBuf::from("/usr/bin/ditto"),
                codesign: self.bin.join("codesign"),
                open: self.bin.join("open"),
            },
            trash: Arc::new(FolderTrash::new(&self.trash)),
            quit_timeout: Duration::from_secs(20),
            ..UpdateTuning::default()
        };
        TestDaemon::start_tweaked("", move |config| config.updates = tuning)
    }

    fn logged(&self, name: &str) -> String {
        std::fs::read_to_string(self.bin.join(name)).unwrap_or_default()
    }
}

struct Restore(PathBuf);

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// The window, whose *Quit and Install* tells the daemon to wait for `wait_pid`.
fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    wait_pid: Option<u32>,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let mut options = WorkbenchOptions::builtin(settings, daemon.connection(), None);
    if let Some(pid) = wait_pid {
        options = options.with_update_wait_pid(pid);
    }
    let (view, cx) = open_options(cx, options);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    (view, cx)
}

fn install_now(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
) -> Option<hutan_protocol::update::UpdateInstall> {
    view.read_with(cx, |wb, cx| {
        wb.update_directory()
            .read(cx)
            .status()
            .and_then(|status| status.install.clone())
    })
}

fn open_the_sheet(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    click(view, cx, Region::StatusItem("update"));
    wait_until(cx, "the sheet", |cx| {
        sheet_open(view, cx) && drawn(view, cx, Region::Update("sheet"))
    });
    settle(cx);
}

// ---- the tests --------------------------------------------------------------------------------

#[gpui::test]
fn update_now_shows_the_download_as_it_arrives(cx: &mut TestAppContext) {
    let releases = Releases::serving(&[]);
    let fixture = Fixture::new();
    fixture.serve(&releases, &fixture.digest(), &fixture.digest());
    // Half of the archive arrives, then the server holds: that is what the window shows.
    let half = fixture.archive.len() / 2;
    let mut reply = Reply::ndjson(Vec::<String>::new());
    reply.headers = vec![("content-type".into(), "application/zip".into())];
    reply.steps = vec![Step::Send(fixture.archive[..half].to_vec()), Step::Hold];
    releases.server.set_route("GET", ARCHIVE, reply);
    let daemon = fixture.daemon(&releases);
    let (view, cx) = open(cx, &daemon, None);

    wait_for_item(&view, cx, "Update to 0.3.2");
    open_the_sheet(&view, cx);
    // Before the button: what will happen, no progress, nothing in place changed.
    assert!(drawn(&view, cx, Region::Update("now")));
    assert!(!drawn(&view, cx, Region::Update("progress")));
    assert!(!drawn(&view, cx, Region::Update("quit-install")));

    click(&view, cx, Region::Update("now"));
    let percent = half * 100 / fixture.archive.len();
    wait_for_item(&view, cx, &format!("Downloading 0.3.2 · {percent}%"));
    wait_until(cx, "the sheet's progress", |cx| {
        drawn(&view, cx, Region::Update("progress"))
    });
    // Updating…, not Update now; not yet Quit and Install.
    assert!(drawn(&view, cx, Region::Update("running")));
    assert!(!drawn(&view, cx, Region::Update("now")));
    assert!(!drawn(&view, cx, Region::Update("quit-install")));
    let install = install_now(&view, cx).unwrap();
    assert_eq!(install.stage, Some(InstallStage::Downloading));
    assert_eq!(install.downloaded, Some(half as u64));
    assert_eq!(install.total, Some(fixture.archive.len() as u64));
    assert_eq!(marker(&fixture.old), "old", "nothing in place changed");
}

#[gpui::test]
fn a_verified_update_offers_quit_and_install_which_quits_and_has_the_daemon_replace_the_app(
    cx: &mut TestAppContext,
) {
    let releases = Releases::serving(&[]);
    let fixture = Fixture::new();
    fixture.serve(&releases, &fixture.digest(), &fixture.digest());
    let daemon = fixture.daemon(&releases);
    // The window's own process cannot exit in a test: the daemon waits for this one.
    let mut app = std::process::Command::new("/bin/sleep")
        .arg("600")
        .spawn()
        .unwrap();
    let (view, cx) = open(cx, &daemon, Some(app.id()));

    wait_for_item(&view, cx, "Update to 0.3.2");
    open_the_sheet(&view, cx);
    click(&view, cx, Region::Update("now"));
    wait_for_item(&view, cx, "Quit and Install 0.3.2");
    wait_until(cx, "Quit and Install in the sheet", |cx| {
        drawn(&view, cx, Region::Update("quit-install"))
    });
    // Download, verify, ready: all three done; Update now is gone; the app is untouched.
    assert!(drawn(&view, cx, Region::Update("progress")));
    assert!(!drawn(&view, cx, Region::Update("now")));
    assert!(!drawn(&view, cx, Region::Update("running")));
    let install = install_now(&view, cx).unwrap();
    assert_eq!(install.stage, Some(InstallStage::Ready));
    assert_eq!(marker(&fixture.old), "old");
    assert!(
        fixture
            .logged("codesign.log")
            .contains("--verify --deep --strict")
    );

    click(&view, cx, Region::Update("quit-install"));
    wait_until(cx, "the window to quit", |cx| {
        view.read_with(cx, |wb, _| wb.quitting())
    });
    // The daemon is waiting for the window's process: nothing moved yet.
    wait_until(cx, "the daemon to be waiting for the app to quit", |cx| {
        install_now(&view, cx).is_some_and(|i| i.stage == Some(InstallStage::Installing))
    });
    assert_eq!(marker(&fixture.old), "old");
    assert_eq!(fixture.logged("open.log"), "");

    // The process is gone: the old app goes to the Trash, the new one is put in place, opened.
    app.kill().unwrap();
    app.wait().unwrap();
    wait_until(cx, "the new app in place", |_| {
        marker(&fixture.old) == "new" && !fixture.logged("open.log").is_empty()
    });
    assert_eq!(marker(&fixture.trash.join("0/Hutan.app")), "old");
    assert_eq!(
        fixture.logged("open.log"),
        format!("-n {}\n", fixture.old.display())
    );
}

#[gpui::test]
fn a_download_that_does_not_match_says_so_and_offers_to_try_again(cx: &mut TestAppContext) {
    let releases = Releases::serving(&[]);
    let fixture = Fixture::new();
    // GitHub lists another digest than the archive has.
    fixture.serve(&releases, &"cd".repeat(32), &fixture.digest());
    let daemon = fixture.daemon(&releases);
    let (view, cx) = open(cx, &daemon, None);

    wait_for_item(&view, cx, "Update to 0.3.2");
    open_the_sheet(&view, cx);
    click(&view, cx, Region::Update("now"));
    wait_for_item(&view, cx, "Update failed");
    let install = install_now(&view, cx).unwrap();
    assert_eq!(install.state, InstallState::Failed);
    assert_eq!(install.stage, Some(InstallStage::Verifying));
    assert!(
        install
            .message
            .as_deref()
            .is_some_and(|m| m.contains("not the one GitHub lists")),
        "{install:?}"
    );
    wait_until(cx, "the reason in the sheet", |cx| {
        drawn(&view, cx, Region::Update("outcome"))
    });
    // Try again is Update now's place; Quit and Install is not offered; nothing changed.
    assert!(drawn(&view, cx, Region::Update("now")));
    assert!(!drawn(&view, cx, Region::Update("quit-install")));
    assert!(!drawn(&view, cx, Region::Update("progress")));
    assert_eq!(marker(&fixture.old), "old");

    // Fixed upstream: a check finds the release with its right digest, and trying again gets
    // to ready.
    fixture.serve(&releases, &fixture.digest(), &fixture.digest());
    cx.dispatch_action(actions::CheckForUpdates);
    let right = fixture.digest();
    wait_until(cx, "the release with its new digest", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.update_directory()
                .read(cx)
                .status()
                .and_then(|s| s.available.as_ref())
                .is_some_and(|r| r.assets.iter().any(|a| a.sha256.as_deref() == Some(&right)))
        })
    });
    click(&view, cx, Region::Update("now"));
    wait_for_item(&view, cx, "Quit and Install 0.3.2");
}

#[gpui::test]
fn a_folder_hutan_cannot_write_shows_the_command_instead_of_quit_and_install(
    cx: &mut TestAppContext,
) {
    let releases = Releases::serving(&[]);
    let fixture = Fixture::new();
    fixture.serve(&releases, &fixture.digest(), &fixture.digest());
    let daemon = fixture.daemon(&releases);
    let (view, cx) = open(cx, &daemon, None);

    wait_for_item(&view, cx, "Update to 0.3.2");
    open_the_sheet(&view, cx);
    // The folder became read-only after the daemon found the install.
    let _restore = Restore(fixture.apps.clone());
    std::fs::set_permissions(&fixture.apps, std::fs::Permissions::from_mode(0o555)).unwrap();
    click(&view, cx, Region::Update("now"));
    wait_for_item(&view, cx, "Update failed");
    wait_until(cx, "the command in the sheet", |cx| {
        drawn(&view, cx, Region::Update("copy-command"))
    });
    assert!(drawn(&view, cx, Region::Update("open-release")));
    assert!(!drawn(&view, cx, Region::Update("quit-install")));
    assert!(!drawn(&view, cx, Region::Update("now")));
    let install = install_now(&view, cx).unwrap();
    assert!(install.manual_command.unwrap().starts_with("sudo mv "));
    assert_eq!(marker(&fixture.old), "old");
    draw(cx);
}
