//! The checkpoint snapshot engine (ADR 0019, slice CK-1) on git repositories the tests create
//! in temporary directories: what a snapshot captures and skips, and that the user's index,
//! HEAD, branches, stash, reflogs and status are the same before and after one.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

use hutan_daemon::checkpoints::{
    Caps, CheckpointError, Checkpoints, Config, Kind, Label, SkipReason, Snapshot, Unsupported,
};
use tempfile::TempDir;

// Fixtures ------------------------------------------------------------------------------------

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(["-c", "protocol.file.allow=always"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        // A read here must not refresh the user's index either.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .unwrap()
}

/// Runs git in `dir` without the user's configuration; returns stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = run(dir, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn git_bytes(dir: &Path, args: &[&str]) -> Vec<u8> {
    let output = run(dir, args);
    assert!(output.status.success(), "git {args:?}");
    output.stdout
}

fn write(root: &Path, path: &str, content: impl AsRef<[u8]>) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// A temporary directory with a repository `repo` on `main` (one commit of `a.txt`, `b.txt`,
/// `.gitignore` ignoring `*.log` and `target/`) and Hutan's store `store`.
struct Fixture {
    dir: TempDir,
    root: PathBuf,
    store: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self::empty();
        write(&fixture.root, "a.txt", "alpha\n");
        write(&fixture.root, "b.txt", "bravo\n");
        write(&fixture.root, ".gitignore", "*.log\ntarget/\n");
        git(&fixture.root, &["add", "."]);
        git(&fixture.root, &["commit", "-q", "-m", "init"]);
        fixture
    }

    /// `git init` only: unborn `main`, no index.
    fn empty() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        let root = base.join("repo");
        git(&base, &["init", "-q", "-b", "main", "repo"]);
        Self {
            store: base.join("store"),
            root,
            dir,
        }
    }

    fn engine(&self) -> Checkpoints {
        self.engine_with(Caps::default())
    }

    fn engine_with(&self, caps: Caps) -> Checkpoints {
        let mut config = Config::new(self.store.clone(), None);
        config.caps = caps;
        config.git_env = BTreeMap::from([
            ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
            ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
        ]);
        Checkpoints::new(config)
    }

    fn path(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }
}

fn label(turn: &str, kind: Kind) -> Label {
    Label::new("01SESSION", turn, kind)
}

/// `path → (mode, blob id)` of a tree.
fn tree(root: &Path, tree: &str) -> BTreeMap<String, (String, String)> {
    git(root, &["ls-tree", "-r", "-z", tree])
        .split('\0')
        .filter(|l| !l.is_empty())
        .map(|line| {
            let (head, path) = line.split_once('\t').unwrap();
            let mut fields = head.split(' ');
            let mode = fields.next().unwrap().to_owned();
            let _kind = fields.next();
            let oid = fields.next().unwrap().to_owned();
            (path.to_owned(), (mode, oid))
        })
        .collect()
}

fn blob(root: &Path, files: &BTreeMap<String, (String, String)>, path: &str) -> Vec<u8> {
    let (_, oid) = files
        .get(path)
        .unwrap_or_else(|| panic!("{path} not in {:?}", files.keys()));
    git_bytes(root, &["cat-file", "blob", oid])
}

fn has_object(root: &Path, oid: &str) -> bool {
    run(root, &["cat-file", "-e", oid]).status.success()
}

fn skipped(snapshot: &Snapshot) -> BTreeMap<String, SkipReason> {
    snapshot
        .skipped
        .iter()
        .map(|s| (s.path.to_string_lossy().into_owned(), s.reason.clone()))
        .collect()
}

fn loose_objects(root: &Path) -> u64 {
    let counts = git(root, &["count-objects", "-v"]);
    counts
        .lines()
        .find_map(|l| l.strip_prefix("count: "))
        .unwrap()
        .parse()
        .unwrap()
}

/// Everything of the user's that a snapshot must leave alone.
#[derive(Debug, PartialEq, Eq)]
struct UserState {
    index: Option<(Vec<u8>, SystemTime)>,
    head_file: Vec<u8>,
    head: String,
    refs: String,
    stash: String,
    status: String,
    reflogs: BTreeMap<PathBuf, Vec<u8>>,
}

