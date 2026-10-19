#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The first-run setup (D-2), the agent integrations that install themselves, and "Install…" for a
//! missing harness, against an in-process daemon. Every system seam is a fake: the login item,
//! the notification permission, the CLI link (a temporary folder), the integration installer and
//! the package runner. No test registers anything with macOS, asks for a permission, writes into
//! `$HOME`, `~/.omp`, `~/.claude` or `~/.codex`, or runs `npm`.

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::mac::{Fakes, open_with_services};
use common::{TestDaemon, draw, open_options, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::cli_link::{CliLink, Manual, Plan};
use hutan_desktop::harness_install::{FakeRunner, SOURCES};
use hutan_desktop::integrations::{FakeInstaller, IntegrationStatus};
use hutan_desktop::macos::{LoginItemCall, LoginItemStatus, NotificationAnswer};
use hutan_desktop::workbench::actions::{self, daemon as daemon_actions};
use hutan_desktop::workbench::{
    Detection, HARNESSES, Listing, NotificationUi, Region, SetupStep, Workbench, WorkbenchOptions,
};
use hutan_protocol::assistant::AssistantBackend;
use hutan_settings::{AppearanceMode, Density, SettingsPaths, SettingsStore};

// ---- fixtures ----------------------------------------------------------------------------------

/// Everything a setup test fakes.
struct Env {
    fakes: Fakes,
    installer: FakeInstaller,
    runner: FakeRunner,
    /// Holds the CLI link's folders.
    sandbox: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let sandbox = tempfile::tempdir().unwrap();
        let macos = sandbox.path().join("Hutan.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(macos.join("hutan"), "").unwrap();
        std::fs::create_dir(sandbox.path().join("bin")).unwrap();
        Self {
            fakes: Fakes::new(),
            installer: FakeInstaller::new(),
            runner: FakeRunner::new(),
            sandbox,
        }
    }

    fn link(&self) -> CliLink {
        CliLink {
            bin_dir: self.sandbox.path().join("bin"),
            target: self.sandbox.path().join("Hutan.app/Contents/MacOS/hutan"),
        }
    }

    fn link_path(&self) -> PathBuf {
        self.link().link()
    }
}

/// The user turned "Keep hooks installed on every start" on before this launch.
fn auto_install_on(daemon: &TestDaemon) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[integrations]\nauto_install = true\n",
    )
    .unwrap();
}

fn store(daemon: &TestDaemon) -> SettingsStore {
    SettingsStore::new(SettingsPaths::in_dir(daemon.home()))
}

/// A workbench showing the first-run setup, with every seam faked.
fn open_first_run<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    env: &Env,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let options = WorkbenchOptions::builtin(store(daemon), daemon.connection(), None)
        .with_mac(env.fakes.services())
        .with_first_run_setup()
        .with_cli_link(Some(env.link()))
        .with_installer(Arc::new(env.installer.clone()))
        .with_package_runner(Arc::new(env.runner.clone()));
    let (view, cx) = open_options(cx, options);
    wait_synced(&view, cx);
    (view, cx)
}

/// A workbench without the setup (the integrations and installs are what is tested).
fn open_plain<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    env: &Env,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let options = WorkbenchOptions::builtin(store(daemon), daemon.connection(), None)
        .with_mac(env.fakes.services())
        .with_installer(Arc::new(env.installer.clone()))
        .with_package_runner(Arc::new(env.runner.clone()));
    let (view, cx) = open_options(cx, options);
    wait_synced(&view, cx);
    (view, cx)
}

fn wait_synced(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
    });
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

fn drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
}

fn saved(daemon: &TestDaemon) -> String {
    std::fs::read_to_string(daemon.home().join("settings.toml")).unwrap_or_default()
}

fn step(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<SetupStep> {
    view.read_with(cx, |wb, _| wb.setup().map(|s| s.step))
}

fn setup_open(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| wb.setup_open())
}

fn go_to(view: &Entity<Workbench>, cx: &mut VisualTestContext, target: SetupStep) {
    while step(view, cx) != Some(target) {
        view.update_in(cx, |wb, window, cx| wb.setup_continue(window, cx));
        cx.run_until_parked();
        assert!(setup_open(view, cx), "the setup closed before {target:?}");
    }
}

fn close_window(cx: &mut VisualTestContext) {
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
}

fn pump_for(cx: &mut VisualTestContext, span: Duration) {
    let until = std::time::Instant::now() + span;
    while std::time::Instant::now() < until {
        draw(cx);
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn notice(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn acp_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-acp-fixture");
    assert!(bin.is_file(), "run `cargo build -p hutan-agents --bins`");
    bin
}

fn codex_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-codex-fixture");
    assert!(bin.is_file(), "run `cargo build -p hutan-agents --bins`");
    bin
}

/// What `codex app-server` says to `account/read` for a user logged in with ChatGPT.
const CODEX_CHATGPT: &str = r#"{"dir":"out","msg":{"method":"initialize","id":1,"params":{}}}
{"dir":"in","msg":{"id":1,"result":{"userAgent":"hutan/0.162.1 (Mac OS; arm64)"}}}
{"dir":"out","msg":{"method":"initialized"}}
{"dir":"out","msg":{"method":"account/read","id":2,"params":{}}}
{"dir":"in","msg":{"id":2,"result":{"account":{"type":"chatgpt","email":"me@example.com","planType":"plus"},"requiresOpenaiAuth":false}}}
"#;

/// A daemon whose OMP is logged in to `anthropic` and whose Claude Code CLI is logged out (its
/// ACP adapter does not exist); Codex is whatever the harness gives.
fn daemon_with_harnesses(scripts: &Path) -> TestDaemon {
    let omp = script(
        scripts,
        "omp",
        r#"case "$1" in
  models) echo '{"models":[{"provider":"anthropic","kind":"chat","id":"claude-opus-4-5","selector":"anthropic/claude-opus-4-5","name":"Claude Opus 4.5"}]}' ;;
  usage) echo '{"reports":[]}' ;;
  *) echo "fake omp $*" ;;
