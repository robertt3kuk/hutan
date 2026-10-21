//! `hutan lsp …` and `hutan diagnostics` (protocol 0.39) against a real daemon, `hutan-fake-lsp`
//! standing in for rust-analyzer and the real `hutan` binary — with no desktop attached: the
//! CLI opens the files itself.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use crate::common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{HUTAN, Out, TestDaemon, WAIT, command, finish};
use hutan_daemon::lsp::LspTuning;
use serde_json::{Value, json};
use tokio::time::{sleep, timeout};

fn fake_bin() -> PathBuf {
    let bin = Path::new(HUTAN).parent().unwrap().join("hutan-fake-lsp");
    assert!(
        bin.is_file(),
        "hutan-fake-lsp not built: run `cargo build -p hutan-lsp-client --bins` (or `just test`)"
    );
    bin
}

struct Env {
    t: TestDaemon,
    root: PathBuf,
    script: PathBuf,
    _work: tempfile::TempDir,
}

impl Env {
    /// `extra` is more `daemon.toml`; the fake server serves the language `fake` (files
    /// `*.fake`) in folders with a `fake.toml`.
    async fn new(script: Value, extra: &str) -> Self {
        let work = tempfile::tempdir().unwrap();
        let base = work.path().canonicalize().unwrap();
        let root = base.join("repo");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("fake.toml"), "").unwrap();
        std::fs::write(root.join("src/a.fake"), "let error = 1;\n").unwrap();
        std::fs::write(root.join("src/ok.fake"), "all fine\n").unwrap();
        let script_path = base.join("script.json");
        std::fs::write(&script_path, script.to_string()).unwrap();
        let toml = format!(
            "[terminal]\nenv_inherit = \"daemon\"\n\n{extra}\n\
             [lsp.servers.fake]\ncommand = \"{}\"\nargs = [\"--script\", \"{}\"]\n\
             languages = [\"fake\"]\nroot_markers = [\"fake.toml\"]\n",
            fake_bin().display(),
            script_path.display()
        );
        let t = TestDaemon::start_toml(&toml, |config| config.lsp = LspTuning::default()).await;
        let opened = t
            .hutan(&["api", "workspace.open", &json!({"path": root}).to_string()])
            .await
            .ok();
        assert!(opened.stdout.contains("repo"), "{opened:?}");
        Self {
            t,
            root,
            script: script_path,
            _work: work,
        }
    }

    fn file(&self, name: &str) -> String {
        self.root.join("src").join(name).display().to_string()
    }

    async fn run(&self, args: &[&str]) -> Out {
        self.t.hutan(args).await
    }

    async fn stop(self) {
        self.t.stop().await;
        timeout(WAIT, async {
            while fake_processes(&self.script) != 0 {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a fake language server is still running");
    }
}

fn fake_processes(script: &Path) -> usize {
    let out = std::process::Command::new("pgrep")
        .arg("-f")
        .arg(script.display().to_string())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).lines().count()
}

#[tokio::test(flavor = "multi_thread")]
async fn diagnostics_of_a_file_work_with_no_desktop_attached() {
    let env = Env::new(
        json!({"diagnostics": true, "capabilities": {"positionEncoding": "utf-8", "textDocumentSync": 2}}),
        "[lsp]\ntrust = \"always\"\n",
    )
    .await;
    // No desktop: the CLI reads the file through the daemon, opens it and waits for the server.
    let out = env
        .run(&["diagnostics", "--json", &env.file("a.fake")])
        .await
        .ok();
    let result = out.json();
    assert_eq!(result["path"], env.file("a.fake"));
    let files = result["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "{result}");
    assert_eq!(files[0]["path"], env.file("a.fake"));
    let diagnostic = &files[0]["diagnostics"][0];
    assert_eq!(diagnostic["message"], "found error");
    assert_eq!(diagnostic["severity"], "error");
    assert_eq!(diagnostic["source"], "fake");
    assert_eq!(
        diagnostic["range"]["start"],
        json!({"line": 0, "character": 4})
    );
    assert_eq!(result["stale"], false);
    assert_eq!(result["timed_out"], false);
    assert_eq!(result["servers"][0]["name"], "fake");

    // The human form: one line per problem, one-based, and a summary.
    let text = env
        .run(&["diagnostics", &env.file("a.fake")])
        .await
        .ok()
        .stdout;
    assert!(
        text.contains(&format!(
            "{}:1:5: error: found error [fake]",
            env.file("a.fake")
        )),
        "{text}"
    );
    assert!(
        text.contains("1 error(s), 0 warning(s), 0 other(s) in 1 file(s)"),
        "{text}"
    );

    // A clean file says so; a severity filter is honoured.
    let clean = env
        .run(&["diagnostics", &env.file("ok.fake")])
        .await
        .ok()
        .stdout;
    assert!(clean.contains("no problems"), "{clean}");
    let none = env
        .run(&[
            "diagnostics",
            "--severity",
            "warning",
            "--json",
            &env.file("a.fake"),
        ])
        .await
        .ok()
        .json();
    assert_eq!(
        none["files"].as_array().unwrap().len(),
        1,
        "an error is at least a warning"
    );
    let bad = env
        .run(&["diagnostics", "--severity", "loud", &env.file("a.fake")])
        .await;
    assert_ne!(bad.code, 0);

    // A folder: what the server knows below it (nothing left open: the CLI closed the files,
    // the server keeps what it published).
    let folder = env
        .run(&["diagnostics", "--json", &env.root.display().to_string()])
        .await
        .ok()
        .json();
    assert_eq!(folder["files"].as_array().unwrap().len(), 1, "{folder}");

    // `hutan lsp ls` lists the server; its process is the fake.
    let listed = env.run(&["lsp", "ls", "--json"]).await.ok().json();
    let server = &listed["servers"][0];
    assert_eq!(server["name"], "fake");
    assert_eq!(server["root"], env.root.display().to_string());
    assert_eq!(server["documents"], 0);
    let text = env.run(&["lsp", "ls"]).await.ok().stdout;
    assert!(text.contains("fake") && text.contains("STATE"), "{text}");
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_untrusted_folder_asks_and_hutan_lsp_trust_answers() {
    let env = Env::new(json!({"diagnostics": true}), "").await;
    // The default asks: no server, and the CLI says what to do instead of "no problems".
    let first = env
        .run(&["diagnostics", &env.file("a.fake")])
        .await
        .ok()
        .stdout;
    assert!(!first.contains("no problems"), "{first}");
    assert!(first.contains("fake is untrusted"), "{first}");
    assert!(
        first.contains(&format!("hutan lsp trust {}", env.root.display())),
        "{first}"
    );
    assert_eq!(fake_processes(&env.script), 0);
    let json = env
        .run(&["diagnostics", "--json", &env.file("a.fake")])
        .await
        .ok()
        .json();
    assert_eq!(json["servers"][0]["state"], "untrusted");
    assert_eq!(json["servers"][0]["trust_pending"], true);
    assert_eq!(json["files"], json!([]));
    // `lsp ls` shows the request, `lsp status` the decisions (none yet).
    let ls = env.run(&["lsp", "ls"]).await.ok().stdout;
    assert!(
        ls.contains("untrusted") || ls.contains("no language server runs"),
        "{ls}"
    );

    // A plain shell's CLI is the user: it can trust the folder, with the warning.
    let trusted = env
        .run(&["lsp", "trust", &env.root.display().to_string()])
        .await
        .ok()
        .stdout;
    assert!(trusted.contains("build scripts"), "{trusted}");
    assert!(trusted.contains("does not sandbox"), "{trusted}");
    let out = env
        .run(&["diagnostics", "--json", &env.file("a.fake")])
        .await
        .ok()
        .json();
    assert_eq!(out["files"][0]["diagnostics"][0]["message"], "found error");
    let status = env.run(&["lsp", "status"]).await.ok().stdout;
    assert!(
        status.contains("trusted") && status.contains("(by cli)"),
        "{status}"
    );
    assert!(status.contains("trust default: ask"), "{status}");

    // Stopping and restarting by name.
    let stopped = env
        .run(&["lsp", "stop", "fake", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(stopped["state"], "stopped");
    wait_no_process(&env.script).await;
    let restarted = env
        .run(&["lsp", "restart", "fake", "--json"])
        .await
        .ok()
        .json();
    assert_ne!(restarted["state"], "stopped");
    let missing = env.run(&["lsp", "stop", "nope"]).await;
    assert_eq!(missing.code, 1, "{missing:?}");

    // Denying and forgetting.
    let denied = env
        .run(&["lsp", "trust", "--deny", &env.root.display().to_string()])
        .await
        .ok()
        .stdout;
    assert!(denied.contains("denied"), "{denied}");
    wait_no_process(&env.script).await;
    let after = env
        .run(&["diagnostics", "--json", &env.file("a.fake")])
        .await
        .ok()
        .json();
    assert_eq!(after["servers"][0]["state"], "untrusted");
    assert!(
        !after["servers"][0]["trust_pending"]
            .as_bool()
            .unwrap_or(false)
    );
    assert_eq!(fake_processes(&env.script), 0);
    let forgot = env
        .run(&["lsp", "trust", "--forget", &env.root.display().to_string()])
        .await
        .ok()
        .stdout;
    assert!(forgot.contains("asked about again"), "{forgot}");
    env.stop().await;
}

async fn wait_no_process(script: &Path) {
    timeout(WAIT, async {
        while fake_processes(script) != 0 {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a fake language server is still running");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_server_says_how_to_install_it() {
    let env = Env::new(
        json!({}),
        "[lsp]\ntrust = \"always\"\n\n[lsp.servers.rust-analyzer]\ncommand = \"/nonexistent/rust-analyzer\"\n",
    )
    .await;
    let file = env.root.join("src/lib.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();
    let out = env
        .run(&["diagnostics", &file.display().to_string()])
        .await
        .ok()
        .stdout;
    assert!(!out.contains("no problems"), "{out}");
    assert!(
        out.contains("rust-analyzer is not installed: install it with `rustup component add rust-analyzer` (or set `lsp.servers.rust-analyzer.command` in daemon.toml)"),
        "{out}"
    );
    // Nothing waits on the missing server any more (the CLI closed its document): the list is
    // empty rather than stale.
    let ls = env.run(&["lsp", "ls"]).await.ok().stdout;
    assert!(ls.contains("no language server runs"), "{ls}");
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agents_cli_cannot_trust_a_folder_but_can_read_diagnostics() {
    let env = Env::new(
        json!({"diagnostics": true}),
        "[control]\nagents = \"allow\"\n",
    )
    .await;
    let session = env
        .t
        .new_session(&[], &["sh", "-c", "exec sleep 600"])
        .await;
    // The CLI in that session reports an agent, as a hook of the agent does.
    let mut report = command(
        Path::new(HUTAN),
        Some(&env.t.paths.socket),
        &[
            "agent", "report", "--source", "test", "--agent", "fake", "--state", "working",
        ],
    );
    report.env("HUTAN_SESSION_ID", &session);
    finish(report).await.ok();
    timeout(WAIT, async {
        loop {
            if env.t.session(&session).await.get("agent").is_some() {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the agent was not reported");

    let in_session = |args: &[&str]| {
        let mut command = command(Path::new(HUTAN), Some(&env.t.paths.socket), args);
        command.env("HUTAN_SESSION_ID", &session);
        command
    };
    let refused = finish(in_session(&[
        "lsp",
        "trust",
        &env.root.display().to_string(),
    ]))
    .await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(
        refused.stderr.contains("forbidden") || refused.stderr.contains("user"),
        "{refused:?}"
    );
    assert_eq!(fake_processes(&env.script), 0);
    // Reading is allowed: the agent sees that the folder asks.
    let read = finish(in_session(&["diagnostics", "--json", &env.file("a.fake")])).await;
    assert_eq!(read.code, 0, "{read:?}");
    assert_eq!(read.json()["servers"][0]["state"], "untrusted");
    assert_eq!(fake_processes(&env.script), 0);
    env.stop().await;
}

// ---- the MCP tool ------------------------------------------------------------------------------

/// One `tools/call` of `hutan mcp` started in `cwd`: the result's text and structured content.
async fn mcp_call(
    socket: &Path,
    cwd: &Path,
    name: &str,
    arguments: Value,
) -> (String, Value, bool) {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut mcp = command(Path::new(HUTAN), Some(socket), &["mcp"]);
    mcp.current_dir(cwd)
        .env_remove("HUTAN_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = mcp.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    for (id, method, params) in [
        (
            1,
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
        ),
        (
            2,
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        ),
    ] {
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        stdin
            .write_all(format!("{message}\n").as_bytes())
            .await
            .unwrap();
        stdin.flush().await.unwrap();
        let answer = loop {
            let line = timeout(WAIT, lines.next_line())
                .await
                .expect("no answer from hutan mcp")
                .unwrap()
                .expect("hutan mcp closed its output");
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == id {
                break value;
            }
        };
        if id == 2 {
            let text = answer["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_else(|| panic!("no text in {answer}"))
                .to_owned();
            let structured = answer["result"]["structuredContent"].clone();
            let is_error = answer["result"]["isError"] == true;
            let _ = child.start_kill();
            return (text, structured, is_error);
        }
    }
    unreachable!()
}

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {out:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_mcp_tool_checks_the_changed_files_and_their_direct_importers_only() {
    // The fake server stands in for rust-analyzer: it serves `rust` files below a `Cargo.toml`.
    let work = tempfile::tempdir().unwrap();
    let base = work.path().canonicalize().unwrap();
    let root = base.join("repo");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), "mod router; // error here\n").unwrap();
    std::fs::write(root.join("src/router.rs"), "fn route() {}\n").unwrap();
    std::fs::write(root.join("src/other.rs"), "// an error nobody touched\n").unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "x"]);
    // The agent's edit: router.rs now has an error; lib.rs (which imports it) and other.rs are
    // unchanged.
    std::fs::write(root.join("src/router.rs"), "fn route() { error }\n").unwrap();
    let script = base.join("script.json");
    std::fs::write(&script, json!({"diagnostics": true}).to_string()).unwrap();
    let toml = format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[lsp]\ntrust = \"always\"\n\n\
         [lsp.servers.rust-analyzer]\ncommand = \"{}\"\nargs = [\"--script\", \"{}\"]\n",
        fake_bin().display(),
        script.display()
    );
    let t = TestDaemon::start_toml(&toml, |_| {}).await;
    t.hutan(&["api", "workspace.open", &json!({"path": root}).to_string()])
        .await
        .ok();

    let (text, structured, is_error) =
        mcp_call(&t.paths.socket, &root.join("src"), "diagnostics", json!({})).await;
    assert!(!is_error, "{text}");
    let router = root.join("src/router.rs");
    let lib = root.join("src/lib.rs");
    assert_eq!(structured["changed"], json!([router]));
    assert_eq!(structured["importers"], json!([lib]));
    let files: Vec<&str> = structured["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(files, [lib.to_str().unwrap(), router.to_str().unwrap()]);
    assert!(
        !text.contains("other.rs"),
        "an untouched file that nothing changed imports is not checked: {text}"
    );
    assert!(text.contains(&format!("{}:1:", router.display())), "{text}");
    assert!(text.contains("2 error(s)"), "{text}");
    assert!(
        text.contains("1 changed file(s) and 1 file(s) that import them"),
        "{text}"
    );
    assert_eq!(structured["stale"], false);

    // The files asked for, exactly; relative ones are relative to the agent's directory.
    let (text, structured, _) = mcp_call(
        &t.paths.socket,
        &root.join("src"),
        "diagnostics",
        json!({"paths": ["other.rs"], "min_severity": "error"}),
    )
    .await;
    assert_eq!(
        structured["checked"],
        json!([root.join("src/other.rs")]),
        "{text}"
    );
    assert!(text.contains("other.rs:1:"), "{text}");

    // A checkout with nothing changed has nothing to check; a project-wide check is asked for.
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "y"]);
    let (text, _, is_error) = mcp_call(&t.paths.socket, &root, "diagnostics", json!({})).await;
    assert!(!is_error && text.contains("nothing to check"), "{text}");
    let (_, project, is_error) = mcp_call(
        &t.paths.socket,
        &root,
        "diagnostics",
        json!({"scope": "project"}),
    )
    .await;
    assert!(!is_error);
    // The earlier calls' documents were closed; the servers kept what they published.
    assert!(project["files"].as_array().unwrap().len() >= 2, "{project}");
    let (_, status, _) = mcp_call(&t.paths.socket, &root, "lsp_status", json!({})).await;
    assert_eq!(status["servers"][0]["name"], "rust-analyzer");
    t.stop().await;
    timeout(WAIT, async {
        while fake_processes(&script) != 0 {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a fake language server is still running");
}
