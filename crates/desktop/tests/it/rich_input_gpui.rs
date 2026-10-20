#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The composer's rich input (protocol 0.23, R-1) against an in-process daemon whose
//! `codex app-server` is `hutan-codex-fixture replay`: files and images dropped on the chat,
//! pasted images and files, long pastes folded into chips, chip removal, refusals shown before
//! sending, and what a send hands to the daemon.

use crate::common;

use std::path::PathBuf;

use common::chat::ChatEnv;
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{
    ClipboardEntry, ClipboardItem, Entity, ExternalPaths, FileDropEvent, Focusable as _, Modifiers,
    TestAppContext, VisualTestContext,
};
use hutan_core::SessionId;
use hutan_desktop::workbench::chat::{ChatModel, ChatView, ContextChip, LoadStatus};
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::assistant::{Attachment, AttachmentKind};
use hutan_settings::{SettingsPaths, SettingsStore};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nxyz";

fn open_chat<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    session: SessionId,
) -> (
    Entity<Workbench>,
    Entity<ChatView>,
    Entity<ChatModel>,
    &'a mut VisualTestContext,
) {
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
    view.update_in(cx, |wb, window, cx| wb.open_session(session, window, cx));
    wait_until(cx, "the chat to load", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(session)
                .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
        })
    });
    draw(cx);
    let chat = view.read_with(cx, |wb, _| wb.chat_view_of(session).expect("a chat view"));
    let model = view.read_with(cx, |wb, _| wb.chat_model_of(session).expect("a model"));
    chat.update_in(cx, |chat, window, cx| {
        window.focus(&chat.focus_handle(cx), cx)
    });
    (view, chat, model, cx)
}

fn chips(model: &Entity<ChatModel>, cx: &VisualTestContext) -> Vec<ContextChip> {
    model.read_with(cx, |m, _| m.context().to_vec())
}

fn kinds(model: &Entity<ChatModel>, cx: &VisualTestContext) -> Vec<AttachmentKind> {
    chips(model, cx).iter().map(ContextChip::kind).collect()
}

/// Drops `paths` on the middle of the chat's composer, as Finder does.
fn drop_files(view: &Entity<Workbench>, cx: &mut VisualTestContext, paths: &[PathBuf]) {
    draw(cx);
    let at = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Chat("composer")))
        .expect("the composer is drawn")
        .center();
    let dragged = || ExternalPaths(paths.iter().cloned().collect());
    cx.simulate_event(FileDropEvent::Entered {
        position: at,
        paths: dragged(),
    });
    cx.simulate_event(FileDropEvent::Submit { position: at });
    cx.simulate_event(FileDropEvent::Exited);
    cx.simulate_event(FileDropEvent::Ended);
    draw(cx);
}

fn env_with_chat() -> (ChatEnv, TestDaemon, SessionId) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    (env, daemon, info.id)
}

#[gpui::test]
fn dropping_a_file_and_an_image_adds_chips_and_sending_passes_them(cx: &mut TestAppContext) {
    let (env, daemon, session) = env_with_chat();
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes.txt");
    std::fs::write(&notes, "remember the milk\n").unwrap();
    let shot = dir.path().join("shot.png");
    std::fs::write(&shot, PNG).unwrap();
    let (view, chat, model, cx) = open_chat(cx, &daemon, session);
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Chat("drop-overlay")))
            .is_some(),
        "the whole chat is a drop target"
    );

    drop_files(&view, cx, &[notes.clone(), shot.clone()]);
    assert_eq!(
        kinds(&model, cx),
        [AttachmentKind::File, AttachmentKind::Image]
    );
    let chips = chips(&model, cx);
    assert!(chips.iter().all(|c| c.refused.is_none()), "{chips:?}");
    assert_eq!(
        chips[0].attachment,
        Attachment::File {
            path: notes.display().to_string(),
            range: None,
            picked: true
        }
    );
    assert!(matches!(
        &chips[1].attachment,
        Attachment::Image { path: Some(p), data_base64: None, media_type }
            if p == &shot.display().to_string() && media_type == "image/png"
    ));
    assert!(chips[1].thumb.is_some(), "an image chip has a thumbnail");

    // Sending passes the attachments; the daemon read them (a file anywhere: it was dropped).
    model.update(cx, |m, cx| {
        m.edit(|input| input.insert("what is this?"), cx);
    });
    assert!(chat.update(cx, |c, cx| c.send(cx)));
    wait_until(cx, "the turn to reach the agent", |_| {
        env.received().iter().any(|(m, _)| m == "turn/start")
    });
    let received = env.received();
    let start = received.iter().find(|(m, _)| m == "turn/start").unwrap();
    let input = &start.1["params"]["input"];
    let text = input[0]["text"].as_str().unwrap();
    assert!(text.starts_with("what is this?"), "{text}");
    assert!(text.contains("remember the milk"), "{text}");
    assert!(text.contains("[image: shot.png"), "{text}");
    assert_eq!(input[1]["type"], "localImage");
    assert_eq!(
        std::fs::canonicalize(input[1]["path"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&shot).unwrap()
    );
    wait_until(cx, "the composer to empty", |cx| chips_len(&model, cx) == 0);
}

