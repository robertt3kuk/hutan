//! The file service (protocol 0.33, capabilities `fs`, `fs.watch`): a real daemon on a
//! temporary `HUTAN_HOME` and real client connections. Confinement, chunked atomic writes with
//! the version precondition, listings, the finder, searches, watching with the origin of each
//! change, and what an agent-originated write has to ask for.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, WAIT, create_in, sh, wait_event};
use hutan_client::{Client, ClientError};
use hutan_core::{AgentEventKind, ApprovalId, Seq, SessionId, WorkspaceId};
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{
    ApprovalInfo, ApprovalList, ApprovalListRequest, ApprovalRespond, ChatCreate,
};
use hutan_protocol::fs::{
    AgentConfidence, ChangeKind, FileVersion, FsChange, FsFind, FsList, FsOrigin, FsRead, FsSearch,
    FsUnwatch, FsWatch, FsWatchPath, FsWriteBegin, SEARCHES_MAX, UPLOADS_MAX,
};
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Empty, Event, Origin, Request, SessionInfo,
    WorkspaceOpen, WorkspaceOpened,
};
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

/// A daemon that tracks `root` as a workspace, a connection to it that listens to events, and
/// a directory outside every root.
pub(crate) struct Env {
    pub(crate) t: TestDaemon,
    pub(crate) client: Client,
    pub(crate) events: broadcast::Receiver<(Seq, Event)>,
    /// Canonical, a project of the daemon.
    pub(crate) root: PathBuf,
    /// Canonical, tracked by nobody.
    pub(crate) outside: PathBuf,
    pub(crate) workspace: WorkspaceId,
    _work: tempfile::TempDir,
}

impl Env {
    pub(crate) async fn new() -> Self {
        Self::with("", |_| {}).await
    }

    pub(crate) async fn with(
        toml: &str,
        tweak: impl FnOnce(&mut hutan_daemon::DaemonConfig),
    ) -> Self {
        Self::start(|_| toml.to_owned(), tweak).await
    }

    /// `toml` is told the (canonical) root it will track.
    pub(crate) async fn start(
        toml: impl FnOnce(&Path) -> String,
        tweak: impl FnOnce(&mut hutan_daemon::DaemonConfig),
    ) -> Self {
        let work = tempfile::tempdir().unwrap();
        let base = work.path().canonicalize().unwrap();
        let root = base.join("repo");
        let outside = base.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let t = TestDaemon::start_with(&toml(&root), tweak).await;
        let client = t.connect().await;
        let events = client.events();
        let opened: WorkspaceOpened = client
            .call(Request::WorkspaceOpen(WorkspaceOpen {
                path: root.clone(),
                created_by: None,
            }))
            .await
            .unwrap();
        assert!(client.supports_fs() && client.supports_fs_watch());
        Self {
            t,
            client,
            events,
            root,
            outside,
            workspace: opened.workspace.id,
            _work: work,
        }
    }

