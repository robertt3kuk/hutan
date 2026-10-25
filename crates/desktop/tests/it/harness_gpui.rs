#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers
//! The desktop for harness chats (M8-B): the new-agent picker with OMP first, the model switch of
//! a harness chat, the Accounts page with Log in, `assistant.enabled`, and the assistant's
//! backend per scope. The daemon runs in this process; OMP is `hutan-acp-fixture replay` and the
//! harnesses' own CLIs are shell scripts. No real harness, login or model is touched.

use crate::common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use common::{TestDaemon, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::{Detection, LaunchAgent, Models, Region, Workbench};
use hutan_protocol::chat::{ChatCreate, ChatModels, ChatModelsRequest};
use hutan_protocol::messages::{Request, SessionResult};
use hutan_settings::{SettingsPaths, SettingsStore};

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

fn transcript(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/acp-omp-18.11")
        .join(format!("{name}.jsonl"))
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A daemon whose OMP replays `transcript` and whose `omp` / `claude` CLIs are scripts: OMP is
/// logged in to `anthropic`, Claude Code is logged out, and its ACP adapter is missing. Every
/// start of OMP's ACP agent and every `omp models` leaves a line in [`spawns`].
fn daemon(scripts: &Path, transcript_name: &str, extra: &str) -> TestDaemon {
    let count = scripts.join("spawns");
    let omp = script(
        scripts,
        "omp",
        &format!(
            r#"case "$1" in
  models) echo models >> {count:?}; echo '{{"models":[{{"provider":"anthropic","kind":"chat","id":"claude-opus-4-5","selector":"anthropic/claude-opus-4-5","name":"Claude Opus 4.5"}}]}}' ;;
  usage) echo '{{"reports":[]}}' ;;
  *) echo "fake omp $*" ;;
esac"#
        ),
    );
    let acp = script(
        scripts,
        "omp-acp",
        &format!("echo acp >> {count:?}\nexec {:?} \"$@\"", acp_bin()),
    );
    let claude = script(
        scripts,
        "claude",
        r#"if [ "$1 $2" = "auth status" ]; then echo '{"loggedIn":false,"authMethod":"none"}'; else echo "fake claude $*"; fi"#,
    );
    TestDaemon::start_with_config(&format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {acp:?}\nargs = [\"replay\", {t:?}, \"acp\"]\ncli = {omp:?}\n\n[harness.claude]\nbin = {missing:?}\ncli = {claude:?}\n{extra}",
        t = transcript(transcript_name),
        missing = scripts.join("no-such-claude-code-acp"),
    ))
}

/// How many times the daemon of [`daemon`] started OMP (its ACP agent or `omp models`).
fn spawns(scripts: &Path) -> usize {
    std::fs::read_to_string(scripts.join("spawns")).map_or(0, |text| text.lines().count())
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    open_at(cx, daemon, None)
}

/// [`open`] with the layout (and what is kept next to it) at `state`, as the app runs.
fn open_at<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    state: Option<PathBuf>,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        common::with_assistant(SettingsStore::new(SettingsPaths::in_dir(daemon.home()))),
        daemon.connection(),
        state,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

/// The header's backend·model chip opens a menu; its second row is the chooser of backends and
/// models.
fn open_backend_chooser(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    click(view, cx, Region::Assistant("backend"));
    let entries = view.read_with(cx, |wb, _| wb.context_menu_entries().unwrap());
    assert_eq!(entries[1].0.as_ref(), "Backends and models…");
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(1, window, cx));
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
    });
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
}