esac"#,
    );
    let claude = script(
        scripts,
        "claude",
        r#"if [ "$1 $2" = "auth status" ]; then echo '{"loggedIn":false,"authMethod":"none"}'; else echo "fake claude $*"; fi"#,
    );
    // The assistant's model is picked by the daemon (one tier below the session's Opus), so the
    // session is one whose transcript can switch models.
    let transcript = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/acp-omp-18.11/model-switch.jsonl");
    TestDaemon::start_with_config(&format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {acp:?}\nargs = [\"replay\", {t:?}, \"acp\"]\ncli = {omp:?}\n\n[harness.claude]\nbin = {missing:?}\ncli = {claude:?}\n",
        acp = acp_bin(),
        t = transcript,
        missing = scripts.join("no-such-claude-code-acp"),
    ))
}

fn wait_listing(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the account listing", |cx| {
        view.read_with(cx, |wb, cx| {
            *wb.harness_directory().read(cx).listing() == Listing::Ready
        })
    });
}

/// The harnesses the daemon found.
fn detected(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<&'static str> {
    view.read_with(cx, |wb, cx| {
        let directory = wb.harness_directory().read(cx);
        HARNESSES
            .iter()
            .filter(|h| directory.detection(h.id) == Detection::Installed)
            .map(|h| h.id)
            .collect()
    })
}

// ---- the sheet ---------------------------------------------------------------------------------

#[gpui::test]
fn the_setup_appears_on_the_first_launch_only(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    assert!(setup_open(&view, cx), "no app.setup_done yet");
    assert_eq!(step(&view, cx), Some(SetupStep::Welcome));
    assert!(drawn(&view, cx, Region::Setup("sheet")), "and it is drawn");
    assert_eq!(saved(&daemon), "", "showing it writes nothing");

    // Skip: done, and nothing else is written.
    click(&view, cx, Region::Setup("skip"));
    assert!(!setup_open(&view, cx));
    assert_eq!(saved(&daemon).trim(), "[app]\nsetup_done = true");

    // The next launch does not show it again.
    close_window(cx);
    drop(view);
    let (view, cx) = open_first_run(cx, &daemon, &env);
    assert!(!setup_open(&view, cx));
    assert!(!drawn(&view, cx, Region::Setup("sheet")));
}

#[gpui::test]
fn a_workbench_not_asked_for_the_setup_never_shows_it(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    let (view, cx) = open_plain(cx, &daemon, &env);
    assert!(!setup_open(&view, cx));
    assert_eq!(saved(&daemon), "");
}

#[gpui::test]
fn the_setup_is_modal_shortcuts_wait_and_escape_skips_it(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    assert!(setup_open(&view, cx));
    let sessions = |cx: &VisualTestContext| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).sessions().len())
    };

    // ⌘T and ⌘B do nothing behind the sheet.
    let left = view.read_with(cx, |wb, _| wb.state().docks.left.mode());
    cx.simulate_keystrokes("cmd-t cmd-b");
    pump_for(cx, Duration::from_millis(300));
    assert_eq!(sessions(cx), 0, "no terminal started behind the setup");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().docks.left.mode()),
        left
    );
    assert!(setup_open(&view, cx));

    // Escape skips it; the shortcuts work again.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!setup_open(&view, cx));
    assert!(
        saved(&daemon).contains("setup_done = true"),
        "{}",
        saved(&daemon)
    );
    cx.simulate_keystrokes("cmd-t");
    wait_until(cx, "the terminal after the setup", |cx| sessions(cx) == 1);
}

#[gpui::test]
fn back_continue_and_skip_walk_the_six_steps(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    assert!(
        !drawn(&view, cx, Region::Setup("back")),
        "nothing before Welcome"
    );

    let mut seen = vec![step(&view, cx).unwrap()];
    for _ in 0..5 {
        click(&view, cx, Region::Setup("continue"));
        seen.push(step(&view, cx).unwrap());
    }
    assert_eq!(seen, SetupStep::ALL);

    click(&view, cx, Region::Setup("back"));
    assert_eq!(step(&view, cx), Some(SetupStep::CommandLine));
    click(&view, cx, Region::Setup("continue"));
    assert_eq!(step(&view, cx), Some(SetupStep::Done));
    assert!(setup_open(&view, cx));
    assert!(
        !saved(&daemon).contains("setup_done"),
        "not done until it is finished"
    );

    // Enter continues too; on the last step it finishes.
    cx.simulate_keystrokes("enter");
    assert!(!setup_open(&view, cx));
    assert!(saved(&daemon).contains("setup_done = true"));
}

#[gpui::test]
fn the_setup_reopens_from_the_palette_and_from_settings(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    click(&view, cx, Region::Setup("skip"));
    assert!(!setup_open(&view, cx));

    // ⌘K lists it.
    assert!(view.read_with(cx, |wb, _| {
        wb.commands()
            .iter()
            .any(|c| c.title.as_ref() == "Hutan: Run Setup…")
    }));
    cx.dispatch_action(actions::RunSetup);
    cx.run_until_parked();
    assert!(setup_open(&view, cx));
    assert_eq!(step(&view, cx), Some(SetupStep::Welcome));
    click(&view, cx, Region::Setup("skip"));
    assert!(!setup_open(&view, cx));

    // Settings → app has "Run setup again".
    cx.dispatch_action(actions::OpenSettingsFor("app".into()));
    draw(cx);
    click(&view, cx, Region::Setup("run-setup"));
    assert!(setup_open(&view, cx));
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().is_none()));
}