fn chips_len(model: &Entity<ChatModel>, cx: &VisualTestContext) -> usize {
    model.read_with(cx, |m, _| m.context().len())
}

#[gpui::test]
fn pasting_an_image_files_and_long_text_attach(cx: &mut TestAppContext) {
    let (_env, daemon, session) = env_with_chat();
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes.txt");
    std::fs::write(&notes, "hello\n").unwrap();
    let (view, _chat, model, cx) = open_chat(cx, &daemon, session);

    // A clipboard image attaches inline.
    cx.write_to_clipboard(ClipboardItem::new_image(&gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        PNG.to_vec(),
    )));
    cx.simulate_keystrokes("cmd-v");
    assert_eq!(kinds(&model, cx), [AttachmentKind::Image]);
    assert!(matches!(
        &chips(&model, cx)[0].attachment,
        Attachment::Image { path: None, data_base64: Some(data), .. } if data == "iVBORw0KGgp4eXo="
    ));

    // Files copied in Finder attach as files.
    cx.write_to_clipboard(ClipboardItem {
        entries: vec![ClipboardEntry::ExternalPaths(ExternalPaths(
            [notes.clone()].into_iter().collect(),
        ))],
    });
    cx.simulate_keystrokes("cmd-v");
    assert_eq!(
        kinds(&model, cx),
        [AttachmentKind::Image, AttachmentKind::File]
    );

    // A short text is typed; a long one folds into a chip that opens on a click.
    cx.write_to_clipboard(ClipboardItem::new_string("short".into()));
    cx.simulate_keystrokes("cmd-v");
    assert_eq!(
        model.read_with(cx, |m, _| m.input().text().to_owned()),
        "short"
    );
    let long = (0..312)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    cx.write_to_clipboard(ClipboardItem::new_string(long.clone()));
    cx.simulate_keystrokes("cmd-v");
    assert_eq!(
        model.read_with(cx, |m, _| m.input().text().to_owned()),
        "short",
        "the long text did not go into the composer"
    );
    let all = chips(&model, cx);
    assert_eq!(all.len(), 3);
    assert_eq!(all[2].label(), "Pasted · 312 lines");
    assert_eq!(all[2].attachment, Attachment::PastedText { text: long });
    assert!(!all[2].expanded);
    draw(cx);
    let at = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::ChatContextChip(2)))
        .unwrap()
        .center();
    cx.simulate_click(at, Modifiers::none());
    assert!(
        chips(&model, cx)[2].expanded,
        "a click opens the folded paste"
    );
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Chat("paste")))
            .is_some()
    );
}

#[gpui::test]
fn chips_are_removed_with_the_cross_and_with_backspace(cx: &mut TestAppContext) {
    let (_env, daemon, session) = env_with_chat();
    let (view, _chat, model, cx) = open_chat(cx, &daemon, session);
    for n in 0..3 {
        model.update(cx, |m, cx| {
            m.attach_context(ContextChip::file(&format!("f{n}.txt"), None), cx);
        });
    }
    draw(cx);
    // The cross is the right edge of the chip.
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::ChatContextChip(0)))
        .unwrap();
    cx.simulate_click(
        gpui::point(bounds.right() - gpui::px(8.0), bounds.center().y),
        Modifiers::none(),
    );
    assert_eq!(chips_len(&model, cx), 2);
    // Backspace in an empty composer takes the last chip off; with text it edits the text.
    cx.simulate_keystrokes("backspace");
    assert_eq!(chips_len(&model, cx), 1);
    cx.simulate_input("ab");
    cx.simulate_keystrokes("backspace");
    assert_eq!(chips_len(&model, cx), 1);
    assert_eq!(model.read_with(cx, |m, _| m.input().text().to_owned()), "a");
}

#[gpui::test]
fn a_chip_that_cannot_be_sent_says_why_before_send(cx: &mut TestAppContext) {
    let (env, daemon, session) = env_with_chat();
    let dir = tempfile::tempdir().unwrap();
    let blob = dir.path().join("blob.bin");
    std::fs::write(&blob, [0u8, 1, 2, 3]).unwrap();
    let (view, chat, model, cx) = open_chat(cx, &daemon, session);
    drop_files(&view, cx, &[blob.clone(), dir.path().to_path_buf()]);
    let all = chips(&model, cx);
    assert_eq!(all.len(), 2);
    assert!(
        all[0]
            .refused
            .as_deref()
            .unwrap()
            .contains("not a text file")
    );
    assert!(all[1].refused.as_deref().unwrap().contains("folders"));
    model.update(cx, |m, cx| m.edit(|input| input.insert("go"), cx));
    assert!(
        !chat.update(cx, |c, cx| c.send(cx)),
        "a refused chip holds the message back"
    );
    assert!(env.received().iter().all(|(m, _)| m != "turn/start"));
    // Removing the refused chips frees it.
    model.update(cx, |m, cx| {
        m.remove_context(1, cx);
        m.remove_context(0, cx);
    });
    assert!(chat.update(cx, |c, cx| c.send(cx)));
}
