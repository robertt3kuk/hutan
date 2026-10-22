#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! `FX-REINSTALL-SETUP` against an in-process daemon, with injected install identities (no test
//! reads a real bundle): the first launch of a bundle offers nothing; the same version under a
//! new install id after the setup was done offers it again — **Run setup** opens it with today's
//! values, **Not now** never shows again for that install; a higher version says *Updated to …
//! · What's new* and opens the changelog; a development build says nothing.

use crate::common;

use std::path::{Path, PathBuf};

use common::{TestDaemon, draw, open_options, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::install::Install;
use hutan_desktop::workbench::persistence;
use hutan_desktop::workbench::{FileOpen, Region, Workbench, WorkbenchOptions};
use hutan_settings::{AppearanceMode, Density, SettingsPaths, SettingsStore};

// ---- fixtures ----------------------------------------------------------------------------------

fn store(daemon: &TestDaemon) -> SettingsStore {
    SettingsStore::new(SettingsPaths::in_dir(daemon.home()))
}

fn state_file(daemon: &TestDaemon) -> PathBuf {
    daemon.home().join("desktop-state.json")
}

fn saved(daemon: &TestDaemon) -> String {
    std::fs::read_to_string(daemon.home().join("settings.toml")).unwrap_or_default()
}

/// The setup was done on an earlier launch.
fn setup_done(daemon: &TestDaemon, extra: &str) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        format!("[app]\nsetup_done = true\n{extra}"),
    )
    .unwrap();
}

/// An install of Hutan `version` with the install id `id`, in a bundle that has a changelog.
fn install(dir: &Path, version: &str, id: &str) -> Install {
    let bundle = dir.join("Hutan.app");
    let resources = bundle.join("Contents/Resources");
    std::fs::create_dir_all(&resources).unwrap();
    std::fs::write(bundle.join("Contents/Info.plist"), "plist").unwrap();
    std::fs::write(
        resources.join("CHANGELOG.md"),
        "# Changelog\n\n## Unreleased\n\n- next\n\n## 0.2.0 — 2026-10-23\n\n- one\n\n## 0.1.0 — 2026-10-19\n\n- old\n",
    )
    .unwrap();
    Install {
        version: version.to_owned(),
        bundle,
        id: id.to_owned(),
    }
}

/// One launch of the desktop: runs `body`, then closes the window (saving the state).
fn launch<R>(
    cx: &mut TestAppContext,
    daemon: &TestDaemon,
    install: Option<Install>,
    body: impl FnOnce(&Entity<Workbench>, &mut VisualTestContext) -> R,
) -> R {
    let options =
        WorkbenchOptions::builtin(store(daemon), daemon.connection(), Some(state_file(daemon)))
            .with_first_run_setup()
            .with_install(install);
    let (view, cx) = open_options(cx, options);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    let result = body(&view, cx);
    view.update(cx, |wb, _| wb.flush_save());
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
    result
}

fn drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        drawn(view, cx, region)
    });
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

