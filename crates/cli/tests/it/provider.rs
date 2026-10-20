//! `hutan provider …` and direct chats (`hutan chat new --direct`, `chat model`) against a
//! real daemon (in-memory credential store) and a mock provider on a loopback port.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use common::{HUTAN, Out, TestDaemon, WAIT, command};
use hutan_daemon::GatewayConfig;
use hutan_gateway::testing::{MockServer, Reply};
use hutan_gateway::{CredentialStore, HttpConfig, MemoryStore, ProbeTarget, ProviderKind};
use tokio::io::AsyncWriteExt;
use tokio::time::timeout;

const SECRET: &str = "LEAKCHECK-cli-0a1b2c3d4e5f-api-key";

fn openai_reply(text: &str) -> Reply {
    Reply::sse([
        format!(
            "data: {}\n\n",
            serde_json::json!({"choices":[{"delta":{"content": text}}]})
        ),
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n".to_owned(),
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":1,\"total_tokens\":5}}\n\n".to_owned(),
        "data: [DONE]\n\n".to_owned(),
    ])
}

struct Env {
    t: TestDaemon,
    store: Arc<MemoryStore>,
    work: tempfile::TempDir,
}

impl Env {
    async fn start(targets: Vec<ProbeTarget>) -> Self {
        let store = Arc::new(MemoryStore::new());
        let gateway = GatewayConfig {
            credentials: Some(store.clone() as Arc<dyn CredentialStore>),
            http: HttpConfig {
                no_proxy: true,
                retry_backoff: Duration::from_millis(5),
                ..HttpConfig::default()
            },
            discovery: Some(targets),
        };
        let t = TestDaemon::start_with(move |c| c.gateway = gateway).await;
        Self {
            t,
            store,
            work: tempfile::tempdir().unwrap(),
        }
    }

