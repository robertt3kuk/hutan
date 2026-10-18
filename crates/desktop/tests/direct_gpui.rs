#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Direct chats exercise the real daemon and gateway over loopback HTTP, never Keychain.

mod common;

use common::{TestDaemon, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_chat_view::Body;
use hutan_desktop::workbench::{LaunchAgent, LauncherField, Models, Region, Workbench};
use hutan_gateway::testing::{MockServer, Reply};
use hutan_gateway::{HttpConfig, MemoryStore};
use hutan_protocol::messages::{Empty, Request};
use hutan_protocol::provider::ProviderList;
use hutan_settings::{SettingsPaths, SettingsStore};
use std::path::Path;
use std::sync::Arc;

const KEY: &str = "sk-synthetic-desktop-test-1234567890";

fn daemon() -> TestDaemon {
    TestDaemon::start_with_gateway(hutan_daemon::GatewayConfig {
        credentials: Some(Arc::new(MemoryStore::new())),
        http: HttpConfig {
            no_proxy: true,
            ..Default::default()
        },
        discovery: Some(Vec::new()),
    })
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    );
    wait_until(cx, "workspace synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

fn confirm(view: &Entity<Workbench>, cx: &mut VisualTestContext, input: &str) {
    view.update_in(cx, |wb, window, cx| {
        wb.set_modal_input(input, cx);
        wb.confirm_modal(window, cx);
    });
}

fn configure(view: &Entity<Workbench>, cx: &mut VisualTestContext, mock: &MockServer) -> String {
    mock.route(
        "GET",
        "/v1/models",
        Reply::json(200, r#"{"data":[{"id":"first"},{"id":"second"}]}"#),
    );
    view.update_in(cx, |wb, window, cx| {
        wb.open_models_settings(window, cx);
        wb.edit_provider(None, "openai", window, cx);
    });
    confirm(view, cx, "Local mock");
    confirm(view, cx, &format!("{}/v1", mock.url()));
    confirm(view, cx, "first");
    wait_until(cx, "provider creation", |cx| {
        view.read_with(cx, |wb, _| {
            wb.providers().iter().any(|p| p.name == "Local mock")
        })
    });
    let id = view.read_with(cx, |wb, _| {
        wb.providers()
            .iter()
            .find(|p| p.name == "Local mock")
            .unwrap()
            .id
            .clone()
    });
    view.update_in(cx, |wb, window, cx| wb.set_provider_key(&id, window, cx));
    view.update(cx, |wb, cx| wb.set_modal_input(KEY, cx));
    assert!(view.read_with(cx, |wb, _| wb.modal().unwrap().secure_input));
    assert!(
        !view
            .read_with(cx, |wb, _| format!("{:?}", wb.modal()))
            .contains(KEY)
    );
    confirm(view, cx, KEY);
    wait_until(cx, "key acknowledgement", |cx| {
        view.read_with(cx, |wb, _| {
            wb.providers()
                .iter()
                .any(|p| p.id == id && p.has_credential)
        })
    });
    assert!(view.read_with(cx, |wb, _| wb.modal().is_none()));
    id
}

fn reply(text: &str) -> Reply {
    Reply::sse([
        format!("data: {{\"id\":\"chat-test\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":\"{text}\"}}}}]}}\n\n"),
        "data: {\"id\":\"chat-test\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"total_tokens\":15}}\n\n".into(),
        "data: [DONE]\n\n".into(),
    ])
}

fn scan(path: &Path, secret: &[u8]) {
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            scan(&path, secret);
        } else if path.is_file() {
            let bytes = std::fs::read(&path).unwrap();
            assert!(
                !bytes.windows(secret.len()).any(|w| w == secret),
                "secret in {}",
                path.display()
            );
        }
    }
}

#[gpui::test]
fn direct_chat_uses_its_provider_and_switches_the_next_turns_model(cx: &mut TestAppContext) {
    let daemon = daemon();
    let mock = daemon.http_mock();
    mock.route("POST", "/v1/chat/completions", reply("first reply"));
    mock.route("POST", "/v1/chat/completions", reply("second reply"));
    let (view, cx) = open(cx, &daemon);
    let id = configure(&view, cx, &mock);
    view.update_in(cx, |wb, window, cx| {
        wb.close_settings(window, cx);
        wb.new_direct_chat(window, cx);
    });
    wait_until(cx, "direct model catalog", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher().is_some_and(|l| {
                l.agent() == &LaunchAgent::Direct(id.clone())
                    && matches!(wb.launcher_models(cx), Models::Ready(m) if m.len() == 2)
            })
        })
    });
    // The provider's default model is preselected; picking another is a click on the list.
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.launcher_chosen_model(cx).map(|m| m.id)),
        Some("first".to_owned())
    );
    view.update(cx, |wb, cx| {
        wb.launcher_choose_model(Some("second".to_owned()), cx);
    });
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.launcher_chosen_model(cx).map(|m| m.id)),
        Some("second".to_owned())
    );
    view.update(cx, |wb, cx| {
        wb.launcher_choose_model(Some("first".to_owned()), cx);
        wb.launcher_toggle_more(cx);
        wb.launcher_set_field(LauncherField::System, "Be concise", cx);
    });
    view.update_in(cx, |wb, window, cx| wb.launcher_start(false, window, cx));
    wait_until(cx, "direct chat opens", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.focused_session().is_some_and(|id| {
                wb.sessions()
                    .read(cx)
                    .session(id)
                    .is_some_and(|s| s.chat.as_ref().is_some_and(|c| c.agent == "direct"))
            })
        })
    });
    let session = view.read_with(cx, |wb, _| wb.focused_session()).unwrap();
    wait_until(cx, "composer drawn", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Chat("composer")).is_some()
        })
    });
    cx.simulate_input("first question");
    cx.simulate_keystrokes("cmd-enter");
    wait_until(cx, "first streamed answer", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(session).is_some_and(|m| {
                m.read(cx).transcript().entries().iter().any(
                    |e| matches!(&e.body, Body::Assistant(a) if a.text == "first reply" && a.done),
                )
            })
        })
    });
    wait_until(cx, "turn completes", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(session)
                .unwrap()
                .chat
                .as_ref()
                .is_some_and(|c| !c.turn_active)
        })
    });
    view.update_in(cx, |wb, window, cx| {
        wb.open_model_switch(session, window, cx)
    });
    wait_until(cx, "switch catalog", |cx| {
        view.read_with(cx, |wb, _| {
            wb.agent_picker().is_some_and(|p| p.shown().len() == 2)
        })
    });
    view.update(cx, |wb, cx| wb.agent_picker_choose(1, cx));
    view.update_in(cx, |wb, window, cx| wb.agent_picker_confirm(window, cx));
    wait_until(cx, "model switches", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.agent_picker().is_none()
                && wb
                    .sessions()
                    .read(cx)
                    .session(session)
                    .unwrap()
                    .chat
                    .as_ref()
                    .is_some_and(|c| c.model == "second")
        })
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(session, window, cx));
    cx.simulate_input("second question");
    cx.simulate_keystrokes("cmd-enter");
    wait_until(cx, "second streamed answer", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(session).is_some_and(|m| {
                m.read(cx).transcript().entries().iter().any(
                    |e| matches!(&e.body, Body::Assistant(a) if a.text == "second reply" && a.done),
                )
            })
        })
    });
    let requests = mock.requests();
    let chat: Vec<_> = requests
        .iter()
        .filter(|r| r.path() == "/v1/chat/completions")
        .collect();
    assert_eq!(
        chat.iter()
            .map(|r| r.json()["model"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert_eq!(
        chat[0].header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    assert_eq!(chat[0].json()["messages"][0]["role"], "system");
    assert_eq!(chat[0].json()["messages"][0]["content"], "Be concise");
    assert!(
        chat[1].json()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "assistant" && m["content"] == "first reply")
    );
    scan(daemon.home(), KEY.as_bytes());
}

#[gpui::test]
fn rejected_key_offers_a_masked_replacement_and_provider_removal_clears_it(
    cx: &mut TestAppContext,
) {
    let daemon = daemon();
    let mock = daemon.http_mock();
    mock.route(
        "POST",
        "/v1/chat/completions",
        Reply::json(401, r#"{"error":{"message":"key rejected"}}"#),
    );
    let (view, cx) = open(cx, &daemon);
    let id = configure(&view, cx, &mock);
    view.update_in(cx, |wb, window, cx| {
        wb.close_settings(window, cx);
        wb.new_direct_chat(window, cx);
    });
    wait_until(cx, "models loaded", |cx| {
        view.read_with(cx, |wb, cx| wb.launcher_chosen_model(cx).is_some())
    });
    view.update_in(cx, |wb, window, cx| wb.launcher_start(false, window, cx));
    wait_until(cx, "composer drawn", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Chat("composer")).is_some()
        })
    });
    cx.simulate_input("hello");
    cx.simulate_keystrokes("cmd-enter");
    wait_until(cx, "Set key recovery action", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Chat("fix-key")).is_some()
        })
    });
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Chat("fix-key")))
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    assert!(view.read_with(cx, |wb, _| wb.modal().is_some_and(|m| m.secure_input)));
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    view.update(cx, |wb, cx| wb.clear_provider_key_now(&id, cx));
    wait_until(cx, "key cleared", |cx| {
        view.read_with(cx, |wb, _| {
            wb.providers()
                .iter()
                .any(|p| p.id == id && !p.has_credential)
        })
    });
    view.update_in(cx, |wb, window, cx| wb.remove_provider(&id, window, cx));
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "provider removed", |cx| {
        view.read_with(cx, |wb, _| !wb.providers().iter().any(|p| p.id == id))
    });
    let answer: ProviderList = daemon.call(Request::ProviderList(Empty {}));
    assert!(!answer.providers.iter().any(|p| p.id == id));
    scan(daemon.home(), KEY.as_bytes());
}