#[gpui::test]
fn start_lists_the_harnesses_and_starts_an_omp_chat(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon(scripts.path(), "plain-turn", "");
    let (view, cx) = open(cx, &daemon);
    cx.simulate_keystrokes("cmd-shift-t");
    view.update(cx, |wb, cx| {
        wb.launcher_set_kind(hutan_desktop::sessions::launch::LaunchKind::Chat, cx);
    });
    // OMP is first and preselected; its models arrive grouped by provider.
    wait_until(cx, "the OMP models", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher()
                .is_some_and(|l| l.agent() == &LaunchAgent::Harness("omp".into()))
                && matches!(wb.launcher_models(cx), Models::Ready(_))
        })
    });
    // What is installed and logged in: OMP is, Claude Code's adapter is not, Codex is a stand-in.
    wait_until(cx, "the detection", |cx| {
        view.read_with(cx, |wb, cx| {
            let directory = wb.harness_directory().read(cx);
            directory.detection("omp") == Detection::Installed
                && matches!(
                    directory.detection("claude"),
                    Detection::NotFound(_) | Detection::Installed
                )
                && !directory.logged_in("omp").is_empty()
        })
    });
    view.read_with(cx, |wb, cx| {
        let directory = wb.harness_directory().read(cx);
        assert_eq!(directory.logged_in("omp")[0].provider, "anthropic");
        let Models::Ready(models) = wb.launcher_models(cx) else {
            panic!("the models are ready");
        };
        assert!(!models.is_empty());
        let groups: Vec<String> = models
            .iter()
            .filter_map(|m| m.id.split_once('/').map(|(p, _)| p.to_owned()))
            .collect();
        assert!(!groups.is_empty());
        // Grouped: a provider's models are contiguous.
        let mut seen = Vec::new();
        for g in groups {
            if seen.last() != Some(&g) {
                assert!(!seen.contains(&g), "{g} is split");
                seen.push(g);
            }
        }
    });
    common::draw(cx);
    for id in ["omp", "claude", "codex"] {
        assert!(
            view.read_with(cx, |wb, _| wb
                .region_bounds(Region::LauncherChoice("agent", id))
                .is_some()),
            "{id} choice"
        );
    }
    click(&view, cx, Region::Launcher("prompt"));
    cx.simulate_input("Fix the build");
    view.update_in(cx, |wb, window, cx| wb.launcher_start(false, window, cx));
    // The first prompt to this agent and model asks before it is sent.
    view.update_in(cx, |wb, window, cx| wb.launcher_confirm_start(window, cx));
    wait_until(cx, "the chat tab", |cx| {
        view.read_with(cx, |wb, _| {
            wb.launcher().is_none()
                && wb
                    .state()
                    .active_item()
                    .is_some_and(|i| matches!(i, ItemRef::Chat { .. }))
        })
    });
    let session = view
        .read_with(cx, |wb, cx| {
            wb.sessions().read(cx).sessions().first().cloned()
        })
        .unwrap();
    let chat = session.chat.unwrap();
    assert_eq!(chat.agent, "omp");
    assert!(!chat.model.is_empty());
}

/// SANDBOX-1: a chat created with a preset draws the safety chip in its header (in place of
/// the harness's mode chip), built from what the daemon reports of it, not from the request.
#[gpui::test]
fn a_chat_started_with_a_preset_draws_the_safety_chip_the_daemon_reported(cx: &mut TestAppContext) {
    use hutan_core::{SafetyEnforcer, SafetyPreset};
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon(scripts.path(), "plain-turn", "");
    let cwd = scripts.path().canonicalize().unwrap();
    let created: SessionResult = daemon.call(Request::ChatCreate(ChatCreate {
        workspace_id: None,
        cwd,
        agent: "omp".into(),
        model: None,
        approval_policy: None,
        sandbox: None,
        title: None,
        mode: None,
        provider_id: None,
        system_prompt: None,
        account: None,
        safety: Some(SafetyPreset::Full),
    }));
    let safety = created.session.chat.clone().unwrap().safety.unwrap();
    assert_eq!(
        (safety.preset, safety.enforced_by),
        (SafetyPreset::Full, SafetyEnforcer::None)
    );
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| {
        wb.open_session(created.session.id, window, cx)
    });
    wait_until(cx, "the safety chip in the header", |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Chat("mode")).is_some())
    });
    let chip = view.read_with(cx, |wb, cx| {
        let chat = wb
            .sessions()
            .read(cx)
            .session(created.session.id)
            .and_then(|s| s.chat.clone())
            .unwrap();
        hutan_desktop::workbench::chat::safety_chip(&chat).unwrap()
    });
    assert_eq!(chip.text, "Full access · no boundary");
    assert!(chip.warning, "Full access is drawn as a warning");
}