    pub(crate) fn at(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    pub(crate) async fn stop(self) {
        self.t.stop().await;
    }
}

pub(crate) fn reason(error: &ClientError) -> String {
    match error {
        ClientError::Remote(error) => error
            .data
            .as_ref()
            .and_then(|data| data["reason"].as_str())
            .unwrap_or("")
            .to_owned(),
        other => panic!("not a remote error: {other:?}"),
    }
}

pub(crate) fn refused<T: std::fmt::Debug>(
    result: Result<T, ClientError>,
    code: ErrorCode,
) -> ClientError {
    match result {
        Err(ClientError::Remote(body)) => {
            assert_eq!(body.code, code, "{body:?}");
            ClientError::Remote(body)
        }
        other => panic!("expected {code:?}, got {other:?}"),
    }
}

pub(crate) fn code_of(error: &ClientError) -> ErrorCode {
    match error {
        ClientError::Remote(body) => body.code,
        other => panic!("not a remote error: {other:?}"),
    }
}

fn read_request(path: &Path) -> FsRead {
    FsRead {
        path: path.to_owned(),
        offset: None,
        max_bytes: None,
        picked: false,
    }
}

fn begin(path: &Path, expect: Option<FileVersion>, create: bool) -> FsWriteBegin {
    FsWriteBegin {
        path: path.to_owned(),
        expect,
        create,
        size_hint: None,
        picked: false,
    }
}

/// Polls `check` until it returns `Some`; fails after [`WAIT`].
pub(crate) async fn eventually<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    timeout(WAIT, async {
        loop {
            if let Some(found) = check() {
                return found;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// The next `fs.changed` change of `path` that satisfies `pick`.
pub(crate) async fn change_of(
    events: &mut broadcast::Receiver<(Seq, Event)>,
    path: &Path,
    mut pick: impl FnMut(&FsChange) -> bool,
) -> FsChange {
    wait_event(
        events,
        &format!("fs.changed for {}", path.display()),
        |event| match event {
            Event::FsChanged { changes, .. } => changes
                .iter()
                .find(|change| change.path == path && pick(change))
                .cloned(),
            _ => None,
        },
    )
    .await
}

pub(crate) async fn watch_file(client: &Client, path: &Path) {
    let watching = client
        .fs_watch(FsWatch {
            paths: vec![FsWatchPath {
                path: path.to_owned(),
                recursive: false,
            }],
            picked: false,
        })
        .await
        .unwrap();
    assert!(watching.watching >= 1);
}

fn temp_files(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            name.starts_with(".hutan-write-").then_some(name)
        })
        .collect()
}

// ---- confinement ------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn paths_outside_the_roots_are_refused_and_picked_paths_are_the_users() {
    let env = Env::new().await;
    std::fs::write(env.at("a.txt"), "inside").unwrap();
    std::fs::write(env.outside.join("secret.txt"), "outside").unwrap();
    std::os::unix::fs::symlink(env.outside.join("secret.txt"), env.at("leak")).unwrap();
    std::os::unix::fs::symlink(&env.outside, env.at("leakdir")).unwrap();
    std::os::unix::fs::symlink(env.at("a.txt"), env.at("alias")).unwrap();
    let c = &env.client;

    assert_eq!(
        c.fs_read(read_request(&env.at("a.txt")))
            .await
            .unwrap()
            .text,
        "inside"
    );
    // A link that stays inside the roots is followed, with the canonical path in the answer.
    let alias = c.fs_read(read_request(&env.at("alias"))).await.unwrap();
    assert_eq!(
        (alias.text.as_str(), &alias.path),
        ("inside", &env.at("a.txt"))
    );

    // `..` out of the root, a link to a file outside, a link to a directory outside.
    let up = env.root.join("../outside/secret.txt");
    let error = refused(c.fs_read(read_request(&up)).await, ErrorCode::Forbidden);
    assert_eq!(reason(&error), "outside_roots");
    let error = refused(
        c.fs_read(read_request(&env.at("leak"))).await,
        ErrorCode::Forbidden,
    );
    assert_eq!(reason(&error), "symlink");
    let error = refused(
        c.fs_read(read_request(&env.at("leakdir/secret.txt"))).await,
        ErrorCode::Forbidden,
    );
    assert_eq!(reason(&error), "symlink");
    refused(c.fs_stat(&up, false).await, ErrorCode::Forbidden);
    refused(
        c.fs_list(FsList {
            dir: env.at("leakdir"),
            ignore: true,
            limit: None,
            picked: false,
        })
        .await,
        ErrorCode::Forbidden,
    );
    refused(
        c.fs_find(FsFind {
            root: env.outside.clone(),
            query: "s".into(),
            limit: None,
        })
        .await,
        ErrorCode::Forbidden,
    );
    refused(
        c.fs_write(begin(&env.outside.join("new.txt"), None, true), "x")
            .await,
        ErrorCode::Forbidden,
    );
    refused(
        c.fs_write(begin(&env.at("leakdir/new.txt"), None, true), "x")
            .await,
        ErrorCode::Forbidden,
    );
    assert!(!env.outside.join("new.txt").exists());
    // Relative paths and missing files are other mistakes.
    refused(
        c.fs_read(read_request(Path::new("a.txt"))).await,
        ErrorCode::BadRequest,
    );
    refused(
        c.fs_read(read_request(&env.at("nope.txt"))).await,
        ErrorCode::NotFound,
    );

    // The user chose the file themselves: `picked` reads it, writes it and lists its folder.
    let mut picked = read_request(&env.outside.join("secret.txt"));
    picked.picked = true;
    assert_eq!(c.fs_read(picked).await.unwrap().text, "outside");
    assert_eq!(
        c.fs_stat(&env.outside.join("secret.txt"), true)
            .await
            .unwrap()
            .size,
        7
    );
    let mut save = begin(&env.outside.join("saved.txt"), None, true);
    save.picked = true;
    c.fs_write(save, "saved as").await.unwrap();
    assert_eq!(
        std::fs::read_to_string(env.outside.join("saved.txt")).unwrap(),
        "saved as"
    );
    let listing = c
        .fs_list(FsList {
            dir: env.outside.clone(),
            ignore: true,
            limit: None,
            picked: true,
        })
        .await
        .unwrap();
    assert_eq!(listing.entries.len(), 2);

    // Writing below `.git` is refused even for a picked path.
    std::fs::create_dir_all(env.at(".git")).unwrap();
    let error = refused(
        c.fs_write(begin(&env.at(".git/hooks-new"), None, true), "x")
            .await,
        ErrorCode::Forbidden,
    );
    assert_eq!(reason(&error), "git_dir");
    env.stop().await;
}

// ---- reading and writing ----------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_six_megabyte_file_round_trips_in_chunks_byte_for_byte() {
    let env = Env::new().await;
    let path = env.at("big.txt");
    // A byte-order mark, CRLF line ends and multi-byte characters: nothing is normalised.
    let line = "héllo wörld — 日本語 \u{1F980} fn main() { println!(\"x\"); }\r\n";
    let mut text = String::from('\u{feff}');
    while text.len() < 6 * 1024 * 1024 + 17 {
        text.push_str(line);
    }
    let written = env
        .client
        .fs_write(begin(&path, None, true), &text)
        .await
        .unwrap();
    assert_eq!(written.path, path);
    assert_eq!(std::fs::read(&path).unwrap(), text.as_bytes());
    assert_eq!(written.version.size, text.len() as u64);
    assert!(temp_files(&env.root).is_empty());

    // The first chunk is 1 MiB at most, ends on a character and says there is more.
    let first = env.client.fs_read(read_request(&path)).await.unwrap();
    assert!(first.text.len() <= 1 << 20 && first.text.len() > (1 << 20) - 8);
    assert!(!first.eof && first.bom);
    assert_eq!(first.offset, 0);
    assert_eq!(first.size, text.len() as u64);
    assert_eq!(first.line_ending, hutan_protocol::fs::LineEnding::Crlf);
    assert_eq!(first.version, written.version);

    let all = env.client.fs_read_all(&path, false).await.unwrap();
    assert_eq!(all.text, text);
    assert_eq!(all.version, written.version);
    assert!(all.bom);

    // Over the cap: refused when it is begun, and when it is read.
    let error = refused(
        env.client
            .fs_write_begin(FsWriteBegin {
                size_hint: Some(65 << 20),
                ..begin(&env.at("huge.txt"), None, true)
            })
            .await,
        ErrorCode::Unsupported,
    );
    assert_eq!(reason(&error), "too_large");
    let huge = env.at("sparse.bin");
    std::fs::File::create(&huge)
        .unwrap()
        .set_len(65 << 20)
        .unwrap();
    let error = refused(
        env.client.fs_read(read_request(&huge)).await,
        ErrorCode::Unsupported,
    );
    assert_eq!(reason(&error), "too_large");
    // Binary files are not text.
    std::fs::write(env.at("bin"), b"a\0b").unwrap();
    let error = refused(
        env.client.fs_read(read_request(&env.at("bin"))).await,
        ErrorCode::Unsupported,
    );
    assert_eq!(reason(&error), "binary");
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_with_a_stale_version_is_a_conflict_that_carries_the_current_one() {
    let env = Env::new().await;
    let path = env.at("a.txt");
    std::fs::write(&path, "one").unwrap();
    let read = env.client.fs_read_all(&path, false).await.unwrap();

    let saved = env
        .client
        .fs_write(begin(&path, Some(read.version), false), "two")
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
    assert_ne!(saved.version, read.version);

    // The version of the first read is stale now.
    let error = env
        .client
        .fs_write(begin(&path, Some(read.version), false), "three")
        .await
        .unwrap_err();
    assert_eq!(code_of(&error), ErrorCode::Conflict);
    assert_eq!(reason(&error), "version");
    assert_eq!(error.conflict_current(), Some(saved.version));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
    assert!(temp_files(&env.root).is_empty());

    // Changed behind the client's back between the begin and the commit.
    let upload = env
        .client
        .fs_write_begin(begin(&path, Some(saved.version), false))
        .await
        .unwrap();
    env.client
        .fs_write_chunk(hutan_protocol::fs::FsWriteChunk {
            upload_id: upload.upload_id.clone(),
            seq: 0,
            text: "mine".into(),
        })
        .await
        .unwrap();
    std::fs::write(&path, "theirs, and longer").unwrap();
    let error = env
        .client
        .fs_write_commit(hutan_protocol::fs::FsWriteCommit {
            upload_id: upload.upload_id,
            total_bytes: 4,
        })
        .await
        .unwrap_err();
    assert_eq!(reason(&error), "version");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "theirs, and longer"
    );

    // A deleted file is a conflict too (nothing to overwrite), a new one must not exist.
    std::fs::remove_file(&path).unwrap();
    let error = env
        .client
        .fs_write(begin(&path, Some(saved.version), false), "x")
        .await
        .unwrap_err();
    assert_eq!(
        (reason(&error), error.conflict_current()),
        ("missing".to_owned(), None)
    );
    env.client
        .fs_write(begin(&path, None, true), "fresh")
        .await
        .unwrap();
    let error = env
        .client
        .fs_write(begin(&path, None, true), "again")
        .await
        .unwrap_err();
    assert_eq!(reason(&error), "exists");
    assert!(error.conflict_current().is_some());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "fresh");
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn uploads_are_bounded_owned_by_their_connection_and_expire() {
    let env = Env::with("", |c| {
        c.fs_limits.upload_idle = Duration::from_millis(300);
    })
    .await;
    let mut open = Vec::new();
    for index in 0..UPLOADS_MAX {
        let upload = env
            .client
            .fs_write_begin(begin(&env.at(&format!("f{index}.txt")), None, true))
            .await
            .unwrap();
        open.push(upload.upload_id);
    }
    assert_eq!(temp_files(&env.root).len(), UPLOADS_MAX);
    // The ninth waits for room.
    let error = refused(
        env.client
            .fs_write_begin(begin(&env.at("f9.txt"), None, true))
            .await,
        ErrorCode::InvalidState,
    );
    assert!(error.to_string().contains("uploads are open"), "{error}");
    // Nobody else's upload can be continued.
    let other = env.t.connect().await;
    refused(
        other
            .fs_write_commit(hutan_protocol::fs::FsWriteCommit {
                upload_id: open[0].clone(),
                total_bytes: 0,
            })
            .await,
        ErrorCode::InvalidState,
    );
    // Aborting one makes room, and its temporary file goes.
    env.client.fs_write_abort(&open.remove(0)).await.unwrap();
    assert_eq!(temp_files(&env.root).len(), UPLOADS_MAX - 1);
    let replaced = env
        .client
        .fs_write_begin(begin(&env.at("f9.txt"), None, true))
        .await
        .unwrap();
    // Left alone, every upload expires and takes its temporary file along.
    eventually("the idle uploads to be discarded", || {
        temp_files(&env.root).is_empty().then_some(())
    })
    .await;
    let error = refused(
        env.client
            .fs_write_chunk(hutan_protocol::fs::FsWriteChunk {
                upload_id: replaced.upload_id,
                seq: 0,
                text: "late".into(),
            })
            .await,
        ErrorCode::InvalidState,
    );
    assert!(error.to_string().contains("expired"), "{error}");
    assert!(!env.at("f9.txt").exists());

    // A connection that goes away takes its uploads with it.
    let leaving = env.t.connect().await;
    leaving
        .fs_write_begin(begin(&env.at("gone.txt"), None, true))
        .await
        .unwrap();
    assert_eq!(temp_files(&env.root).len(), 1);
    drop(leaving);
    eventually("the closed connection's upload to be discarded", || {
        temp_files(&env.root).is_empty().then_some(())
    })
    .await;
    env.stop().await;
}

// ---- listing, finding, searching --------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn listings_finder_and_search_answer_over_the_wire() {
    let env = Env::new().await;
    std::fs::create_dir_all(env.at(".git")).unwrap();
    std::fs::create_dir_all(env.at("src/deep")).unwrap();
    std::fs::create_dir_all(env.at("target")).unwrap();
    std::fs::write(env.at(".gitignore"), "target/\n").unwrap();
    std::fs::write(
        env.at("src/router.rs"),
        "fn route() {}\n// TODO(x): later\n",
    )
    .unwrap();
    std::fs::write(env.at("src/deep/registry.rs"), "fn register() {}\n").unwrap();
    std::fs::write(env.at("target/out.rs"), "fn route() {}\n").unwrap();
    std::fs::write(env.at("README.md"), "route everything\n").unwrap();
    let c = &env.client;