#[gpui::test]
fn model_switch_is_unavailable_during_streaming_and_interrupt_restores_it(cx: &mut TestAppContext) {
    let daemon = daemon();
    let mock = daemon.http_mock();
    mock.route("POST", "/v1/chat/completions", Reply::sse([
        "data: {\"id\":\"held\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Still working\"}}]}\n\n"
    ]).then_hold());
    let (view, cx) = open(cx, &daemon);
    configure(&view, cx, &mock);
    view.update_in(cx, |wb, window, cx| {
        wb.close_settings(window, cx);
        wb.new_direct_chat(window, cx);
    });
    wait_until(cx, "direct catalog", |cx| {
        view.read_with(cx, |wb, cx| wb.launcher_chosen_model(cx).is_some())
    });
    view.update_in(cx, |wb, window, cx| wb.launcher_start(false, window, cx));
    wait_until(cx, "composer", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Chat("composer")).is_some()
        })
    });
    let session = view.read_with(cx, |wb, _| wb.focused_session()).unwrap();
    cx.simulate_input("hold the turn");
    cx.simulate_keystrokes("cmd-enter");
    wait_until(cx, "active stream", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(session)
                .unwrap()
                .chat
                .as_ref()
                .unwrap()
                .turn_active
        })
    });
    view.update_in(cx, |wb, window, cx| {
        wb.open_model_switch(session, window, cx)
    });
    assert!(view.read_with(cx, |wb, _| wb.agent_picker().is_none()));
    assert_eq!(
        view.read_with(cx, |wb, cx| wb
            .sessions()
            .read(cx)
            .session(session)
            .unwrap()
            .chat
            .as_ref()
            .unwrap()
            .model
            .clone()),
        "first"
    );
    cx.simulate_keystrokes("cmd-.");
    wait_until(cx, "interrupted turn", |cx| {
        view.read_with(cx, |wb, cx| {
            !wb.sessions()
                .read(cx)
                .session(session)
                .unwrap()
                .chat
                .as_ref()
                .unwrap()
                .turn_active
        })
    });
    view.update_in(cx, |wb, window, cx| {
        wb.open_model_switch(session, window, cx)
    });
    wait_until(cx, "switch available after interrupt", |cx| {
        view.read_with(cx, |wb, _| {
            wb.agent_picker()
                .is_some_and(|p| p.switch_session == session && p.chosen().is_some())
        })
    });
}

/// Opening the Models sheet requests the provider list; a provider added before that answer
/// arrives must not be wiped by it (the answer is older than the change).
#[gpui::test]
fn a_provider_added_while_the_list_is_loading_survives_the_list(cx: &mut TestAppContext) {
    let daemon = daemon();
    let mock = daemon.http_mock();
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| {
        wb.open_models_settings(window, cx);
        wb.edit_provider(None, "openai", window, cx);
    });
    assert!(
        view.read_with(cx, |wb, _| wb.providers_loading()),
        "the list is still on its way"
    );
    confirm(&view, cx, "Raced");
    confirm(&view, cx, &format!("{}/v1", mock.url()));
    confirm(&view, cx, "first");
    wait_until(cx, "the provider and the list", |cx| {
        view.read_with(cx, |wb, _| {
            wb.providers().iter().any(|p| p.name == "Raced") && !wb.providers_loading()
        })
    });
    // Let any late answer land.
    for _ in 0..50 {
        common::draw(cx);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        view.read_with(cx, |wb, _| wb.providers().iter().any(|p| p.name == "Raced")),
        "an older list must not undo the new provider"
    );
}