#[gpui::test]
fn a_harness_chat_switches_its_model_between_turns(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon(scripts.path(), "model-switch", "");
    // A chat on OMP's default model, made as `hutan chat new` would (the recorded transcript
    // sets the model when the chat starts, and once more for the switch).
    let models: ChatModels = daemon.call(Request::ChatModels(ChatModelsRequest {
        agent: "omp".into(),
    }));
    let default = models
        .models
        .iter()
        .find(|m| m.is_default)
        .or_else(|| models.models.first())
        .expect("OMP offers models")
        .id
        .clone();
    let cwd = scripts.path().canonicalize().unwrap();
    let created: SessionResult = daemon.call(Request::ChatCreate(ChatCreate {
        workspace_id: None,
        cwd,
        agent: "omp".into(),
        model: Some(default),
        approval_policy: None,
        sandbox: None,
        title: None,
        mode: None,
        provider_id: None,
        system_prompt: None,
        account: None,
        safety: None,
    }));
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| {
        wb.open_session(created.session.id, window, cx)
    });
    let session = created.session;
    let before = session.chat.clone().unwrap().model;
    click(&view, cx, Region::Chat("switch-model"));
    wait_until(cx, "the models of the chat's harness", |cx| {
        view.read_with(cx, |wb, _| {
            wb.agent_picker().is_some_and(|p| {
                p.switch_session == session.id && p.agent == "omp" && p.shown().len() > 1
            })
        })
    });
    // The model the chat runs is marked, and the cursor starts on it.
    let current_of = |view: &Entity<Workbench>, cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, _| {
            let picker = wb.agent_picker().unwrap();
            let rows = picker.rows();
            let marked: Vec<(usize, String)> = rows
                .iter()
                .enumerate()
                .filter(|(_, r)| r.current)
                .map(|(at, r)| (at, r.model.id.clone()))
                .collect();
            (marked, picker.selected)
        })
    };
    let (marked, selected) = current_of(&view, cx);
    assert_eq!(marked.len(), 1, "one current model: {marked:?}");
    assert_eq!(marked[0].1, before);
    assert_eq!(
        selected, marked[0].0,
        "the cursor starts on the current model"
    );
    let other = view.update(cx, |wb, _| {
        let picker = wb.agent_picker().unwrap();
        picker
            .shown()
            .iter()
            .position(|m| m.id != before)
            .expect("another model")
    });
    view.update(cx, |wb, cx| wb.agent_picker_choose(other, cx));
    // Moving the cursor does not move the mark: it says what runs.
    assert_eq!(current_of(&view, cx).0[0].1, before);
    view.update_in(cx, |wb, window, cx| wb.agent_picker_confirm(window, cx));
    wait_until(cx, "the switch to be answered", |cx| {
        view.read_with(cx, |wb, _| wb.agent_picker().is_none())
    });
    // Once the chat runs another model, the dialog marks that one.
    let running = |view: &Entity<Workbench>, cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(session.id)
                .and_then(|s| s.chat.as_ref().map(|c| c.model.clone()))
                .unwrap_or_default()
        })
    };
    wait_until(cx, "the chat to run another model", |cx| {
        running(&view, cx) != before
    });
    let now = running(&view, cx);
    click(&view, cx, Region::Chat("switch-model"));
    wait_until(cx, "the dialog again", |cx| {
        view.read_with(cx, |wb, _| {
            wb.agent_picker().is_some_and(|p| p.shown().len() > 1)
        })
    });
    assert_eq!(current_of(&view, cx).0[0].1, now);
}

#[gpui::test]
fn accounts_list_the_harnesses_and_log_in_focuses_the_login_session(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon(scripts.path(), "plain-turn", "");
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| wb.open_accounts(window, cx));
    wait_until(cx, "the accounts", |cx| {
        view.read_with(cx, |wb, cx| {
            !matches!(
                wb.harness_directory().read(cx).listing(),
                hutan_desktop::workbench::Listing::Idle
                    | hutan_desktop::workbench::Listing::Loading
            )
        })
    });
    assert_eq!(
        view.read_with(cx, |wb, cx| wb
            .harness_directory()
            .read(cx)
            .listing()
            .clone()),
        hutan_desktop::workbench::Listing::Ready
    );
    view.read_with(cx, |wb, cx| {
        let directory = wb.harness_directory().read(cx);
        let summary: Vec<_> = directory
            .accounts()
            .iter()
            .map(|a| (a.harness.as_str(), a.provider.as_str(), a.status))
            .collect();
        assert!(summary.contains(&(
            "omp",
            "anthropic",
            hutan_protocol::account::AccountStatus::LoggedIn
        )));
        assert!(summary.contains(&(
            "claude",
            "anthropic",
            hutan_protocol::account::AccountStatus::LoggedOut
        )));
        // Never a token or an address.
        let json = serde_json::to_string(directory.accounts()).unwrap();
        assert!(!json.contains("example.com") && !json.contains("token"));
    });
    common::draw(cx);
    assert!(view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::Account("omp", "card")).is_some()
    }));
    click(&view, cx, Region::Account("omp", "login"));
    wait_until(cx, "the login session to be shown", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.settings_sheet().is_none()
                && wb.state().active_item().is_some_and(|item| match item {
                    ItemRef::Terminal { session } => session
                        .parse()
                        .ok()
                        .and_then(|id| wb.sessions().read(cx).session(id).cloned())
                        .is_some_and(|s| s.title.contains("Log in to omp")),
                    _ => false,
                })
        })
    });
}