fn card(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> bool {
    let shown = view.read_with(cx, |wb, _| wb.reinstall_card_shown());
    assert_eq!(
        drawn(view, cx, Region::Offer("reinstall")),
        shown,
        "the card is drawn exactly while it is shown"
    );
    shown
}

fn notice(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Option<String> {
    let shown = view.read_with(cx, |wb, _| wb.whats_new_shown().map(str::to_owned));
    assert_eq!(drawn(view, cx, Region::Offer("whats-new")), shown.is_some());
    shown
}

fn recorded(daemon: &TestDaemon) -> Option<hutan_desktop::install::InstallRecord> {
    persistence::load_install(&state_file(daemon))
}

// ---- first launch ------------------------------------------------------------------------------

#[gpui::test]
fn the_first_launch_of_a_bundle_offers_nothing_and_the_first_screen_shows(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    launch(
        cx,
        &daemon,
        Some(install(dir.path(), "0.2.0", "a")),
        |view, cx| {
            assert!(!card(view, cx));
            assert_eq!(notice(view, cx), None);
            // Setup never ran: the normal first screen is what the centre shows.
            wait_until(cx, "the first screen", |cx| {
                drawn(view, cx, Region::FirstRun("screen"))
            });
            assert!(!view.read_with(cx, |wb, _| wb.setup_open()));
        },
    );
    let record = recorded(&daemon).expect("the install is recorded");
    assert_eq!(record.version, "0.2.0");
    assert_eq!(record.install_id, "a");
    assert_eq!(
        record.bundle_path,
        dir.path().join("Hutan.app").display().to_string()
    );
    assert_eq!(saved(&daemon), "", "recording an install writes no setting");
}

#[gpui::test]
fn another_install_before_the_setup_ever_ran_still_shows_the_first_screen(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    launch(
        cx,
        &daemon,
        Some(install(dir.path(), "0.2.0", "a")),
        |_, _| {},
    );
    launch(
        cx,
        &daemon,
        Some(install(dir.path(), "0.2.0", "b")),
        |view, cx| {
            assert!(!card(view, cx), "the first screen is the offer");
            wait_until(cx, "the first screen", |cx| {
                drawn(view, cx, Region::FirstRun("screen"))
            });
        },
    );
    assert_eq!(recorded(&daemon).unwrap().install_id, "b");
}

// ---- reinstall ---------------------------------------------------------------------------------

#[gpui::test]
fn a_reinstall_offers_the_setup_and_run_setup_opens_it_with_todays_values(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    setup_done(
        &daemon,
        "keep_running = true\n[appearance]\nappearance = \"dark\"\ndensity = \"comfortable\"\n",
    );
    launch(
        cx,
        &daemon,
        Some(install(dir.path(), "0.2.0", "a")),
        |view, cx| {
            assert!(!card(view, cx), "the same install says nothing");
        },
    );
    let before = std::fs::read_to_string(daemon.home().join("settings.toml")).unwrap();

    launch(
        cx,
        &daemon,
        Some(install(dir.path(), "0.2.0", "b")),
        |view, cx| {
            assert!(card(view, cx), "another install of the same version");
            assert_eq!(notice(view, cx), None, "an update is not a reinstall");
            assert!(
                !view.read_with(cx, |wb, _| wb.setup_open()),
                "a card, not a modal"
            );
            click(view, cx, Region::Offer("reinstall-run-setup"));
            assert!(view.read_with(cx, |wb, _| wb.setup_open()));
            assert!(!card(view, cx), "answered");
            let (appearance, density, keep_running) = view.read_with(cx, |wb, _| {
                let state = wb.setup().expect("the setup is open");
                (state.appearance, state.density, state.keep_running)
            });
            assert_eq!(appearance, AppearanceMode::Dark);
            assert_eq!(density, Density::Comfortable);
            assert!(keep_running, "every answer starts at today's value");
        },
    );
    assert_eq!(
        std::fs::read_to_string(daemon.home().join("settings.toml")).unwrap(),
        before,
        "offering and opening the setup resets nothing"
    );
    assert_eq!(recorded(&daemon).unwrap().install_id, "b");

    launch(
        cx,
        &daemon,
        Some(install(dir.path(), "0.2.0", "b")),
        |view, cx| {
            assert!(!card(view, cx), "answered for this install");
        },
    );
}

#[gpui::test]
fn not_now_is_remembered_for_that_install_only(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    setup_done(&daemon, "");
    launch(
        cx,
        &daemon,
        Some(install(dir.path(), "0.2.0", "a")),
        |_, _| {},
    );

    launch(
        cx,
        &daemon,
        Some(install(dir.path(), "0.2.0", "b")),
        |view, cx| {
            assert!(card(view, cx));
            click(view, cx, Region::Offer("reinstall-not-now"));
            assert!(!card(view, cx));
            assert!(!view.read_with(cx, |wb, _| wb.setup_open()));
        },
    );
    launch(
        cx,
        &daemon,
        Some(install(dir.path(), "0.2.0", "b")),
        |view, cx| {
            assert!(!card(view, cx), "never again for this install");
        },
    );
    // The next reinstall is a new install.
    launch(
        cx,
        &daemon,
        Some(install(dir.path(), "0.2.0", "c")),
        |view, cx| {
            assert!(card(view, cx));
        },
    );
}

#[gpui::test]
fn an_unanswered_card_comes_back_at_the_next_launch(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    setup_done(&daemon, "");
    launch(
        cx,
        &daemon,
        Some(install(dir.path(), "0.2.0", "a")),
        |_, _| {},
    );
    for _ in 0..2 {
        launch(
            cx,
            &daemon,
            Some(install(dir.path(), "0.2.0", "b")),
            |view, cx| {
                assert!(card(view, cx));
            },
        );
    }
}

#[gpui::test]
fn a_lower_version_in_a_new_bundle_is_a_reinstall_too(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    setup_done(&daemon, "");
    launch(
        cx,
        &daemon,
        Some(install(dir.path(), "0.3.0", "a")),
        |_, _| {},
    );
    launch(
        cx,
        &daemon,
        Some(install(dir.path(), "0.2.0", "b")),
        |view, cx| {
            assert!(card(view, cx));
            assert_eq!(notice(view, cx), None);
        },
    );
}

// ---- update ------------------------------------------------------------------------------------

#[gpui::test]
fn a_higher_version_says_whats_new_and_offers_no_setup(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    setup_done(&daemon, "");
    launch(
        cx,
        &daemon,
        Some(install(dir.path(), "0.1.0", "a")),
        |_, _| {},
    );

    launch(
        cx,
        &daemon,
        Some(install(dir.path(), "0.2.0", "b")),
        |view, cx| {
            assert!(!card(view, cx), "an update offers no setup");
            assert_eq!(notice(view, cx).as_deref(), Some("0.2.0"));
            click(view, cx, Region::Offer("whats-new"));
            // The version's section of the bundled changelog, in a file tab.
            wait_until(cx, "the changelog's tab", |cx| {
                view.read_with(cx, |wb, _| {
                    wb.state()
                        .center
                        .panes()
                        .into_iter()
                        .flat_map(|pane| pane.items.iter())
                        .filter_map(|item| item.as_file())
                        .any(|file| file.ends_with("Contents/Resources/CHANGELOG.md"))
                })
            });
            assert_eq!(notice(view, cx), None, "opened: the notice is done");
        },
    );
    let record = recorded(&daemon).unwrap();
    assert_eq!(
        (record.version.as_str(), record.install_id.as_str()),
        ("0.2.0", "b")
    );

    launch(
        cx,
        &daemon,
        Some(install(dir.path(), "0.2.0", "b")),
        |view, cx| {
            assert_eq!(notice(view, cx), None, "one session only");
            assert!(!card(view, cx));
        },
    );
}

// ---- development builds ------------------------------------------------------------------------

#[gpui::test]
fn a_development_build_records_nothing_and_never_offers(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    setup_done(&daemon, "");
    launch(cx, &daemon, None, |view, cx| {
        assert!(!card(view, cx));
        assert_eq!(notice(view, cx), None);
    });
    assert_eq!(recorded(&daemon), None);

    // What a bundle recorded earlier stays while a development build runs, and is not offered
    // against.
    launch(
        cx,
        &daemon,
        Some(install(dir.path(), "0.2.0", "a")),
        |_, _| {},
    );
    launch(cx, &daemon, None, |view, cx| {
        assert!(!card(view, cx));
        assert_eq!(notice(view, cx), None);
    });
    assert_eq!(recorded(&daemon).unwrap().install_id, "a");
}

// ---- the changelog tab -------------------------------------------------------------------------

fn fake_lsp_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-fake-lsp");
    assert!(
        bin.is_file(),
        "hutan-fake-lsp not built: run `cargo build -p hutan-lsp-client --bins` (or `just test`)"
    );
    bin
}

#[gpui::test]
fn whats_new_is_read_only_and_never_reaches_a_language_server(cx: &mut TestAppContext) {
    // A server for Markdown that waits for trust, with a root marker the bundle has too: an
    // ordinary Markdown file gets a document and the trust bar; the bundled changelog must not.
    let work = tempfile::tempdir().unwrap();
    let base = work.path().canonicalize().unwrap();
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("fake.toml"), "").unwrap();
    std::fs::write(repo.join("notes.md"), "# notes\n").unwrap();
    let script = base.join("script.json");
    std::fs::write(&script, "{}").unwrap();
    let daemon = TestDaemon::start_with_config(&format!(
        "[lsp]\ntrust = \"ask\"\n\n[lsp.servers.fake]\ncommand = {:?}\nargs = [\"--script\", {:?}]\n\
         languages = [\"markdown\"]\nroot_markers = [\"fake.toml\", \"Info.plist\"]\n",
        fake_lsp_bin().display().to_string(),
        script.display().to_string(),
    ));
    setup_done(&daemon, "");
    let app = base.join("app");
    std::fs::create_dir_all(&app).unwrap();
    launch(cx, &daemon, Some(install(&app, "0.1.0", "a")), |_, _| {});

    launch(
        cx,
        &daemon,
        Some(install(&app, "0.2.0", "b")),
        |view, cx| {
            let opens = |cx: &VisualTestContext| {
                view.read_with(cx, |wb, cx| wb.lsp().read(cx).opens_sent())
            };
            // The control: an ordinary Markdown file is a document and shows the trust bar.
            let notes = repo.join("notes.md");
            view.update_in(cx, |wb, window, cx| {
                wb.open_file(FileOpen::picked(&notes), window, cx);
            });
            wait_until(cx, "the trust bar of an ordinary file", |cx| {
                drawn(view, cx, Region::FileLsp("trust"))
            });
            assert_eq!(opens(cx), 1);

            // What's new: the changelog opens in a tab, read-only, and nothing reaches a server.
            click(view, cx, Region::Offer("whats-new"));
            wait_until(cx, "the changelog's tab", |cx| {
                view.read_with(cx, |wb, cx| {
                    wb.focused_file_view().is_some_and(|v| {
                        let buffer = v.read(cx).buffer().read(cx);
                        buffer.path().ends_with("Contents/Resources/CHANGELOG.md")
                            && buffer.is_ready()
                    })
                })
            });
            for _ in 0..5 {
                draw(cx);
            }
            assert_eq!(opens(cx), 1, "no doc.open for a file of the app bundle");
            assert!(!drawn(view, cx, Region::FileLsp("trust")), "no trust bar");
            view.read_with(cx, |wb, cx| {
                let view = wb.focused_file_view().unwrap();
                let buffer = view.read(cx).buffer().read(cx);
                assert!(buffer.is_read_only() && !buffer.is_editable());
            });
        },
    );
}
