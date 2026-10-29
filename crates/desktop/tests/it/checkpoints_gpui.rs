#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Checkpoints in the desktop (ADR 0019, slice CK-5) over a real daemon whose Codex is
//! `hutan-codex-fixture replay`, in a temporary git repository: the turn footer's *Rewind…* only
//! on a turn that changed files, the preview sheet's groups and ticks as the daemon plans them,
//! Esc and Return (on *Cancel*) writing nothing, a conflict restored only through its own box,
//! *Restore* writing the files with the toast and the footer's `Restored — Undo`, an open file
//! tab showing the restored text, the toast's *Undo* restoring again; the sheet naming the
//! project, and `Restored — Undo` in a new window over a restarted daemon (FX-SMALL-1); a folder
//! outside git saying why once, in the chat's ⋯ menu; and nothing at all without the capability.

use crate::common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::chat::{ChatEnv, held_turns};
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_chat_view::Body;
use hutan_core::{CheckpointAvailability, SessionId};
use hutan_desktop::workbench::chat::LoadStatus;
use hutan_desktop::workbench::rewind::{DiffState, Group, Phase, SheetFocus};
use hutan_desktop::workbench::{FileOpen, Region, Workbench};
use hutan_protocol::chat::{ChatSend, ChatSent};
use hutan_protocol::checkpoint::{CheckpointPreview, CheckpointPreviewRequest};
use hutan_protocol::messages::Request;
use hutan_settings::{SettingsPaths, SettingsStore};

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_env() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
        ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
    ])
}

/// `<dir>/repo`: a repository on `main` with one commit of `a.txt` and `b.txt`.
fn repository(dir: &Path) -> PathBuf {
    git(dir, &["init", "-q", "-b", "main", "repo"]);
    let root = dir.join("repo");
    fs::write(root.join("a.txt"), "alpha\n").unwrap();
    fs::write(root.join("b.txt"), "beta\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    root
}

/// A workspace with a Codex chat in `cwd` whose turns wait on `gates`.
struct Setup {
    env: ChatEnv,
    daemon: TestDaemon,
    session: SessionId,
    gates: Vec<PathBuf>,
}

fn setup(turns: usize, in_git: bool) -> (Setup, PathBuf) {
    let env = ChatEnv::new();
    let base = env.cwd();
    let cwd = if in_git {
        repository(&base)
    } else {
        let plain = base.join("plain");
        fs::create_dir(&plain).unwrap();
        fs::write(plain.join("a.txt"), "alpha\n").unwrap();
        plain
    };
    let gates: Vec<PathBuf> = (0..turns).map(|n| base.join(format!("gate-{n}"))).collect();
    let transcript = held_turns(&base, &gates);
    let daemon = TestDaemon::start_with_codex_tweaked(env.replay(&transcript), |c| {
        c.checkpoints.git_env = git_env();
    });
    let session = daemon.create_chat(&cwd).id;
    (
        Setup {
            env,
            daemon,
            session,
            gates,
        },
        cwd,
    )
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    connection: hutan_client::Connection,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        connection,
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

fn show_chat(view: &Entity<Workbench>, cx: &mut VisualTestContext, session: SessionId) {
    view.update_in(cx, |wb, window, cx| wb.open_session(session, window, cx));
    wait_until(cx, "the chat to load", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(session)
                .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
                && wb.chat_view_of(session).is_some()
        })
    });
}

/// Footers of the chat's transcript, oldest first, by entry index.
fn footers(view: &Entity<Workbench>, cx: &VisualTestContext, session: SessionId) -> Vec<usize> {
    view.read_with(cx, |wb, cx| {
        wb.chat_model_of(session)
            .map(|m| {
                m.read(cx)
                    .transcript()
                    .entries()
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| matches!(e.body, Body::Footer(_)))
                    .map(|(at, _)| at)
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Sends a message, lets `edit` change files while the turn runs, ends the turn and waits for
/// its footer.
fn turn(
    s: &Setup,
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    n: usize,
    edit: impl FnOnce(),
) {
    let before = footers(view, cx, s.session).len();
    let _: ChatSent = s.daemon.call(Request::ChatSend(ChatSend {
        session_id: s.session,
        text: format!("turn {n}"),
        attachments: vec![],
    }));
    edit();
    fs::write(&s.gates[n], "").unwrap();
    wait_until(cx, "the turn's footer", |cx| {
        footers(view, cx, s.session).len() > before
    });
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, target: Region) {
    wait_until(cx, &format!("{target:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(target).is_some())
    });
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(target))
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    draw(cx);
}

fn drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext, target: Region) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(target).is_some())
}