fn user_state(root: &Path) -> UserState {
    let git_dir = PathBuf::from(git(root, &["rev-parse", "--absolute-git-dir"]).trim());
    let common_dir = PathBuf::from(
        git(
            root,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .trim(),
    );
    let index = git_dir.join("index");
    let index = fs::read(&index)
        .ok()
        .map(|bytes| (bytes, fs::metadata(&index).unwrap().modified().unwrap()));
    let refs = git(root, &["for-each-ref", "--format=%(refname) %(objectname)"])
        .lines()
        .filter(|l| !l.starts_with("refs/hutan/"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut reflogs = BTreeMap::new();
    for logs in [git_dir.join("logs"), common_dir.join("logs")] {
        collect_files(&logs, &mut reflogs);
    }
    UserState {
        index,
        head_file: fs::read(git_dir.join("HEAD")).unwrap(),
        head: String::from_utf8_lossy(&run(root, &["rev-parse", "-q", "--verify", "HEAD"]).stdout)
            .into_owned(),
        refs,
        stash: git(root, &["stash", "list"]),
        status: git(
            root,
            &["status", "--porcelain=v2", "-uall", "--ignored", "-z"],
        ),
        reflogs,
    }
}

fn collect_files(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.insert(path.clone(), fs::read(&path).unwrap());
        }
    }
}

// Invariants ----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn user_state_is_untouched_and_staged_unstaged_untracked_are_captured_under_a_held_index_lock()
 {
    let fx = Fixture::new();
    let root = &fx.root;
    write(root, "c.txt", "charlie\n");
    git(root, &["add", "c.txt"]);
    git(root, &["commit", "-q", "-m", "c"]);
    // A stash entry, then staged + unstaged + untracked + deleted + ignored.
    write(root, "b.txt", "stashed\n");
    git(root, &["stash", "-q"]);
    write(root, "a.txt", "alpha staged\n");
    git(root, &["add", "a.txt"]);
    write(root, "a.txt", "alpha staged then edited\n");
    write(root, "new-staged.txt", "staged new\n");
    git(root, &["add", "new-staged.txt"]);
    write(root, "dir/untracked.txt", "untracked\n");
    fs::remove_file(fx.path("c.txt")).unwrap();
    write(root, "build.log", "ignored\n");
    write(root, "target/out.bin", "ignored\n");
    std::os::unix::fs::symlink("a.txt", fx.path("link")).unwrap();
    // Another process is in the middle of writing the index.
    write(root, ".git/index.lock", "someone else's lock");

    let before = user_state(root);
    let engine = fx.engine();
    let start = engine
        .snapshot(root, label("1", Kind::Start))
        .await
        .unwrap();
    assert!(start.seeded);
    assert_eq!(user_state(root), before);
    assert_eq!(
        fs::read(fx.path(".git/index.lock")).unwrap(),
        b"someone else's lock"
    );

    let files = tree(root, &start.tree);
    assert_eq!(blob(root, &files, "a.txt"), b"alpha staged then edited\n");
    assert_eq!(blob(root, &files, "b.txt"), b"bravo\n");
    assert_eq!(blob(root, &files, "new-staged.txt"), b"staged new\n");
    assert_eq!(blob(root, &files, "dir/untracked.txt"), b"untracked\n");
    assert_eq!(files["link"].0, "120000");
    assert_eq!(blob(root, &files, "link"), b"a.txt");
    assert!(!files.contains_key("c.txt"));
    assert!(
        !files
            .keys()
            .any(|p| p.ends_with(".log") || p.starts_with("target/"))
    );
    assert!(start.skipped.is_empty(), "{:?}", start.skipped);
    let start_commit = start.commit.clone().unwrap();
    assert_eq!(
        start.reference.as_deref(),
        Some("refs/hutan/checkpoints/01SESSION/1-start")
    );
    assert_eq!(
        git(
            root,
            &["rev-parse", "refs/hutan/checkpoints/01SESSION/1-start"]
        )
        .trim(),
        start_commit
    );
    let message = git(root, &["cat-file", "commit", &start_commit]);
    assert!(
        message.contains("\nauthor Hutan <hutan@localhost> "),
        "{message}"
    );
    assert!(message.contains("Hutan-Session: 01SESSION\nHutan-Turn: 1\nHutan-Kind: start\n"));

    // The turn edits a file; the end snapshot follows the start one and leaves the user's
    // state alone as well.
    write(root, "dir/untracked.txt", "edited by the agent\n");
    let before = user_state(root);
    let end = engine
        .snapshot(root, label("1", Kind::End).with_parent(&start_commit))
        .await
        .unwrap();
    assert_eq!(user_state(root), before);
    assert!(!end.seeded);
    assert_eq!(end.files_hashed, 1);
    let end_commit = end.commit.clone().unwrap();
    assert_eq!(
        end.reference.as_deref(),
        Some("refs/hutan/checkpoints/01SESSION/1")
    );
    assert_eq!(
        git(root, &["rev-parse", &format!("{end_commit}^")]).trim(),
        start_commit
    );
    let files = tree(root, &end.tree);
    assert_eq!(
        blob(root, &files, "dir/untracked.txt"),
        b"edited by the agent\n"
    );
    assert_eq!(blob(root, &files, "a.txt"), b"alpha staged then edited\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn ignored_files_are_never_read() {
    let fx = Fixture::new();
    let root = &fx.root;
    write(root, "secret.log", "never read");
    chmod(&fx.path("secret.log"), 0o000);
    write(root, "target/debug/out", "never read");
    chmod(&fx.path("target/debug/out"), 0o000);
    write(root, "notes.txt", "captured, then ignored");

    let engine = fx.engine();
    let first = engine
        .snapshot(root, label("1", Kind::Start))
        .await
        .unwrap();
    let files = tree(root, &first.tree);
    assert!(files.contains_key("notes.txt"));
    assert!(!files.contains_key("secret.log"));

    // Captured once, then ignored and unreadable: the next snapshot drops it without reading.
    write(root, ".gitignore", "*.log\ntarget/\nnotes.txt\n");
    write(root, "notes.txt", "changed after it was ignored");
    chmod(&fx.path("notes.txt"), 0o000);
    let second = engine.snapshot(root, label("1", Kind::End)).await;
    for path in ["secret.log", "target/debug/out", "notes.txt"] {
        chmod(&fx.path(path), 0o644);
    }
    let second = second.unwrap();
    let files = tree(root, &second.tree);
    assert!(!files.contains_key("notes.txt"));
    assert!(
        !files
            .keys()
            .any(|p| p.ends_with(".log") || p.starts_with("target/"))
    );
    assert!(second.skipped.is_empty(), "{:?}", second.skipped);
    assert_eq!(second.files_removed, 1);

    // The same through `.git/info/exclude` alone (no `.gitignore` changes), then through a new
    // `.gitignore` in a subdirectory alone.
    write(root, "kept.txt", "captured");
    write(root, "sub/inner.txt", "captured");
    let third = engine
        .snapshot(root, label("2", Kind::Start))
        .await
        .unwrap();
    let files = tree(root, &third.tree);
    assert!(files.contains_key("kept.txt") && files.contains_key("sub/inner.txt"));
    let mut exclude = fs::read_to_string(fx.path(".git/info/exclude")).unwrap();
    exclude.push_str("kept.txt\n");
    write(root, ".git/info/exclude", exclude);
    write(root, "kept.txt", "changed after it was ignored");
    chmod(&fx.path("kept.txt"), 0o000);
    let fourth = engine.snapshot(root, label("2", Kind::End)).await.unwrap();
    // Dropped as ignored, not met as an unreadable file.
    assert!(fourth.skipped.is_empty(), "{:?}", fourth.skipped);
    let files = tree(root, &fourth.tree);
    assert!(!files.contains_key("kept.txt"));
    assert!(files.contains_key("sub/inner.txt"));

    write(root, "sub/.gitignore", "inner.txt\n");
    write(root, "sub/inner.txt", "changed after it was ignored");
    chmod(&fx.path("sub/inner.txt"), 0o000);
    let fifth = engine.snapshot(root, label("3", Kind::Start)).await;
    for path in ["kept.txt", "sub/inner.txt"] {
        chmod(&fx.path(path), 0o644);
    }
    let fifth = fifth.unwrap();
    assert!(fifth.skipped.is_empty(), "{:?}", fifth.skipped);
    let files = tree(root, &fifth.tree);
    assert!(!files.contains_key("sub/inner.txt"));
    assert!(files.contains_key("sub/.gitignore"));

    // A tool's cache directory that ignores itself (`.gitignore` = `*`): the new `.gitignore`
    // is in no list, yet a captured file there that changes is not read again.
    write(root, "cache/data.txt", "captured");
    let sixth = engine
        .snapshot(root, label("4", Kind::Start))
        .await
        .unwrap();
    assert!(tree(root, &sixth.tree).contains_key("cache/data.txt"));
    write(root, "cache/.gitignore", "*\n");
    write(root, "cache/data.txt", "changed after it was ignored");
    chmod(&fx.path("cache/data.txt"), 0o000);
    let seventh = engine.snapshot(root, label("4", Kind::End)).await;
    chmod(&fx.path("cache/data.txt"), 0o644);
    let seventh = seventh.unwrap();
    assert!(seventh.skipped.is_empty(), "{:?}", seventh.skipped);
    let files = tree(root, &seventh.tree);
    assert!(!files.keys().any(|p| p.starts_with("cache/")), "{files:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_repositories_and_submodules_are_skipped_and_listed() {
    let fx = Fixture::new();
    let root = &fx.root;
    let base = fx.dir.path().canonicalize().unwrap();
    git(&base, &["init", "-q", "-b", "main", "lib"]);
    write(&base.join("lib"), "lib.rs", "pub fn f() {}\n");
    git(&base.join("lib"), &["add", "."]);
    git(&base.join("lib"), &["commit", "-q", "-m", "lib"]);
    git(root, &["submodule", "add", "-q", "../lib", "vendor/lib"]);
    git(root, &["commit", "-q", "-m", "submodule"]);
    git(root, &["init", "-q", "nested"]);
    write(root, "nested/inner.txt", "inside a nested repository\n");

    let before = user_state(root);
    let snapshot = fx
        .engine()
        .snapshot(root, label("1", Kind::Start))
        .await
        .unwrap();
    assert_eq!(user_state(root), before);
    let files = tree(root, &snapshot.tree);
    assert!(
        files.values().all(|(mode, _)| mode != "160000"),
        "{files:?}"
    );
    assert!(
        !files
            .keys()
            .any(|p| p.starts_with("nested/") || p.starts_with("vendor/lib"))
    );
    assert!(files.contains_key(".gitmodules"));
    assert_eq!(
        skipped(&snapshot),
        BTreeMap::from([
            ("nested".to_owned(), SkipReason::NestedRepository),
            ("vendor/lib".to_owned(), SkipReason::NestedRepository),
        ])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn files_over_the_caps_are_skipped_before_anything_is_hashed() {
    let fx = Fixture::new();
    let root = &fx.root;
    write(root, "big-tracked.bin", "small at first\n");
    git(root, &["add", "big-tracked.bin"]);
    git(root, &["commit", "-q", "-m", "big"]);
    let engine = fx.engine_with(Caps {
        max_file_bytes: 1024 * 1024,
        ..Caps::default()
    });
    let first = engine
        .snapshot(root, label("1", Kind::Start))
        .await
        .unwrap();
    assert!(tree(root, &first.tree).contains_key("big-tracked.bin"));

    // Over 1 MiB, random-ish so no object with this content can exist yet.
    let big: Vec<u8> = (0..1_200_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    write(root, "big-tracked.bin", &big);
    write(root, "big-untracked.bin", &big[100..]);
    write(root, ".env.local", "TOKEN=1");
    write(root, "certs/server.pem", "-----BEGIN-----");
    let fifo = fx.path("fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    git(root, &["add", "-f", ".env.local"]);
    write(root, ".env.local", "TOKEN=2");
    let oids: Vec<String> = [
        "big-tracked.bin",
        "big-untracked.bin",
        ".env.local",
        "certs/server.pem",
    ]
    .iter()
    .map(|p| {
        git(root, &["hash-object", "--no-filters", p])
            .trim()
            .to_owned()
    })
    .collect();

    let second = engine.snapshot(root, label("1", Kind::End)).await.unwrap();
    assert_eq!(
        skipped(&second),
        BTreeMap::from([
            (".env.local".to_owned(), SkipReason::SecretName),
            ("big-tracked.bin".to_owned(), SkipReason::TooLarge),
            ("big-untracked.bin".to_owned(), SkipReason::TooLarge),
            ("certs/server.pem".to_owned(), SkipReason::SecretName),
        ])
    );
    let big_skip = second
        .skipped
        .iter()
        .find(|s| s.path == Path::new("big-tracked.bin"))
        .unwrap();
    assert_eq!(big_skip.bytes, Some(1_200_000));
    for oid in &oids {
        assert!(!has_object(root, oid), "object {oid} was written");
    }
    let files = tree(root, &second.tree);
    // A skipped path is left out of the tree rather than recorded with stale content.
    for path in [
        "big-tracked.bin",
        "big-untracked.bin",
        ".env.local",
        "certs/server.pem",
        "fifo",
    ] {
        assert!(!files.contains_key(path), "{path}");
    }
    assert_eq!(second.files_hashed, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_byte_and_file_budgets_skip_the_largest_files_first() {
    let fx = Fixture::new();
    let root = &fx.root;
    write(root, "s1.txt", "1");
    write(root, "s2.txt", "22");
    write(root, "m.txt", "x".repeat(300));
    write(root, "l.txt", "y".repeat(600));
    // a.txt (6 bytes) and b.txt (6 bytes) are tracked and unchanged: they cost nothing.
    let engine = fx.engine_with(Caps {
        snapshot_budget_bytes: 400,
        max_files: 2,
        ..Caps::default()
    });
    let snapshot = engine
        .snapshot(root, label("1", Kind::Start))
        .await
        .unwrap();
    assert_eq!(
        skipped(&snapshot),
        BTreeMap::from([
            ("l.txt".to_owned(), SkipReason::TooManyFiles),
            ("m.txt".to_owned(), SkipReason::TooManyFiles),
        ])
    );
    let engine = fx.engine_with(Caps {
        snapshot_budget_bytes: 400,
        ..Caps::default()
    });
    let snapshot = engine
        .snapshot(root, label("2", Kind::Start))
        .await
        .unwrap();
    assert_eq!(
        skipped(&snapshot),
        BTreeMap::from([("l.txt".to_owned(), SkipReason::OverBudget)])
    );
    let files = tree(root, &snapshot.tree);
    for path in ["a.txt", "b.txt", "s1.txt", "s2.txt", "m.txt"] {
        assert!(files.contains_key(path), "{path}");
    }
    assert_eq!(snapshot.files_hashed, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_snapshot_of_an_unchanged_tree_reuses_its_objects() {
    let fx = Fixture::new();
    let root = &fx.root;
    write(root, "new.txt", "new\n");
    let engine = fx.engine();
    let first = engine
        .snapshot(root, label("1", Kind::Start))
        .await
        .unwrap();
    let objects = loose_objects(root);
    let second = engine.snapshot(root, label("1", Kind::End)).await.unwrap();
    assert_eq!(second.tree, first.tree);
    assert_eq!(second.files_hashed, 0);
    assert_ne!(second.commit, first.commit);
    // Only the new commit object.
    assert_eq!(loose_objects(root), objects + 1);

    // An end that changed nothing since its start, and a warm-up, write no object and no ref.
    let refs = git(root, &["for-each-ref", "refs/hutan/"]);
    let third = engine
        .snapshot(
            root,
            label("2", Kind::End)
                .with_parent(first.commit.as_deref().unwrap())
                .unless_tree(&first.tree),
        )
        .await
        .unwrap();
    assert_eq!(third.tree, first.tree);
    assert_eq!((third.commit, third.reference), (None, None));
    let warm = engine
        .snapshot(root, Label::warm("01SESSION"))
        .await
        .unwrap();
    assert_eq!((warm.commit, warm.reference), (None, None));
    assert_eq!(loose_objects(root), objects + 1);
    assert_eq!(git(root, &["for-each-ref", "refs/hutan/"]), refs);

    // An end that changed something is written as usual.
    write(root, "new.txt", "changed\n");
    let fourth = engine
        .snapshot(
            root,
            label("2", Kind::End)
                .with_parent(first.commit.as_deref().unwrap())
                .unless_tree(&first.tree),
        )
        .await
        .unwrap();
    assert_ne!(fourth.tree, first.tree);
    assert_eq!(
        fourth.reference.as_deref(),
        Some("refs/hutan/checkpoints/01SESSION/2")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_snapshots_of_one_checkout_run_one_after_the_other() {
    let fx = Fixture::new();
    let root = &fx.root;
    for i in 0..50 {
        write(root, &format!("src/f{i}.txt"), format!("file {i}\n"));
    }
    let engine = fx.engine();
    let (a, b) = tokio::join!(
        engine.snapshot(root, label("1", Kind::Start)),
        engine.snapshot(root, label("2", Kind::Start)),
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert!(
        a.finished <= b.started || b.finished <= a.started,
        "snapshots overlapped"
    );
    assert_eq!(a.tree, b.tree);
    assert_ne!(a.reference, b.reference);
    assert_eq!(a.files_hashed + b.files_hashed, 50);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unborn_branch_seeds_from_the_users_index_or_starts_empty() {
    let fx = Fixture::empty();
    let root = &fx.root;
    write(root, "first.txt", "untracked on an unborn branch\n");
    let before = user_state(root);
    assert!(before.index.is_none());
    let snapshot = fx
        .engine()
        .snapshot(root, label("1", Kind::Start))
        .await
        .unwrap();
    assert_eq!(user_state(root), before);
    assert_eq!(tree(root, &snapshot.tree).len(), 1);

    let fx = Fixture::empty();
    let root = &fx.root;
    write(root, "staged.txt", "staged on an unborn branch\n");
    git(root, &["add", "staged.txt"]);
    write(root, "staged.txt", "then edited\n");
    let before = user_state(root);
    let snapshot = fx
        .engine()
        .snapshot(root, label("1", Kind::Start))
        .await
        .unwrap();
    assert_eq!(user_state(root), before);
    let files = tree(root, &snapshot.tree);
    assert_eq!(blob(root, &files, "staged.txt"), b"then edited\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_index_with_conflicts_seeds_from_head() {
    let fx = Fixture::new();
    let root = &fx.root;
    git(root, &["checkout", "-q", "-b", "other"]);
    write(root, "a.txt", "theirs\n");
    git(root, &["commit", "-q", "-am", "theirs"]);
    git(root, &["checkout", "-q", "main"]);
    write(root, "a.txt", "ours\n");
    git(root, &["commit", "-q", "-am", "ours"]);
    assert!(!run(root, &["merge", "-q", "other"]).status.success());
    assert!(!git(root, &["ls-files", "-u"]).is_empty());

    let before = user_state(root);
    let snapshot = fx
        .engine()
        .snapshot(root, label("1", Kind::Start))
        .await
        .unwrap();
    assert_eq!(user_state(root), before);
    let files = tree(root, &snapshot.tree);
    let a = String::from_utf8(blob(root, &files, "a.txt")).unwrap();
    assert!(
        a.contains("<<<<<<<") && a.contains("theirs") && a.contains("ours"),
        "{a}"
    );
    assert!(files.contains_key("b.txt"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_linked_worktree_has_its_own_index_and_shares_the_refs() {
    let fx = Fixture::new();
    let root = &fx.root;
    let worktree = fx.dir.path().canonicalize().unwrap().join("wt");
    git(
        root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            worktree.to_str().unwrap(),
        ],
    );
    write(&worktree, "a.txt", "edited in the worktree\n");
    write(&worktree, "sub/wt-only.txt", "new in the worktree\n");

    let main_before = user_state(root);
    let wt_before = user_state(&worktree);
    let engine = fx.engine();
    let in_wt = engine
        // From a directory inside it: the snapshot is of the whole checkout.
        .snapshot(&worktree.join("sub"), label("1", Kind::Start))
        .await
        .unwrap();
    let in_main = engine
        .snapshot(root, label("2", Kind::Start))
        .await
        .unwrap();
    assert_eq!(user_state(root), main_before);
    assert_eq!(user_state(&worktree), wt_before);

    assert_eq!(in_wt.checkout, worktree);
    assert_eq!(in_wt.common_dir, in_main.common_dir);
    let wt_files = tree(root, &in_wt.tree);
    assert_eq!(blob(root, &wt_files, "a.txt"), b"edited in the worktree\n");
    assert!(wt_files.contains_key("sub/wt-only.txt"));
    let main_files = tree(root, &in_main.tree);
    assert_eq!(blob(root, &main_files, "a.txt"), b"alpha\n");
    // Both refs are in the one ref store.
    let refs = git(
        root,
        &["for-each-ref", "--format=%(refname)", "refs/hutan/"],
    );
    assert_eq!(
        refs,
        "refs/hutan/checkpoints/01SESSION/1-start\nrefs/hutan/checkpoints/01SESSION/2-start\n"
    );
    let indexes: Vec<_> = fs::read_dir(&fx.store)
        .unwrap()
        .flat_map(|repo| fs::read_dir(repo.unwrap().path()).unwrap())
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(indexes.len(), 2, "{indexes:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn hooks_and_hostile_configuration_are_ignored() {
    let fx = Fixture::new();
    let root = &fx.root;
    let marker = fx.dir.path().join("hook-ran");
    let hook = format!("#!/bin/sh\necho \"$0\" >> '{}'\n", marker.display());
    let hooks = fx.dir.path().join("hooks");
    for dir in [hooks.clone(), fx.path(".git/hooks")] {
        for name in [
            "reference-transaction",
            "post-commit",
            "post-index-change",
            "pre-commit",
        ] {
            write(&dir, name, &hook);
            chmod(&dir.join(name), 0o755);
        }
    }
    for (key, value) in [
        ("core.hooksPath", hooks.to_str().unwrap()),
        ("commit.gpgSign", "true"),
        ("gpg.program", "false"),
        ("core.logAllRefUpdates", "always"),
        ("user.name", "Someone Else"),
        ("core.fsmonitor", "true"),
        ("core.splitIndex", "true"),
    ] {
        git(root, &["config", key, value]);
    }
    write(root, "x.txt", "x\n");

    let snapshot = fx
        .engine()
        .snapshot(root, label("1", Kind::Start))
        .await
        .unwrap();
    assert!(
        !marker.exists(),
        "a hook ran: {}",
        fs::read_to_string(&marker).unwrap()
    );
    let commit = git(
        root,
        &["cat-file", "commit", snapshot.commit.as_deref().unwrap()],
    );
    assert!(!commit.contains("gpgsig"), "{commit}");
    assert!(
        commit.contains("\ncommitter Hutan <hutan@localhost> "),
        "{commit}"
    );
    assert!(!fx.path(".git/logs/refs/hutan").exists());
    assert!(fs::read_dir(fx.path(".git")).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("sharedindex")
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn content_is_stored_as_raw_bytes_and_filter_drivers_never_run() {
    let fx = Fixture::new();
    let root = &fx.root;
    write(
        root,
        ".gitattributes",
        "*.txt text=auto\n*.id ident\n*.bin filter=lfs\n",
    );
    write(root, "crlf.txt", "one\r\ntwo\r\n");
    write(root, "file.id", "$Id$\n");
    write(root, "asset.bin", "raw asset\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "attributes"]);
    // The user's index holds the converted blob; the file on disk keeps its CRLF.
    assert_eq!(
        git_bytes(root, &["cat-file", "blob", ":crlf.txt"]),
        b"one\ntwo\n"
    );
    // A filter driver that leaves a trace if it ever runs.
    let marker = fx.dir.path().join("filter-ran");
    let filter = format!("sh -c 'touch {}; cat'", marker.display());
    git(root, &["config", "filter.lfs.clean", &filter]);
    git(root, &["config", "filter.lfs.smudge", &filter]);
    git(root, &["config", "filter.lfs.process", &filter]);
    write(root, "new.txt", "three\r\nfour\r\n");
    write(root, "asset.bin", "changed asset\n");
    write(root, "other.bin", "untracked asset\n");

    let engine = fx.engine();
    let start = engine
        .snapshot(root, label("1", Kind::Start))
        .await
        .unwrap();
    let files = tree(root, &start.tree);
    assert_eq!(blob(root, &files, "crlf.txt"), b"one\r\ntwo\r\n");
    assert_eq!(blob(root, &files, "new.txt"), b"three\r\nfour\r\n");
    assert_eq!(blob(root, &files, "file.id"), b"$Id$\n");
    assert!(!files.contains_key("asset.bin") && !files.contains_key("other.bin"));
    let driver = SkipReason::Filter {
        driver: "lfs".to_owned(),
    };
    assert_eq!(
        skipped(&start),
        BTreeMap::from([
            ("asset.bin".to_owned(), driver.clone()),
            ("other.bin".to_owned(), driver),
        ])
    );
    // Stat-dirty but the same bytes: nothing to hash, and still raw.
    let crlf = fx.path("crlf.txt");
    let file = fs::File::options().append(true).open(&crlf).unwrap();
    file.set_modified(SystemTime::now()).unwrap();
    drop(file);
    let end = engine.snapshot(root, label("1", Kind::End)).await.unwrap();
    assert_eq!(end.tree, start.tree);
    assert!(!marker.exists(), "a filter driver ran");
}

// Switch-off and unsupported --------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn low_disk_switches_the_checkout_off_before_anything_is_written() {
    let fx = Fixture::new();
    let root = &fx.root;
    write(root, "x.txt", "x\n");
    let objects = loose_objects(root);
    let engine = fx.engine_with(Caps {
        min_free_bytes: u64::MAX,
        ..Caps::default()
    });
    let error = engine
        .snapshot(root, label("1", Kind::Start))
        .await
        .unwrap_err();
    assert!(matches!(error, CheckpointError::LowDisk { .. }), "{error}");
    let error = engine
        .snapshot(root, label("2", Kind::Start))
        .await
        .unwrap_err();
    assert!(
        matches!(error, CheckpointError::SwitchedOff { .. }),
        "{error}"
    );
    assert!(
        engine
            .switched_off(root)
            .await
            .unwrap()
            .unwrap()
            .contains("free")
    );
    assert_eq!(loose_objects(root), objects);
    assert!(!fx.store.exists());
    assert!(git(root, &["for-each-ref", "refs/hutan/"]).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn three_failures_in_a_row_switch_the_checkout_off_until_switched_on() {
    let fx = Fixture::new();
    let root = &fx.root;
    // Someone holds the ref's lock: `update-ref` fails.
    write(
        root,
        ".git/refs/hutan/checkpoints/01SESSION/1-start.lock",
        "",
    );
    let engine = fx.engine();
    for _ in 0..3 {
        let error = engine
            .snapshot(root, label("1", Kind::Start))
            .await
            .unwrap_err();
        assert!(
            matches!(&error, CheckpointError::GitFailed { step, .. } if step == "update-ref"),
            "{error}"
        );
    }
    let error = engine
        .snapshot(root, label("1", Kind::Start))
        .await
        .unwrap_err();
    assert!(
        matches!(error, CheckpointError::SwitchedOff { .. }),
        "{error}"
    );
    let reason = engine.switched_off(root).await.unwrap().unwrap();
    assert!(reason.contains("3 snapshots in a row failed"), "{reason}");

    fs::remove_file(fx.path(".git/refs/hutan/checkpoints/01SESSION/1-start.lock")).unwrap();
    engine.switch_on(root).await.unwrap();
    assert_eq!(engine.switched_off(root).await.unwrap(), None);
    engine
        .snapshot(root, label("1", Kind::Start))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn unsupported_checkouts_are_refused_with_a_reason() {
    let fx = Fixture::new();
    let plain = fx.dir.path().join("plain");
    fs::create_dir(&plain).unwrap();
    let engine = fx.engine();
    let error = engine
        .snapshot(&plain, label("1", Kind::Start))
        .await
        .unwrap_err();
    assert!(
        matches!(error, CheckpointError::NotARepository { .. }),
        "{error}"
    );

    let base = fx.dir.path().canonicalize().unwrap();
    git(&base, &["init", "-q", "--bare", "bare.git"]);
    let error = engine
        .snapshot(&base.join("bare.git"), label("1", Kind::Start))
        .await
        .unwrap_err();
    assert!(
        matches!(error, CheckpointError::Unsupported(Unsupported::NoWorkTree)),
        "{error}"
    );

    let mut config = Config::new(fx.store.clone(), Some(fx.root.clone()));
    config.git_env = BTreeMap::from([("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned())]);
    let error = Checkpoints::new(config)
        .snapshot(&fx.root, label("1", Kind::Start))
        .await
        .unwrap_err();
    assert!(
        matches!(error, CheckpointError::Unsupported(Unsupported::Root(_))),
        "{error}"
    );

    for i in 0..4 {
        write(&fx.root, &format!("u{i}.txt"), "u");
    }
    let objects = loose_objects(&fx.root);
    let error = fx
        .engine_with(Caps {
            max_untracked_files: 3,
            ..Caps::default()
        })
        .snapshot(&fx.root, label("1", Kind::Start))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            CheckpointError::Unsupported(Unsupported::TooManyUntracked { count: 4, limit: 3 })
        ),
        "{error}"
    );
    assert_eq!(loose_objects(&fx.root), objects);
    assert!(git(&fx.root, &["for-each-ref", "refs/hutan/"]).is_empty());

    let error = engine
        .snapshot(&fx.root, Label::new("a/b", "1", Kind::Start))
        .await
        .unwrap_err();
    assert!(matches!(error, CheckpointError::InvalidLabel(_)), "{error}");
}
