//! `hutan ask` and `hutan assistant` against a real daemon (in-memory credential store, history
//! index on temporary directories) and a mock provider on a loopback port: every attachment
//! kind, scopes, refusals.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{HUTAN, Out, TestDaemon, WAIT};
use hutan_client::{Client, ConnectOptions};
use hutan_daemon::{DaemonConfig, GatewayConfig, Paths};
use hutan_gateway::testing::{MockServer, Reply};
use hutan_gateway::{CredentialStore, HttpConfig, MemoryStore};
use hutan_protocol::messages::{ClientInfo, ClientKind};
use tokio::time::sleep;

const PATH: &str = "/v1/chat/completions";
const SECRET: &str = "sk-abcdefghijklmnopqrstuvwxyz0123456789ABCD";

fn reply(text: &str) -> Reply {
    Reply::sse([
        format!(
            "data: {}\n\n",
            serde_json::json!({"choices":[{"delta":{"content": text}}]})
        ),
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n".to_owned(),
        "data: [DONE]\n\n".to_owned(),
    ])
}

struct Env {
    t: TestDaemon,
    mock: MockServer,
    work: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Env {
    async fn start() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let work = base.join("work");
        fs::create_dir_all(work.join("src")).unwrap();
        for dir in ["claude", "codex", "omp"] {
            fs::create_dir_all(base.join("sources").join(dir)).unwrap();
        }
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::in_home(home.path());
        fs::write(
            &paths.config_file,
            format!(
                "[assistant]\nprovider = \"mock\"\nmodel = \"m-1\"\n\
                 [history]\nenabled = true\n[history.roots]\nclaude = \"{}\"\ncodex = \"{}\"\nomp = \"{}\"\n",
                base.join("sources/claude").display(),
                base.join("sources/codex").display(),
                base.join("sources/omp").display(),
            ),
        )
        .unwrap();
        let store = Arc::new(MemoryStore::new());
        let mut config = DaemonConfig {
            hutan_bin: Some(PathBuf::from(HUTAN)),
            ..DaemonConfig::new(paths.clone())
        };
        config.gateway = GatewayConfig {
            credentials: Some(store as Arc<dyn CredentialStore>),
            http: HttpConfig {
                no_proxy: true,
                retry_backoff: Duration::from_millis(5),
                ..HttpConfig::default()
            },
            discovery: Some(Vec::new()),
        };
        let daemon = hutan_daemon::start(config).await.unwrap();
        let t = TestDaemon {
            _dir: home,
            paths,
            daemon,
        };
        let mock = MockServer::start().await;
        let client = Client::connect(ConnectOptions {
            endpoint: Some(t.paths.socket.clone().into()),
            autostart: None,
            client: ClientInfo {
                kind: ClientKind::Cli,
                version: "test".into(),
                origin: None,
            },
        })
        .await
        .unwrap();
        client
            .provider_add(hutan_protocol::provider::ProviderAdd {
                kind: "openai-compatible".into(),
                name: "mock".into(),
                base_url: Some(format!("{}/v1", mock.url())),
                default_model: Some("m-1".into()),
            })
            .await
            .unwrap();
        client
            .provider_set_credential("mock", "mock-key")
            .await
            .unwrap();
        Self {
            t,
            mock,
            work,
            _tmp: tmp,
        }
    }

    async fn hutan(&self, args: &[&str]) -> Out {
        self.t.hutan(args).await
    }

