#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers
//! The desktop for harness chats (M8-B): the new-agent picker with OMP first, the model switch of
//! a harness chat, the Accounts page with Log in, `assistant.enabled`, and the assistant's
//! backend per scope. The daemon runs in this process; OMP is `hutan-acp-fixture replay` and the
//! harnesses' own CLIs are shell scripts. No real harness, login or model is touched.

mod common;

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
/// logged in to `anthropic`, Claude Code is logged out, and its ACP adapter is missing.
fn daemon(scripts: &Path, transcript_name: &str, extra: &str) -> TestDaemon {
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
    TestDaemon::start_with_config(&format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {acp:?}\nargs = [\"replay\", {t:?}, \"acp\"]\ncli = {omp:?}\n\n[harness.claude]\nbin = {missing:?}\ncli = {claude:?}\n{extra}",
        acp = acp_bin(),
        t = transcript(transcript_name),
        missing = scripts.join("no-such-claude-code-acp"),
    ))
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        common::with_assistant(SettingsStore::new(SettingsPaths::in_dir(daemon.home()))),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
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
    let other = view.update(cx, |wb, _| {
        let picker = wb.agent_picker().unwrap();
        picker
            .shown()
            .iter()
            .position(|m| m.id != before)
            .expect("another model")
    });
    view.update(cx, |wb, cx| wb.agent_picker_choose(other, cx));
    view.update_in(cx, |wb, window, cx| wb.agent_picker_confirm(window, cx));
    wait_until(cx, "the switch to be answered", |cx| {
        view.read_with(cx, |wb, _| wb.agent_picker().is_none())
    });
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
        wb.commands().iter().any(|c| c.title.contains("Assistant"))
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
        wb.commands().iter().any(|c| c.title.contains("Assistant"))
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
    wait_until(cx, "the OMP assistant", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).sessions().iter().any(|s| {
                s.chat.as_ref().is_some_and(|c| {
                    c.agent == "omp" && c.assistant && c.model == "gitlab-duo/duo-chat-sonnet-4-6"
                })
            })
        })
    });
    common::draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Assistant("use-omp")))
            .is_none(),
        "nothing is left to choose"
    );
    // The header says what runs; the chooser's default is the automatic choice.
    click(&view, cx, Region::Assistant("backend"));
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
    click(&view, cx, Region::Assistant("backend"));
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
                        .is_some_and(|c| c.agent == "omp" && c.assistant && !c.model.is_empty())
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