    let listing = c
        .fs_list(FsList {
            dir: env.root.clone(),
            ignore: true,
            limit: None,
            picked: false,
        })
        .await
        .unwrap();
    let shown: Vec<(&str, bool)> = listing
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e.ignored))
        .collect();
    assert_eq!(
        shown,
        [
            (".git", true),
            ("src", false),
            ("target", true),
            (".gitignore", false),
            ("README.md", false),
        ]
    );

    let found = c
        .fs_find(FsFind {
            root: env.root.clone(),
            query: "rout".into(),
            limit: Some(10),
        })
        .await
        .unwrap();
    assert_eq!(found.root, env.root);
    assert_eq!(found.matches.len(), 1, "{found:?}");
    assert_eq!(found.matches[0].path, "src/router.rs");
    assert_eq!(
        found.indexed, 4,
        "the index leaves out .git, target and ignored files"
    );

    // A search: hits in path order, one progress report at least, noise left out.
    let mut events = c.events();
    let result = c
        .fs_search(FsSearch {
            search_id: "s1".into(),
            root: env.root.clone(),
            pattern: "route".into(),
            regex: false,
            case_sensitive: None,
            globs: vec![],
            limit: None,
        })
        .await
        .unwrap();
    let places: Vec<(&str, u32)> = result
        .hits
        .iter()
        .map(|h| (h.path.as_str(), h.line))
        .collect();
    assert_eq!(places, [("README.md", 1), ("src/router.rs", 1)]);
    assert!(!result.truncated && !result.cancelled);
    let progress = wait_event(&mut events, "fs.search.progress", |event| match event {
        Event::FsSearchProgress {
            search_id,
            files_searched,
            hits,
        } if search_id == "s1" => Some((*files_searched, *hits)),
        _ => None,
    })
    .await;
    assert_eq!(progress, (result.files_searched, 2));
    // Cancelling what is not running is not an error.
    c.fs_search_cancel("s1").await.unwrap();
    let error = refused(
        c.fs_search(FsSearch {
            search_id: "bad".into(),
            root: env.root.clone(),
            pattern: "(".into(),
            regex: true,
            case_sensitive: None,
            globs: vec![],
            limit: None,
        })
        .await,
        ErrorCode::BadRequest,
    );
    assert!(error.to_string().contains("regular expression"), "{error}");
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_search_can_be_cancelled_and_searches_are_limited_per_connection() {
    let env = Env::new().await;
    // Enough files that the search is still going when the first progress report comes.
    for dir in 0..40 {
        std::fs::create_dir_all(env.at(&format!("d{dir}"))).unwrap();
        for file in 0..100 {
            std::fs::write(
                env.at(&format!("d{dir}/f{file}.txt")),
                "needle in a haystack\n",
            )
            .unwrap();
        }
    }
    let c = env.t.connect().await;
    let mut events = c.events();
    let searching = {
        let c = c.clone();
        let root = env.root.clone();
        tokio::spawn(async move {
            c.fs_search(FsSearch {
                search_id: "long".into(),
                root,
                pattern: "needle".into(),
                regex: false,
                case_sensitive: None,
                globs: vec![],
                limit: Some(1000),
            })
            .await
        })
    };
    // The connection reads on while the search runs: the cancel is answered.
    c.fs_search_cancel("long").await.unwrap();
    let result = timeout(WAIT, searching).await.unwrap().unwrap().unwrap();
    assert!(
        result.cancelled || result.truncated || result.files_searched == 4000,
        "the search ended neither cancelled nor complete: {result:?}"
    );
    assert!(result.hits.len() <= 1000);
    let _ = wait_event(&mut events, "a progress report", |event| {
        matches!(event, Event::FsSearchProgress { .. }).then_some(())
    })
    .await;

    // Names are unique among a connection's searches, and there are only so many at once.
    let mut running = Vec::new();
    for index in 0..SEARCHES_MAX + 1 {
        let c = c.clone();
        let root = env.root.clone();
        running.push(tokio::spawn(async move {
            c.fs_search(FsSearch {
                search_id: format!("s{index}"),
                root,
                pattern: "needle".into(),
                regex: false,
                case_sensitive: None,
                globs: vec![],
                limit: Some(1000),
            })
            .await
        }));
    }
    let mut refused_count = 0;
    for task in running {
        match task.await.unwrap() {
            Ok(_) => {}
            Err(ClientError::Remote(error)) => {
                assert_eq!(error.code, ErrorCode::InvalidState);
                refused_count += 1;
            }
            Err(other) => panic!("{other:?}"),
        }
    }
    // Searches that finished before the next one began leave room; the cap is a ceiling.
    assert!(refused_count <= 1, "{refused_count}");
    env.stop().await;
}

