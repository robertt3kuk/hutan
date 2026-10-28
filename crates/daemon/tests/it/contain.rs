//! Contain (protocol 0.52, capability `sandbox`, ADR 0021 decision 3) end to end: a real daemon
//! on a temporary `HUTAN_HOME`, real PTYs, real `sandbox-exec` (this Mac has Seatbelt; the
//! tests are skipped elsewhere). Each test starts a session that asks for the boundary and
//! reads what the process inside it saw: the shell prints markers, the test waits for them on
//! the screen.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(target_os = "macos")]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::common::{TestDaemon, View, create, remote_code, spec, terminal_of};
use hutan_client::Client;
use hutan_core::{Contain, ContainKind, SafetyEnforcer, SafetyPreset};
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::ChatCreate;
use hutan_protocol::messages::{
    Request, SessionCreate, SessionInfo, SessionList, SessionListFilter, SessionRef, SessionResult,
    SessionSpec, TerminalSpec,
};

const TOML: &str = "[terminal]\nenv_inherit = \"daemon\"\n\n";

/// A folder outside every folder a contained session may write: neither a temp folder nor a
/// checkout. Removed with the guard.
struct Outside(PathBuf);

impl Outside {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "contain-outside-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir.canonicalize().unwrap())
    }
}

impl Drop for Outside {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A repository with one commit and a linked worktree, like the ones Hutan makes.
struct Repo {
    _dir: tempfile::TempDir,
    main: PathBuf,
    wt: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn repo() -> Repo {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let main = root.join("repo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let wt = root.join("wt");
    git(&main, &["worktree", "add", "-q", "-b", "feat", "../wt"]);
    Repo {
        _dir: dir,
        main,
        wt,
    }
}

fn contained_shell(
    cwd: &Path,
    contain: Contain,
    script: &str,
    env: &[(&str, &str)],
) -> TerminalSpec {
    let mut spec = spec(&["/bin/sh", "-c", script]);
    spec.cwd = Some(cwd.to_owned());
    spec.contain = Some(contain);
    for (key, value) in env {
        spec.env.insert((*key).to_owned(), (*value).to_owned());
    }
    spec
}

fn spec_in(cwd: &Path) -> TerminalSpec {
    let mut spec = spec(&["/bin/sh", "-c", "exec cat"]);
    spec.cwd = Some(cwd.to_owned());
    spec
}

fn chat_request() -> ChatCreate {
    ChatCreate {
        extra_dirs: Vec::new(),
        contain: None,
        workspace_id: None,
        cwd: PathBuf::new(),
        agent: "omp".into(),
        model: None,
        approval_policy: None,
        sandbox: None,
        title: None,
        mode: None,
        provider_id: None,
        system_prompt: None,
        account: None,
        safety: None,
    }
}

async fn try_create(
    client: &Client,
    spec: TerminalSpec,
) -> Result<SessionInfo, hutan_client::ClientError> {
    client
        .call::<SessionResult>(Request::SessionCreate(SessionCreate {
            workspace_id: None,
            spec: SessionSpec::Terminal(spec),
        }))
        .await
        .map(|r| r.session)
}

/// Runs `script` contained in `cwd` and waits for `DONE`, which it prints last (it then keeps
/// the session alive with `cat`, so the screen stays).
async fn run(
    client: &Client,
    cwd: &Path,
    contain: Contain,
    script: &str,
    env: &[(&str, &str)],
) -> (SessionInfo, View) {
    let script = format!("{script}\necho DONE\nexec cat");
    let session = create(client, contained_shell(cwd, contain, &script, env)).await;
    let mut view = View::attach(client, terminal_of(&session), None).await;
    view.wait_for("the script finished", |s| s.contains("DONE"))
        .await;
    (session, view)
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_outside_the_roots_fail_with_eperm_and_inside_succeed() {
    let t = TestDaemon::start_with(TOML, |_| {}).await;
    let client = t.connect().await;
    assert!(client.supports_sandbox());
    let repo = repo();
    let outside = Outside::new();
    let temp = std::env::temp_dir().canonicalize().unwrap();
    let (session, view) = run(
        &client,
        &repo.wt,
        Contain::default(),
        "echo in > \"$WT/in.txt\"; echo \"IN=$?\"\n\
         ( echo out > \"$OUT/out.txt\" ) 2>/dev/null; echo \"OUT=$?\"\n\
         ( echo out > \"$OUT/again.txt\" ) 2>&1\n\
         echo tmp > \"$TMP/hutan-contain-probe-$$\"; echo \"TMP=$?\"; rm -f \"$TMP\"/hutan-contain-probe-$$\n\
         security list-keychains > /dev/null 2>&1; echo \"KEYCHAIN=$?\"\n\
         echo \"CONTAINED=$HUTAN_CONTAINED\"",
        &[
            ("WT", repo.wt.to_str().unwrap()),
            ("OUT", outside.0.to_str().unwrap()),
            ("TMP", temp.to_str().unwrap()),
        ],
    )
    .await;
    let text = view.screen.text();
    assert!(text.contains("IN=0"), "{text}");
    assert!(text.contains("OUT=1"), "{text}");
    assert!(text.contains("Operation not permitted"), "EPERM: {text}");
    assert!(
        text.contains("TMP=0"),
        "the temp folder is writable: {text}"
    );
    assert!(
        !text.contains("KEYCHAIN=0"),
        "the Keychain is out of reach: {text}"
    );
    assert!(text.contains("CONTAINED=1"), "{text}");
    assert_eq!(
        std::fs::read_to_string(repo.wt.join("in.txt")).unwrap(),
        "in\n"
    );
    assert!(!outside.0.join("out.txt").exists());

    // The session says what was applied, not what was asked.
    let containment = session.containment.expect("containment");
    assert_eq!(containment.kind, ContainKind::Seatbelt);
    assert!(containment.applied && containment.network && containment.reason.is_none());
    assert_eq!(
        containment.write_roots,
        [repo.wt.clone(), repo.main.join(".git")]
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn git_works_in_a_linked_worktree_and_hooks_and_config_stay_read_only() {
    let t = TestDaemon::start_with(TOML, |_| {}).await;
    let client = t.connect().await;
    let repo = repo();
    let git_dir = repo.main.join(".git");
    let (_, view) = run(
        &client,
        &repo.wt,
        Contain::default(),
        "echo hello > file.txt\n\
         git -c user.email=a@b -c user.name=n add file.txt; echo \"ADD=$?\"\n\
         git -c user.email=a@b -c user.name=n commit -q -m contained; echo \"COMMIT=$?\"\n\
         echo \"COMMITS=$(git log --oneline | wc -l | tr -d ' ')\"\n\
         git config user.name zz 2>/dev/null; echo \"CONFIG=$?\"\n\
         ( echo '#!/bin/sh' > \"$GITDIR/hooks/pre-commit\" ) 2>/dev/null; echo \"HOOK=$?\"\n\
         ( echo x > \"$GITDIR/config\" ) 2>/dev/null; echo \"CONFIGFILE=$?\"",
        &[("GITDIR", git_dir.to_str().unwrap())],
    )
    .await;
    let text = view.screen.text();
    assert!(
        text.contains("ADD=0") && text.contains("COMMIT=0"),
        "{text}"
    );
    assert!(text.contains("COMMITS=2"), "{text}");
    assert!(!text.contains("CONFIG=0"), "git config is refused: {text}");
    assert!(!text.contains("HOOK=0"), "hooks stay read-only: {text}");
    assert!(!text.contains("CONFIGFILE=0"), "{text}");
    assert!(!git_dir.join("hooks/pre-commit").exists());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn network_off_blocks_a_loopback_connect_and_on_allows_it() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let t = TestDaemon::start_with(TOML, |_| {}).await;
    let client = t.connect().await;
    let repo = repo();
    let script = "/usr/bin/nc -z -w 2 127.0.0.1 \"$PORT\" > /dev/null 2>&1; echo \"NC=$?\"";
    let (off, view) = run(
        &client,
        &repo.wt,
        Contain {
            network: Some(false),
            ..Contain::default()
        },
        script,
        &[("PORT", &port)],
    )
    .await;
    assert!(view.screen.contains("NC=1"), "{}", view.screen.text());
    assert!(!off.containment.unwrap().network);

    let (on, view) = run(
        &client,
        &repo.wt,
        Contain::default(),
        script,
        &[("PORT", &port)],
    )
    .await;
    assert!(view.screen.contains("NC=0"), "{}", view.screen.text());
    assert!(on.containment.unwrap().network);
    drop(listener);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn network_on_still_refuses_the_daemons_own_socket() {
    let t = TestDaemon::start_with(TOML, |_| {}).await;
    let client = t.connect().await;
    let repo = repo();
    let socket = t.paths.socket.to_str().unwrap().to_owned();
    let script = "/usr/bin/perl -MIO::Socket::UNIX -e 'exit(IO::Socket::UNIX->new(Peer => $ARGV[0]) ? 0 : 1)' \"$SOCK\" > /dev/null 2>&1; echo \"SOCK=$?\"";
    let (_, view) = run(
        &client,
        &repo.wt,
        Contain::default(),
        script,
        &[("SOCK", &socket)],
    )
    .await;
    assert!(view.screen.contains("SOCK=1"), "{}", view.screen.text());

    // The same call from an uncontained session reaches it: the refusal is the boundary's.
    let mut open = spec(&["/bin/sh", "-c", &format!("{script}; exec cat")]);
    open.env.insert("SOCK".into(), socket);
    let open = create(&client, open).await;
    let mut view = View::attach(&client, terminal_of(&open), None).await;
    view.wait_for("the uncontained connect", |s| s.contains("SOCK="))
        .await;
    assert!(view.screen.contains("SOCK=0"), "{}", view.screen.text());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn extra_write_folders_are_writable_and_must_exist() {
    let t = TestDaemon::start_with(TOML, |_| {}).await;
    let client = t.connect().await;
    let repo = repo();
    let extra = Outside::new();
    let (_, view) = run(
        &client,
        &repo.wt,
        Contain {
            extra_write: vec![extra.0.clone()],
            ..Contain::default()
        },
        "echo x > \"$EXTRA/x\"; echo \"EXTRA=$?\"",
        &[("EXTRA", extra.0.to_str().unwrap())],
    )
    .await;
    assert!(view.screen.contains("EXTRA=0"), "{}", view.screen.text());

    for bad in [extra.0.join("nope"), PathBuf::from("relative")] {
        let refused = try_create(
            &client,
            contained_shell(
                &repo.wt,
                Contain {
                    extra_write: vec![bad],
                    ..Contain::default()
                },
                "true",
                &[],
            ),
        )
        .await;
        assert_eq!(remote_code(refused), ErrorCode::BadRequest);
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_contained_terminal_needs_a_folder() {
    let t = TestDaemon::start_with(TOML, |_| {}).await;
    let client = t.connect().await;
    let mut spec = spec(&["/bin/sh", "-c", "true"]);
    spec.contain = Some(Contain::default());
    let error = try_create(&client, spec).await.unwrap_err();
    assert!(error.to_string().contains("cwd"), "{error}");
    assert_eq!(remote_code(Err::<(), _>(error)), ErrorCode::BadRequest);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_boundary_that_cannot_be_applied_refuses_the_session_and_starts_nothing() {
    let repo = repo();
    // `sandbox-exec` is missing, and it is present but does not apply a profile.
    for (bin, reason) in [
        ("/nonexistent/sandbox-exec", "is missing"),
        ("/usr/bin/false", "could not run a command"),
    ] {
        let t = TestDaemon::start_with(&format!("{TOML}[sandbox]\nbin = {bin:?}\n"), |_| {}).await;
        let client = t.connect().await;
        let error = try_create(
            &client,
            contained_shell(&repo.wt, Contain::default(), "touch \"$WT/started\"", &[]),
        )
        .await
        .unwrap_err();
        let text = error.to_string();
        assert!(
            text.contains("Contain is not available") && text.contains(reason),
            "{bin}: {text}"
        );
        assert!(text.contains("not started uncontained"), "{text}");
        assert_eq!(remote_code(Err::<(), _>(error)), ErrorCode::Unsupported);
        let list: SessionList = client
            .call(Request::SessionList(SessionListFilter::default()))
            .await
            .unwrap();
        assert!(list.sessions.is_empty(), "no session was made: {list:?}");
        assert!(!repo.wt.join("started").exists());

        // An OMP chat that asks for *Contained* is refused the same way, before a process.
        let chat = client
            .call::<SessionResult>(Request::ChatCreate(ChatCreate {
                cwd: repo.wt.clone(),
                safety: Some(SafetyPreset::Contained),
                ..chat_request()
            }))
            .await
            .unwrap_err();
        assert_eq!(remote_code(Err::<(), _>(chat)), ErrorCode::Unsupported);
        // An uncontained terminal is not affected.
        let plain = create(&client, spec_in(&repo.wt)).await;
        assert!(plain.containment.is_none());
        t.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_harness_with_its_own_sandbox_is_refused_because_seatbelt_does_not_nest() {
    let t = TestDaemon::start_with(TOML, |_| {}).await;
    let client = t.connect().await;
    let repo = repo();
    for argv in [
        vec!["codex".to_owned()],
        vec!["codex".into(), "-s".into(), "workspace-write".into()],
        vec![
            "claude".into(),
            "--settings".into(),
            r#"{"sandbox":{"enabled":true}}"#.into(),
        ],
    ] {
        let mut spec = spec_in(&repo.wt);
        spec.command = argv.clone();
        spec.contain = Some(Contain::default());
        let error = try_create(&client, spec).await.unwrap_err();
        assert!(
            error.to_string().contains("does not nest"),
            "{argv:?}: {error}"
        );
        assert_eq!(remote_code(Err::<(), _>(error)), ErrorCode::BadRequest);
    }
    // Chats of Codex and Claude Code: their own *Contained* preset is their sandbox.
    for agent in ["codex", "claude"] {
        let error = client
            .call::<SessionResult>(Request::ChatCreate(ChatCreate {
                cwd: repo.wt.clone(),
                agent: agent.into(),
                contain: Some(Contain::default()),
                ..chat_request()
            }))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("does not nest"),
            "{agent}: {error}"
        );
        assert_eq!(remote_code(Err::<(), _>(error)), ErrorCode::BadRequest);
    }
    // The rule itself: a sandbox applied from inside the boundary is refused by macOS.
    let (_, view) = run(
        &client,
        &repo.wt,
        Contain::default(),
        "sandbox-exec -p '(version 1)(allow default)' /usr/bin/true 2>&1; echo \"NEST=$?\"",
        &[],
    )
    .await;
    let text = view.screen.text();
    assert!(
        text.contains("sandbox_apply: Operation not permitted") && text.contains("NEST=71"),
        "{text}"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_applies_the_boundary_again_and_an_uncontained_session_says_nothing() {
    let t = TestDaemon::start_with(TOML, |_| {}).await;
    let client = t.connect().await;
    let repo = repo();
    let outside = Outside::new();
    let script = "( echo x > \"$OUT/x\" ) 2>/dev/null; echo \"OUT=$?\"; exec cat";
    let session = create(
        &client,
        contained_shell(
            &repo.wt,
            Contain::default(),
            script,
            &[("OUT", outside.0.to_str().unwrap())],
        ),
    )
    .await;
    let mut view = View::attach(&client, terminal_of(&session), None).await;
    view.wait_for("first run", |s| s.contains("OUT=1")).await;
    crate::common::stop(&client, session.id).await.unwrap();

    let restarted = client
        .call::<SessionResult>(Request::SessionRestart(SessionRef {
            session_id: session.id,
        }))
        .await
        .unwrap()
        .session;
    let containment = restarted
        .containment
        .clone()
        .expect("still contained after a restart");
    assert!(containment.applied);
    let mut view = View::attach(&client, terminal_of(&restarted), None).await;
    view.wait_for("second run", |s| s.contains("OUT=1")).await;
    assert!(
        !outside.0.join("x").exists(),
        "the restarted process is contained too"
    );

    // The same script without `contain` writes outside.
    let mut open = spec(&["/bin/sh", "-c", script]);
    open.cwd = Some(repo.wt.clone());
    open.env
        .insert("OUT".into(), outside.0.to_str().unwrap().into());
    let open = create(&client, open).await;
    assert!(open.containment.is_none());
    let mut view = View::attach(&client, terminal_of(&open), None).await;
    view.wait_for("uncontained run", |s| s.contains("OUT=0"))
        .await;
    assert!(outside.0.join("x").exists());
    t.stop().await;
}

/// An OMP chat runs inside the boundary: the probe stands in for `omp acp`, tries two writes
/// before it becomes the fixture, and logs what happened.
#[tokio::test(flavor = "multi_thread")]
async fn an_omp_chat_that_asks_for_the_boundary_runs_its_process_inside_it() {
    let work = tempfile::tempdir().unwrap();
    let outside = Outside::new();
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/acp-omp-18.11");
    let acp = {
        let exe = std::env::current_exe().unwrap();
        exe.parent()
            .and_then(Path::parent)
            .unwrap()
            .join("hutan-acp-fixture")
    };
    assert!(
        acp.is_file(),
        "hutan-acp-fixture not built: run `cargo build -p hutan-agents --bins`"
    );
    let probe = work.path().join("probe.sh");
    std::fs::write(
        &probe,
        "#!/bin/sh\n\
         if ( : > \"$OUTSIDE/probe\" ) 2>/dev/null; then echo outside=written; else echo outside=denied; fi >> \"$LOG\"\n\
         if ( : > \"$INSIDE/probe\" ) 2>/dev/null; then echo inside=written; else echo inside=denied; fi >> \"$LOG\"\n\
         printf '%s\\n' \"$@\" | head -n 8 >> \"$LOG\"\n\
         exec \"$FIXTURE\" \"$@\"\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let log = work.path().join("probe.log");
    let cwd = work.path().canonicalize().unwrap();
    let toml = format!(
        "{TOML}[harness.omp]\nbin = {probe:?}\nargs = [\"replay\", \"--received\", {received:?}, {transcript:?}, \"acp\"]\nenv = {{ LOG = {log:?}, OUTSIDE = {outside:?}, INSIDE = {inside:?}, FIXTURE = {acp:?} }}\n",
        received = work.path().join("received.jsonl"),
        transcript = fixtures.join("plain-turn.jsonl"),
        outside = outside.0,
        inside = cwd,
    );
    let t = TestDaemon::start_with(&toml, |_| {}).await;
    let client = t.connect().await;
    let session = client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            cwd: cwd.clone(),
            safety: Some(SafetyPreset::Contained),
            contain: Some(Contain {
                network: Some(true),
                extra_write: vec![],
            }),
            ..chat_request()
        }))
        .await
        .unwrap()
        .session;
    let log = std::fs::read_to_string(&log).unwrap();
    assert!(log.contains("outside=denied"), "{log}");
    assert!(log.contains("inside=written"), "{log}");
    assert!(log.contains("--approval-mode\nyolo"), "{log}");
    assert!(!outside.0.join("probe").exists());

    let containment = session.containment.expect("the applied boundary");
    assert!(containment.applied && containment.network);
    assert_eq!(containment.write_roots, [cwd]);
    let safety = session.chat.unwrap().safety.unwrap();
    assert_eq!(safety.enforced_by, SafetyEnforcer::Hutan);
    t.stop().await;
}