/// The selected file's diff is in (or failed): it is drawn above the buttons, so the buttons
/// move down when it arrives. A click that straddles that frame lands its press on the button
/// and its release beside it, which is no click at all.
fn diff_shown(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the selected file's diff", |cx| {
        view.read_with(cx, |wb, _| {
            wb.rewind_sheet().is_some_and(|s| {
                s.rows.get(s.selected).is_none_or(|row| {
                    !matches!(s.diffs.get(&row.entry.path), Some(DiffState::Loading))
                })
            })
        })
    });
}

fn sheet_ready(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the preview", |cx| {
        view.read_with(cx, |wb, _| {
            wb.rewind_sheet()
                // The turn's end may reach the daemon's session list after its footer: the
                // plan is ready once no turn holds the checkout.
                .is_some_and(|s| matches!(s.phase, Phase::Ready) && !s.working && !s.busy())
        })
    });
    diff_shown(view, cx);
}

/// `(path, group, ticked)` of the sheet's files.
fn sheet_rows(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<(String, Group, bool)> {
    view.read_with(cx, |wb, _| {
        wb.rewind_sheet()
            .unwrap()
            .rows
            .iter()
            .map(|r| (r.entry.path.clone(), r.group, r.checked))
            .collect()
    })
}

fn restore_label(view: &Entity<Workbench>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |wb, _| wb.rewind_sheet().unwrap().restore_label())
}

