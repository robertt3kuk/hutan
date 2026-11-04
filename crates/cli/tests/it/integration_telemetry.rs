//! `hutan integration install claude|codex --telemetry`, its status words and its conflicts, and
//! `hutan telemetry env` (TEL-2, ADR 0026 decision 8), on temporary copies of the agents'
//! configuration. Every command gets `--dir` and a temporary `HUTAN_HOME` (`daemon.toml` with the
//! receiver's port) and `HUTAN_MANAGED_CONFIG_ROOT`: nothing here can reach the user's own agents
//! or a managed policy of this computer.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use common::{HUTAN, Out, TestDaemon, command, finish};
use serde_json::{Value, json};
use tempfile::TempDir;

const PORT: u16 = 50123;

/// A throwaway computer: the agent's folder, the Hutan home with the receiver's port, and an
/// empty managed-policy root.
struct World {
    _root: TempDir,
    agent: PathBuf,
    hutan_home: PathBuf,
    managed: PathBuf,
}

impl World {
    fn new(port: Option<u16>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent dir");
        let hutan_home = root.path().join("hutan-home");
        let managed = root.path().join("managed");
        for dir in [&agent, &hutan_home, &managed] {
            fs::create_dir_all(dir).unwrap();
        }
        if let Some(port) = port {
            fs::write(
                hutan_home.join("daemon.toml"),
                format!("[telemetry]\nport = {port}\n"),
            )
            .unwrap();
        }
        Self {
            _root: root,
            agent,
            hutan_home,
            managed,
        }
    }

    fn set_port(&self, port: u16) {
        fs::write(
            self.hutan_home.join("daemon.toml"),
            format!("[telemetry]\nport = {port}\n"),
        )
        .unwrap();
    }

    async fn hutan(&self, args: &[&str]) -> Out {
        let mut full = args.to_vec();
        full.extend(["--dir", self.agent.to_str().unwrap()]);
        let mut command = command(Path::new(HUTAN), None, &full);
        command
            .env("HUTAN_HOME", &self.hutan_home)
            .env("HUTAN_MANAGED_CONFIG_ROOT", &self.managed)
            .env("HOME", &self.hutan_home);
        for variable in [
            "CLAUDE_CODE_ENABLE_TELEMETRY",
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
            "OTEL_LOGS_EXPORTER",
        ] {
            command.env_remove(variable);
        }
        finish(command).await
    }

    fn file(&self, name: &str) -> PathBuf {
        self.agent.join(name)
    }
}

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap();
    text.push('\n');
    text
}

fn backups(dir: &Path, name: &str) -> usize {
    fs::read_dir(dir)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("{name}.hutan-"))
        })
        .count()
}

fn claude_settings() -> Value {
    json!({
        "model": "opus",
        "env": {"EDITOR": "vim"},
        "permissions": {"allow": ["Bash(ls:*)"]}
    })
}

const CODEX_CONFIG: &str = "model = \"gpt-5\"\n\n[projects.\"/work\"]\ntrust_level = \"trusted\"\n";