// ---- watching ---------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_clients_own_write_is_marked_self_and_everything_else_is_not() {
    let mut env = Env::new().await;
    let path = env.at("doc.txt");
    std::fs::write(&path, "v1").unwrap();
    let watcher = env.t.connect().await;
    let mut watcher_events = watcher.events();
    watch_file(&env.client, &path).await;
    watch_file(&watcher, &path).await;

    let version = env.client.fs_read_all(&path, false).await.unwrap().version;
    let written = env
        .client
        .fs_write(begin(&path, Some(version), false), "v2 by the editor")
        .await
        .unwrap();

    // The writer's own save carries its write id and the version it produced...
    let own = change_of(&mut env.events, &path, |change| {
        change.version == Some(written.version)
    })
    .await;
    assert_eq!(
        own.origin,
        Some(FsOrigin::Own {
            write_id: written.write_id.clone()
        })
    );
    // ...another client sees somebody else's change.
    let seen = change_of(&mut watcher_events, &path, |change| {
        change.version == Some(written.version)
    })
    .await;
    assert_eq!(seen.origin, Some(FsOrigin::External));

    // A change from outside, later, is the writer's news too.
    std::fs::write(&path, "v3 from a shell, longer").unwrap();
    let external = change_of(&mut env.events, &path, |change| {
        change.version.is_some_and(|v| v.size == 23)
    })
    .await;
    assert_eq!(external.origin, Some(FsOrigin::External));
    assert_eq!(external.kind, ChangeKind::Modified);

    // Deleting says so, with no version.
    std::fs::remove_file(&path).unwrap();
    let removed = change_of(&mut env.events, &path, |change| {
        change.kind == ChangeKind::Removed
    })
    .await;
    assert_eq!(
        (removed.version, removed.origin),
        (None, Some(FsOrigin::External))
    );

    // The generation of the root moved, at most once a second.
    let root = env.root.clone();
    let generation = wait_event(&mut env.events, "fs.generation", |event| match event {
        Event::FsGeneration {
            root: r,
            generation,
        } if *r == root => Some(*generation),
        _ => None,
    })
    .await;
    assert!(generation >= 1);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_come_in_bounded_events_and_overflow_says_when_some_were_dropped() {
    let mut env = Env::with("", |c| c.fs_limits.changes_max = 5).await;
    std::fs::create_dir_all(env.at("burst")).unwrap();
    let dir = env.at("burst");
    let watching = env
        .client
        .fs_watch(FsWatch {
            paths: vec![FsWatchPath {
                path: dir.clone(),
                recursive: false,
            }],
            picked: false,
        })
        .await
        .unwrap();
    assert_eq!(
        (watching.watching, watching.roots),
        (1, vec![env.root.clone()])
    );
    for index in 0..60 {
        std::fs::write(dir.join(format!("f{index:02}.txt")), "x").unwrap();
    }
    // One window held at least six of them, so one event is cut and says so; none is longer
    // than the limit. A path of the burst arrives in some event.
    let mut overflowed = false;
    let mut longest = 0;
    timeout(WAIT, async {
        while !overflowed {
            let (_, event) = env.events.recv().await.unwrap();
            if let Event::FsChanged { changes, overflow } = event {
                longest = longest.max(changes.len());
                assert!(changes.len() <= 5, "{} changes in one event", changes.len());
                overflowed |= overflow;
            }
        }
    })
    .await
    .expect("no event said it overflowed");
    assert!(longest >= 1);

    // Paths beyond 256 are refused, and unwatching makes room.
    let many: Vec<FsWatchPath> = (0..256)
        .map(|i| FsWatchPath {
            path: env.at(&format!("m{i}.txt")),
            recursive: false,
        })
        .collect();
    for path in &many {
        std::fs::write(&path.path, "x").unwrap();
    }
    let error = refused(
        env.client
            .fs_watch(FsWatch {
                paths: many,
                picked: false,
            })
            .await,
        ErrorCode::BadRequest,
    );
    assert!(error.to_string().contains("256"), "{error}");
    let after = env
        .client
        .fs_unwatch(FsUnwatch {
            paths: vec![dir.clone()],
        })
        .await
        .unwrap();
    assert_eq!(after.watching, 0);
    env.stop().await;
}