#[gpui::test]
fn the_assistant_can_be_turned_off(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon(scripts.path(), "plain-turn", "");
    let (view, cx) = open(cx, &daemon);
    assert!(view.read_with(cx, |wb, _| {
        wb.commands().iter().any(|c| c.title.contains("Mawas"))
    }));
    view.update_in(cx, |wb, window, cx| wb.open_assistant(window, cx));
    assert!(view.read_with(cx, |wb, _| wb.panel_open("assistant")));
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[assistant]\nenabled = false\n",
    )
    .unwrap();
    wait_until(cx, "the setting to apply", |cx| {
        view.read_with(cx, |wb, _| !wb.assistant_enabled())
    });
    assert!(!view.read_with(cx, |wb, _| wb.panel_open("assistant")));
    assert!(!view.read_with(cx, |wb, _| {
        wb.commands().iter().any(|c| c.title.contains("Mawas"))
    }));
    // Opening it does nothing now.
    cx.dispatch_action(hutan_desktop::workbench::actions::OpenAssistant);
    assert!(!view.read_with(cx, |wb, _| wb.panel_open("assistant")));
}

#[gpui::test]
fn an_unconfigured_assistant_opens_on_the_ready_omp_with_the_model_one_tier_down(
    cx: &mut TestAppContext,
) {
    // OMP is installed and logged in and nothing names a backend: the daemon picks OMP and, of
    // the models of its session (current: Opus 4.6), the newest Sonnet of that provider. There
    // is no first-run card.
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon(scripts.path(), "model-switch", "");
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| wb.open_assistant(window, cx));
    let omp_assistant = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .sessions()
                .iter()
                .find(|s| {
                    s.chat
                        .as_ref()
                        .is_some_and(|c| c.agent == "omp" && c.assistant)
                })
                .cloned()
        })
    };
    wait_until(cx, "the OMP assistant", |cx| omp_assistant(cx).is_some());
    // Opening started nothing: the model is picked when the agent first starts, with the
    // first message.
    let session = omp_assistant(cx).unwrap();
    assert_eq!(session.chat.as_ref().unwrap().model, "");
    let _: hutan_protocol::chat::ChatSent = daemon.call(
        hutan_protocol::messages::Request::ChatSend(hutan_protocol::chat::ChatSend {
            session_id: session.id,
            text: "hello".into(),
            attachments: Vec::new(),
        }),
    );
    wait_until(
        cx,
        "the first message to start the agent on the picked model",
        |cx| {
            omp_assistant(cx)
                .is_some_and(|s| s.chat.as_ref().unwrap().model == "gitlab-duo/duo-chat-sonnet-4-6")
        },
    );
    common::draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Assistant("use-omp")))
            .is_none(),
        "nothing is left to choose"
    );
    // The header says what runs; the chooser's default is the automatic choice.
    open_backend_chooser(&view, cx);
    click(&view, cx, Region::Assistant("backend-omp"));
    common::draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::Assistant("use-default")))
            .is_some()
    );
}

#[gpui::test]
fn the_panel_header_switches_the_backend_and_model(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon(
        scripts.path(),
        "model-switch",
        "[assistant]\nbackend = \"omp\"\n",
    );
    let (view, cx) = open(cx, &daemon);
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
    // The header says what it runs on; the chooser lists the backends and the models of the
    // one picked, and the default model of OMP is one click.
    open_backend_chooser(&view, cx);
    click(&view, cx, Region::Assistant("backend-omp"));
    click(&view, cx, Region::Assistant("use-default"));
    wait_until(
        cx,
        "the chooser to close and the assistant to open again",
        |cx| {
            view.read_with(cx, |wb, cx| {
                wb.sessions().read(cx).sessions().iter().any(|s| {
                    s.chat
                        .as_ref()
                        .is_some_and(|c| c.agent == "omp" && c.assistant)
                })
            })
        },
    );
    // The choice is the daemon's: a restart of this window finds the same backend.
    let listed: hutan_protocol::assistant::AssistantList = daemon.call(
        hutan_protocol::messages::Request::AssistantList(Default::default()),
    );
    assert_eq!(
        listed.assistants[0].backend,
        hutan_protocol::assistant::AssistantBackend::Omp
    );
}