// ---- Look --------------------------------------------------------------------------------------

#[gpui::test]
fn the_look_step_previews_live_and_writes_when_left(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::Look);

    click(&view, cx, Region::Setup("appearance-dark"));
    click(&view, cx, Region::Setup("density-comfortable"));
    view.read_with(cx, |wb, _| {
        assert_eq!(wb.shown_appearance(), AppearanceMode::Dark, "live preview");
        assert_eq!(wb.density(), Density::Comfortable);
    });
    assert!(
        !saved(&daemon).contains("appearance"),
        "not written yet: {}",
        saved(&daemon)
    );

    click(&view, cx, Region::Setup("continue"));
    let text = saved(&daemon);
    assert!(text.contains("appearance = \"dark\""), "{text}");
    assert!(text.contains("density = \"comfortable\""), "{text}");

    // Skipping a later look puts the window back to what is saved.
    close_window(cx);
    drop(view);
    let (view, cx) = open_first_run(cx, &daemon, &env);
    // (`app.setup_done` is not written yet: the setup is open again.)
    go_to(&view, cx, SetupStep::Look);
    click(&view, cx, Region::Setup("appearance-light"));
    click(&view, cx, Region::Setup("density-compact"));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.shown_appearance()),
        AppearanceMode::Light
    );
    click(&view, cx, Region::Setup("skip"));
    view.read_with(cx, |wb, _| {
        assert_eq!(wb.shown_appearance(), AppearanceMode::Dark);
        assert_eq!(wb.density(), Density::Comfortable);
    });
    assert!(saved(&daemon).contains("appearance = \"dark\""));
}

// ---- Agents ------------------------------------------------------------------------------------

#[gpui::test]
fn the_agents_step_lists_the_harnesses_and_writes_the_assistant_choice(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon_with_harnesses(scripts.path());
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::Agents);
    wait_listing(&view, cx);
    // The assistant is off until it is turned on (its backends show once it is).
    assert!(!drawn(&view, cx, Region::Setup("backend-omp")));
    click(&view, cx, Region::Setup("assistant"));

    // OMP is installed and logged in, Claude Code installed and logged out.
    view.read_with(cx, |wb, cx| {
        let directory = wb.harness_directory().read(cx);
        assert_eq!(directory.detection("omp"), Detection::Installed);
        assert!(!directory.logged_in("omp").is_empty());
        assert!(directory.logged_in("claude").is_empty());
        assert_eq!(HARNESSES[0].id, "omp", "OMP first");
    });
    assert!(drawn(&view, cx, Region::SetupHarness("omp", "login")));
    assert!(drawn(&view, cx, Region::SetupHarness("claude", "login")));
    // The default backend is OMP while it is logged in.
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.setup_backend(cx)),
        Some(AssistantBackend::Omp)
    );

    // Hooks and assistant: pending until the step is left. Hooks are off until asked for.
    assert!(!view.read_with(cx, |wb, _| wb.integrations_auto_install()));
    click(&view, cx, Region::Setup("auto-install"));
    click(&view, cx, Region::Setup("backend-none"));
    assert_eq!(view.read_with(cx, |wb, cx| wb.setup_backend(cx)), None);
    assert!(!saved(&daemon).contains("auto_install"));
    click(&view, cx, Region::Setup("backend-omp"));
    click(&view, cx, Region::Setup("continue"));
    let text = saved(&daemon);
    assert!(text.contains("auto_install = true"), "{text}");
    assert!(text.contains("enabled = true"), "{text}");

    // The backend reached the daemon: the assistant opens on OMP.
    view.update_in(cx, |wb, window, cx| wb.open_assistant(window, cx));
    wait_until(cx, "the OMP assistant", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).sessions().iter().any(|s| {
                s.chat
                    .as_ref()
                    .is_some_and(|c| c.agent == "omp" && c.assistant)
            })
        })
    });
}

#[gpui::test]
fn with_omp_missing_and_codex_logged_in_the_agents_step_preselects_codex(cx: &mut TestAppContext) {
    // The test daemon's OMP and Claude Code do not exist; Codex answers `account/read` as a
    // logged-in ChatGPT user. The assistant's default is the first ready harness: Codex.
    let scripts = tempfile::tempdir().unwrap();
    let account = scripts.path().join("codex-account.jsonl");
    std::fs::write(&account, CODEX_CHATGPT).unwrap();
    let daemon = TestDaemon::start_with_codex(hutan_agents::CodexConfig {
        program: codex_bin(),
        args: vec![
            "replay".to_owned(),
            account.display().to_string(),
            "app-server".to_owned(),
        ],
        env: Vec::new(),
    });
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::Agents);
    wait_listing(&view, cx);
    click(&view, cx, Region::Setup("assistant"));

    view.read_with(cx, |wb, cx| {
        let directory = wb.harness_directory().read(cx);
        assert!(matches!(directory.detection("omp"), Detection::NotFound(_)));
        assert!(directory.logged_in("omp").is_empty());
        assert!(!directory.logged_in("codex").is_empty());
    });
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.setup_backend(cx)),
        Some(AssistantBackend::Codex)
    );
    // Only the logged-in harness is offered, and it is the selected one.
    assert!(drawn(&view, cx, Region::Setup("backend-codex")));
    assert!(!drawn(&view, cx, Region::Setup("backend-omp")));
}

