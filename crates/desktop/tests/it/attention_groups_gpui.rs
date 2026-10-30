#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! `UX-ATTN-2` against an in-process daemon: *Requests* holds only what blocks an agent, a
//! finished session goes to *Ready for review* and a failed one to *Failed* — neither is ever
//! counted by the status bar — opening a session clears what the rules say, and no surface
//! says "Needs you" any more.

use crate::common;

use std::path::{Path, PathBuf};

use common::agents::{Node, Reporter};
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_agents_view::attention::{Attention, AttentionKind};
use hutan_core::SessionId;
use hutan_desktop::sessions::SessionsStore;
use hutan_desktop::workbench::{Region, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
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
        None,
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
    f: impl FnOnce(&SessionsStore) -> R,
) -> R {
    view.read_with(cx, |wb, cx| f(wb.sessions().read(cx)))
}

/// A session that runs `exit <code>` once `go` exists. One that exits 0 works in a repository
/// with an uncommitted file: its exit leaves work to review (a clean exit with nothing changed
/// is only *ended*, FX-UI-A 4).
fn ends_with(daemon: &TestDaemon, title: &str, go: &Path, code: i32) -> SessionId {
    let script = format!(
        "while [ ! -e {} ]; do sleep 0.05; done; exit {code}",
        go.display()
    );
    if code == 0 {
        let repo = TestDaemon::dirty_repo(daemon.home());
        return daemon.sh_titled_in(title, &script, &repo).id;
    }
    daemon.sh_titled(title, &script).id
}

/// Whether the status bar draws the count of requests.
fn counts_requests(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::StatusItem("attention"))
            .is_some_and(|b| b.size.width > gpui::px(10.0))
    })
}

#[gpui::test]
fn a_request_counts_in_the_status_bar_but_finished_work_and_failures_never_do(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let go: PathBuf = daemon.home().join("go");
    let asks = daemon.sh_titled("asks", "sleep 300").id;
    let failing = ends_with(&daemon, "failing", &go, 3);
    let finishing = ends_with(&daemon, "finishing", &go, 0);
    let (view, cx) = open(cx, &daemon);
    for id in [asks, failing, finishing] {
        wait_until(cx, "the session to be listed", |cx| {
            store(&view, cx, |s| s.session(id).is_some())
        });
    }
    assert!(!counts_requests(&view, cx), "nothing waits yet");

    // An agent that waits for a permission is a request, and the status bar counts it.
    let reporter = Reporter::new(asks, "omp-extension", "omp");
    reporter.report(&daemon, Node::main("m").blocked());
    wait_until(cx, "the request", |cx| {
        store(&view, cx, |s| s.requests() == [asks])
    });
    wait_until(cx, "the status bar to count it", |cx| {
        counts_requests(&view, cx)
    });

    // Two sessions end: finished work is ready for review, a failure is failed. Neither is a
    // request, and neither is a badge for the menu bar's item beyond the failure's own.
    std::fs::write(&go, "").unwrap();
    wait_until(cx, "both to end", |cx| {
        store(&view, cx, |s| {
            s.in_attention(Attention::Review) == [finishing]
                && s.in_attention(Attention::Failed) == [failing]
        })
    });
    store(&view, cx, |s| {
        assert_eq!(s.requests(), [asks], "only the question of the agent");
        let kinds: Vec<(SessionId, AttentionKind)> = s
            .attention_entries()
            .into_iter()
            .map(|e| (e.session, e.kind))
            .collect();
        assert!(
            !kinds.iter().any(|(id, _)| *id == finishing),
            "finished work is no badge: {kinds:?}"
        );
        assert!(kinds.contains(&(failing, AttentionKind::Failed)));
    });

    // The request is answered: nothing counts any more, though a failure and finished work
    // are still listed.
    reporter.report(&daemon, Node::main("m").working());
    wait_until(cx, "the request to go", |cx| {
        store(&view, cx, |s| s.requests().is_empty())
    });
    wait_until(cx, "the status bar to stop counting", |cx| {
        !counts_requests(&view, cx)
    });
    store(&view, cx, |s| {
        assert_eq!(s.in_attention(Attention::Review), [finishing]);
        assert_eq!(s.in_attention(Attention::Failed), [failing]);
    });
}

#[gpui::test]
fn opening_a_session_clears_finished_work_and_keeps_a_seen_failure_until_dismissed(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let go: PathBuf = daemon.home().join("go");
    let failing = ends_with(&daemon, "failing", &go, 3);
    let finishing = ends_with(&daemon, "finishing", &go, 0);
    let (view, cx) = open(cx, &daemon);
    for id in [failing, finishing] {
        wait_until(cx, "the session to be listed", |cx| {
            store(&view, cx, |s| s.session(id).is_some())
        });
    }
    std::fs::write(&go, "").unwrap();
    wait_until(cx, "both to end", |cx| {
        store(&view, cx, |s| {
            s.in_attention(Attention::Review) == [finishing]
                && s.in_attention(Attention::Failed) == [failing]
        })
    });

    // Opening the finished one marks its work seen: it leaves Ready for review.
    view.update_in(cx, |wb, window, cx| wb.open_session(finishing, window, cx));
    wait_until(cx, "the finished work to be seen", |cx| {
        store(&view, cx, |s| s.in_attention(Attention::Review).is_empty())
    });

    // Opening the failed one sees the failure: it is no badge any more, but it stays in Failed
    // until a new run or a dismissal.
    view.update_in(cx, |wb, window, cx| wb.open_session(failing, window, cx));
    wait_until(cx, "the failure to be seen", |cx| {
        store(&view, cx, |s| {
            s.attention_entries().iter().all(|e| e.session != failing)
        })
    });
    draw(cx);
    store(&view, cx, |s| {
        assert_eq!(s.in_attention(Attention::Failed), [failing]);
        let presentation = s.presentation(s.session(failing).unwrap());
        assert_eq!(presentation.state_word(), Some("Failed"));
        assert!(!presentation.filled, "seen: a hollow glyph");
    });

    view.update(cx, |wb, cx| {
        wb.sessions().update(cx, |s, cx| s.mark_read(failing, cx));
    });
    assert!(store(&view, cx, |s| s
        .in_attention(Attention::Failed)
        .is_empty()));
}

/// No surface says "Needs you": the groups are Requests, Failed and Ready for review.
#[test]
fn no_string_of_the_desktop_says_needs_you() {
    fn scan(dir: &Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path, found);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                for (n, line) in text.lines().enumerate() {
                    let lower = line.to_lowercase();
                    if lower.contains("needs you") || lower.contains("need you") {
                        found.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                    }
                }
            }
        }
    }
    let mut found = Vec::new();
    scan(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut found,
    );
    assert!(found.is_empty(), "{}", found.join("\n"));
}