#[gpui::test]
fn opening_mawas_starts_no_agent_and_resources_says_it_is_idle(cx: &mut TestAppContext) {
    use hutan_desktop::workbench::actions;
    use hutan_desktop::workbench::panels::{ResourceLine, ResourcesPanel};

    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon(
        scripts.path(),
        "model-switch",
        "[assistant]\nbackend = \"omp\"\n",
    );
    let (view, cx) = open(cx, &daemon);
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
    let title = view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .sessions()
            .iter()
            .find(|s| s.chat.as_ref().is_some_and(|c| c.assistant))
            .map(|s| s.title.clone())
            .unwrap()
    });
    // Nothing runs for it: no process rows, only the idle line.
    common::draw(cx);
    cx.dispatch_action(actions::ShowResources);
    common::draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<ResourcesPanel>("resources"))
        .expect("the resources panel is shown");
    wait_until(cx, "a resource sample and the idle line", |cx| {
        panel.read_with(cx, |p, cx| {
            p.lines(cx)
                .iter()
                .any(|l| matches!(l, ResourceLine::Note(t) if t == &format!("{title} · idle, no process")))
        })
    });
    let lines = panel.read_with(cx, |p, cx| p.lines(cx));
    assert!(
        lines
            .iter()
            .all(|l| !matches!(l, ResourceLine::Session(_) | ResourceLine::Process { .. })),
        "no row of an agent process: {lines:?}"
    );
}

/// The chat's model in the fixture's catalog: `Duo Chat Opus 4.6` (FX-E2E2-B).
const FIXTURE_MODEL: &str = "gitlab-duo/duo-chat-opus-4-6";
const FIXTURE_NAME: &str = "Duo Chat Opus 4.6";

#[gpui::test]
fn model_names_survive_an_app_restart_with_no_harness_spawn(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon(scripts.path(), "plain-turn", "");
    let app = tempfile::tempdir().unwrap();
    let state = app.path().join("desktop-state.json");
    let names_file = app
        .path()
        .join(hutan_desktop::sessions::models::MODEL_NAMES_FILE);

    // The first run asks OMP for its models once (the Start sheet does) and starts a chat.
    let (view, vcx) = open_at(cx, &daemon, Some(state.clone()));
    view.update(vcx, |wb, cx| {
        wb.harness_directory()
            .update(cx, |directory, cx| directory.load_models("omp", cx));
    });
    wait_until(vcx, "the catalog to be learned and kept", |_| {
        std::fs::read_to_string(&names_file).is_ok_and(|text| text.contains(FIXTURE_NAME))
    });
    let cwd = scripts.path().canonicalize().unwrap();
    let created: SessionResult = daemon.call(Request::ChatCreate(ChatCreate {
        workspace_id: None,
        cwd,
        agent: "omp".into(),
        model: Some(FIXTURE_MODEL.into()),
        approval_policy: None,
        sandbox: None,
        title: None,
        mode: None,
        provider_id: None,
        system_prompt: None,
        account: None,
        safety: None,
    }));
    let session = created.session.id;
    wait_until(vcx, "the chat in the first window", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).session(session).is_some()
        })
    });
    let before = spawns(scripts.path());
    assert!(before >= 1, "the catalog and the chat started OMP");
    vcx.update(|window, _| window.remove_window());
    drop(view);

    // The app again: nothing asks OMP, and every surface reads the model by its name.
    let (view, vcx) = open_at(cx, &daemon, Some(state));
    view.update_in(vcx, |wb, window, cx| wb.open_session(session, window, cx));
    wait_until(vcx, "the chat header", |cx| {
        view.read_with(cx, |wb, _| wb.chat_view_of(session).is_some())
    });
    common::draw(vcx);
    let (who, tooltip) = view.read_with(vcx, |wb, cx| {
        let info = wb.sessions().read(cx).session(session).cloned().unwrap();
        wb.chat_view_of(session)
            .unwrap()
            .read(cx)
            .who(&info, cx)
            .unwrap()
    });
    assert_eq!(who, format!("OMP · {FIXTURE_NAME}"));
    assert_eq!(
        tooltip.as_deref(),
        Some(FIXTURE_MODEL),
        "the tooltip carries the id"
    );
    // The Agents page and rail rows.
    let record = view.read_with(vcx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .agent_presentation()
            .records
            .iter()
            .find(|r| r.session == session)
            .cloned()
            .unwrap()
    });
    assert_eq!(record.model.as_deref(), Some(FIXTURE_NAME));
    assert_eq!(record.model_id.as_deref(), Some(FIXTURE_MODEL));
    assert_eq!(
        hutan_desktop::sessions::agents_text::harness_text(&record),
        format!("OMP · {FIXTURE_NAME}")
    );
    // The session list's state words too.
    let detail = view.read_with(vcx, |wb, cx| {
        let store = wb.sessions().read(cx);
        store.presentation(store.session(session).unwrap()).detail
    });
    assert_eq!(detail, format!("omp · {FIXTURE_NAME}"));
    assert!(
        view.read_with(vcx, |wb, cx| wb
            .harness_directory()
            .read(cx)
            .models("omp")
            .is_none()),
        "the catalog was not asked for"
    );
    assert_eq!(
        spawns(scripts.path()),
        before,
        "no harness was started for the names"
    );
}