#[gpui::test]
fn rewind_previews_restores_a_turns_files_and_undo_brings_them_back(cx: &mut TestAppContext) {
    let (s, root) = setup(2, true);
    let (view, cx) = open(cx, &s.daemon, s.daemon.connection());
    show_chat(&view, cx, s.session);
    let a = root.join("a.txt");
    let c = root.join("c.txt");

    // Turn 0 edits a.txt and creates c.txt; turn 1 changes nothing.
    turn(&s, &view, cx, 0, || {
        fs::write(&a, "edited by the agent\n").unwrap();
        fs::write(&c, "created by the agent\n").unwrap();
    });
    turn(&s, &view, cx, 1, || {});
    let footer = footers(&view, cx, s.session);
    assert_eq!(footer.len(), 2);
    // Only the turn with changes says so and offers Rewind….
    wait_until(cx, "turn 0's Rewind…", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::ChatRewind(footer[0])).is_some()
        })
    });
    assert!(!drawn(&view, cx, Region::ChatRewind(footer[1])));
    assert!(!drawn(&view, cx, Region::ChatCheckpoint(footer[1])));
    let store = view.read_with(cx, |wb, cx| wb.sessions().read(cx).checkpoints().clone());
    let checkpoint = store.read_with(cx, |store, _| {
        let list = store.of(s.session);
        assert_eq!(list.len(), 1, "one checkpoint keeps files: {list:?}");
        assert_eq!(list[0].files_changed, 2);
        list[0].id
    });

    // a.txt is open in a file tab; the chat is shown again.
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(&a), window, cx);
    });
    let key = a.display().to_string();
    wait_until(cx, "a.txt's tab", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.buffers()
                .get(&key)
                .is_some_and(|b| b.read(cx).text() == "edited by the agent\n")
        })
    });
    let buffer = view.read_with(cx, |wb, _| wb.buffers().get(&key).unwrap().clone());
    show_chat(&view, cx, s.session);

    // The user changes c.txt after the turn: a conflict.
    fs::write(&c, "the user's change\n").unwrap();

    // The sheet shows the daemon's plan: a.txt clean and ticked, c.txt edited after, unticked.
    click(&view, cx, Region::ChatRewind(footer[0]));
    sheet_ready(&view, cx);
    let daemon_plan: CheckpointPreview =
        s.daemon
            .call(Request::CheckpointPreview(CheckpointPreviewRequest {
                checkpoint_id: checkpoint,
                through: None,
                paths: vec![],
            }));
    let mut planned: Vec<(String, Group)> = daemon_plan
        .entries
        .iter()
        .map(|e| (e.path.clone(), Group::of(e)))
        .collect();
    planned.sort();
    let mut shown: Vec<(String, Group)> = sheet_rows(&view, cx)
        .into_iter()
        .map(|(path, group, _)| (path, group))
        .collect();
    shown.sort();
    assert_eq!(shown, planned);
    assert_eq!(
        sheet_rows(&view, cx),
        [
            ("a.txt".to_owned(), Group::Clean, true),
            ("c.txt".to_owned(), Group::After, false),
        ]
    );
    // Cancel has the keyboard; Restore is never the default.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.rewind_sheet().unwrap().focus),
        SheetFocus::Cancel
    );
    assert_eq!(restore_label(&view, cx), "Restore 1 file");

    // Esc cancels: nothing written.
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(view.read_with(cx, |wb, _| wb.rewind_sheet().is_none()));
    assert_eq!(fs::read_to_string(&a).unwrap(), "edited by the agent\n");

    // Return presses the focused button, Cancel: nothing written either.
    click(&view, cx, Region::ChatRewind(footer[0]));
    sheet_ready(&view, cx);
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert!(view.read_with(cx, |wb, _| wb.rewind_sheet().is_none()));
    assert_eq!(fs::read_to_string(&a).unwrap(), "edited by the agent\n");

    // The conflict counts only through its own box.
    click(&view, cx, Region::ChatRewind(footer[0]));
    sheet_ready(&view, cx);
    click(&view, cx, Region::RewindFile(1));
    diff_shown(&view, cx);
    assert_eq!(restore_label(&view, cx), "Restore 2 files");
    click(&view, cx, Region::RewindFile(1));
    assert_eq!(restore_label(&view, cx), "Restore 1 file");
    // Restore all, including edited-after, takes two clicks: the first only arms it.
    click(&view, cx, Region::Rewind("restore-all"));
    assert!(view.read_with(cx, |wb, _| wb.rewind_sheet().unwrap().armed_all));
    assert_eq!(fs::read_to_string(&c).unwrap(), "the user's change\n");

    // Restore writes the clean file only, and says so.
    click(&view, cx, Region::Rewind("restore"));
    wait_until(cx, "the restore's toast", |cx| {
        view.read_with(cx, |wb, _| {
            wb.restore_toast().is_some() && wb.rewind_sheet().is_none()
        })
    });
    assert_eq!(fs::read_to_string(&a).unwrap(), "alpha\n");
    assert_eq!(fs::read_to_string(&c).unwrap(), "the user's change\n");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.restore_toast().unwrap().text()),
        "Restored 1 file"
    );
    // The open tab reloads the restored text.
    wait_until(cx, "the tab to show the restored text", |cx| {
        buffer.read_with(cx, |b, _| b.text() == "alpha\n")
    });
    // The footer of the restored turn: Restored — Undo.
    assert!(store.read_with(cx, |store, _| store.restored_by(checkpoint).is_some()));
    assert!(drawn(&view, cx, Region::ChatRewind(footer[0])));
    // The note for the agent waits in the composer.
    let draft = view.read_with(cx, |wb, cx| {
        wb.chat_model_of(s.session)
            .unwrap()
            .read(cx)
            .input()
            .text()
            .to_owned()
    });
    assert!(
        draft.starts_with("Hutan restored these files to before the turn")
            && draft.contains("a.txt"),
        "{draft}"
    );

    // The toast's Undo restores the restore: the agent's text is back.
    click(&view, cx, Region::RestoreToast("undo"));
    wait_until(cx, "the undo", |cx| {
        view.read_with(cx, |wb, _| wb.restore_toast().is_some_and(|t| t.undid))
    });
    assert_eq!(fs::read_to_string(&a).unwrap(), "edited by the agent\n");
    assert!(store.read_with(cx, |store, _| store.restored_by(checkpoint).is_none()));
    wait_until(cx, "the tab to show the agent's text again", |cx| {
        buffer.read_with(cx, |b, _| b.text() == "edited by the agent\n")
    });
    let _ = &s.env;
}

