//! UX-SESSION-NAMES: terminals nobody named are told apart by what they are (the folder, the
//! command, a number), not called by the path their shell prints. Real PTYs against an
//! in-process daemon on a temporary `HUTAN_HOME`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::path::{Path, PathBuf};

use common::*;
use hutan_client::Client;
use hutan_core::SessionId;
use hutan_protocol::messages::{
    Empty, Event, Request, SessionClose, SessionInfo, SessionList, SessionListFilter,
    SessionRename, SessionResult,
};

/// A canonical folder called `name` in a temporary directory (kept alive by the guard).
fn folder(name: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap().join(name);
    std::fs::create_dir(&dir).unwrap();
    (tmp, dir)
}

async fn cat_in(client: &Client, dir: &Path) -> SessionInfo {
    let mut spec = sh("exec cat");
    spec.cwd = Some(dir.to_owned());
    create(client, spec).await
}

async fn titles(client: &Client) -> Vec<(SessionId, String)> {
    client
        .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
        .await
        .unwrap()
        .sessions
        .into_iter()
        .map(|s| (s.id, s.title))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn three_sessions_in_one_folder_get_three_names() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let (_tmp, dir) = folder("proj-demo");

    let a = cat_in(&client, &dir).await;
    let b = cat_in(&client, &dir).await;
    let c = cat_in(&client, &dir).await;
    assert_eq!(
        [&a.title, &b.title, &c.title],
        ["proj-demo", "proj-demo 2", "proj-demo 3"]
    );
    assert_eq!(
        titles(&client).await,
        [
            (a.id, "proj-demo".to_owned()),
            (b.id, "proj-demo 2".to_owned()),
            (c.id, "proj-demo 3".to_owned()),
        ]
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shell_path_title_is_never_the_name_but_a_real_title_is() {
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let (_tmp, dir) = folder("proj-demo");

    let session = cat_in(&client, &dir).await;
    let tid = terminal_of(&session);
    // What fish prints: an abbreviated path. `cat` echoes it back to the terminal.
    client
        .send_input(tid, b"\x1b]2;~/h/u/proj-demo\x07\r")
        .await
        .unwrap();
    wait_event(&mut events, "the path title", |e| match e {
        Event::TerminalTitle { title, .. } if title.as_deref() == Some("~/h/u/proj-demo") => {
            Some(())
        }
        _ => None,
    })
    .await;
    assert_eq!(titles(&client).await[0].1, "proj-demo");

    client
        .send_input(tid, b"\x1b]2;vim main.rs\x07\r")
        .await
        .unwrap();
    wait_event(&mut events, "the real title wins", |e| match e {
        Event::SessionUpdated { session: s } if s.id == session.id && s.title == "vim main.rs" => {
            Some(())
        }
        Event::SessionUpdated { session: s } if s.title.starts_with('~') => {
            panic!("a path became the session name: {}", s.title)
        }
        _ => None,
    })
    .await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_renamed_session_survives_a_restart_and_the_others_keep_their_names() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let (_tmp, dir) = folder("proj-demo");
    let first = cat_in(&client, &dir).await;
    let second = cat_in(&client, &dir).await;
    assert_eq!(second.title, "proj-demo 2");
    client
        .call::<SessionResult>(Request::SessionRename(SessionRename {
            session_id: first.id,
            title: "build".into(),
        }))
        .await
        .unwrap();
    // The user's name is a name of its own: the other session is no longer a namesake of it.
    assert_eq!(
        titles(&client).await,
        [
            (first.id, "build".to_owned()),
            (second.id, "proj-demo".to_owned())
        ]
    );
    drop(client);

    let t = t.restart().await;
    let client = t.connect().await;
    assert_eq!(
        titles(&client).await,
        [
            (first.id, "build".to_owned()),
            (second.id, "proj-demo".to_owned())
        ]
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_session_gives_its_number_back() {
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let (_tmp, dir) = folder("proj-demo");
    let a = cat_in(&client, &dir).await;
    let b = cat_in(&client, &dir).await;
    assert_eq!(b.title, "proj-demo 2");
    client
        .call::<Empty>(Request::SessionClose(SessionClose {
            session_id: a.id,
            stop_running: true,
        }))
        .await
        .unwrap();
    wait_event(&mut events, "the survivor renamed", |e| match e {
        Event::SessionUpdated { session: s } if s.id == b.id && s.title == "proj-demo" => Some(()),
        _ => None,
    })
    .await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_command_in_the_foreground_follows_the_name() {
    if !Path::new("/bin/zsh").exists() {
        eprintln!("skipping: zsh is not installed");
        return;
    }
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let (_tmp, dir) = folder("proj-demo");
    let zsh_home = tempfile::tempdir().unwrap();
    let mut zsh = spec(&["/bin/zsh", "-i"]);
    zsh.cwd = Some(dir);
    zsh.size = Some(size(200, 10));
    zsh.env.insert(
        "HOME".into(),
        zsh_home.path().to_string_lossy().into_owned(),
    );
    let session = create(&client, zsh).await;
    assert_eq!(session.title, "proj-demo");
    let mut view = View::attach(&client, terminal_of(&session), None).await;
    view.wait_for("a prompt", |s| s.contains("%")).await;

    client
        .send_input(terminal_of(&session), b"sleep 31\r")
        .await
        .unwrap();
    wait_event(&mut events, "the command in the name", |e| match e {
        Event::SessionUpdated { session: s }
            if s.id == session.id && s.title == "proj-demo \u{b7} sleep 31" =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    // Ctrl-C ends the command; the name is the folder's again.
    client
        .send_input(terminal_of(&session), b"\x03")
        .await
        .unwrap();
    wait_event(&mut events, "the plain name back", |e| match e {
        Event::SessionUpdated { session: s } if s.id == session.id && s.title == "proj-demo" => {
            Some(())
        }
        _ => None,
    })
    .await;
    t.stop().await;
}