pub(crate) async fn report_working(
    t: &TestDaemon,
    session: SessionId,
    state: AgentState,
    seq: u64,
) {
    let client = t.connect().await;
    let applied: AgentApplied = client
        .call(Request::AgentReport(AgentReportRequest {
            session_id: session,
            source: "test-hooks".into(),
            agent: "fake".into(),
            state: Some(state),
            message: None,
            seq,
            agent_session_id: None,
            resume_argv: None,
            pid: None,
            node: None,
        }))
        .await
        .unwrap();
    assert!(applied.applied);
}

pub(crate) async fn shell_in(env: &Env) -> SessionInfo {
    let mut spec = sh("sleep 60");
    spec.cwd = Some(env.root.clone());
    create_in(&env.client, Some(env.workspace), spec).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_while_an_agent_works_in_the_folder_is_attributed_to_its_session() {
    let mut env = Env::new().await;
    let path = env.at("agent.txt");
    std::fs::write(&path, "0").unwrap();
    watch_file(&env.client, &path).await;
    let session = shell_in(&env).await;

    report_working(&env.t, session.id, AgentState::Working, 1).await;
    std::fs::write(&path, "1 by an agent").unwrap();
    let change = change_of(&mut env.events, &path, |c| {
        c.version.is_some_and(|v| v.size == 13)
    })
    .await;
    assert_eq!(
        change.origin,
        Some(FsOrigin::Agent {
            session_id: session.id,
            tool_call_id: None,
            confidence: AgentConfidence::Session,
        })
    );

    // Once the agent waits for the user again, a change is somebody else's.
    report_working(&env.t, session.id, AgentState::Idle, 2).await;
    std::fs::write(&path, "2 by the user, longer text").unwrap();
    let change = change_of(&mut env.events, &path, |c| {
        c.version.is_some_and(|v| v.size == 26)
    })
    .await;
    assert_eq!(change.origin, Some(FsOrigin::External));
    env.stop().await;
}

// ---- the agent's `file_diff` names the tool call -----------------------------------------------

fn acp_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-acp-fixture");
    assert!(
        bin.is_file(),
        "hutan-acp-fixture not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    bin
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_diff_in_a_chat_names_the_tool_call_of_the_change_that_follows() {
    let transcript = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/acp-omp-18.11/permission-allow.jsonl");
    let scratch = tempfile::tempdir().unwrap();
    let mut env = Env::start(
        |root| {
            // The recorded agent edits `/tmp/hutan-fixture/src/lib.rs`: the replay is told it
            // edits the file below this test's own folder instead.
            let replay = std::fs::read_to_string(&transcript)
                .unwrap()
                .replace("/tmp/hutan-fixture", &root.display().to_string());
            let replay_path = scratch.path().join("replay.jsonl");
            std::fs::write(&replay_path, replay).unwrap();
            format!(
                "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {bin:?}\nargs = [\"replay\", \"--received\", {received:?}, {replay_path:?}, \"acp\"]\n",
                bin = acp_bin(),
                received = scratch.path().join("received.jsonl"),
            )
        },
        |_| {},
    )
    .await;
    std::fs::create_dir_all(env.at("src")).unwrap();
    let lib = env.at("src/lib.rs");
    std::fs::write(&lib, "// before\n").unwrap();
    watch_file(&env.client, &lib).await;
    let (subscribed, mut chat) = env.t.connect_subscribed().await;

    let created: hutan_protocol::messages::SessionResult = subscribed
        .call(Request::ChatCreate(ChatCreate {
            extra_dirs: Vec::new(),
            contain: None,
            workspace_id: Some(env.workspace),
            cwd: env.root.clone(),
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
        }))
        .await
        .unwrap();
    let session = created.session.id;
    subscribed.chat_send(session, "edit lib.rs").await.unwrap();
    let approval: ApprovalInfo = wait_event(&mut chat, "approval.required", |event| match event {
        Event::ApprovalRequired { approval } => Some((**approval).clone()),
        _ => None,
    })
    .await;
    subscribed
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: approval.id,
            option_id: "allow_once".into(),
        }))
        .await
        .unwrap();
    // The journal's `file_diff` of the turn: which item, which file.
    let item = wait_event(
        &mut chat,
        "the file_diff of the turn",
        |event| match event {
            Event::ChatEvent { event, .. }
                if matches!(event.kind, AgentEventKind::FileDiff { .. }) =>
            {
                Some(event.item_id)
            }
            _ => None,
        },
    )
    .await;
    assert!(item.is_some());

    let after = "// after, by the tool call\n";
    std::fs::write(&lib, after).unwrap();
    let change = change_of(&mut env.events, &lib, |c| {
        c.version.is_some_and(|v| v.size == after.len() as u64)
    })
    .await;
    assert_eq!(
        change.origin,
        Some(FsOrigin::Agent {
            session_id: session,
            tool_call_id: item,
            confidence: AgentConfidence::ToolCall,
        })
    );
    env.stop().await;
}