#[gpui::test]
fn the_assistant_is_off_in_the_setup_unless_it_is_turned_on(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::Agents);
    assert!(!view.read_with(cx, |wb, _| wb.setup().unwrap().assistant_enabled));
    click(&view, cx, Region::Setup("continue"));
    let text = saved(&daemon);
    assert!(text.contains("[assistant]\nenabled = false"), "{text}");
    assert!(!view.read_with(cx, |wb, _| wb.assistant_enabled()));
}

#[gpui::test]
fn turning_the_assistant_on_in_the_setup_is_written(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::Agents);
    click(&view, cx, Region::Setup("assistant"));
    assert!(view.read_with(cx, |wb, _| wb.setup().unwrap().assistant_enabled));
    click(&view, cx, Region::Setup("continue"));
    let text = saved(&daemon);
    assert!(text.contains("[assistant]\nenabled = true"), "{text}");
    // The Assistant is a page of the left sidebar now.
    assert!(view.read_with(cx, |wb, _| wb.assistant_enabled()));
}

#[gpui::test]
fn logging_in_pauses_the_setup_and_the_palette_continues_it(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon_with_harnesses(scripts.path());
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::Agents);
    wait_listing(&view, cx);

    click(&view, cx, Region::SetupHarness("claude", "login"));
    wait_until(cx, "the login terminal", |cx| {
        view.read_with(cx, |wb, cx| !wb.sessions().read(cx).sessions().is_empty())
    });
    assert!(!setup_open(&view, cx), "the window shows the terminal");
    assert_eq!(
        step(&view, cx),
        Some(SetupStep::Agents),
        "kept where it was"
    );
    assert!(!saved(&daemon).contains("setup_done"), "paused is not done");

    cx.dispatch_action(actions::RunSetup);
    cx.run_until_parked();
    assert!(setup_open(&view, cx));
    assert_eq!(step(&view, cx), Some(SetupStep::Agents), "continues");
}

// ---- Background --------------------------------------------------------------------------------

#[gpui::test]
fn the_background_step_has_both_toggles_off_and_applies_them_when_left(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::Background);
    view.read_with(cx, |wb, _| {
        let state = wb.setup().unwrap();
        assert!(!state.keep_running && !state.start_at_login, "both off");
    });

    click(&view, cx, Region::Setup("keep-running"));
    click(&view, cx, Region::Setup("start-at-login"));
    assert!(
        env.fakes.login.calls().is_empty(),
        "nothing before Continue"
    );
    assert!(!saved(&daemon).contains("keep_running"));

    click(&view, cx, Region::Setup("continue"));
    assert_eq!(env.fakes.login.calls(), [LoginItemCall::Register]);
    let text = saved(&daemon);
    assert!(text.contains("keep_running = true"), "{text}");
    assert!(text.contains("start_at_login = true"), "{text}");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.start_at_login_status()),
        LoginItemStatus::Enabled
    );
}

#[gpui::test]
fn keep_running_in_the_setup_does_not_register_the_login_item(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::Background);
    click(&view, cx, Region::Setup("keep-running"));
    click(&view, cx, Region::Setup("continue"));
    assert!(env.fakes.login.calls().is_empty());
    let text = saved(&daemon);
    assert!(text.contains("keep_running = true"), "{text}");
    assert!(text.contains("start_at_login = false"), "{text}");
}

#[gpui::test]
fn an_unapproved_login_item_is_shown_with_its_status(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    env.fakes
        .login
        .set_after_register(LoginItemStatus::RequiresApproval);
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::Background);
    click(&view, cx, Region::Setup("start-at-login"));
    click(&view, cx, Region::Setup("continue"));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.start_at_login_status()),
        LoginItemStatus::RequiresApproval
    );
    assert!(notice(&view, cx).is_some_and(|n| n.contains("Login Items")));
}

#[gpui::test]
fn outside_the_bundle_the_login_choice_is_unavailable(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let options =
        WorkbenchOptions::builtin(store(&daemon), daemon.connection(), None).with_first_run_setup();
    let (view, cx) = open_options(cx, options);
    wait_synced(&view, cx);
    go_to(&view, cx, SetupStep::Background);
    click(&view, cx, Region::Setup("start-at-login"));
    view.read_with(cx, |wb, _| assert!(!wb.setup().unwrap().start_at_login));
    click(&view, cx, Region::Setup("notifications"));
    // The button is inert: nothing to ask outside Hutan.app.
    view.read_with(cx, |wb, _| {
        assert_eq!(wb.setup().unwrap().notifications, NotificationUi::NotAsked);
    });
    click(&view, cx, Region::Setup("continue"));
    assert!(!saved(&daemon).contains("start_at_login"));
}

#[gpui::test]
fn notifications_are_asked_for_only_when_the_button_is_pressed(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::Background);
    draw(cx);
    assert_eq!(
        env.fakes.notifications.requests(),
        0,
        "showing the step asks nothing"
    );
    go_to(&view, cx, SetupStep::CommandLine);
    go_to(&view, cx, SetupStep::Done);
    assert_eq!(
        env.fakes.notifications.requests(),
        0,
        "neither does leaving it"
    );

    click(&view, cx, Region::Setup("back"));
    click(&view, cx, Region::Setup("back"));
    assert_eq!(step(&view, cx), Some(SetupStep::Background));

    // The prompt is up until the user answers.
    env.fakes.notifications.hold_answers();
    click(&view, cx, Region::Setup("notifications"));
    assert_eq!(env.fakes.notifications.requests(), 1);
    view.read_with(cx, |wb, _| {
        assert_eq!(wb.setup().unwrap().notifications, NotificationUi::Asking);
    });
    env.fakes.notifications.release();
    wait_until(cx, "the answer", |cx| {
        view.read_with(cx, |wb, _| {
            wb.setup().unwrap().notifications
                == NotificationUi::Answered(NotificationAnswer::Allowed)
        })
    });

    // A refusal is shown as such, and pressing again asks again.
    env.fakes
        .notifications
        .set_answer(NotificationAnswer::Denied);
    click(&view, cx, Region::Setup("notifications"));
    wait_until(cx, "the refusal", |cx| {
        view.read_with(cx, |wb, _| {
            wb.setup().unwrap().notifications
                == NotificationUi::Answered(NotificationAnswer::Denied)
        })
    });
    assert_eq!(env.fakes.notifications.requests(), 2);
}