#[gpui::test]
fn the_start_sheet_marks_the_model_a_start_would_use(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon(scripts.path(), "plain-turn", "");
    let (view, cx) = open(cx, &daemon);
    cx.simulate_keystrokes("cmd-shift-t");
    view.update(cx, |wb, cx| {
        wb.launcher_set_kind(hutan_desktop::sessions::launch::LaunchKind::Chat, cx);
    });
    wait_until(cx, "the OMP models", |cx| {
        view.read_with(cx, |wb, cx| {
            matches!(wb.launcher_models(cx), Models::Ready(_))
        })
    });
    let marked = |view: &Entity<Workbench>, cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            let Models::Ready(list) = wb.launcher_models(cx) else {
                panic!("the models are ready");
            };
            wb.launcher_model_rows(&list, cx)
                .iter()
                .filter(|r| r.current)
                .map(|r| r.model.id.clone())
                .collect::<Vec<_>>()
        })
    };
    // Nothing picked: the default is the current one.
    assert_eq!(marked(&view, cx), [FIXTURE_MODEL]);
    view.update(cx, |wb, cx| {
        wb.launcher_choose_model(Some("gitlab-duo/duo-chat-sonnet-4-6".into()), cx);
    });
    assert_eq!(marked(&view, cx), ["gitlab-duo/duo-chat-sonnet-4-6"]);
    // Opus before Sonnet before Haiku, newest first, the default first in its family.
    let order: Vec<String> = view.read_with(cx, |wb, cx| {
        let Models::Ready(list) = wb.launcher_models(cx) else {
            panic!("the models are ready");
        };
        list.iter().map(|m| m.id.clone()).collect()
    });
    let at = |id: &str| order.iter().position(|m| m == id).unwrap();
    assert!(at(FIXTURE_MODEL) < at("gitlab-duo/duo-chat-opus-4-5"));
    assert!(at("gitlab-duo/duo-chat-opus-4-5") < at("gitlab-duo/duo-chat-sonnet-4-6"));
    assert!(at("gitlab-duo/duo-chat-sonnet-4-6") < at("gitlab-duo/duo-chat-sonnet-4-5"));
    assert!(at("gitlab-duo/duo-chat-sonnet-4-5") < at("gitlab-duo/duo-chat-haiku-4-5"));
    common::draw(cx);
}

#[gpui::test]
fn the_start_sheet_tells_apart_models_of_the_same_name(cx: &mut TestAppContext) {
    // OMP's ACP agent offers no models here, so the catalog is `omp models`, as on a machine
    // with OMP 14: two "Claude Haiku 4.5", the alias and the dated id.
    let scripts = tempfile::tempdir().unwrap();
    let omp = script(
        scripts.path(),
        "omp",
        r#"case "$1" in
  models) echo '{"models":[{"provider":"anthropic","kind":"chat","id":"claude-haiku-4-5-20251001","name":"Claude Haiku 4.5"},{"provider":"anthropic","kind":"chat","id":"claude-haiku-4-5","name":"Claude Haiku 4.5"},{"provider":"anthropic","kind":"chat","id":"claude-opus-4-5","name":"Claude Opus 4.5"}]}' ;;
  usage) echo '{"reports":[]}' ;;
  *) echo "fake omp $*" ;;
