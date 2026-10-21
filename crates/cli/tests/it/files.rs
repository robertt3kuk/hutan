//! `hutan fs ls|cat|stat|write|find|search` and `hutan open` (protocol 0.33) against a real
//! daemon, a temporary repository it tracks, and the real `hutan` binary: the smoke of the file
//! service from a shell, the conflict path included.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use crate::common;

use std::path::{Path, PathBuf};
use std::process::Stdio;

use common::{HUTAN, Out, TestDaemon, command, finish};
use tokio::io::AsyncWriteExt as _;

/// A daemon that tracks a temporary repository (canonical path).
struct Repo {
    t: TestDaemon,
    root: PathBuf,
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
        let t = TestDaemon::start().await;
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
