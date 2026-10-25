#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! ⌘V of an image into a terminal (TERM-IMAGE-PASTE): the clipboard image is saved under the
//! data folder's `scratch/<session>`, its path reaches the program (a real `cat` in a PTY echoes
//! what it is typed), a toast says where it went and *Reveal* shows it, the daemon removes the
//! file when the session is closed, a terminal that runs a known agent gets the path in that
//! agent's form, and `terminal.paste_images = false` restores the refusal. What the view does
//! with each kind of image is tested in `hutan-terminal-view`.

use crate::common;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use common::{TestDaemon, open_with, screen_text, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Empty, Request, SessionClose,
};
use hutan_settings::{SettingsPaths, SettingsStore};
use hutan_terminal_view::TerminalView;

/// A PNG of a few pixels.
fn png() -> Vec<u8> {
    let picture = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        4,
        3,
        image::Rgb([200, 100, 50]),
    ));
    let mut out = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn put_image_on_the_clipboard(cx: &mut VisualTestContext) {
    cx.write_to_clipboard(gpui::ClipboardItem::from(gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        png(),
    )));
}

/// A workbench whose data folder is `data`, showing a `cat` session.
fn open_cat<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    data: &Path,
    settings_toml: &str,
) -> (
    Entity<Workbench>,
    &'a mut VisualTestContext,
    Entity<TerminalView>,
    SessionId,
) {
    std::fs::create_dir_all(data).unwrap();
    std::fs::write(data.join("settings.toml"), settings_toml).unwrap();
    let info = daemon.create_session(&["/bin/cat"], None);
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(data)),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(info.id, window, cx));
    wait_until(cx, "the terminal view", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_of(info.id).is_some())
    });
    let terminal = view.read_with(cx, |wb, _| wb.terminal_view_of(info.id).unwrap());
    (view, cx, terminal, info.id)
}

/// What is on the terminal's screen, wrapped lines joined.
fn screen(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId) -> String {
    view.read_with(cx, |wb, cx| {
        wb.terminal_view_of(id)
            .map(|v| screen_text(&v.read(cx).model()))
            .unwrap_or_default()
            .replace('\n', "")
    })
}

fn paste(terminal: &Entity<TerminalView>, cx: &mut VisualTestContext) {
    terminal.update(cx, |view, cx| view.paste(cx));
}

fn scratch_file(data: &Path, id: SessionId) -> PathBuf {
    data.join("scratch")
        .join(id.to_string())
        .join("paste-1.png")
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, "the control to be drawn", |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
    });
    let at = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap()
        .center();
    cx.simulate_click(at, Modifiers::none());
    common::draw(cx);
}

#[gpui::test]
fn an_image_is_saved_its_path_reaches_the_program_and_closing_the_session_removes_it(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    // The desktop and the daemon share the data folder, as they do for a real user.
    let data = daemon.home().to_path_buf();
    let (view, cx, terminal, id) = open_cat(cx, &daemon, &data, "");
    let revealed: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let sink = revealed.clone();
    view.update(cx, |wb, _| {
        wb.set_reveal_handler(move |path, _| sink.borrow_mut().push(path.to_owned()));
    });

    put_image_on_the_clipboard(cx);
    paste(&terminal, cx);
    let saved = scratch_file(&data, id);
    wait_until(cx, "the file", |_| saved.exists());
    assert_eq!(std::fs::read(&saved).unwrap(), png());

    // Typed into the program like a drop: `cat` echoes it.
    let shown = saved.display().to_string();
    wait_until(cx, "the path on the screen", |cx| {
        screen(&view, cx, id).contains(&shown)
    });

    // A toast says where it went, and Reveal shows the file.
    wait_until(cx, "the toast", |cx| {
        view.read_with(cx, |wb, _| wb.paste_toast().is_some())
    });
    let toast = view.read_with(cx, |wb, _| wb.paste_toast().cloned().unwrap());
    assert_eq!(toast.path, saved);
    assert!(
        toast.text.starts_with("Image saved to ") && toast.text.ends_with("paste-1.png"),
        "{}",
        toast.text
    );
    common::draw(cx);
    click(&view, cx, Region::PasteToast("reveal"));
    assert_eq!(*revealed.borrow(), [saved.clone()]);
    assert!(view.read_with(cx, |wb, _| wb.paste_toast().is_none()));

    // Closing the session removes its images: the daemon owns the session.
    let _: Empty = daemon.call(Request::SessionClose(SessionClose {
        session_id: id,
        stop_running: true,
    }));
    assert!(!saved.exists(), "the image goes with the session");
    assert!(!saved.parent().unwrap().exists(), "and so does its folder");
}

#[gpui::test]
fn a_terminal_that_runs_an_agent_gets_the_path_in_its_form(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    // A folder with a space, like `~/Library/Application Support/Hutan`; short, so the echoed
    // path wraps little.
    let parent = tempfile::Builder::new()
        .prefix("hip")
        .tempdir_in("/tmp")
        .unwrap();
    let data = parent.path().join("A B");
    let (view, cx, terminal, id) = open_cat(cx, &daemon, &data, "");

    // The session's agent reports itself (the hook of Claude Code does this).
    let applied: AgentApplied = daemon.call(Request::AgentReport(AgentReportRequest {
        pid: None,
        session_id: id,
        source: "claude-hooks".into(),
        agent: "claude".into(),
        state: Some(AgentState::Idle),
        message: None,
        seq: 1,
        agent_session_id: None,
        resume_argv: None,
        node: None,
    }));
    assert!(applied.applied);
    wait_until(cx, "the store to know the agent", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(id)
                .is_some_and(|s| s.agent.is_some())
        })
    });

    put_image_on_the_clipboard(cx);
    paste(&terminal, cx);
    let saved = scratch_file(&data, id);
    let escaped = saved.display().to_string().replace(' ', "\\ ");
    wait_until(cx, "the backslash form on the screen", |cx| {
        screen(&view, cx, id).contains(&escaped)
    });
    assert!(
        !screen(&view, cx, id).contains('\''),
        "an agent's prompt gets the escaped path, not a quoted one"
    );
}

#[gpui::test]
fn without_an_agent_a_path_with_a_space_is_quoted(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let parent = tempfile::Builder::new()
        .prefix("hip")
        .tempdir_in("/tmp")
        .unwrap();
    let data = parent.path().join("A B");
    let (view, cx, terminal, id) = open_cat(cx, &daemon, &data, "");
    put_image_on_the_clipboard(cx);
    paste(&terminal, cx);
    let quoted = format!("'{}'", scratch_file(&data, id).display());
    wait_until(cx, "the quoted path on the screen", |cx| {
        screen(&view, cx, id).contains(&quoted)
    });
}

#[gpui::test]
fn with_paste_images_off_the_image_is_refused_and_nothing_is_saved(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let data = daemon.home().to_path_buf();
    let (view, cx, terminal, _id) =
        open_cat(cx, &daemon, &data, "[terminal]\npaste_images = false\n");
    put_image_on_the_clipboard(cx);
    paste(&terminal, cx);
    cx.run_until_parked();
    let notice = view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string));
    assert_eq!(
        notice.as_deref(),
        Some("Images can't be pasted into a terminal — attach them in a chat")
    );
    assert!(!data.join("scratch").exists());
    assert!(view.read_with(cx, |wb, _| wb.paste_toast().is_none()));
}