#[gpui::test]
fn the_global_shortcut_is_an_optional_choice_written_when_left(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::Background);
    click(&view, cx, Region::Setup("shortcut-cmd-shift-space"));
    assert!(!saved(&daemon).contains("global_shortcut"));
    click(&view, cx, Region::Setup("continue"));
    assert!(saved(&daemon).contains("global_shortcut = \"cmd-shift-space\""));
    click(&view, cx, Region::Setup("back"));
    click(&view, cx, Region::Setup("shortcut-off"));
    click(&view, cx, Region::Setup("continue"));
    assert!(saved(&daemon).contains("global_shortcut = \"\""));
}

// ---- Command line ------------------------------------------------------------------------------

#[gpui::test]
fn the_command_line_step_links_hutan_only_when_asked_and_only_when_free(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::CommandLine);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.setup_cli_plan()),
        Some(Plan::Create)
    );

    // Off by default: leaving the step makes nothing.
    click(&view, cx, Region::Setup("continue"));
    assert!(!env.link_path().exists() && env.link_path().symlink_metadata().is_err());
    click(&view, cx, Region::Setup("back"));

    click(&view, cx, Region::Setup("cli-link"));
    click(&view, cx, Region::Setup("continue"));
    assert_eq!(
        std::fs::read_link(env.link_path()).unwrap(),
        env.link().target,
        "a symlink to the bundle's hutan, in the folder that was given"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.setup().unwrap().cli_outcome.clone())
            .map(|o| o.contains("Linked")),
        Some(true)
    );
}

#[gpui::test]
fn a_foreign_hutan_is_left_alone_and_the_command_is_shown(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    std::fs::write(env.link_path(), "#!/bin/sh\necho mine\n").unwrap();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::CommandLine);
    let plan = view.read_with(cx, |wb, _| wb.setup_cli_plan()).unwrap();
    let Plan::Manual { why, command } = plan else {
        panic!("expected a manual plan");
    };
    assert_eq!(why, Manual::Taken);
    assert!(command.starts_with("ln -sf "), "{command}");
    assert!(
        drawn(&view, cx, Region::Setup("copy-command")),
        "the command is on screen"
    );

    click(&view, cx, Region::Setup("cli-link"));
    click(&view, cx, Region::Setup("continue"));
    assert_eq!(
        std::fs::read_to_string(env.link_path()).unwrap(),
        "#!/bin/sh\necho mine\n",
        "the file is untouched"
    );
    assert!(notice(&view, cx).is_some_and(|n| n.contains("ln -sf")));
}

#[gpui::test]
fn without_a_bundle_the_command_line_step_has_nothing_to_link(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let options =
        WorkbenchOptions::builtin(store(&daemon), daemon.connection(), None).with_first_run_setup();
    let (view, cx) = open_options(cx, options);
    wait_synced(&view, cx);
    go_to(&view, cx, SetupStep::CommandLine);
    assert_eq!(view.read_with(cx, |wb, _| wb.setup_cli_plan()), None);
    click(&view, cx, Region::Setup("cli-link"));
    view.read_with(cx, |wb, _| assert!(!wb.setup().unwrap().cli_link));
}

// ---- Done --------------------------------------------------------------------------------------

#[gpui::test]
fn the_done_step_can_start_a_terminal(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::Done);
    assert!(drawn(&view, cx, Region::Setup("open-folder")));
    click(&view, cx, Region::Setup("new-terminal"));
    assert!(!setup_open(&view, cx));
    assert!(saved(&daemon).contains("setup_done = true"));
    wait_until(cx, "the terminal", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).sessions().len() == 1)
    });
}

#[gpui::test]
fn the_daemon_toggles_of_the_settings_sheet_are_the_same_choices(cx: &mut TestAppContext) {
    // Settings → app shows both and the setup writes the very keys they read.
    let daemon = TestDaemon::start();
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::Background);
    click(&view, cx, Region::Setup("keep-running"));
    click(&view, cx, Region::Setup("continue"));
    view.update_in(cx, |wb, window, cx| {
        wb.open_settings(Some("app"), window, cx)
    });
    let on = view.read_with(cx, |wb, _| wb.sheet_value("keep_running"));
    assert_eq!(on, Some(serde_json::Value::Bool(true)));
    cx.dispatch_action(daemon_actions::ToggleKeepRunning);
    cx.run_until_parked();
    assert!(!view.read_with(cx, |wb, _| wb.keep_running_enabled()));
}

// ---- integrations that install themselves -------------------------------------------------------