#[tokio::test]
async fn claude_gets_its_settings_and_a_helper_and_an_uninstall_leaves_the_original_bytes() {
    let world = World::new(Some(PORT));
    let original = pretty(&claude_settings());
    fs::write(world.file("settings.json"), &original).unwrap();

    let out = world
        .hutan(&["integration", "install", "claude", "--telemetry", "--json"])
        .await
        .ok();
    let report = out.json();
    assert_eq!(report["telemetry"]["status"], "installed", "{report}");
    assert_eq!(report["telemetry"]["conflicts"], json!([]));

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(world.file("settings.json")).unwrap()).unwrap();
    // Hutan's keys, and the user's own beside them.
    assert_eq!(settings["env"]["EDITOR"], "vim");
    assert_eq!(settings["env"]["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
    assert_eq!(settings["env"]["OTEL_LOGS_EXPORTER"], "otlp");
    assert_eq!(
        settings["env"]["OTEL_EXPORTER_OTLP_PROTOCOL"],
        "http/protobuf"
    );
    assert_eq!(
        settings["env"]["OTEL_EXPORTER_OTLP_ENDPOINT"],
        format!("http://127.0.0.1:{PORT}")
    );
    // Logs only (the mapper reads no metrics or traces) and every content gate off.
    assert!(settings["env"].get("OTEL_METRICS_EXPORTER").is_none());
    assert!(settings["env"].get("OTEL_TRACES_EXPORTER").is_none());
    for gate in [
        "OTEL_LOG_USER_PROMPTS",
        "OTEL_LOG_TOOL_DETAILS",
        "OTEL_LOG_TOOL_CONTENT",
    ] {
        assert_eq!(settings["env"][gate], "0", "{gate}");
    }
    let helper = world.file("hooks/hutan-otel-headers.sh");
    assert_eq!(settings["otelHeadersHelper"], helper.to_str().unwrap());
    // No token, no port-specific secret in any file.
    let on_disk = fs::read_to_string(world.file("settings.json")).unwrap();
    assert!(!on_disk.contains("/s/"), "{on_disk}");
    assert_eq!(
        fs::metadata(&helper).unwrap().permissions().mode() & 0o111,
        0o111
    );

    // The helper prints the session's token from the environment of the process that runs it,
    // and no header outside a Hutan terminal.
    let token = "0123456789abcdef0123456789abcdef";
    let mut run = command(&helper, None, &[]);
    run.env("HUTAN_OTLP_TOKEN", token);
    let printed = finish(run).await.ok().stdout;
    assert_eq!(
        serde_json::from_str::<Value>(&printed).unwrap(),
        json!({"x-hutan-token": token})
    );
    let printed = finish(command(&helper, None, &[])).await.ok().stdout;
    assert_eq!(serde_json::from_str::<Value>(&printed).unwrap(), json!({}));
    let mut run = command(&helper, None, &[]);
    run.env("HUTAN_OTLP_TOKEN", "x\"; echo hacked");
    let printed = finish(run).await.ok().stdout;
    assert_eq!(serde_json::from_str::<Value>(&printed).unwrap(), json!({}));

    // Installing again changes nothing: same bytes, no second backup.
    let installed = fs::read_to_string(world.file("settings.json")).unwrap();
    let backups_before = backups(&world.agent, "settings.json");
    let again = world
        .hutan(&["integration", "install", "claude", "--telemetry", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(again["changed"], false, "{again}");
    assert_eq!(
        fs::read_to_string(world.file("settings.json")).unwrap(),
        installed
    );
    assert_eq!(backups(&world.agent, "settings.json"), backups_before);

    // Uninstalling restores the file byte for byte and removes the helper.
    world
        .hutan(&["integration", "uninstall", "claude"])
        .await
        .ok();
    assert_eq!(
        fs::read_to_string(world.file("settings.json")).unwrap(),
        original
    );
    assert!(!helper.exists());
    let status = world
        .hutan(&["integration", "status", "claude", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(status["telemetry"]["status"], "not_installed");
}

#[tokio::test]
async fn codex_gets_an_otel_block_with_a_log_exporter_only_and_an_uninstall_leaves_the_original_bytes()
 {
    let world = World::new(Some(PORT));
    for original in [CODEX_CONFIG, "model = \"gpt-5\"", ""] {
        if original.is_empty() {
            let _ = fs::remove_file(world.file("config.toml"));
        } else {
            fs::write(world.file("config.toml"), original).unwrap();
        }
        world
            .hutan(&["integration", "install", "codex", "--telemetry"])
            .await
            .ok();
        let text = fs::read_to_string(world.file("config.toml")).unwrap();
        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        let otel = doc["otel"].as_table().unwrap();
        assert_eq!(
            otel["exporter"]["otlp-http"]["endpoint"].as_str(),
            Some(format!("http://127.0.0.1:{PORT}/v1/logs").as_str()),
            "{text}"
        );
        assert_eq!(
            otel["exporter"]["otlp-http"]["protocol"].as_str(),
            Some("binary")
        );
        assert_eq!(otel["trace_exporter"].as_str(), Some("none"));
        assert_eq!(otel["metrics_exporter"].as_str(), Some("none"));
        assert_eq!(otel["log_user_prompt"].as_bool(), Some(false));
        // The file holds no token: Codex's `headers` are literal and a token is per session.
        assert!(!text.contains("token") && !text.contains("/s/"), "{text}");
        let status = world
            .hutan(&["integration", "status", "codex", "--json"])
            .await
            .ok()
            .json();
        assert_eq!(status["telemetry"]["status"], "installed", "{status}");

        world
            .hutan(&["integration", "uninstall", "codex"])
            .await
            .ok();
        let after = fs::read_to_string(world.file("config.toml")).unwrap_or_default();
        assert_eq!(after, original, "from {original:?}");
    }
}

#[tokio::test]
async fn nothing_is_written_without_the_receivers_port() {
    let world = World::new(None);
    let original = pretty(&claude_settings());
    fs::write(world.file("settings.json"), &original).unwrap();
    let out = world
        .hutan(&["integration", "install", "claude", "--telemetry"])
        .await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stderr.contains("hutan telemetry enable claude"),
        "{}",
        out.stderr
    );
    assert_eq!(
        fs::read_to_string(world.file("settings.json")).unwrap(),
        original
    );
    assert!(!world.file("hooks").exists());
}

#[tokio::test]
async fn an_otel_setting_of_the_user_a_policy_or_the_environment_is_reported_and_never_overridden()
{
    let world = World::new(Some(PORT));

    // The user's own settings export to their collector.
    let ours = pretty(&json!({
        "env": {"OTEL_EXPORTER_OTLP_ENDPOINT": "https://collector.example", "OTEL_EXPORTER_OTLP_HEADERS": "Authorization=Bearer secret-value"},
        "otelHeadersHelper": "/usr/local/bin/their-helper"
    }));
    fs::write(world.file("settings.json"), &ours).unwrap();
    let out = world
        .hutan(&["integration", "install", "claude", "--telemetry"])
        .await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("Nothing was changed"), "{}", out.stderr);
    assert!(
        out.stderr.contains("OTEL_EXPORTER_OTLP_ENDPOINT"),
        "{}",
        out.stderr
    );
    assert!(!out.stderr.contains("secret-value"), "{}", out.stderr);
    assert_eq!(
        fs::read_to_string(world.file("settings.json")).unwrap(),
        ours
    );
    assert!(!world.file("hooks").exists(), "no helper was written");
    // `status` says so, in words, with where it is, and without the values.
    let text = world
        .hutan(&["integration", "status", "claude"])
        .await
        .ok()
        .stdout;
    assert!(text.contains("telemetry conflict:"), "{text}");
    assert!(
        text.contains("settings.json sets OTEL_EXPORTER_OTLP_ENDPOINT in `env`"),
        "{text}"
    );
    assert!(text.contains("sets `otelHeadersHelper`"), "{text}");
    assert!(text.contains("does not change them"), "{text}");
    assert!(
        !text.contains("secret-value") && !text.contains("collector.example"),
        "{text}"
    );
    let json = world
        .hutan(&["integration", "status", "claude", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(json["telemetry"]["status"], "not_installed");
    assert_eq!(json["telemetry"]["conflicts"].as_array().unwrap().len(), 3);

    // A managed policy.
    fs::write(world.file("settings.json"), pretty(&claude_settings())).unwrap();
    fs::create_dir_all(world.managed.join("ClaudeCode")).unwrap();
    fs::write(
        world.managed.join("ClaudeCode/managed-settings.json"),
        r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"1"}}"#,
    )
    .unwrap();
    let out = world
        .hutan(&["integration", "install", "claude", "--telemetry"])
        .await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("the managed policy"), "{}", out.stderr);
    assert_eq!(
        fs::read_to_string(world.file("settings.json")).unwrap(),
        pretty(&claude_settings())
    );
    fs::remove_file(world.managed.join("ClaudeCode/managed-settings.json")).unwrap();

    // The shell's own environment.
    let mut command = command(
        Path::new(HUTAN),
        None,
        &[
            "integration",
            "install",
            "claude",
            "--telemetry",
            "--dir",
            world.agent.to_str().unwrap(),
        ],
    );
    command
        .env("HUTAN_HOME", &world.hutan_home)
        .env("HUTAN_MANAGED_CONFIG_ROOT", &world.managed)
        .env("OTEL_LOGS_EXPORTER", "console");
    let out = finish(command).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stderr
            .contains("OTEL_LOGS_EXPORTER is set in the environment of this shell"),
        "{}",
        out.stderr
    );

    // Codex: an [otel] table of the user's, and one of a managed config.
    let theirs = "model = \"gpt-5\"\n\n[otel]\nenvironment = \"dev\"\n";
    fs::write(world.file("config.toml"), theirs).unwrap();
    let out = world
        .hutan(&["integration", "install", "codex", "--telemetry"])
        .await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stderr.contains("already has an [otel] table"),
        "{}",
        out.stderr
    );
    assert_eq!(
        fs::read_to_string(world.file("config.toml")).unwrap(),
        theirs
    );
    assert!(!world.file("hooks.json").exists());
    fs::write(world.file("config.toml"), CODEX_CONFIG).unwrap();
    fs::create_dir_all(world.managed.join("codex")).unwrap();
    fs::write(
        world.managed.join("codex/managed_config.toml"),
        "[otel]\nexporter = \"none\"\n",
    )
    .unwrap();
    let out = world
        .hutan(&["integration", "install", "codex", "--telemetry"])
        .await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("managed policy"), "{}", out.stderr);
    assert_eq!(
        fs::read_to_string(world.file("config.toml")).unwrap(),
        CODEX_CONFIG
    );
}