    fn sent_user_text(&self, n: usize) -> String {
        let body = self.mock.requests_to(PATH)[n].json();
        body["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_streams_the_answer_with_every_kind_of_attachment() {
    let e = Env::start().await;
    let work = e.work.display().to_string();
    fs::write(
        e.work.join("src/lib.rs"),
        format!("pub fn alpha() {{}}\nconst KEY: &str = \"{SECRET}\";\npub fn gamma() {{}}\n"),
    )
    .unwrap();
    fs::write(e.work.join("private.txt"), "UNATTACHED-FILE").unwrap();
    e.hutan(&["ws", "new", "proj", &work]).await.ok();

    // A terminal with one finished block.
    let script = format!(
        "printf '\\033]133;A\\007$ \\033]133;B\\007cargo test -p daemon\\r\\n\\033]133;C\\007flibbertigibbet test output\\r\\nsecond line key={SECRET}\\r\\n\\033]133;D;0\\007\\033]133;A\\007$ \\033]133;B\\007'; exec cat"
    );
    let session =
        e.t.new_session(&["--ws", "proj"], &["sh", "-c", &script])
            .await;
    let terminal = e.t.session(&session).await["terminal_id"].as_u64().unwrap();
    let terminal_arg = terminal.to_string();
    let block = {
        let deadline = Instant::now() + WAIT;
        loop {
            let blocks = e
                .hutan(&[
                    "api",
                    "terminal.blocks",
                    &format!("{{\"terminal_id\":{terminal},\"limit\":10}}"),
                    "--json",
                ])
                .await
                .ok()
                .json();
            if let Some(done) = blocks["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|b| b["state"] == "finished")
            {
                break done["id"].as_u64().unwrap();
            }
            assert!(Instant::now() < deadline, "the block never finished");
            sleep(Duration::from_millis(30)).await;
        }
    };
    let block_arg = block.to_string();

    // The index knows the terminal's output (the `search` attachment needs it) ...
    let deadline = Instant::now() + WAIT;
    while e
        .hutan(&["search", "flibbertigibbet"])
        .await
        .stdout
        .contains("no matches")
        || !e
            .hutan(&["search", "flibbertigibbet"])
            .await
            .stdout
            .contains("flibbertigibbet")
    {
        assert!(Instant::now() < deadline, "the output was never indexed");
        sleep(Duration::from_millis(50)).await;
    }
    // ... and an earlier chat provides a transcript to attach.
    e.mock.route("POST", PATH, reply("pangolin answer"));
    let created = e
        .hutan(&[
            "chat",
            "new",
            "--direct",
            "--provider",
            "mock",
            "--model",
            "m-1",
            "--cwd",
            &work,
        ])
        .await
        .ok();
    let chat = created.stdout.split_whitespace().nth(1).unwrap().to_owned();
    e.hutan(&["chat", "send", &chat, "tell me about pangolins", "--wait"])
        .await
        .ok();

    e.mock
        .set_route("POST", PATH, reply("It fails because of the flag."));
    let attach_block = format!("block:{terminal_arg}:{block_arg}");
    let attach_recent = format!("recent:{terminal_arg}:10");
    let attach_agent = format!("agent:{chat}");
    let asked = e
        .hutan(&[
            "ask",
            "why does this fail?",
            "--ws",
            "proj",
            "--attach",
            &attach_block,
            "--attach",
            &attach_recent,
            "--attach",
            &attach_agent,
            "--attach",
            "file:src/lib.rs:1-2",
            "--attach",
            "search:\"flibbertigibbet\"",
        ])
        .await
        .ok();
    assert_eq!(asked.stdout, "It fails because of the flag.\n", "{asked:?}");

    // What the provider got: the question with all five parts, redacted, nothing unattached.
    let text = e.sent_user_text(1);
    assert!(
        text.starts_with("why does this fail?\n\n--- attached context"),
        "{text}"
    );
    for part in [
        "[shell: cargo test -p daemon · exit 0 · 2 lines]",
        &format!("[shell: terminal {terminal} · last "),
        "hutan chat · 1 turns]\nuser: tell me about pangolins\n\nassistant: pangolin answer",
        "[file: src/lib.rs · lines 1-2 · 2 lines]\npub fn alpha() {}",
        "[search: \"flibbertigibbet\" · 1 hits]",
    ] {
        assert!(text.contains(part), "missing {part:?} in\n{text}");
    }
    for absent in [SECRET, "UNATTACHED", "gamma"] {
        assert!(!text.contains(absent), "{absent} leaked:\n{text}");
    }
    let system = e.mock.requests_to(PATH)[1].json()["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(system.contains("Hutan's assistant"), "{system}");

    // The assistant is a session of the workspace; `ls` and `open` agree on it, `open --new`
    // adds one.
    let opened = e
        .hutan(&["assistant", "open", "--ws", "proj", "--json"])
        .await
        .ok()
        .json();
    let id = opened["session"]["id"].as_str().unwrap().to_owned();
    assert_eq!(opened["session"]["chat"]["agent"], "assistant");
    let listed = e.hutan(&["assistant", "ls", "--json"]).await.ok().json();
    assert_eq!(listed["assistants"].as_array().unwrap().len(), 1);
    assert_eq!(listed["assistants"][0]["scope"], "workspace");
    assert_eq!(listed["assistants"][0]["session"]["id"], id.as_str());
    assert_eq!(listed["assistants"][0]["default"], true);
    let table = e.hutan(&["assistant", "ls"]).await.ok();
    assert!(
        table.stdout.contains("SCOPE")
            && table.stdout.contains(&id)
            && table.stdout.contains("mock / m-1"),
        "{table:?}"
    );
    let more = e
        .hutan(&["assistant", "open", "--ws", "proj", "--new", "--json"])
        .await
        .ok()
        .json();
    assert_ne!(more["session"]["id"], id.as_str());
    let log = e.hutan(&["chat", "log", &id]).await.ok();
    assert!(
        log.stdout.contains("why does this fail?") && log.stdout.contains("It fails"),
        "{log:?}"
    );
    e.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_global_assistant_answers_and_refuses_files_by_default() {
    let e = Env::start().await;
    e.mock.route("POST", PATH, reply("global answer"));
    let answered = e.hutan(&["ask", "hello there"]).await.ok();
    assert_eq!(answered.stdout, "global answer\n", "{answered:?}");
    let listed = e.hutan(&["assistant", "ls", "--json"]).await.ok().json();
    assert_eq!(listed["assistants"][0]["scope"], "global");

    let refused = e.hutan(&["ask", "read it", "--attach", "file:x.txt"]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(
        refused.stderr.contains("assistant.global_context.files"),
        "{refused:?}"
    );
    assert_eq!(
        e.mock.requests_to(PATH).len(),
        1,
        "the refused question was not sent"
    );

    // Usage errors are usage errors.
    let bad = e.hutan(&["ask", "q", "--attach", "nonsense"]).await;
    assert_eq!(bad.code, 2, "{bad:?}");
    let bad = e.hutan(&["ask", "q", "--attach", "recent:1:many"]).await;
    assert_eq!(bad.code, 2, "{bad:?}");
    e.t.stop().await;
}
