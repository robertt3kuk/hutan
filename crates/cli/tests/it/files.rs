//! `hutan fs ls|cat|stat|write|find|search|mkdir|touch|mv|cp|trash|restore` and `hutan open`
//! (protocol 0.33, 0.54) against a real
//! daemon, a temporary repository it tracks, and the real `hutan` binary: the smoke of the file
//! service from a shell, the conflict path included.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use crate::common;

use std::path::{Path, PathBuf};
use std::process::Stdio;

use std::sync::Arc;

use common::{HUTAN, Out, TestDaemon, command, finish};
use hutan_daemon::files::trash::FolderTrash;
use tokio::io::AsyncWriteExt as _;

/// A daemon that tracks a temporary repository (canonical path).
struct Repo {
    t: TestDaemon,
    root: PathBuf,
    /// Where `fs trash` puts files: a folder, never the user's Trash.
    trash: PathBuf,
    _work: tempfile::TempDir,
}

impl Repo {
    async fn new() -> Self {
        let work = tempfile::tempdir().unwrap();
        let root = work.path().canonicalize().unwrap().join("repo");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(
            root.join("src/router.rs"),
            "fn route() {}\n// TODO(ann): later\n",
        )
        .unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub mod router;\n").unwrap();
        std::fs::write(root.join("README.md"), "# repo\n").unwrap();
        std::fs::write(root.join("target/out.rs"), "fn route() {}\n").unwrap();
        let trash = work.path().canonicalize().unwrap().join("trash");
        std::fs::create_dir_all(&trash).unwrap();
        let backend = Arc::new(FolderTrash::new(&trash));
        let t = TestDaemon::start_with(|config| config.fs_trash = Some(backend)).await;
        let opened = t
            .hutan(&[
                "api",
                "workspace.open",
                &serde_json::json!({"path": root}).to_string(),
            ])
            .await
            .ok();
        assert!(opened.stdout.contains("repo"), "{opened:?}");
        Self {
            t,
            root,
            trash,
            _work: work,
        }
    }

