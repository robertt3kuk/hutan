//! Agent chats in GPUI tests: a daemon whose `codex app-server` is `hutan-codex-fixture
//! replay` over a recorded (or edited) transcript, and helpers to read what the fake server
//! received.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use hutan_agents::CodexConfig;
use hutan_core::SessionId;
use hutan_protocol::chat::ChatCreate;
use hutan_protocol::messages::{Request, SessionInfo, SessionResult};
use serde_json::{Value, json};

use super::TestDaemon;

const FIXTURE_BIN_NAME: &str = "hutan-codex-fixture";

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/codex-0.162")
}

/// The fake app server, next to the test binaries; `just test` builds the workspace binaries
/// first.
pub fn fixture_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join(FIXTURE_BIN_NAME);
    assert!(
        bin.is_file(),
        "{FIXTURE_BIN_NAME} not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    bin
}

/// A scratch directory: the agent's working directory, edited transcripts, and the log of what
/// the fake server received.
pub struct ChatEnv {
    pub work: tempfile::TempDir,
}

impl ChatEnv {
    pub fn new() -> Self {
        Self {
            work: tempfile::tempdir().unwrap(),
        }
    }

    /// The agent's working directory (canonical, so it compares equal to what the daemon says).
    pub fn cwd(&self) -> PathBuf {
        self.work.path().canonicalize().unwrap()
    }

    pub fn received_path(&self) -> PathBuf {
        self.work.path().join("received.jsonl")
    }

    /// A codex that replays the committed fixture `name`.
    pub fn fixture(&self, name: &str) -> CodexConfig {
        self.replay(&fixtures().join(format!("{name}.jsonl")))
    }

    /// A codex that replays `transcript`.
    pub fn replay(&self, transcript: &Path) -> CodexConfig {
        CodexConfig {
            program: fixture_bin(),
            args: vec![
                "replay".to_owned(),
                "--received".to_owned(),
                self.received_path().display().to_string(),
                transcript.display().to_string(),
                "app-server".to_owned(),
            ],
            env: Vec::new(),
        }
    }

    /// The fixture `name` after `edit` changed its entries (`{"dir", "msg"}` objects).
    pub fn edited(&self, name: &str, edit: impl FnOnce(&mut Vec<Value>)) -> CodexConfig {
        let text = std::fs::read_to_string(fixtures().join(format!("{name}.jsonl"))).unwrap();
        let mut entries: Vec<Value> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        edit(&mut entries);
        let path = self.work.path().join(format!("{name}-edited.jsonl"));
        let out: Vec<String> = entries.iter().map(Value::to_string).collect();
        std::fs::write(&path, out.join("\n") + "\n").unwrap();
        self.replay(&path)
    }

    /// Everything the fake server received, as `(method or "response", message)`.
    pub fn received(&self) -> Vec<(String, Value)> {
        std::fs::read_to_string(self.received_path())
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let wrapper: Value = serde_json::from_str(line).unwrap();
                let msg = wrapper["msg"].clone();
                (msg["method"].as_str().unwrap_or("response").to_owned(), msg)
            })
            .collect()
    }

    /// The texts of the `turn/start` requests the server received.
    pub fn sent_texts(&self) -> Vec<String> {
        self.received()
            .into_iter()
            .filter(|(method, _)| method == "turn/start")
            .filter_map(|(_, msg)| {
                msg["params"]["input"][0]["text"]
                    .as_str()
                    .map(str::to_owned)
            })
            .collect()
    }
}

/// A transcript entry the server says.
pub fn says(msg: Value) -> Value {
    serde_json::json!({"dir": "in", "msg": msg})
}

/// The method of a transcript entry, if it is a notification or request.
pub fn method(entry: &Value) -> Option<&str> {
    entry["msg"]["method"].as_str()
}

impl TestDaemon {
    /// A chat session working in `cwd`.
    ///
    /// The chat goes to Home, where the desktop of this version shows it: since protocol 0.19 a
    /// chat without a workspace would make the project of its folder, which the sidebar only
    /// learns to show with the project sidebar (M18-B).
    pub fn create_chat(&self, cwd: &Path) -> SessionInfo {
        let workspaces: hutan_protocol::messages::WorkspaceList =
            self.call(Request::WorkspaceList(hutan_protocol::messages::Empty {}));
        let home = workspaces
            .workspaces
            .iter()
            .find(|w| w.kind == hutan_protocol::messages::WorkspaceKind::Home)
            .map(|w| w.id);
        let created: SessionResult = self.call(Request::ChatCreate(ChatCreate {
            workspace_id: home,
            cwd: cwd.to_path_buf(),
            agent: "codex".to_owned(),
            model: None,
            approval_policy: None,
            sandbox: None,
            title: None,
            mode: None,
            provider_id: None,
            system_prompt: None,
            account: None,
            safety: None,
        }));
        created.session
    }

    #[allow(dead_code)]
    pub fn chat_session_id(&self, cwd: &Path) -> SessionId {
        self.create_chat(cwd).id
    }
}

/// `plain-turn` with its turn repeated once per gate, each held after `turn/started` until its
/// gate exists (the test edits files meanwhile, as the agent would); each turn has ids of its
/// own.
pub fn held_turns(dir: &Path, gates: &[PathBuf]) -> PathBuf {
    let real = fs::read_to_string(fixtures().join("plain-turn.jsonl")).unwrap();
    let lines: Vec<&str> = real.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains("\"method\":\"turn/start\""))
        .unwrap();
    let mut out: Vec<String> = lines[..at].iter().map(|l| (*l).to_owned()).collect();
    for (n, gate) in gates.iter().enumerate() {
        for line in &lines[at..] {
            let line = line
                .replace(
                    "01a00000-0000-7000-8000-000000000002",
                    &format!("01a00000-0000-7000-8000-0000000010{n:02}"),
                )
                .replace(
                    "01a00000-0000-7000-8000-000000000003",
                    &format!("01a00000-0000-7000-8000-0000000020{n:02}"),
                )
                .replace("msg_fixture_1", &format!("msg_fixture_t{n}"));
            let started = line.contains("\"method\":\"turn/started\"");
            out.push(line);
            if started {
                out.push(json!({"dir": "hold", "file": gate}).to_string());
            }
        }
    }
    let transcript = dir.join("held-turns.jsonl");
    fs::write(&transcript, out.join("\n") + "\n").unwrap();
    transcript
}