#[gpui::test]
fn a_chat_outside_git_says_why_once_in_its_menu_and_nothing_per_turn(cx: &mut TestAppContext) {
    let (s, cwd) = setup(1, false);
    let (view, cx) = open(cx, &s.daemon, s.daemon.connection());
    show_chat(&view, cx, s.session);
    turn(&s, &view, cx, 0, || {
        fs::write(cwd.join("a.txt"), "edited by the agent\n").unwrap();
    });
    wait_until(cx, "the chat's checkpoint state", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(s.session)
                .and_then(|info| info.chat.as_ref()?.checkpoints.clone())
                .is_some_and(|status| status.state == CheckpointAvailability::Unsupported)
        })
    });
    let footer = footers(&view, cx, s.session);
    assert!(!drawn(&view, cx, Region::ChatCheckpoint(footer[0])));
    assert!(!drawn(&view, cx, Region::ChatRewind(footer[0])));

    click(&view, cx, Region::Chat("menu"));
    let entries = view.read_with(cx, |wb, _| wb.context_menu_entries().unwrap());
    let said: Vec<String> = entries
        .iter()
        .map(|(label, _)| label.to_string())
        .filter(|label| label.starts_with("Checkpoints"))
        .collect();
    assert_eq!(said.len(), 1, "{entries:?}");
    // The reason is in the label when it is short, else (FX-UI-B) in the tooltip.
    let tips = view.read_with(cx, |wb, _| wb.context_menu_tips());
    let at = entries
        .iter()
        .position(|(label, _)| label.as_ref() == said[0])
        .unwrap();
    assert!(
        said[0].starts_with("Checkpoints: ") || (said[0] == "Checkpoints" && tips[at].is_some()),
        "{said:?} {tips:?}"
    );
    assert!(
        view.read_with(cx, |wb, _| wb.context_menu_disabled())
            .iter()
            .any(|label| label.as_ref() == said[0]),
        "the reason is greyed: {said:?}"
    );
}

#[gpui::test]
fn without_the_capability_nothing_of_checkpoints_is_drawn(cx: &mut TestAppContext) {
    let (mut s, root) = setup(1, true);
    let connection = s.daemon.connection_without_checkpoints();
    let (view, cx) = open(cx, &s.daemon, connection);
    show_chat(&view, cx, s.session);
    turn(&s, &view, cx, 0, || {
        fs::write(root.join("a.txt"), "edited by the agent\n").unwrap();
    });
    // The daemon recorded the turn, but this client cannot tell.
    wait_until(cx, "the daemon's checkpoint", |_| {
        let list: hutan_protocol::checkpoint::CheckpointList =
            s.daemon.call(Request::CheckpointList(Default::default()));
        !list.checkpoints.is_empty()
    });
    let footer = footers(&view, cx, s.session);
    assert!(!drawn(&view, cx, Region::ChatCheckpoint(footer[0])));
    assert!(!drawn(&view, cx, Region::ChatRewind(footer[0])));
    click(&view, cx, Region::Chat("menu"));
    let entries = view.read_with(cx, |wb, _| wb.context_menu_entries().unwrap());
    assert!(
        !entries
            .iter()
            .any(|(label, _)| label.starts_with("Checkpoints")),
        "{entries:?}"
    );
    cx.simulate_keystrokes("escape");
    // The palette's commands say so instead of acting.
    view.update_in(cx, |wb, window, cx| wb.rewind_focused_chat(window, cx));
    assert!(view.read_with(cx, |wb, _| wb.checkpoints_list().is_none()));
}