    /// `hutan <args>` run in `cwd` with `stdin` as its input.
    async fn run(&self, cwd: &Path, stdin: Option<&str>, args: &[&str]) -> Out {
        let mut command = command(Path::new(HUTAN), Some(&self.t.paths.socket), args);
        command.current_dir(cwd);
        if stdin.is_some() {
            command.stdin(Stdio::piped());
        }
        let Some(text) = stdin else {
            return finish(command).await;
        };
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let mut input = child.stdin.take().unwrap();
        input.write_all(text.as_bytes()).await.unwrap();
        drop(input);
        let output = child.wait_with_output().await.unwrap();
        Out {
            code: output.status.code().unwrap(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }

    async fn hutan(&self, args: &[&str]) -> Out {
        self.run(&self.root, None, args).await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ls_cat_stat_find_search_and_open_work_from_a_shell() {
    let repo = Repo::new().await;

    // `ls` in the repository's own directory: directories first, ignored entries marked.
    let ls = repo.hutan(&["fs", "ls"]).await.ok().stdout;
    let lines: Vec<&str> = ls.lines().collect();
    assert!(
        lines[0].contains(".git/") && lines[0].contains("(ignored)"),
        "{ls}"
    );
    assert!(
        lines[1].contains("src/") && !lines[1].contains("ignored"),
        "{ls}"
    );
    assert!(
        lines[2].contains("target/") && lines[2].contains("(ignored)"),
        "{ls}"
    );
    assert!(
        ls.contains("README.md") && ls.contains(".gitignore"),
        "{ls}"
    );
    let sub = repo.hutan(&["fs", "ls", "src", "--json"]).await.ok().json();
    let names: Vec<&str> = sub["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["lib.rs", "router.rs"]);

    // `cat` prints the bytes as they are; `--json` carries the version.
    let cat = repo.hutan(&["fs", "cat", "src/router.rs"]).await.ok();
    assert_eq!(cat.stdout, "fn route() {}\n// TODO(ann): later\n");
    let json = repo
        .hutan(&["fs", "cat", "src/lib.rs", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(json["text"], "pub mod router;\n");
    assert_eq!(json["version"]["size"], 16);
    let stat = repo.hutan(&["fs", "stat", "src/lib.rs"]).await.ok().stdout;
    assert!(
        stat.contains("kind    file") && stat.contains("size    16"),
        "{stat}"
    );

    // `find` and `search`.
    let found = repo.hutan(&["fs", "find", "rtr"]).await.ok().stdout;
    assert_eq!(found.trim(), "src/router.rs");
    let hits = repo
        .hutan(&["fs", "search", "route", "--root", "src"])
        .await
        .ok()
        .stdout;
    assert_eq!(
        hits.lines().collect::<Vec<_>>(),
        ["lib.rs:1:pub mod router;", "router.rs:1:fn route() {}"]
    );
    let regex = repo
        .hutan(&["fs", "search", r"TODO\(\w+\)", "--regex", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(regex["hits"][0]["path"], "src/router.rs");
    assert_eq!(regex["hits"][0]["line"], 2);
    // The ignored `target/` is not searched.
    assert!(!hits.contains("target"), "{hits}");

    // `open` resolves the path through the daemon and prints where it is.
    let open = repo.hutan(&["open", "src/router.rs:2:4"]).await.ok().stdout;
    assert_eq!(
        open.trim(),
        format!("{}:2:4", repo.root.join("src/router.rs").display())
    );
    let open = repo.hutan(&["open", "README.md"]).await.ok().stdout;
    assert_eq!(
        open.trim(),
        repo.root.join("README.md").display().to_string()
    );
    let missing = repo.hutan(&["open", "nope.rs:3"]).await;
    assert_eq!(missing.code, 1, "{missing:?}");
    assert!(missing.stderr.contains("not_found"), "{missing:?}");
    repo.t.stop().await;
}

/// With a window, `open` runs `workbench::OpenFile` there and says so; a window that refuses is
/// an error, not the path printed as if nothing was open.
#[tokio::test(flavor = "multi_thread")]
async fn open_shows_the_file_in_the_window_and_reports_a_refusing_window() {
    let repo = Repo::new().await;
    let desktop = crate::control::FakeDesktop::start(&repo.t.paths.socket).await;

    let opened = repo
        .hutan(&["open", "src/router.rs:2", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(opened["opened"], true, "{opened}");
    let text = repo.hutan(&["open", "src/lib.rs"]).await.ok().stdout;
    assert_eq!(
        text.trim(),
        format!("opened {} in Hutan", repo.root.join("src/lib.rs").display())
    );
    let seen = desktop.seen();
    assert_eq!(seen[0].0, "workbench::OpenFile");
    assert_eq!(
        seen[0].1,
        serde_json::json!({"path": repo.root.join("src/router.rs"), "line": 2})
    );

    let refused = repo.hutan(&["open", "README.md"]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(refused.stderr.contains("no handler"), "{refused:?}");
    assert!(
        !refused.stdout.contains("README.md"),
        "no path printed: {refused:?}"
    );
    repo.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn write_replaces_a_file_and_a_stale_expect_is_a_conflict() {
    let repo = Repo::new().await;
    let file = repo.root.join("notes.txt");
    std::fs::write(&file, "first\n").unwrap();
    let version = |out: &Out| -> String {
        out.stdout
            .lines()
            .find_map(|line| line.strip_prefix("version "))
            .unwrap()
            .trim()
            .to_owned()
    };
    let stat = repo.hutan(&["fs", "stat", "notes.txt"]).await.ok();
    let v1 = version(&stat);

    // Content from stdin, version checked.
    let written = repo
        .run(
            &repo.root,
            Some("second, from stdin\n"),
            &["fs", "write", "notes.txt", "--expect", &v1],
        )
        .await
        .ok();
    assert!(written.stdout.starts_with("wrote "), "{written:?}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "second, from stdin\n"
    );

    // The same version again is stale now: exit 1, the daemon's message, the current version
    // on stderr for the next try, and the file untouched.
    let conflict = repo
        .run(
            &repo.root,
            Some("lost update\n"),
            &["fs", "write", "notes.txt", "--expect", &v1],
        )
        .await;
    assert_eq!(conflict.code, 1, "{conflict:?}");
    assert!(conflict.stderr.contains("conflict"), "{conflict:?}");
    let v2 = version(&repo.hutan(&["fs", "stat", "notes.txt"]).await.ok());
    assert!(
        conflict.stderr.contains(&format!("current version: {v2}")),
        "{conflict:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "second, from stdin\n"
    );
    // With the current version it goes through.
    repo.hutan(&[
        "fs",
        "write",
        "notes.txt",
        "--expect",
        &v2,
        "--text",
        "third\n",
    ])
    .await
    .ok();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "third\n");

    // `--create` makes a new file once; a second time it conflicts. `--from` copies a file.
    repo.hutan(&["fs", "write", "fresh.txt", "--create", "--text", "new"])
        .await
        .ok();
    let again = repo
        .hutan(&["fs", "write", "fresh.txt", "--create", "--text", "again"])
        .await;
    assert_eq!(again.code, 1, "{again:?}");
    assert_eq!(
        std::fs::read_to_string(repo.root.join("fresh.txt")).unwrap(),
        "new"
    );
    repo.hutan(&["fs", "write", "copy.txt", "--create", "--from", "README.md"])
        .await
        .ok();
    assert_eq!(
        std::fs::read_to_string(repo.root.join("copy.txt")).unwrap(),
        "# repo\n"
    );
    // A malformed version is a usage error before anything is sent.
    let bad = repo
        .hutan(&[
            "fs",
            "write",
            "notes.txt",
            "--expect",
            "nonsense",
            "--text",
            "x",
        ])
        .await;
    assert_eq!(bad.code, 2, "{bad:?}");

    // Outside the roots nothing is read or written without `--picked`, which the user may.
    let outside = repo.root.parent().unwrap().join("outside.txt");
    std::fs::write(&outside, "outside").unwrap();
    let refused = repo.hutan(&["fs", "cat", outside.to_str().unwrap()]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(refused.stderr.contains("forbidden"), "{refused:?}");
    let picked = repo
        .hutan(&["fs", "cat", "--picked", outside.to_str().unwrap()])
        .await
        .ok();
    assert_eq!(picked.stdout, "outside");
    repo.t.stop().await;
}

// ---- file operations (protocol 0.54) -----------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn mkdir_touch_mv_cp_trash_and_restore_work_from_a_shell() {
    let repo = Repo::new().await;
    let at = |relative: &str| repo.root.join(relative);

    // mkdir, mkdir -p, touch.
    let made = repo
        .hutan(&["fs", "mkdir", "docs", "build/out", "-p", "--json"])
        .await;
    assert_eq!(made.code, 0, "{made:?}");
    assert_eq!(made.json()["items"].as_array().unwrap().len(), 2);
    assert!(at("docs").is_dir() && at("build/out").is_dir());
    assert_eq!(repo.hutan(&["fs", "mkdir", "docs"]).await.code, 1);
    let again = repo.hutan(&["fs", "mkdir", "-p", "docs"]).await.ok();
    assert!(again.stdout.contains("docs"), "{again:?}");
    let touched = repo.hutan(&["fs", "touch", "docs/new.txt"]).await.ok();
    assert!(touched.stdout.starts_with("created "), "{touched:?}");
    assert_eq!(std::fs::read_to_string(at("docs/new.txt")).unwrap(), "");
    let exists = repo.hutan(&["fs", "touch", "docs/new.txt"]).await;
    assert_eq!(exists.code, 1, "{exists:?}");
    assert!(exists.stderr.contains("exists"), "{exists:?}");

    // mv: a rename, into a folder, several into a folder, the refusals.
    let renamed = repo
        .hutan(&["fs", "mv", "docs/new.txt", "docs/renamed.txt"])
        .await
        .ok();
    assert!(renamed.stdout.contains("->"), "{renamed:?}");
    assert!(at("docs/renamed.txt").exists() && !at("docs/new.txt").exists());
    repo.hutan(&["fs", "mv", "README.md", "src", "build"])
        .await
        .ok();
    assert!(at("build/README.md").exists() && at("build/src/lib.rs").exists());
    let not_a_folder = repo
        .hutan(&["fs", "mv", "build/README.md", "docs/renamed.txt", "nowhere"])
        .await;
    assert_eq!(not_a_folder.code, 2, "{not_a_folder:?}");
    let taken = repo
        .hutan(&["fs", "mv", "build/README.md", "docs/renamed.txt"])
        .await;
    assert_eq!(taken.code, 1, "{taken:?}");
    assert!(taken.stderr.contains("exists"), "{taken:?}");
    assert!(at("build/README.md").exists(), "nothing was overwritten");
    // A case-only rename.
    repo.hutan(&["fs", "mv", "build/README.md", "build/Readme.md"])
        .await
        .ok();
    assert!(
        std::fs::read_dir(at("build"))
            .unwrap()
            .any(|e| e.unwrap().file_name() == "Readme.md")
    );

    // cp: a file, a tree into a folder; the original stays.
    repo.hutan(&["fs", "cp", "build/Readme.md", "docs/copy.md"])
        .await
        .ok();
    repo.hutan(&["fs", "cp", "build/src", "docs"]).await.ok();
    assert_eq!(
        std::fs::read_to_string(at("docs/copy.md")).unwrap(),
        "# repo\n"
    );
    assert!(at("docs/src/router.rs").exists() && at("build/src/router.rs").exists());
    let json = repo
        .hutan(&["fs", "cp", "docs/copy.md", "docs/copy.md", "--json"])
        .await;
    assert_eq!(json.code, 1, "{json:?}");
    assert_eq!(json.json()["items"][0]["error"]["data"]["reason"], "exists");

    // trash prints a ticket and removes nothing for good; restore puts it back.
    let trashed = repo
        .hutan(&["fs", "trash", "docs/copy.md", "docs/missing.md", "docs/src"])
        .await;
    assert_eq!(trashed.code, 1, "one of the paths is missing: {trashed:?}");
    assert!(
        trashed.stdout.contains("undo with: hutan fs restore"),
        "{trashed:?}"
    );
    assert!(!at("docs/copy.md").exists() && !at("docs/src").exists());
    assert_eq!(std::fs::read_dir(&repo.trash).unwrap().count(), 2);
    let ticket = trashed
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("undo with: hutan fs restore "))
        .unwrap()
        .to_owned();
    let restored = repo.hutan(&["fs", "restore", &ticket]).await.ok();
    assert!(restored.stdout.contains("restored"), "{restored:?}");
    assert!(at("docs/copy.md").exists() && at("docs/src/lib.rs").exists());
    let used = repo.hutan(&["fs", "restore", &ticket]).await;
    assert_eq!(used.code, 1, "{used:?}");
    let json = repo
        .hutan(&["fs", "trash", "docs/copy.md", "--json"])
        .await
        .ok()
        .json();
    assert!(json["ticket"].as_str().is_some());
    assert_eq!(json["items"][0]["entry"]["name"], "copy.md");

    // Outside the workspaces, and below .git: refused per item, the exit code says so.
    let outside = repo._work.path().canonicalize().unwrap().join("loose.txt");
    std::fs::write(&outside, "loose").unwrap();
    let refused = repo
        .hutan(&["fs", "trash", outside.to_str().unwrap()])
        .await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(refused.stderr.contains("refused"), "{refused:?}");
    assert!(outside.exists());
    let git = repo.hutan(&["fs", "touch", ".git/hook"]).await;
    assert_eq!(git.code, 1, "{git:?}");
    assert!(!at(".git/hook").exists());
    // cp --picked takes a source from outside the workspaces, mv never does.
    let picked = repo
        .hutan(&[
            "fs",
            "cp",
            "--picked",
            outside.to_str().unwrap(),
            "from-outside.txt",
        ])
        .await
        .ok();
    assert!(picked.stdout.contains("copied"), "{picked:?}");
    assert_eq!(
        std::fs::read_to_string(at("from-outside.txt")).unwrap(),
        "loose"
    );
    assert_eq!(
        repo.hutan(&["fs", "cp", outside.to_str().unwrap(), "again.txt"])
            .await
            .code,
        1
    );
}