#[gpui::test]
fn integrations_install_for_detected_harnesses_that_lack_them(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon_with_harnesses(scripts.path());
    let env = Env::new();
    // OMP has them already; nothing is said about the others, so they are not installed.
    env.installer
        .set_status("omp", IntegrationStatus::Installed);
    auto_install_on(&daemon);
    let (view, cx) = open_plain(cx, &daemon, &env);
    wait_until(cx, "the install", |cx| {
        view.read_with(cx, |wb, _| wb.auto_install_done())
    });
    wait_listing(&view, cx);
    pump_for(cx, Duration::from_millis(200));

    let found = detected(&view, cx);
    assert!(
        found.contains(&"omp") && found.contains(&"claude"),
        "{found:?}"
    );
    let installs = env.installer.installs();
    let expected: Vec<&str> = found.iter().copied().filter(|h| *h != "omp").collect();
    assert_eq!(
        installs, expected,
        "only detected harnesses that lack the hooks"
    );
    for (_, harness) in env.installer.calls() {
        assert!(
            found.contains(&harness.as_str()),
            "{harness} was not detected"
        );
    }
    assert_eq!(
        view.read_with(cx, |wb, _| wb.integration_status("claude")),
        Some(IntegrationStatus::Installed)
    );
    assert_eq!(notice(&view, cx), None, "success is only logged");
}

#[gpui::test]
fn nothing_installs_while_the_setting_is_off_and_it_follows_when_turned_on(
    cx: &mut TestAppContext,
) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon_with_harnesses(scripts.path());
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[integrations]\nauto_install = false\n",
    )
    .unwrap();
    let env = Env::new();
    let (view, cx) = open_plain(cx, &daemon, &env);
    view.update(cx, |wb, cx| wb.refresh_integration_statuses(cx));
    // The harnesses are listed (the Accounts page would do it): still nothing is installed.
    view.update(cx, |wb, cx| {
        wb.harness_directory()
            .clone()
            .update(cx, |d, cx| d.refresh(cx));
    });
    wait_listing(&view, cx);
    pump_for(cx, Duration::from_millis(400));
    assert!(env.installer.installs().is_empty());
    assert!(!view.read_with(cx, |wb, _| wb.auto_install_done()));

    view.read_with(cx, |wb, _| {
        wb.settings_store()
            .write_user("integrations.auto_install", toml::Value::Boolean(true))
    })
    .unwrap();
    wait_until(cx, "the installs", |cx| {
        view.read_with(cx, |wb, _| wb.auto_install_done())
    });
    wait_until(cx, "a claude install", |_| {
        env.installer.installs().contains(&"claude".to_owned())
    });
}

#[gpui::test]
fn a_failed_install_shows_one_notice_with_the_commands(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon_with_harnesses(scripts.path());
    let env = Env::new();
    env.installer.fail_install("claude", "permission denied");
    env.installer.fail_install("omp", "disk full");
    auto_install_on(&daemon);
    let (view, cx) = open_plain(cx, &daemon, &env);
    wait_until(cx, "the notice", |cx| notice(&view, cx).is_some());
    let text = notice(&view, cx).unwrap();
    assert!(text.contains("hutan integration install claude"), "{text}");
    assert!(text.contains("hutan integration install omp"), "{text}");
    assert!(
        text.contains("permission denied") || text.contains("disk full"),
        "{text}"
    );
    pump_for(cx, Duration::from_millis(300));
    let attempts = env
        .installer
        .calls()
        .iter()
        .filter(|(a, _)| *a == "install")
        .count();
    assert_eq!(
        attempts,
        detected(&view, cx).len(),
        "once per harness, nothing retried"
    );
}

#[gpui::test]
fn the_agents_step_uninstalls_the_hooks_of_one_harness(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon_with_harnesses(scripts.path());
    let env = Env::new();
    env.installer
        .set_status("claude", IntegrationStatus::Installed);
    env.installer
        .set_status("omp", IntegrationStatus::Installed);
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::Agents);
    wait_listing(&view, cx);
    wait_until(cx, "the hook status", |cx| {
        view.read_with(cx, |wb, _| wb.integration_status("claude").is_some())
    });
    click(&view, cx, Region::SetupHarness("claude", "uninstall"));
    wait_until(cx, "the uninstall", |cx| {
        view.read_with(cx, |wb, _| {
            wb.integration_status("claude") == Some(IntegrationStatus::NotInstalled)
        })
    });
    assert!(
        env.installer
            .calls()
            .contains(&("uninstall", "claude".to_owned()))
    );
    assert!(
        !env.installer
            .calls()
            .contains(&("uninstall", "omp".to_owned())),
        "only the one that was asked for"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.integration_status("omp")),
        Some(IntegrationStatus::Installed)
    );
}

#[gpui::test]
fn hooks_are_off_until_ticked_and_only_the_ticked_agents_get_them(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon_with_harnesses(scripts.path());
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    // A fresh start installs nothing by itself, and the setting is off.
    pump_for(cx, Duration::from_millis(400));
    assert!(!view.read_with(cx, |wb, _| wb.integrations_auto_install()));
    assert!(env.installer.calls().iter().all(|(a, _)| *a != "install"));

    go_to(&view, cx, SetupStep::Agents);
    wait_listing(&view, cx);
    let found = detected(&view, cx);
    assert!(
        found.contains(&"omp") && found.contains(&"claude"),
        "{found:?}"
    );
    for harness in &found {
        assert!(
            drawn(&view, cx, Region::SetupHarness(harness, "hooks")),
            "a checkbox for {harness}"
        );
    }
    assert!(view.read_with(cx, |wb, _| wb.setup().unwrap().hooks.is_empty()));

    // Ticking Claude Code writes nothing yet…
    click(&view, cx, Region::SetupHarness("claude", "hooks"));
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .setup()
            .unwrap()
            .hooks
            .iter()
            .copied()
            .collect::<Vec<_>>()),
        ["claude"]
    );
    pump_for(cx, Duration::from_millis(200));
    assert!(env.installer.installs().is_empty());
    // …leaving the step installs the ticked one and only it.
    click(&view, cx, Region::Setup("continue"));
    wait_until(cx, "the claude install", |_| {
        env.installer.installs() == ["claude"]
    });
    pump_for(cx, Duration::from_millis(300));
    assert_eq!(env.installer.installs(), ["claude"], "OMP was not ticked");
    assert!(saved(&daemon).contains("auto_install = false"));
    assert!(!view.read_with(cx, |wb, _| wb.integrations_auto_install()));
}