esac"#,
    );
    let acp = script(scripts.path(), "omp-acp", "exit 1");
    let daemon = TestDaemon::start_with_config(&format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {acp:?}\ncli = {omp:?}\n"
    ));
    let (view, cx) = open(cx, &daemon);
    cx.simulate_keystrokes("cmd-shift-t");
    view.update(cx, |wb, cx| {
        wb.launcher_set_kind(hutan_desktop::sessions::launch::LaunchKind::Chat, cx);
    });
    wait_until(cx, "the OMP models", |cx| {
        view.read_with(
            cx,
            |wb, cx| matches!(wb.launcher_models(cx), Models::Ready(list) if list.len() == 3),
        )
    });
    let rows: Vec<(String, String, Option<String>)> = view.read_with(cx, |wb, cx| {
        let Models::Ready(list) = wb.launcher_models(cx) else {
            panic!("the models are ready");
        };
        wb.launcher_model_rows(&list, cx)
            .iter()
            .map(|r| {
                (
                    r.model.id.clone(),
                    r.model.display_name.clone(),
                    r.variant.clone(),
                )
            })
            .collect()
    });
    assert_eq!(
        rows,
        [
            (
                "anthropic/claude-opus-4-5".to_owned(),
                "Claude Opus 4.5".to_owned(),
                None
            ),
            (
                "anthropic/claude-haiku-4-5".to_owned(),
                "Claude Haiku 4.5".to_owned(),
                None
            ),
            (
                "anthropic/claude-haiku-4-5-20251001".to_owned(),
                "Claude Haiku 4.5".to_owned(),
                Some("(20251001)".to_owned())
            ),
        ]
    );
    common::draw(cx);
}

#[gpui::test]
fn mawas_switch_model_marks_the_model_it_runs_and_names_it(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon(
        scripts.path(),
        "model-switch",
        "[assistant]\nbackend = \"omp\"\n",
    );
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| wb.open_assistant(window, cx));
    let assistant = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .sessions()
                .iter()
                .find(|s| {
                    s.chat
                        .as_ref()
                        .is_some_and(|c| c.agent == "omp" && c.assistant)
                })
                .cloned()
        })
    };
    wait_until(cx, "the OMP assistant", |cx| assistant(cx).is_some());
    let session = assistant(cx).unwrap().id;
    let _: hutan_protocol::chat::ChatSent =
        daemon.call(Request::ChatSend(hutan_protocol::chat::ChatSend {
            session_id: session,
            text: "hello".into(),
            attachments: Vec::new(),
        }));
    wait_until(cx, "the agent to start on a model", |cx| {
        assistant(cx).is_some_and(|s| !s.chat.unwrap().model.is_empty())
    });
    let running = assistant(cx).unwrap().chat.unwrap().model;
    // Mawas's Switch model… is the Switch model dialog of its session.
    view.update_in(cx, |wb, window, cx| {
        wb.open_model_switch(session, window, cx)
    });
    wait_until(cx, "the models of OMP", |cx| {
        view.read_with(cx, |wb, _| {
            wb.agent_picker().is_some_and(|p| p.shown().len() > 1)
        })
    });
    let marked: Vec<String> = view.read_with(cx, |wb, _| {
        wb.agent_picker()
            .unwrap()
            .rows()
            .iter()
            .filter(|r| r.current)
            .map(|r| r.model.id.clone())
            .collect()
    });
    assert_eq!(marked, std::slice::from_ref(&running));
    // The catalog is known now: Home's header names the model by its name.
    let name = view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .model_name("omp", &running)
            .map(str::to_owned)
            .unwrap()
    });
    let label = view.read_with(cx, |wb, cx| {
        wb.panel::<hutan_desktop::workbench::panels::AssistantPanel>("assistant")
            .unwrap()
            .read(cx)
            .backend_label(cx)
    });
    assert!(label.ends_with(&format!("OMP · {name}")), "{label}");
}