#[tokio::test]
async fn status_says_what_to_do_and_notices_a_port_that_changed() {
    let world = World::new(Some(PORT));
    let text = world
        .hutan(&["integration", "status", "claude"])
        .await
        .ok()
        .stdout;
    assert!(text.contains("telemetry: not installed"), "{text}");
    assert!(
        text.contains("telemetry is not enabled for Claude Code: `hutan telemetry enable claude`"),
        "{text}"
    );
    world
        .hutan(&["integration", "install", "claude", "--telemetry"])
        .await
        .ok();
    let text = world
        .hutan(&["integration", "status", "claude"])
        .await
        .ok()
        .stdout;
    assert!(text.contains("telemetry: installed"), "{text}");

    // The daemon's port moved (a busy one was replaced by hand): the installed settings are stale.
    world.set_port(50999);
    let status = world
        .hutan(&["integration", "status", "claude", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(status["telemetry"]["status"], "partial", "{status}");
    assert!(
        status["telemetry"]["missing"][0]
            .as_str()
            .unwrap()
            .contains("install claude --telemetry"),
        "{status}"
    );
    // Installing again follows the port and keeps one set of keys.
    world
        .hutan(&["integration", "install", "claude", "--telemetry"])
        .await
        .ok();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(world.file("settings.json")).unwrap()).unwrap();
    assert_eq!(
        settings["env"]["OTEL_EXPORTER_OTLP_ENDPOINT"],
        "http://127.0.0.1:50999"
    );
}

// ---- hutan telemetry env, against a real daemon -------------------------------------------------

/// `hutan telemetry env <agent>` the way a shell function runs it in a Hutan terminal.
async fn env_lines(
    t: &TestDaemon,
    agent: &str,
    agent_dir: &Path,
    managed: &Path,
    extra: &[(&str, &str)],
    otlp: bool,
) -> String {
    let mut command = command(
        Path::new(HUTAN),
        Some(&t.paths.socket),
        &["telemetry", "env", agent],
    );
    command
        .env("HOME", agent_dir)
        .env("CLAUDE_CONFIG_DIR", agent_dir)
        .env("CODEX_HOME", agent_dir)
        .env("HUTAN_MANAGED_CONFIG_ROOT", managed)
        .env_remove("OTEL_LOGS_EXPORTER");
    if otlp {
        command
            .env(
                "HUTAN_OTLP_ENDPOINT",
                "http://127.0.0.1:50123/s/0123456789abcdef0123456789abcdef",
            )
            .env("HUTAN_OTLP_TOKEN", "0123456789abcdef0123456789abcdef");
    }
    for (key, value) in extra {
        command.env(key, value);
    }
    let out = finish(command).await;
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(
        out.stderr, "",
        "a shell function throws this away, but it never speaks"
    );
    out.stdout
}

#[tokio::test(flavor = "multi_thread")]
async fn env_prints_what_goes_in_front_of_one_launch_and_nothing_whenever_hutan_has_nothing_to_add()
{
    let t = TestDaemon::start().await;
    let dir = tempfile::tempdir().unwrap();
    let managed = dir.path().join("managed");
    let agent = dir.path().join("agent");
    fs::create_dir_all(&agent).unwrap();
    let ask = |name: &'static str, extra: Vec<(&'static str, &'static str)>, otlp: bool| {
        let (t, agent, managed) = (&t, agent.clone(), managed.clone());
        async move { env_lines(t, name, &agent, &managed, &extra, otlp).await }
    };

    // The receiver is off: nothing, whatever else is true.
    assert_eq!(ask("claude", vec![], true).await, "");

    t.hutan(&["telemetry", "enable", "claude"]).await.ok();
    // Not in a Hutan terminal (no HUTAN_OTLP_*): nothing. An agent that is not enabled: nothing.
    assert_eq!(ask("claude", vec![], false).await, "");
    assert_eq!(ask("codex", vec![], true).await, "");
    assert_eq!(ask("omp", vec![], true).await, "");

    // Enabled, in a terminal: Claude Code's variables, logs only, the endpoint with the token in
    // its path, every content gate off.
    let text = ask("claude", vec![], true).await;
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.iter().all(|line| line.starts_with("env ")), "{text}");
    for wanted in [
        "env CLAUDE_CODE_ENABLE_TELEMETRY=1",
        "env OTEL_LOGS_EXPORTER=otlp",
        "env OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf",
        "env OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:50123/s/0123456789abcdef0123456789abcdef",
        "env OTEL_LOG_USER_PROMPTS=0",
        "env OTEL_LOG_TOOL_CONTENT=0",
    ] {
        assert!(lines.contains(&wanted), "{wanted} missing:\n{text}");
    }
    assert!(
        !text.contains("METRICS") && !text.contains("TRACES"),
        "{text}"
    );

    // Anything already there wins, and Hutan says nothing: the user's own variable...
    assert_eq!(
        ask(
            "claude",
            vec![("OTEL_EXPORTER_OTLP_ENDPOINT", "https://theirs.example")],
            true
        )
        .await,
        ""
    );
    // ...their settings, a managed policy...
    fs::write(
        agent.join("settings.json"),
        r#"{"env":{"OTEL_LOGS_EXPORTER":"console"}}"#,
    )
    .unwrap();
    assert_eq!(ask("claude", vec![], true).await, "");
    fs::remove_file(agent.join("settings.json")).unwrap();
    fs::create_dir_all(managed.join("ClaudeCode")).unwrap();
    fs::write(
        managed.join("ClaudeCode/managed-settings.json"),
        r#"{"otelHeadersHelper":"/policy/helper"}"#,
    )
    .unwrap();
    assert_eq!(ask("claude", vec![], true).await, "");
    fs::remove_file(managed.join("ClaudeCode/managed-settings.json")).unwrap();
    assert_ne!(ask("claude", vec![], true).await, "");

    // Codex, once enabled: the token in a header variable and the log exporter as `-c` arguments;
    // the token is in no argument.
    t.hutan(&["telemetry", "enable", "codex"]).await.ok();
    let text = ask("codex", vec![], true).await;
    assert!(
        text.contains(
            "env OTEL_EXPORTER_OTLP_LOGS_HEADERS=x-hutan-token=0123456789abcdef0123456789abcdef"
        ),
        "{text}"
    );
    let args: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("arg "))
        .collect();
    assert!(args.contains(&"arg otel.trace_exporter=\"none\""), "{text}");
    assert!(
        args.iter()
            .any(|arg| arg.contains("endpoint=\"http://127.0.0.1:50123/v1/logs\"")),
        "{text}"
    );
    assert!(
        args.iter().all(|arg| !arg.contains("0123456789abcdef")),
        "{text}"
    );
    // Their [otel] table: nothing.
    fs::write(agent.join("config.toml"), "[otel]\nenvironment = \"dev\"\n").unwrap();
    assert_eq!(ask("codex", vec![], true).await, "");
    // Hutan's own block in config.toml: only the header, the endpoint is already there.
    fs::remove_file(agent.join("config.toml")).unwrap();
    let mut install = command(
        Path::new(HUTAN),
        None,
        &[
            "integration",
            "install",
            "codex",
            "--telemetry",
            "--dir",
            agent.to_str().unwrap(),
        ],
    );
    install
        .env("HUTAN_HOME", t.paths.base.clone())
        .env("HUTAN_MANAGED_CONFIG_ROOT", &managed);
    finish(install).await.ok();
    let text = ask("codex", vec![], true).await;
    assert!(
        text.starts_with("env OTEL_EXPORTER_OTLP_LOGS_HEADERS="),
        "{text}"
    );
    assert!(!text.contains("arg "), "{text}");
    t.stop().await;
}
