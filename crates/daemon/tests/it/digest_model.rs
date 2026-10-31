//! The scheduled model digest (MAWAS-DIGEST-B, protocol 0.69) end to end: a real daemon on a
//! temporary `HUTAN_HOME`, `hutan-acp-fixture replay` standing in for `omp acp` (the transcripts
//! in `tests/fixtures/digest-model/` answer `/tools`, `/mcp list` and the digest itself), a fake
//! `omp --version` and a fake `hutan` that lists the unattended tools. Nothing here starts a
//! real harness or calls a model. The daemon's "hour" is [`HOUR`] ms, so a period is long and
//! the schedule fires when the setting is turned on, not by the wall clock.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::assistant_backend::{argv_logging_wrapper, fake_cli, fixture_bin, gateway};
use crate::common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, WAIT, create, sh, wait_event};
use hutan_client::{Client, ConnectOptions};
use hutan_core::agent::AgentEventKind;
use hutan_core::{Seq, SessionId};
use hutan_daemon::DaemonConfig;
use hutan_daemon::digest::DigestTuning;
use hutan_protocol::assistant::{AssistantListRequest, AssistantOpen, AssistantScope};
use hutan_protocol::chat::{ApprovalList, ApprovalListRequest};
use hutan_protocol::messages::{
    Event, Origin, Request, SessionInfo, SessionList, SessionListFilter, SessionRename,
    SessionResult,
};
use hutan_protocol::notify::{DigestStatus, IntentKind, NotificationIntent};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

/// The length of the daemon's "hour": a period is long, so a run happens when the test says.
const HOUR: u64 = 600_000;
/// How often the daemon looks.
const TICK: Duration = Duration::from_millis(20);
/// Long enough for the daemon to have looked many times.
const MANY_TICKS: Duration = Duration::from_millis(400);

/// A fake `hutan` that lists the tools of `hutan mcp --unattended-tools` (and the other two).
const FAKE_HUTAN: &str = "case \"$2\" in\n--unattended-tools) printf 'agents_board\\nagents_activity\\nagents_stats\\n';;\n--read-only-tools) printf 'agents_board\\nagents_activity\\nagents_stats\\nmemory_propose\\n';;\n--gated-tools) printf 'task_add\\nask_suggest\\n';;\nesac";

fn transcript(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/digest-model")
        .join(format!("{name}.jsonl"))
}

struct Options {
    transcript: &'static str,
    /// What the fake `omp --version` prints.
    omp_version: &'static str,
    /// The lines of `[assistant]`.
    assistant: &'static str,
    /// The lines of `[notify]`.
    notify: &'static str,
    run_limit: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            transcript: "pass",
            omp_version: "omp/18.13.3",
            assistant: "backend = \"omp\"\nmodel = \"gitlab-duo/duo-chat-opus-4-6\"",
            notify: "",
            run_limit: Duration::from_secs(30),
        }
    }
}

struct Env {
    daemon: TestDaemon,
    logs: tempfile::TempDir,
    client: Client,
    events: broadcast::Receiver<(Seq, Event)>,
    mawas: SessionId,
}

impl Env {
    async fn start(options: Options) -> Self {
        let logs = tempfile::tempdir().unwrap();
        let wrapper = argv_logging_wrapper();
        let acp = fixture_bin("hutan-acp-fixture");
        let omp_cli = fake_cli("omp", &format!("echo {}", options.omp_version));
        let toml = format!(
            "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {wrapper:?}\nargs = [\"replay\", \"--received\", {received:?}, {transcript:?}, \"acp\"]\nenv = {{ ARGV_LOG = {log:?}, FIXTURE = {acp:?} }}\ncli = {omp_cli:?}\n\n[assistant]\n{assistant}\n\n[notify]\n{notify}\n",
            received = logs.path().join("omp-received.jsonl"),
            transcript = transcript(options.transcript),
            log = logs.path().join("omp-argv.log"),
            assistant = options.assistant,
            notify = options.notify,
        );
        let hutan = fake_cli("hutan", FAKE_HUTAN);
        let run_limit = options.run_limit;
        let daemon = TestDaemon::start_with(&toml, move |config: &mut DaemonConfig| {
            config.gateway = gateway();
            config.hutan_bin = Some(hutan);
            config.digest = DigestTuning {
                tick: TICK,
                hour_ms: HOUR,
                run_limit,
                poll: Duration::from_millis(10),
                utc_offset_secs: Some(0),
            };
        })
        .await;
        let (client, events) = daemon.connect_subscribed().await;
        // Global Mawas, opened and never asked (no model is called for her): the origin of the
        // request that waits.
        let mawas = client
            .call::<SessionResult>(Request::AssistantOpen(AssistantOpen {
                scope: AssistantScope::Global,
                workspace_id: None,
                new: false,
            }))
            .await
            .unwrap()
            .session
            .id;
        Self {
            daemon,
            logs,
            client,
            events,
            mawas,
        }
    }