// ---- an agent's write asks ---------------------------------------------------------------------

pub(crate) async fn connect_from(t: &TestDaemon, session: SessionId) -> Client {
    let mut options = t.options();
    options.client.origin = Some(Origin {
        session_id: session,
    });
    Client::connect(options).await.unwrap()
}

pub(crate) async fn next_approval(user: &Client) -> ApprovalInfo {
    timeout(WAIT, async {
        loop {
            let list: ApprovalList = user
                .call(Request::ApprovalList(ApprovalListRequest::default()))
                .await
                .unwrap();
            if let Some(info) = list.approvals.into_iter().next() {
                return info;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("no approval appeared")
}

pub(crate) async fn answer(user: &Client, approval: ApprovalId, option: &str) {
    user.call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
        approval_id: approval,
        option_id: option.to_owned(),
    }))
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agents_write_asks_once_and_its_reads_do_not() {
    let env = Env::with(
        "[control]\nagents = \"ask\"\napproval_timeout_secs = 20\n",
        |_| {},
    )
    .await;
    let path = env.at("notes.txt");
    std::fs::write(&path, "old").unwrap();
    std::fs::write(env.outside.join("secret.txt"), "outside").unwrap();
    let session = shell_in(&env).await;
    report_working(&env.t, session.id, AgentState::Working, 1).await;
    let agent = connect_from(&env.t, session.id).await;

    // Reading, listing, finding and watching are free; `picked` counts for nothing here.
    assert_eq!(agent.fs_read_all(&path, false).await.unwrap().text, "old");
    let mut picked = read_request(&env.outside.join("secret.txt"));
    picked.picked = true;
    refused(agent.fs_read(picked).await, ErrorCode::Forbidden);
    assert_eq!(
        agent
            .fs_list(FsList {
                dir: env.root.clone(),
                ignore: true,
                limit: None,
                picked: false,
            })
            .await
            .unwrap()
            .entries
            .len(),
        1
    );
    let pending: ApprovalList = env
        .client
        .call(Request::ApprovalList(ApprovalListRequest::default()))
        .await
        .unwrap();
    assert!(pending.approvals.is_empty());

    // A write waits for the user; one answer covers the chunks and the commit.
    let version = agent.fs_read_all(&path, false).await.unwrap().version;
    let writing = {
        let agent = agent.clone();
        let path = path.clone();
        tokio::spawn(async move {
            agent
                .fs_write(
                    begin(&path, Some(version), false),
                    &"new text\n".repeat(200_000),
                )
                .await
        })
    };
    let approval = next_approval(&env.client).await;
    assert!(
        approval.title.contains("overwrite the file") && approval.title.contains("notes.txt"),
        "{}",
        approval.title
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "old",
        "nothing before the answer"
    );
    answer(&env.client, approval.id, "allow_once").await;
    let written = timeout(WAIT, writing).await.unwrap().unwrap().unwrap();
    assert_eq!(written.version.size, 9 * 200_000);
    assert_eq!(std::fs::read_to_string(&path).unwrap().len(), 9 * 200_000);
    let pending: ApprovalList = env
        .client
        .call(Request::ApprovalList(ApprovalListRequest::default()))
        .await
        .unwrap();
    assert!(
        pending.approvals.is_empty(),
        "one approval for the whole write"
    );

    // Denied: the write fails as `forbidden` and the file stays.
    let denied = {
        let agent = agent.clone();
        let path = path.clone();
        tokio::spawn(async move { agent.fs_write(begin(&path, None, false), "denied").await })
    };
    let approval = next_approval(&env.client).await;
    answer(&env.client, approval.id, "deny").await;
    let error = refused(
        timeout(WAIT, denied).await.unwrap().unwrap(),
        ErrorCode::Forbidden,
    );
    assert!(
        error.to_string().contains("denied") || error.to_string().contains("refused"),
        "{error}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap().len(), 9 * 200_000);
    assert!(temp_files(&env.root).is_empty());

    // The user at a plain shell is never asked.
    env.client
        .fs_write(begin(&path, None, false), "user's own")
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "user's own");
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn under_read_only_an_agent_cannot_write_and_under_allow_it_is_not_asked() {
    for (policy, writes) in [("read_only", false), ("allow", true)] {
        let env = Env::with(&format!("[control]\nagents = \"{policy}\"\n"), |_| {}).await;
        let path = env.at("a.txt");
        std::fs::write(&path, "old").unwrap();
        let session = shell_in(&env).await;
        report_working(&env.t, session.id, AgentState::Working, 1).await;
        let agent = connect_from(&env.t, session.id).await;
        let result = agent.fs_write(begin(&path, None, false), "new").await;
        if writes {
            result.unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        } else {
            let error = refused(result, ErrorCode::Forbidden);
            assert!(error.to_string().contains("read-only"), "{error}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
        }
        // Reading is allowed under every policy.
        assert!(agent.fs_read_all(&path, false).await.is_ok());
        env.stop().await;
    }
}

// ---- no change is lost once a file is watched (FX-FSWATCH) -------------------------------------

async fn watch_picked(client: &Client, path: &Path) {
    client
        .fs_watch(FsWatch {
            paths: vec![FsWatchPath {
                path: path.to_owned(),
                recursive: false,
            }],
            picked: true,
        })
        .await
        .unwrap();
}

/// `text` of `size` bytes, so each write of a test is told apart by the version it produced.
fn text_of(size: usize) -> String {
    "x".repeat(size)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_right_after_fs_watch_is_reported_by_a_new_watcher_and_a_running_one() {
    let mut env = Env::new().await;
    // Outside the roots every directory is a watcher of its own, started by this watch; inside
    // a root the first watch starts the root's watcher and the others join it.
    for round in 0..20 {
        for path in [
            env.outside.join(format!("d{round}")).join("a.txt"),
            env.at(&format!("in{round}.txt")),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "before").unwrap();
            watch_picked(&env.client, &path).await;
            std::fs::write(&path, text_of(100 + round)).unwrap();
            let change = change_of(&mut env.events, &path, |c| {
                c.version.is_some_and(|v| v.size == 100 + round as u64)
            })
            .await;
            assert_eq!(change.origin, Some(FsOrigin::External));
        }
    }
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_between_a_read_and_the_watch_is_reported_and_an_own_write_is_not() {
    let mut env = Env::new().await;
    for (index, path) in [
        env.at("read-then-watch.txt"),
        env.outside.join("picked.txt"),
    ]
    .into_iter()
    .enumerate()
    {
        std::fs::write(&path, "as read").unwrap();
        let read = env.client.fs_read_all(&path, true).await.unwrap();
        // Somebody writes after the client read the file, before its watch is in place: the
        // client still learns of it.
        std::fs::write(&path, text_of(31)).unwrap();
        watch_picked(&env.client, &path).await;
        let change = change_of(&mut env.events, &path, |_| true).await;
        assert_eq!(change.kind, ChangeKind::Modified, "{index}");
        assert_ne!(change.version, Some(read.version));
        assert_eq!(change.version.map(|v| v.size), Some(31));
        assert_eq!(change.origin, Some(FsOrigin::External));
    }

    // The client's own write after its read moves what it knows: watching then reports
    // nothing until the next change of somebody else.
    let path = env.at("own.txt");
    std::fs::write(&path, "as read").unwrap();
    let read = env.client.fs_read_all(&path, false).await.unwrap();
    env.client
        .fs_write(begin(&path, Some(read.version), false), &text_of(12))
        .await
        .unwrap();
    // Its own change may still be in the window: it is reported as `self`, never as news.
    watch_file(&env.client, &path).await;
    std::fs::write(&path, text_of(44)).unwrap();
    let change = change_of(&mut env.events, &path, |c| {
        !matches!(c.origin, Some(FsOrigin::Own { .. }))
    })
    .await;
    assert_eq!(change.version.map(|v| v.size), Some(44));
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_are_reported_while_other_roots_and_sessions_are_added() {
    let mut env = Env::new().await;
    let path = env.at("busy.txt");
    std::fs::write(&path, "0").unwrap();
    watch_file(&env.client, &path).await;
    let other = env.t.connect().await;
    for round in 0..10usize {
        // Another connection watches a new directory (a watcher starts) while this file is
        // written...
        let dir = env.outside.join(format!("other{round}"));
        std::fs::create_dir_all(&dir).unwrap();
        let started = {
            let other = other.clone();
            let dir = dir.clone();
            tokio::spawn(async move { watch_picked(&other, &dir).await })
        };
        std::fs::write(&path, text_of(200 + round)).unwrap();
        started.await.unwrap();
        // (each write is waited for before the next: two in one window are one change)
        change_of(&mut env.events, &path, |c| {
            c.version.is_some_and(|v| v.size == 200 + round as u64)
        })
        .await;
        // ...then a session starts in the root and another workspace is opened.
        let added = {
            let (client, workspace, root) = (env.client.clone(), env.workspace, env.root.clone());
            tokio::spawn(async move {
                let mut spec = sh("sleep 60");
                spec.cwd = Some(root);
                let session = create_in(&client, Some(workspace), spec).await;
                let _: WorkspaceOpened = client
                    .call(Request::WorkspaceOpen(WorkspaceOpen {
                        path: dir,
                        created_by: None,
                    }))
                    .await
                    .unwrap();
                session
            })
        };
        std::fs::write(&path, text_of(300 + round)).unwrap();
        assert!(added.await.unwrap().state.is_live());
        change_of(&mut env.events, &path, |c| {
            c.version.is_some_and(|v| v.size == 300 + round as u64)
        })
        .await;
    }
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_in_place_write_and_an_atomic_rename_are_both_reported() {
    let mut env = Env::new().await;
    let path = env.at("both.txt");
    std::fs::write(&path, "0").unwrap();
    watch_file(&env.client, &path).await;

    // Truncated and written in place: same inode.
    let before = std::fs::metadata(&path).unwrap();
    std::fs::write(&path, text_of(10)).unwrap();
    let in_place = change_of(&mut env.events, &path, |c| {
        c.version.is_some_and(|v| v.size == 10)
    })
    .await;
    assert_eq!(in_place.kind, ChangeKind::Modified);
    assert_eq!(
        in_place.version.map(|v| v.ino),
        Some(std::os::unix::fs::MetadataExt::ino(&before))
    );

    // Written next to it and renamed over it, the way editors save: a new inode.
    let temporary = env.at(".both.txt.swp");
    std::fs::write(&temporary, text_of(20)).unwrap();
    std::fs::rename(&temporary, &path).unwrap();
    let renamed = change_of(&mut env.events, &path, |c| {
        c.version.is_some_and(|v| v.size == 20)
    })
    .await;
    assert_eq!(renamed.kind, ChangeKind::Modified);
    assert_ne!(
        renamed.version.map(|v| v.ino),
        in_place.version.map(|v| v.ino)
    );

    // A file new to the watched folder is created; written again later, it is modified.
    watch_file(&env.client, &env.root.clone()).await;
    let fresh = env.at("fresh.txt");
    std::fs::write(&fresh, text_of(30)).unwrap();
    let created = change_of(&mut env.events, &fresh, |_| true).await;
    assert_eq!(created.kind, ChangeKind::Created);
    std::fs::write(&fresh, text_of(31)).unwrap();
    let modified = change_of(&mut env.events, &fresh, |c| {
        c.version.is_some_and(|v| v.size == 31)
    })
    .await;
    assert_eq!(modified.kind, ChangeKind::Modified);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_watched_through_a_symlinked_directory_is_reported_by_its_canonical_path() {
    let mut env = Env::new().await;
    // On macOS the temporary directory is below `/var`, a symlink to `/private/var`; elsewhere
    // a symlink of the test's own stands in for it.
    let real = env.outside.join("real");
    std::fs::create_dir_all(&real).unwrap();
    let linked = env.outside.join("linked");
    std::os::unix::fs::symlink(&real, &linked).unwrap();
    let system = tempfile::tempdir().unwrap();
    let mut cases = vec![(linked.join("a.txt"), real.join("a.txt"))];
    if system.path() != system.path().canonicalize().unwrap() {
        // `/var/folders/…` as the user's tools write it, `/private/var/folders/…` on disk.
        cases.push((
            system.path().join("b.txt"),
            system.path().canonicalize().unwrap().join("b.txt"),
        ));
    }
    for (index, (asked, canonical)) in cases.into_iter().enumerate() {
        std::fs::write(&asked, "0").unwrap();
        let read = env.client.fs_read_all(&asked, true).await.unwrap();
        assert_eq!(read.path, canonical);
        watch_picked(&env.client, &asked).await;
        std::fs::write(&asked, text_of(50 + index)).unwrap();
        let change = change_of(&mut env.events, &canonical, |c| {
            c.version.is_some_and(|v| v.size == 50 + index as u64)
        })
        .await;
        assert_eq!(change.kind, ChangeKind::Modified);
        // Unwatching by the path as asked releases it.
        let after = env
            .client
            .fs_unwatch(FsUnwatch {
                paths: vec![asked.clone()],
            })
            .await
            .unwrap();
        assert_eq!(after.watching, 0, "{index}");
    }
    env.stop().await;
}