#[gpui::test]
fn with_a_custom_home_the_agents_step_says_hooks_go_to_the_real_config(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon_with_harnesses(scripts.path());
    let env = Env::new();
    let options = WorkbenchOptions::builtin(store(&daemon), daemon.connection(), None)
        .with_mac(env.fakes.services())
        .with_first_run_setup()
        .with_cli_link(Some(env.link()))
        .with_installer(Arc::new(env.installer.clone()))
        .with_package_runner(Arc::new(env.runner.clone()))
        .with_custom_home(true);
    let (view, cx) = open_options(cx, options);
    wait_synced(&view, cx);
    assert!(view.read_with(cx, |wb, _| wb.integrations_custom_home()));
    go_to(&view, cx, SetupStep::Agents);
    wait_listing(&view, cx);
    assert!(drawn(&view, cx, Region::SetupHarness("claude", "hooks")));
    assert!(env.installer.calls().iter().all(|(a, _)| *a != "install"));
}

// ---- Install… for a harness that is not there --------------------------------------------------

fn claude_source() -> &'static hutan_desktop::harness_install::Source {
    SOURCES.iter().find(|s| s.harness == "claude").unwrap()
}

fn modal_text(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<(String, String)> {
    view.read_with(cx, |wb, _| {
        wb.modal()
            .map(|m| (m.title.to_string(), m.message.to_string()))
    })
}

#[gpui::test]
fn a_package_that_is_not_the_table_s_is_refused_with_a_message(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    env.runner.answer(
        claude_source().package,
        Ok(&serde_json::json!({
            "name": claude_source().package,
            "version": "9.9.9",
            "repository.url": "git+https://github.com/someone-else/claude-agent-acp.git",
            "bin": { "claude-agent-acp": "x.js" },
            "dist.integrity": "sha512-EVIL",
        })
        .to_string()),
    );
    let (view, cx) = open_plain(cx, &daemon, &env);
    view.update_in(cx, |wb, window, cx| {
        wb.install_harness("claude", window, cx)
    });
    wait_until(cx, "the refusal", |cx| {
        notice(&view, cx).is_some_and(|n| n.starts_with("Not installing"))
    });
    assert!(notice(&view, cx).unwrap().contains("someone-else"));
    assert!(modal_text(&view, cx).is_none(), "nothing to confirm");
    assert!(view.read_with(cx, |wb, _| wb.harness_installs_running().is_empty()));
    pump_for(cx, Duration::from_millis(200));
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).sessions().len()),
        0,
        "no terminal was opened"
    );
}

#[gpui::test]
fn a_genuine_package_is_confirmed_with_its_version_and_only_then_installed(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    env.runner.genuine(claude_source(), "0.93.1");
    let (view, cx) = open_plain(cx, &daemon, &env);
    // Nothing installs hooks at launch any more, so nothing lists the accounts by itself either:
    // the Agents step or the Start sheet does, and the test stands in for them.
    view.update(cx, |wb, cx| {
        wb.harness_directory()
            .clone()
            .update(cx, |d, cx| d.refresh(cx));
    });
    view.update_in(cx, |wb, window, cx| {
        wb.install_harness("claude", window, cx)
    });
    wait_until(cx, "the confirmation", |cx| modal_text(&view, cx).is_some());
    let (title, message) = modal_text(&view, cx).unwrap();
    assert!(
        title.contains("@agentclientprotocol/claude-agent-acp"),
        "{title}"
    );
    assert!(title.contains("0.93.1"), "{title}");
    assert!(
        title.contains("github.com/agentclientprotocol/claude-agent-acp"),
        "{title}"
    );
    assert!(message.contains("sha512-GENUINE"), "{message}");
    assert!(
        message.contains("npm install -g @agentclientprotocol/claude-agent-acp@0.93.1"),
        "{message}"
    );
    assert_eq!(
        env.runner.views(),
        ["@agentclientprotocol/claude-agent-acp@latest"]
    );
    pump_for(cx, Duration::from_millis(200));
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).sessions().len()),
        0,
        "nothing runs before the user confirms"
    );

    // Cancel: still nothing.
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    pump_for(cx, Duration::from_millis(200));
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).sessions().len()),
        0
    );

    // Confirm: an ordinary terminal tab; when it exits with 0 the detection asks again.
    view.update_in(cx, |wb, window, cx| {
        wb.install_harness("claude", window, cx)
    });
    wait_until(cx, "the confirmation", |cx| modal_text(&view, cx).is_some());
    wait_listing(&view, cx);
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "the install tab", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).sessions().len() == 1 && wb.state().active_item().is_some()
        })
    });
    wait_until(cx, "the install to finish", |cx| {
        notice(&view, cx).is_some_and(|n| n.starts_with("Installed @agentclientprotocol"))
    });
    assert!(view.read_with(cx, |wb, _| wb.harness_installs_running().is_empty()));
    wait_listing(&view, cx);
}

