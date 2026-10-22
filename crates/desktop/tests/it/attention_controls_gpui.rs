#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! `UX-ATTENTION-CONTROLS` against an in-process daemon: *Mark as unread* survives a restart of
//! the desktop, ⌘⇧J goes to what blocks someone before what is only done, *Snooze until it
//! changes* lapses when the attention changes, and a terminal that runs an agent nobody
//! reported is said to be unrecognised until *Track as <agent>*.

use crate::common;

use std::path::Path;

use common::agents::{Node, Reporter};
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_desktop::sessions::{Attention, Group, Unread};
use hutan_desktop::workbench::{Region, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    state: Option<&Path>,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        state.map(Path::to_path_buf),
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

fn store<R>(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    f: impl FnOnce(&hutan_desktop::sessions::SessionsStore) -> R,
) -> R {
    view.read_with(cx, |wb, cx| f(wb.sessions().read(cx)))
}

/// Whether `id` is in any of the three groups of attention.
fn waits(s: &hutan_desktop::sessions::SessionsStore, id: SessionId) -> bool {
    Attention::ALL
        .into_iter()
        .any(|group| s.in_attention(group).contains(&id))
}

fn known(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId) {
    wait_until(cx, "the session to be listed", |cx| {
        store(view, cx, |s| s.session(id).is_some())
    });
}

#[gpui::test]
fn a_session_marked_unread_is_still_unread_after_the_desktop_restarts(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let id = daemon.sh_titled("quiet", "sleep 300").id;
    let state = daemon.home().join("desktop-state.json");
    {
        let (view, cx) = open(cx, &daemon, Some(&state));
        known(&view, cx, id);
        assert!(!store(&view, cx, |s| waits(s, id)));
        view.update(cx, |wb, cx| {
            wb.sessions()
                .update(cx, |store, cx| assert!(store.mark_unread(id, cx)));
        });
        // A mark is a quiet item of *Ready for review*, never a request.
        assert!(store(&view, cx, |s| s
            .in_attention(Attention::Review)
            .contains(&id)));
        assert!(store(&view, cx, |s| s.requests().is_empty()));
        assert_eq!(store(&view, cx, |s| s.unread(id)), Some(Unread::Marked));
        view.update(cx, |wb, _| wb.flush_save());
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
        assert_eq!(saved["attention"]["unread"][0], id.to_string(), "{saved}");
    }
    // A new window reads the file: the session is marked unread again, and reading ends it.
    let (view, cx) = open(cx, &daemon, Some(&state));
    known(&view, cx, id);
    assert!(store(&view, cx, |s| s.is_marked_unread(id)));
    assert!(store(&view, cx, |s| waits(s, id)));
    view.update(cx, |wb, cx| {
        wb.sessions().update(cx, |s, cx| s.mark_read(id, cx));
    });
    assert!(!store(&view, cx, |s| waits(s, id)));
    view.update(cx, |wb, _| wb.flush_save());
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    assert!(saved.get("attention").is_none(), "{saved}");
}

#[gpui::test]
fn the_jump_goes_to_a_request_before_a_session_that_is_only_done(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    // Listed first: a session that finishes (done, unread). Listed second: an agent that blocks.
    let done = daemon.sh_titled("finishes", "read x").clone();
    let done_terminal = done.terminal_id.expect("a terminal");
    let blocked = daemon.sh_titled("blocks", "sleep 300").id;
    known(&view, cx, done.id);
    known(&view, cx, blocked);
    Reporter::new(blocked, "omp-extension", "omp").report(&daemon, Node::main("m").blocked());
    daemon.send_input(done_terminal, b"\n");
    wait_until(cx, "both to want the user", |cx| {
        store(&view, cx, |s| {
            s.in_attention(Attention::Review).contains(&done.id) && s.requests().contains(&blocked)
        })
    });
    assert_eq!(store(&view, cx, |s| s.requests()), vec![blocked]);
    assert_eq!(
        store(&view, cx, |s| s.in_attention(Attention::Review)),
        vec![done.id],
        "the finished one is ready for review, never a request"
    );
    assert_eq!(
        store(&view, cx, |s| s.next_needing_attention(None)),
        Some(blocked)
    );
    // From the blocked one the jump goes on to the finished one, and wraps.
    assert_eq!(
        store(&view, cx, |s| s.next_needing_attention(Some(blocked))),
        Some(done.id)
    );
    assert_eq!(
        store(&view, cx, |s| s.next_needing_attention(Some(done.id))),
        Some(blocked)
    );
}

#[gpui::test]
fn a_snooze_hides_the_session_until_its_attention_changes(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    let id = daemon.sh_titled("blocks", "sleep 300").id;
    known(&view, cx, id);
    let reporter = Reporter::new(id, "omp-extension", "omp");
    reporter.report(&daemon, Node::main("m").blocked());
    wait_until(cx, "the session to need the user", |cx| {
        store(&view, cx, |s| s.requests().contains(&id))
    });

    view.update(cx, |wb, cx| {
        wb.sessions()
            .update(cx, |store, cx| assert!(store.snooze(id, cx)));
    });
    assert!(store(&view, cx, |s| s.requests().is_empty()));
    assert_eq!(
        store(&view, cx, |s| s
            .agent_presentation()
            .get(id)
            .map(|r| r.group)),
        Some(Group::Snoozed)
    );
    assert_eq!(store(&view, cx, |s| s.next_needing_attention(None)), None);

    // The agent goes on and blocks again: that is a change, so the snooze is over.
    reporter.report(&daemon, Node::main("m").working());
    wait_until(cx, "the snooze to lapse", |cx| {
        !store(&view, cx, |s| s.is_snoozed(id))
    });
    reporter.report(&daemon, Node::main("m").blocked());
    wait_until(cx, "the session to need the user again", |cx| {
        store(&view, cx, |s| s.requests().contains(&id))
    });
}

#[gpui::test]
fn an_agent_nobody_reported_is_noted_and_track_as_makes_it_a_tracked_agent(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    // `claude` as the program's own name (argv[0]), without any integration.
    let session = daemon.create_session(&["bash", "-c", "exec -a claude sleep 300"], None);
    let id = session.id;
    known(&view, cx, id);
    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
    wait_until(cx, "the agent to be found in the process tree", |cx| {
        view.update(cx, |wb, cx| {
            wb.sessions().update(cx, |s, cx| s.scan_agents(cx));
        });
        store(&view, cx, |s| s.untracked_agent(id).is_some())
    });
    assert_eq!(
        store(&view, cx, |s| s.untracked_agent(id).map(|u| u.agent)),
        Some("claude")
    );
    assert!(
        store(&view, cx, |s| s.agent_presentation().get(id).is_none()),
        "it is invisible to the Agents page until tracked"
    );
    wait_until(cx, "the strip to be drawn", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Untracked("strip")).is_some()
                && wb.region_bounds(Region::Untracked("track")).is_some()
        })
    });

    let track = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Untracked("track")))
        .unwrap();
    cx.simulate_click(track.center(), gpui::Modifiers::none());
    draw(cx);
    wait_until(cx, "the daemon to know the agent", |cx| {
        store(&view, cx, |s| {
            s.agent_tree(id).is_some() && s.untracked_agent(id).is_none()
        })
    });
    // Tracked, with no state to go by: unconfirmed, never idle.
    wait_until(cx, "the agent in the presentation", |cx| {
        store(&view, cx, |s| s.agent_presentation().get(id).is_some())
    });
    assert_eq!(
        store(&view, cx, |s| s
            .agent_presentation()
            .get(id)
            .map(|r| r.group)),
        Some(Group::Unconfirmed)
    );

    // Once an integration says it is blocked, it is in *Requests*.
    Reporter::new(id, "omp-extension", "claude").report(&daemon, Node::main("m").blocked());
    wait_until(cx, "the agent to need the user", |cx| {
        store(&view, cx, |s| {
            s.agent_presentation().get(id).map(|r| r.group) == Some(Group::Requests)
        })
    });
}