    /// `hutan <args>` with `stdin` piped in and extra environment.
    async fn hutan_with(&self, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Out {
        let mut cmd = command(Path::new(HUTAN), Some(&self.t.paths.socket), args);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in env {
            cmd.env(name, value);
        }
        let mut child = cmd.spawn().unwrap();
        let mut pipe = child.stdin.take().unwrap();
        if let Some(text) = stdin {
            pipe.write_all(text.as_bytes()).await.unwrap();
        }
        drop(pipe);
        let output = timeout(WAIT, child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        Out {
            code: output.status.code().expect("killed by a signal"),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }

    fn cwd(&self) -> String {
        self.work.path().display().to_string()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn providers_are_added_keyed_listed_and_removed() {
    let e = Env::start(Vec::new()).await;
    let added = e.t.hutan(&["provider", "add", "openai"]).await.ok();
    assert!(
        added
            .stdout
            .contains("provider openai (openai at https://api.openai.com/v1)")
            && added.stdout.contains("hutan provider key openai"),
        "{added:?}"
    );
    let compat =
        e.t.hutan(&[
            "provider",
            "add",
            "openai-compatible",
            "--name",
            "LM Studio",
            "--base-url",
            "http://127.0.0.1:1234/v1",
            "--model",
            "gemma",
            "--json",
        ])
        .await
        .ok()
        .json();
    assert_eq!(compat["provider"]["id"], "lm-studio");
    assert_eq!(compat["provider"]["default_model"], "gemma");

    // A key from a pipe, one from the environment, and none of them in any output.
    let piped = e
        .hutan_with(
            &["provider", "key", "openai"],
            Some(&format!("{SECRET}\n")),
            &[],
        )
        .await
        .ok();
    assert_eq!(
        piped.stdout.trim(),
        "stored the key of provider openai",
        "{piped:?}"
    );
    assert_eq!(e.store.get("openai").unwrap().unwrap().expose(), SECRET);
    let from_env = e
        .hutan_with(
            &[
                "provider",
                "key",
                "lm-studio",
                "--from-env",
                "HUTAN_TEST_KEY",
                "--json",
            ],
            None,
            &[("HUTAN_TEST_KEY", SECRET)],
        )
        .await
        .ok();
    assert_eq!(from_env.json()["provider"]["has_credential"], true);
    assert_eq!(e.store.get("lm-studio").unwrap().unwrap().expose(), SECRET);
    for out in [&piped, &from_env] {
        assert!(
            !out.stdout.contains("LEAKCHECK") && !out.stderr.contains("LEAKCHECK"),
            "{out:?}"
        );
    }

    let list = e.t.hutan(&["provider", "ls"]).await.ok();
    let lines: Vec<&str> = list.stdout.lines().collect();
    assert!(
        lines[0].starts_with("ID") && lines[0].contains("DEFAULT MODEL"),
        "{list:?}"
    );
    assert!(
        lines[1].starts_with("openai") && lines[1].contains("yes"),
        "{list:?}"
    );
    assert!(
        lines[2].starts_with("lm-studio")
            && lines[2].contains("openai-compatible")
            && lines[2].contains("gemma"),
        "{list:?}"
    );
    let json = e.t.hutan(&["provider", "ls", "--json"]).await.ok();
    assert!(!json.stdout.contains("LEAKCHECK"));
    assert_eq!(json.json()["providers"].as_array().unwrap().len(), 2);

    // Errors: no key on stdin, an unset variable, an unknown provider, a bad kind.
    let empty = e
        .hutan_with(&["provider", "key", "openai"], Some("\n"), &[])
        .await;
    assert_eq!(empty.code, 1, "{empty:?}");
    assert!(empty.stderr.contains("the key is empty"), "{empty:?}");
    let unset = e
        .hutan_with(
            &[
                "provider",
                "key",
                "openai",
                "--from-env",
                "HUTAN_NO_SUCH_VAR",
            ],
            None,
            &[],
        )
        .await;
    assert_eq!(unset.code, 1);
    assert!(
        unset.stderr.contains("HUTAN_NO_SUCH_VAR is not set"),
        "{unset:?}"
    );
    let unknown = e
        .hutan_with(&["provider", "key", "nope"], Some("x"), &[])
        .await;
    assert_eq!(unknown.code, 1);
    assert!(unknown.stderr.contains("not_found"), "{unknown:?}");
    let bad_kind = e.t.hutan(&["provider", "add", "gpt"]).await;
    assert_eq!(bad_kind.code, 2, "{bad_kind:?}");
    let no_url = e.t.hutan(&["provider", "add", "openai-compatible"]).await;
    assert_eq!(no_url.code, 1);
    assert!(no_url.stderr.contains("needs a base URL"), "{no_url:?}");

    // Clearing the key, removing the provider.
    let cleared =
        e.t.hutan(&["provider", "key", "openai", "--clear"])
            .await
            .ok();
    assert_eq!(cleared.stdout.trim(), "removed the key of provider openai");
    assert!(e.store.get("openai").unwrap().is_none());
    let removed = e.t.hutan(&["provider", "rm", "lm-studio"]).await.ok();
    assert_eq!(removed.stdout.trim(), "removed provider lm-studio");
    assert!(e.store.get("lm-studio").unwrap().is_none());
    let list = e.t.hutan(&["provider", "ls"]).await.ok();
    assert_eq!(
        list.stdout.lines().count(),
        2,
        "header and openai: {list:?}"
    );
    e.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn models_and_discovery() {
    let provider = MockServer::start().await;
    provider.route(
        "GET",
        "/v1/models",
        Reply::json(
            200,
            r#"{"data":[{"id":"alpha","owned_by":"me"},{"id":"beta"}]}"#,
        ),
    );
    let ollama = MockServer::start().await;
    ollama.route(
        "GET",
        "/api/tags",
        Reply::json(200, r#"{"models":[{"name":"qwen3:8b"}]}"#),
    );
    let e = Env::start(vec![ProbeTarget {
        kind: ProviderKind::Ollama,
        name: "Ollama".into(),
        base_url: ollama.url(),
    }])
    .await;
    e.t.hutan(&[
        "provider",
        "add",
        "openai-compatible",
        "--name",
        "mock",
        "--base-url",
        &format!("{}/v1", provider.url()),
        "--model",
        "beta",
    ])
    .await
    .ok();
    let models = e.t.hutan(&["provider", "models", "mock"]).await.ok();
    assert_eq!(models.stdout, "alpha\tme\nbeta (default)\n", "{models:?}");

    let found = e.t.hutan(&["provider", "discover"]).await.ok();
    assert!(
        found.stdout.contains("Ollama at")
            && found.stdout.contains("1 model")
            && found.stdout.contains("hutan provider add ollama"),
        "{found:?}"
    );
    let added = e.t.hutan(&["provider", "discover", "--add"]).await.ok();
    assert!(added.stdout.contains("added as ollama"), "{added:?}");
    let again = e.t.hutan(&["provider", "discover", "--add"]).await.ok();
    assert!(again.stdout.contains("configured as ollama"), "{again:?}");
    let list = e.t.hutan(&["provider", "ls", "--json"]).await.ok().json();
    assert_eq!(list["providers"].as_array().unwrap().len(), 2);
    e.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_direct_chat_streams_through_the_cli_and_switches_models() {
    let mock = MockServer::start().await;
    mock.route("POST", "/v1/chat/completions", openai_reply("Hello"));
    mock.route("POST", "/v1/chat/completions", openai_reply("Switched"));
    let e = Env::start(Vec::new()).await;
    e.t.hutan(&[
        "provider",
        "add",
        "openai-compatible",
        "--name",
        "mock",
        "--base-url",
        &format!("{}/v1", mock.url()),
    ])
    .await
    .ok();
    e.hutan_with(&["provider", "key", "mock"], Some(SECRET), &[])
        .await
        .ok();

    // --direct needs a provider; the model is mandatory when the provider has no default.
    let missing =
        e.t.hutan(&["chat", "new", "--direct", "--cwd", &e.cwd()])
            .await;
    assert_eq!(missing.code, 2, "{missing:?}");
    let no_model =
        e.t.hutan(&[
            "chat",
            "new",
            "--direct",
            "--provider",
            "mock",
            "--cwd",
            &e.cwd(),
        ])
        .await;
    assert_eq!(no_model.code, 1, "{no_model:?}");
    assert!(no_model.stderr.contains("no model"), "{no_model:?}");

    let created =
        e.t.hutan(&[
            "chat",
            "new",
            "--direct",
            "--provider",
            "mock",
            "--model",
            "m-1",
            "--system",
            "Be brief",
            "--cwd",
            &e.cwd(),
        ])
        .await
        .ok();
    let words: Vec<&str> = created.stdout.split_whitespace().collect();
    assert_eq!(&words[0], &"session", "{created:?}");
    assert_eq!(&words[2..], &["direct", "mock", "/", "m-1"], "{created:?}");
    let id = words[1].to_owned();

    let sent =
        e.t.hutan(&["chat", "send", &id, "hi there", "--wait"])
            .await
            .ok();
    assert_eq!(sent.stdout, "Hello\n", "{sent:?}");
    let first = mock.requests_to("/v1/chat/completions")[0].clone();
    assert_eq!(
        first.header("authorization"),
        Some(&*format!("Bearer {SECRET}"))
    );
    assert_eq!(first.json()["model"], "m-1");
    assert_eq!(first.json()["messages"][0]["content"], "Be brief");

    let switched = e.t.hutan(&["chat", "model", &id, "m-2"]).await.ok();
    assert!(
        switched.stdout.contains("direct mock / m-2"),
        "{switched:?}"
    );
    let sent =
        e.t.hutan(&["chat", "send", &id, "and now?", "--wait"])
            .await
            .ok();
    assert_eq!(sent.stdout, "Switched\n", "{sent:?}");
    let second = mock.requests_to("/v1/chat/completions")[1].json();
    assert_eq!(second["model"], "m-2");
    assert_eq!(
        second["messages"].as_array().unwrap().len(),
        4,
        "system + 3 conversation messages"
    );

    let log = e.t.hutan(&["chat", "log", &id]).await.ok();
    assert!(
        log.stdout.contains("> hi there")
            && log.stdout.contains("Hello")
            && log.stdout.contains("> and now?")
            && log.stdout.contains("Switched"),
        "{log:?}"
    );
    assert!(
        log.stdout.contains("mock / m-1 to mock / m-2"),
        "the switch is in the log: {log:?}"
    );
    assert!(!log.stdout.contains("LEAKCHECK") && !sent.stdout.contains("LEAKCHECK"));

    // A terminal session is not a chat.
    let terminal = e.t.new_session(&[], &["sh", "-c", "sleep 30"]).await;
    let refused = e.t.hutan(&["chat", "model", &terminal, "m"]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    e.t.stop().await;
}