#[gpui::test]
fn a_busy_checkout_waits_for_the_turn_and_a_stale_plan_is_replaced(cx: &mut TestAppContext) {
    let (s, root) = setup(2, true);
    let (view, cx) = open(cx, &s.daemon, s.daemon.connection());
    show_chat(&view, cx, s.session);
    let a = root.join("a.txt");
    turn(&s, &view, cx, 0, || {
        fs::write(&a, "edited by the agent\n").unwrap();
    });
    let footer = footers(&view, cx, s.session);

    // Turn 1 runs (held): the sheet names the busy chat and restores nothing.
    let _: ChatSent = s.daemon.call(Request::ChatSend(ChatSend {
        session_id: s.session,
        text: "turn 1".to_owned(),
        attachments: vec![],
    }));
    click(&view, cx, Region::ChatRewind(footer[0]));
    wait_until(cx, "the busy line", |cx| {
        view.read_with(cx, |wb, _| wb.rewind_sheet().is_some_and(|s| s.busy()))
    });
    assert!(drawn(&view, cx, Region::Rewind("busy")));
    assert!(drawn(&view, cx, Region::Rewind("interrupt")));
    assert!(view.read_with(cx, |wb, _| !wb.rewind_sheet().unwrap().can_restore()));
    click(&view, cx, Region::Rewind("restore"));
    assert_eq!(fs::read_to_string(&a).unwrap(), "edited by the agent\n");

    // The turn ends: the sheet asks again by itself.
    fs::write(&s.gates[1], "").unwrap();
    wait_until(cx, "the plan without the busy turn", |cx| {
        view.read_with(cx, |wb, _| {
            wb.rewind_sheet()
                .is_some_and(|s| !s.busy() && s.can_restore())
        })
    });
    diff_shown(&view, cx);

    // a.txt changes after the preview: the restore writes nothing and shows the new plan.
    fs::write(&a, "changed after the preview\n").unwrap();
    click(&view, cx, Region::Rewind("restore"));
    wait_until(cx, "the new plan", |cx| {
        view.read_with(cx, |wb, _| {
            wb.rewind_sheet().is_some_and(|s| {
                !s.working && s.notice.as_deref() == Some(hutan_desktop::workbench::rewind::STALE)
            })
        })
    });
    assert!(drawn(&view, cx, Region::Rewind("notice")));
    assert_eq!(
        sheet_rows(&view, cx),
        [("a.txt".to_owned(), Group::After, false)]
    );
    assert_eq!(
        fs::read_to_string(&a).unwrap(),
        "changed after the preview\n"
    );
    assert!(view.read_with(cx, |wb, _| wb.restore_toast().is_none()));
}

#[gpui::test]
fn the_palette_picks_the_turn_from_the_checkpoints_list_and_undoes_the_last_restore(
    cx: &mut TestAppContext,
) {
    use hutan_desktop::workbench::actions;
    let (s, root) = setup(1, true);
    let (view, cx) = open(cx, &s.daemon, s.daemon.connection());
    show_chat(&view, cx, s.session);
    let a = root.join("a.txt");
    turn(&s, &view, cx, 0, || {
        fs::write(&a, "edited by the agent\n").unwrap();
    });

    // ⌘K Chat: Rewind Files to Before This Turn…: the list of the focused chat, with the
    // turn's prompt, its files and +/−.
    cx.dispatch_action(actions::RewindFocusedChat);
    wait_until(cx, "the Checkpoints list", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.checkpoints_list().is_some() && !wb.checkpoint_rows(cx).is_empty()
        })
    });
    let rows = view.read_with(cx, |wb, cx| wb.checkpoint_rows(cx));
    assert_eq!(rows.len(), 1);
    // The replayed Codex echoes the prompt of its recording as the turn's user message.
    assert_eq!(
        rows[0].prompt.as_deref(),
        Some("Reply with the single word pong and nothing else.")
    );
    assert_eq!(rows[0].checkpoint.files_changed, 1);
    assert!(drawn(&view, cx, Region::CheckpointRow(0)));

    click(&view, cx, Region::CheckpointRewind(0));
    sheet_ready(&view, cx);
    click(&view, cx, Region::Rewind("restore"));
    wait_until(cx, "the restore", |cx| {
        view.read_with(cx, |wb, _| wb.restore_toast().is_some())
    });
    assert_eq!(fs::read_to_string(&a).unwrap(), "alpha\n");
    view.update_in(cx, |wb, window, cx| wb.close_checkpoints(window, cx));
    show_chat(&view, cx, s.session);

    // ⌘K Chat: Undo Last Restore.
    let store = view.read_with(cx, |wb, cx| wb.sessions().read(cx).checkpoints().clone());
    wait_until(cx, "the restore's checkpoint", |cx| {
        store.read_with(cx, |store, _| store.last_restore(s.session).is_some())
    });
    cx.dispatch_action(actions::UndoLastRestore);
    wait_until(cx, "the undo", |cx| {
        view.read_with(cx, |wb, _| wb.restore_toast().is_some_and(|t| t.undid))
    });
    assert_eq!(fs::read_to_string(&a).unwrap(), "edited by the agent\n");
}