#[gpui::test]
fn a_failed_install_says_what_to_run_by_hand(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    env.runner.genuine(claude_source(), "0.93.1");
    env.runner.set_install_exit(1);
    let (view, cx) = open_plain(cx, &daemon, &env);
    view.update_in(cx, |wb, window, cx| {
        wb.install_harness("claude", window, cx)
    });
    wait_until(cx, "the confirmation", |cx| modal_text(&view, cx).is_some());
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "the failure", |cx| {
        notice(&view, cx).is_some_and(|n| n.contains("exited with 1"))
    });
    assert!(
        notice(&view, cx)
            .unwrap()
            .contains("npm install -g @agentclientprotocol/claude-agent-acp@0.93.1")
    );
}

#[gpui::test]
fn without_npm_the_command_and_the_source_are_shown_and_nothing_runs(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    env.runner.set_programs(&[]);
    let (view, cx) = open_plain(cx, &daemon, &env);
    view.update_in(cx, |wb, window, cx| {
        wb.install_harness("claude", window, cx)
    });
    wait_until(cx, "the notice", |cx| {
        notice(&view, cx).is_some_and(|n| n.contains("npm was not found"))
    });
    let text = notice(&view, cx).unwrap();
    assert!(
        text.contains("npm install -g @agentclientprotocol/claude-agent-acp"),
        "{text}"
    );
    assert!(
        text.contains("https://github.com/agentclientprotocol/claude-agent-acp"),
        "{text}"
    );
    assert!(env.runner.views().is_empty());
    assert!(modal_text(&view, cx).is_none());
}

#[gpui::test]
fn install_buttons_appear_for_missing_harnesses_only_with_a_runner(cx: &mut TestAppContext) {
    // The default daemon finds none of the harnesses.
    let daemon = TestDaemon::start();
    let env = Env::new();
    env.runner.genuine(claude_source(), "0.93.1");
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::Agents);
    wait_listing(&view, cx);
    wait_until(cx, "the Install… button", |cx| {
        drawn(&view, cx, Region::SetupHarness("claude", "install"))
    });
    click(&view, cx, Region::SetupHarness("claude", "install"));
    wait_until(cx, "the confirmation", |cx| modal_text(&view, cx).is_some());
    assert!(
        modal_text(&view, cx)
            .unwrap()
            .0
            .contains("claude-agent-acp")
    );
    close_window(cx);
    drop(view);

    // Without a package runner there is no button: the guide link stays.
    let options = WorkbenchOptions::builtin(store(&daemon), daemon.connection(), None)
        .with_mac(env.fakes.services())
        .with_first_run_setup();
    let (view, cx) = open_options(cx, options);
    wait_synced(&view, cx);
    go_to(&view, cx, SetupStep::Agents);
    wait_listing(&view, cx);
    wait_until(cx, "the guide button", |cx| {
        drawn(&view, cx, Region::SetupHarness("claude", "install-guide"))
    });
    assert!(!drawn(&view, cx, Region::SetupHarness("claude", "install")));
}

#[gpui::test]
fn the_accounts_page_offers_install_for_a_missing_harness(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let env = Env::new();
    env.runner.genuine(claude_source(), "0.93.1");
    let (view, cx) = open_plain(cx, &daemon, &env);
    view.update_in(cx, |wb, window, cx| wb.open_accounts(window, cx));
    wait_listing(&view, cx);
    wait_until(cx, "the Install… button", |cx| {
        drawn(&view, cx, Region::Account("claude", "install"))
    });
    click(&view, cx, Region::Account("claude", "install"));
    wait_until(cx, "the confirmation", |cx| modal_text(&view, cx).is_some());
}

#[gpui::test]
fn the_fakes_of_a_login_item_work_with_the_setup_services(cx: &mut TestAppContext) {
    // A guard for the shared fixture: the services handed to the workbench are the fakes the
    // test keeps, not the inert ones.
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    let (view, cx) = open_with_services(cx, &daemon, fakes.services());
    wait_synced(&view, cx);
    assert!(view.read_with(cx, |wb, _| wb.start_at_login_available()));
}

// ---- Send key ----------------------------------------------------------------------------------

#[gpui::test]
fn the_agents_step_offers_the_send_key_and_leaving_it_stops_the_composer_asking(
    cx: &mut TestAppContext,
) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon_with_harnesses(scripts.path());
    let env = Env::new();
    let (view, cx) = open_first_run(cx, &daemon, &env);
    go_to(&view, cx, SetupStep::Agents);
    wait_listing(&view, cx);
    assert!(!saved(&daemon).contains("send_key"));

    // The one line is on the page and writes the user's choice at once.
    assert!(drawn(&view, cx, Region::Setup("send-key-enter")));
    // It is the last row of a page that scrolls: bring it above the footer.
    let at = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Setup("sheet")))
        .unwrap()
        .center();
    for _ in 0..20 {
        let row = view
            .read_with(cx, |wb, _| {
                wb.region_bounds(Region::Setup("send-key-enter"))
            })
            .unwrap();
        if row.bottom() < gpui::px(900.0) {
            break;
        }
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: at,
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(-60.0))),
            modifiers: Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        draw(cx);
    }
    click(&view, cx, Region::Setup("send-key-enter"));
    let text = saved(&daemon);
    assert!(text.contains("send_key = \"enter\""), "{text}");
    click(&view, cx, Region::Setup("send-key-cmd-enter"));
    assert!(saved(&daemon).contains("send_key = \"cmd-enter\""));

    // Leaving the page counts as having been asked: the composer will not ask again.
    click(&view, cx, Region::Setup("continue"));
    let text = saved(&daemon);
    assert!(text.contains("ask_send_key = false"), "{text}");
}
