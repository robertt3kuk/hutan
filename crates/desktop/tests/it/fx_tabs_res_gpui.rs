#![allow(clippy::unwrap_used, clippy::expect_used)]
//! FX-TABS-RES: one session is one tab. A Mawas conversation is shown by Mawas Home only, from
//! whichever surface it is opened; its title and icon are the same on every surface; a layout
//! saved with duplicate tabs of it is merged once the daemon has said what the session is.

use crate::common;

use common::assistant::{AssistantEnv, Options, open, open_assistant, shown};
use common::wait_until;
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_desktop::icons::Icon;
use hutan_desktop::workbench::Workbench;
use hutan_desktop::workbench::model::{Edge, ItemRef, PanelId};
use hutan_desktop::workbench::panels::session_icon;
use hutan_protocol::messages::SessionInfo;

fn home() -> ItemRef {
    ItemRef::panel("assistant")
}

fn tabs_of(view: &Entity<Workbench>, cx: &VisualTestContext, session: SessionId) -> Vec<ItemRef> {
    let id = session.to_string();
    view.read_with(cx, |wb, _| {
        wb.state()
            .panes()
            .iter()
            .flat_map(|pane| pane.items.iter().cloned())
            .filter(|item| {
                matches!(item, ItemRef::Terminal { session } | ItemRef::Chat { session } if *session == id)
            })
            .collect()
    })
}

fn home_count(view: &Entity<Workbench>, cx: &VisualTestContext) -> usize {
    view.read_with(cx, |wb, _| {
        wb.state()
            .all_items()
            .into_iter()
            .filter(|item| *item == home())
            .count()
    })
}

fn info(view: &Entity<Workbench>, cx: &VisualTestContext, session: SessionId) -> SessionInfo {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).session(session).cloned())
        .expect("the session is known")
}

#[gpui::test]
fn a_mawas_conversation_opens_on_home_from_every_surface_and_has_no_tab_of_its_own(
    cx: &mut TestAppContext,
) {
    let env = AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    });
    let root = env.project("project");
    let workspace = env.daemon.create_workspace("project", &[&root]);
    let (view, cx) = open(cx, &env.daemon);
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace.id, window, cx);
    });
    let panel = open_assistant(&view, cx);
    let (session, _, _) = shown(&panel, cx);
    assert_eq!(info(&view, cx, session).title, "Mawas · project");

    // The sidebar row, the Agents page, the palette, a notification, `hutan ui run` — all open
    // a session through the same entry point: Home is the one tab, never a chat tab beside it.
    for _ in 0..2 {
        view.update_in(cx, |wb, window, cx| wb.open_session(session, window, cx));
        view.update_in(cx, |wb, window, cx| {
            wb.open_session_from_overview(session, window, cx);
        });
        view.update_in(cx, |wb, window, cx| {
            wb.open_session_in_split(session, Edge::Right, window, cx);
        });
    }
    assert_eq!(tabs_of(&view, cx, session), Vec::<ItemRef>::new());
    assert_eq!(home_count(&view, cx), 1);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().active_center_item()),
        Some(home())
    );

    // Title and icon are the session's on every surface: the Home tab, the title bar, the chat
    // tab the session would have, the session row and the picker (`session_icon`).
    let (home_icon, home_title) = view.read_with(cx, |wb, cx| wb.item_icon_and_title(&home(), cx));
    let chat = ItemRef::Chat {
        session: session.to_string(),
    };
    let (chat_icon, chat_title) = view.read_with(cx, |wb, cx| wb.item_icon_and_title(&chat, cx));
    assert_eq!(home_title.as_ref(), "Mawas · project");
    assert_eq!(chat_title, home_title);
    assert_eq!(home_icon, Icon::PanelAssistant);
    assert_eq!(chat_icon, home_icon);
    assert_eq!(session_icon(&info(&view, cx, session)), home_icon);
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.active_title(cx)),
        "Mawas · project"
    );
}

#[gpui::test]
fn duplicate_mawas_tabs_of_a_saved_layout_merge_into_home(cx: &mut TestAppContext) {
    let env = AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    });
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let (session, _, _) = shown(&panel, cx);
    let id = session.to_string();

    // What the user's screenshot shows: Home and a chat tab of the same conversation, plus a
    // terminal tab that was opened before the daemon had said what the session is.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let pane = s.active_pane_id();
            s.add_item(pane, ItemRef::Chat { session: id.clone() })
                .unwrap();
            s.add_item(pane, ItemRef::Terminal { session: id.clone() })
                .unwrap();
        });
    });
    // The layout is reconciled as soon as the store says anything (here: the focus moved).
    wait_until(cx, "the duplicate tabs to merge", |cx| {
        tabs_of(&view, cx, session).is_empty()
    });
    assert_eq!(home_count(&view, cx), 1);

    // Without a Home tab, the conversation's own tab becomes Home.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.close_panel(&PanelId::new("assistant")).unwrap();
            let pane = s.active_pane_id();
            s.add_item(pane, ItemRef::Chat { session: id.clone() })
                .unwrap();
        });
    });
    wait_until(cx, "the chat tab to become Home", |cx| {
        tabs_of(&view, cx, session).is_empty() && home_count(&view, cx) == 1
    });
}