#[gpui::test]
fn the_sheet_names_the_project_and_restored_survives_a_restart_until_undone(
    cx: &mut TestAppContext,
) {
    let (mut s, root) = setup(1, true);
    let root = fs::canonicalize(&root).unwrap();
    let a = root.join("a.txt");
    let (view, cx) = open(cx, &s.daemon, s.daemon.connection());
    show_chat(&view, cx, s.session);
    turn(&s, &view, cx, 0, || {
        fs::write(&a, "edited by the agent\n").unwrap();
    });
    let footer = footers(&view, cx, s.session)[0];
    wait_until(cx, "the turn's Rewind…", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::ChatRewind(footer)).is_some()
        })
    });
    let checkpoint = view.read_with(cx, |wb, cx| {
        wb.sessions().read(cx).checkpoints().read(cx).of(s.session)[0].id
    });

    // The sheet says the project's name, and `this checkout`; the full path is the tooltip.
    click(&view, cx, Region::ChatRewind(footer));
    sheet_ready(&view, cx);
    // The repository is a project (the test daemon put the chat in Home; open it as one).
    let _: hutan_protocol::messages::WorkspaceOpened = s.daemon.call(Request::WorkspaceOpen(
        hutan_protocol::messages::WorkspaceOpen {
            path: root.clone(),
            created_by: None,
        },
    ));
    wait_until(cx, "the project `repo`", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .workspaces()
                .iter()
                .any(|w| w.name == "repo" && w.project.as_ref().is_some_and(|p| p.root == root))
        })
    });
    let header = view
        .read_with(cx, |wb, cx| wb.rewind_sheet_header(cx))
        .unwrap();
    assert_eq!(header.facts, "Codex · 1 turn · repo");
    assert!(
        header
            .label
            .ends_with("anything outside this checkout are not rewound."),
        "{}",
        header.label
    );
    let path = root.display().to_string();
    assert!(!header.facts.contains(&path) && !header.label.contains(&path));
    assert_eq!(header.checkout, path);
    click(&view, cx, Region::Rewind("restore"));
    wait_until(cx, "the restore", |cx| {
        view.read_with(cx, |wb, _| wb.restore_toast().is_some())
    });
    assert_eq!(fs::read_to_string(&a).unwrap(), "alpha\n");

    // The daemon and the desktop restart: a new window over a new daemon still shows the turn
    // as restored. (The restored chat gets a replay that refuses to resume: never a real Codex.)
    let refusing = s.env.fixture("plain-turn");
    s.daemon.stop();
    s.daemon.restart_with(|c| {
        c.codex = Some(refusing);
        c.checkpoints.git_env = git_env();
    });
    let (second, cx) = open(cx, &s.daemon, s.daemon.connection());
    show_chat(&second, cx, s.session);
    let store = second.read_with(cx, |wb, cx| wb.sessions().read(cx).checkpoints().clone());
    wait_until(cx, "Restored — Undo after the restart", |cx| {
        store.read_with(cx, |store, _| store.restored_by(checkpoint).is_some())
    });
    let footer = footers(&second, cx, s.session)[0];

    // Its Undo brings the agent's text back, and the footer offers Rewind… again.
    click(&second, cx, Region::ChatRewind(footer));
    wait_until(cx, "the undo", |cx| {
        second.read_with(cx, |wb, _| wb.restore_toast().is_some_and(|t| t.undid))
    });
    assert_eq!(fs::read_to_string(&a).unwrap(), "edited by the agent\n");
    wait_until(cx, "Rewind… again", |cx| {
        store.read_with(cx, |store, _| store.restored_by(checkpoint).is_none())
    });
    assert!(drawn(&second, cx, Region::ChatRewind(footer)));
}