    /// A request waits for the user (Mawas asks to rename a shell): something happened that
    /// the digest has to say. Returns the task of the blocked call.
    async fn a_request_waits(&self) -> tokio::task::JoinHandle<()> {
        let target = create(&self.client, sh("sleep 120")).await;
        let mut options: ConnectOptions = self.daemon.options();
        options.client.origin = Some(Origin {
            session_id: self.mawas,
        });
        let from_mawas = Client::connect(options).await.unwrap();
        let asked = tokio::spawn(async move {
            let _ = from_mawas
                .call::<SessionResult>(Request::SessionRename(SessionRename {
                    session_id: target.id,
                    title: "renamed".into(),
                }))
                .await;
        });
        timeout(WAIT, async {
            loop {
                let list: ApprovalList = self
                    .client
                    .call(Request::ApprovalList(ApprovalListRequest::default()))
                    .await
                    .unwrap();
                if !list.approvals.is_empty() {
                    return;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the request never reached the user");
        asked
    }

    async fn schedule(&self, value: &str) {
        self.client
            .daemon_setting_set("notify.digest_model", Some(json!(value)))
            .await
            .unwrap();
    }

    /// The status once a run has been recorded.
    async fn status_after_a_run(&self) -> DigestStatus {
        timeout(WAIT, async {
            loop {
                let status = self.client.digest_status().await.unwrap();
                if status.last_run.is_some() {
                    return status;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("no run was recorded")
    }

    async fn next_digest(&mut self) -> NotificationIntent {
        wait_event(&mut self.events, "the digest", |event| match event {
            Event::NotifyIntent { intent } if intent.kind == IntentKind::Digest => {
                Some(intent.clone())
            }
            _ => None,
        })
        .await
    }

    /// What the fake OMP received, as parsed messages.
    fn received(&self) -> Vec<Value> {
        std::fs::read_to_string(self.logs.path().join("omp-received.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["msg"].clone())
            .collect()
    }

    fn prompts(&self) -> Vec<String> {
        self.received()
            .iter()
            .filter(|msg| msg["method"] == "session/prompt")
            .map(|msg| {
                msg["params"]["prompt"]
                    .as_array()
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|b| b["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default()
            })
            .collect()
    }

    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.logs.path().join("omp-argv.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The digest chat, when there is one.
    async fn digest_chat(&self) -> Option<SessionInfo> {
        self.client
            .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
            .await
            .unwrap()
            .sessions
            .into_iter()
            .find(|s| s.chat.as_ref().is_some_and(|chat| chat.digest))
    }

    async fn notices(&self, session: SessionId) -> Vec<String> {
        self.client
            .chat_history_all(session)
            .await
            .unwrap()
            .events
            .into_iter()
            .filter_map(|event| match event.kind {
                AgentEventKind::Notice { message, .. } => Some(message),
                _ => None,
            })
            .collect()
    }

    async fn runs_stay(&self, runs: u32) {
        sleep(MANY_TICKS).await;
        assert_eq!(self.client.digest_status().await.unwrap().runs, runs);
    }

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_verified_launch_runs_the_digest_once_with_exactly_the_recipes_arguments() {
    let mut env = Env::start(Options::default()).await;
    let asked = env.a_request_waits().await;
    // Off by default: nothing ran, and nothing was started.
    assert_eq!(env.client.digest_status().await.unwrap().schedule, "off");
    sleep(MANY_TICKS).await;
    assert!(env.prompts().is_empty());

    env.schedule("every 1 h").await;
    let digest = env.next_digest().await;
    assert_eq!(digest.title, "Digest");
    assert!(digest.body.contains("Needs you"), "{}", digest.body);
    assert_ne!(
        digest.session_id, env.mawas,
        "the digest has a chat of its own"
    );

    // The chat: Global, titled Digest, marked, and never the default Mawas.
    let chat = env.digest_chat().await.expect("a digest chat");
    assert_eq!(chat.id, digest.session_id);
    assert_eq!(chat.title, "Digest");
    let list = env
        .client
        .assistant_list(AssistantListRequest::default())
        .await
        .unwrap();
    let default: Vec<SessionId> = list
        .iter()
        .filter(|a| a.default)
        .map(|a| a.session.id)
        .collect();
    assert_eq!(default, [env.mawas]);
    assert!(list.iter().any(|a| a.session.id == chat.id));

    // The status says so.
    let status = env.status_after_a_run().await;
    let last = status.last_run.clone().unwrap();
    assert_eq!(last.outcome, "done", "{:?}", last.reason);
    assert_eq!(last.session_id, Some(chat.id));
    assert_eq!(last.tokens, Some(12345));
    assert_eq!((status.runs, status.avg_tokens), (1, Some(12345)));
    assert!(status.runnable && status.reason.is_none(), "{status:?}");
    assert_eq!(status.harness.as_deref(), Some("omp"));

    // The pre-flight came before the digest: two commands, then the prompt with the data.
    let prompts = env.prompts();
    assert_eq!(prompts.len(), 3, "{prompts:?}");
    assert_eq!(prompts[0], "/tools");
    assert_eq!(prompts[1], "/mcp list");
    assert!(prompts[2].contains("UNTRUSTED DATA"), "{}", prompts[2]);
    assert!(prompts[2].contains("agents.activity"), "{}", prompts[2]);
    assert!(prompts[2].contains("agents.stats"), "{}", prompts[2]);

    // Exactly the recipe's arguments, once; none of the advisor's tool list.
    let argv = env.argv();
    let at = argv
        .iter()
        .position(|a| a == "--no-tools")
        .expect("--no-tools");
    assert_eq!(argv.iter().filter(|a| *a == "--no-tools").count(), 1);
    assert_eq!(
        argv[at..at + 8],
        [
            "--no-tools",
            "--no-extensions",
            "--no-skills",
            "--no-rules",
            "--no-context-files",
            "--no-lsp",
            "--approval-mode",
            "always-ask"
        ]
    );
    assert_eq!(argv[at + 8], "--config");
    assert!(argv[at + 9].ends_with("omp-digest.yml"), "{}", argv[at + 9]);
    assert_eq!(argv[at + 10], "--append-system-prompt");
    assert!(!argv[at..].iter().any(|a| a == "--tools"), "{argv:?}");
    let overlay = std::fs::read_to_string(&argv[at + 9]).unwrap();
    assert!(overlay.contains("enableProjectConfig: false"), "{overlay}");
    assert!(
        overlay.contains("fetch:") && overlay.contains("web_search:"),
        "{overlay}"
    );
    assert!(overlay.contains("mcp__hutan_agents_board"), "{overlay}");
    assert!(
        !overlay.contains("memory_propose") && !overlay.contains("task_add"),
        "{overlay}"
    );

    // Only Hutan's server, limited to the unattended tools.
    let sessions: Vec<Value> = env
        .received()
        .into_iter()
        .filter(|msg| msg["method"] == "session/new")
        .collect();
    let digest_servers = sessions.last().unwrap()["params"]["mcpServers"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(digest_servers.len(), 1, "{digest_servers:?}");
    assert_eq!(digest_servers[0]["name"], "hutan");
    let args: Vec<&str> = digest_servers[0]["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(args.ends_with(&["--unattended"]), "{args:?}");

    // The answer is a message of the digest chat.
    let answer = env
        .client
        .chat_history_all(chat.id)
        .await
        .unwrap()
        .events
        .into_iter()
        .filter_map(|e| match e.kind {
            AgentEventKind::MessageEnd { text, .. } => Some(text),
            _ => None,
        })
        .next_back()
        .unwrap();
    assert!(answer.contains("1 request waits"), "{answer}");

    // Once per period: many looks later it did not run again.
    env.runs_stay(1).await;
    assert_eq!(env.prompts().len(), 3);
    // One notification for the whole run: not one more for each finished turn of the chat
    // (`/tools`, `/mcp list`, the digest).
    let about_the_chat: Vec<IntentKind> = env
        .client
        .notify_list()
        .await
        .unwrap()
        .into_iter()
        .filter(|i| i.session_id == chat.id)
        .map(|i| i.kind)
        .collect();
    assert_eq!(about_the_chat, [IntentKind::Digest]);
    asked.abort();
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_extra_tool_in_the_launch_skips_the_run_with_the_reason_and_no_model_call() {
    let mut env = Env::start(Options {
        transcript: "extra-tool",
        ..Options::default()
    })
    .await;
    let asked = env.a_request_waits().await;
    env.schedule("every 1 h").await;
    let digest = env.next_digest().await;
    assert_eq!(digest.title, "Digest could not run");
    assert!(digest.body.contains("`read`"), "{}", digest.body);
    let last = env.status_after_a_run().await.last_run.unwrap();
    assert_eq!(last.outcome, "skipped");
    assert!(last.reason.unwrap().contains("`read`"));

    // OMP answered `/tools` itself, and the digest's prompt was never sent.
    assert_eq!(env.prompts(), ["/tools"]);
    // The reason is in the digest chat too, and the chat is where the notification opens.
    let chat = env.digest_chat().await.unwrap();
    assert_eq!(digest.session_id, chat.id);
    let notices = env.notices(chat.id).await;
    assert!(
        notices
            .iter()
            .any(|n| n.contains("did not run") && n.contains("`read`")),
        "{notices:?}"
    );
    // Not retried in a loop.
    env.runs_stay(1).await;
    assert_eq!(env.prompts(), ["/tools"]);
    asked.abort();
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn another_mcp_server_in_the_launch_skips_the_run() {
    let mut env = Env::start(Options {
        transcript: "extra-mcp",
        ..Options::default()
    })
    .await;
    let asked = env.a_request_waits().await;
    env.schedule("every 1 h").await;
    let digest = env.next_digest().await;
    assert_eq!(digest.title, "Digest could not run");
    assert!(digest.body.contains("docs"), "{}", digest.body);
    let last = env.status_after_a_run().await.last_run.unwrap();
    assert_eq!(last.outcome, "skipped");
    assert_eq!(env.prompts(), ["/tools", "/mcp list"]);
    asked.abort();
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn another_omp_version_skips_with_reverify_unless_the_user_allows_it() {
    let mut env = Env::start(Options {
        omp_version: "omp/18.14.0",
        ..Options::default()
    })
    .await;
    let asked = env.a_request_waits().await;
    let status = env.client.digest_status().await.unwrap();
    assert!(!status.runnable);
    assert_eq!(status.omp_version.as_deref(), Some("18.14.0"));
    assert_eq!(status.verified_omp, "18.13.3");
    assert!(status.reason.unwrap().contains("re-verify"));

    env.schedule("every 1 h").await;
    let digest = env.next_digest().await;
    assert_eq!(digest.title, "Digest could not run");
    assert!(digest.body.contains("re-verify") && digest.body.contains("18.14.0"));
    let last = env.status_after_a_run().await.last_run.unwrap();
    assert_eq!(last.outcome, "skipped");
    // Nothing was started: no process, no chat.
    assert!(
        env.argv().iter().all(|a| a != "--no-tools"),
        "{:?}",
        env.argv()
    );
    assert!(env.prompts().is_empty());
    assert!(env.digest_chat().await.is_none());
    asked.abort();
    env.stop().await;

    // The opt-in lets it run.
    let mut env = Env::start(Options {
        omp_version: "omp/18.14.0",
        notify: "digest_model_unverified_omp = true",
        ..Options::default()
    })
    .await;
    let asked = env.a_request_waits().await;
    env.schedule("every 1 h").await;
    let digest = env.next_digest().await;
    assert_eq!(digest.title, "Digest");
    assert_eq!(
        env.status_after_a_run().await.last_run.unwrap().outcome,
        "done"
    );
    asked.abort();
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_code_and_codex_are_refused_until_proven() {
    for (backend, label, assistant) in [
        (
            "claude",
            "Claude Code",
            "backend = \"claude\"\nmodel = \"gitlab-duo/duo-chat-opus-4-6\"",
        ),
        ("codex", "Codex", "backend = \"codex\"\nmodel = \"gpt-5\""),
    ] {
        let mut env = Env::start(Options {
            assistant,
            ..Options::default()
        })
        .await;
        let asked = env.a_request_waits().await;
        env.schedule("every 1 h").await;
        let digest = env.next_digest().await;
        assert_eq!(digest.title, "Digest could not run", "{backend}");
        assert!(
            digest.body.contains(label) && digest.body.contains("OMP only"),
            "{}",
            digest.body
        );
        let status = env.status_after_a_run().await;
        assert_eq!(status.last_run.unwrap().outcome, "skipped");
        assert_eq!(status.harness.as_deref(), Some(backend));
        assert!(!status.runnable);
        // No digest chat, no process, no model call.
        assert!(env.digest_chat().await.is_none(), "{backend}");
        assert!(env.prompts().is_empty(), "{backend}");
        assert!(env.argv().iter().all(|a| a != "--no-tools"), "{backend}");
        asked.abort();
        env.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_run_is_cancelled_reported_and_not_retried() {
    let mut env = Env::start(Options {
        transcript: "stall",
        run_limit: Duration::from_millis(800),
        ..Options::default()
    })
    .await;
    let asked = env.a_request_waits().await;
    env.schedule("every 1 h").await;
    let digest = env.next_digest().await;
    assert_eq!(digest.title, "Digest could not run");
    assert!(digest.body.contains("stopped"), "{}", digest.body);
    let last = env.status_after_a_run().await.last_run.unwrap();
    assert_eq!(last.outcome, "timeout");
    // The agent was asked to cancel (the transcript expects `session/cancel`).
    assert!(
        env.received()
            .iter()
            .any(|m| m["method"] == "session/cancel"),
        "{:?}",
        env.received()
    );
    env.runs_stay(1).await;
    assert_eq!(env.prompts().len(), 3);
    asked.abort();
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_writable_launch_file_the_digest_does_not_start() {
    let mut env = Env::start(Options::default()).await;
    let asked = env.a_request_waits().await;
    // A regular file where the daemon's private `mcp` folder belongs.
    std::fs::write(env.daemon.paths.base.join("mcp"), b"in the way").unwrap();
    env.schedule("every 1 h").await;
    let digest = env.next_digest().await;
    assert_eq!(digest.title, "Digest could not run");
    let last = env.status_after_a_run().await.last_run.unwrap();
    assert_eq!(last.outcome, "skipped");
    let reason = last.reason.unwrap();
    assert!(
        reason.contains("launch settings") && reason.contains("omp-digest.yml"),
        "{reason}"
    );
    assert!(env.argv().is_empty(), "{:?}", env.argv());
    asked.abort();
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_window_in_which_nothing_happened_is_no_model_run() {
    let env = Env::start(Options::default()).await;
    env.schedule("every 1 h").await;
    let status = env.status_after_a_run().await;
    let last = status.last_run.unwrap();
    assert_eq!(last.outcome, "skipped");
    assert!(last.reason.unwrap().contains("nothing happened"));
    assert!(env.prompts().is_empty());
    assert!(env.digest_chat().await.is_none());
    assert!(env.client.notify_list().await.unwrap().is_empty());
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_schedule_setting_is_validated_and_shown() {
    let env = Env::start(Options::default()).await;
    for bad in ["hourly", "daily 25:00", "every 0 h"] {
        let error = env
            .client
            .daemon_setting_set("notify.digest_model", Some(json!(bad)))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("notify.digest_model"),
            "{bad}: {error}"
        );
    }
    env.schedule("daily 08:30").await;
    let status = env.client.digest_status().await.unwrap();
    assert_eq!(status.schedule, "daily 08:30");
    assert!(status.next_run_ms.is_some());
    env.stop().await;
}
