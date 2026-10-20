//! `hutan config ls|get|set|reset` and `hutan assistant reset` against a real daemon on a
//! temporary `HUTAN_HOME`: the daemon edits its own `daemon.toml`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::TestDaemon;

fn file(t: &TestDaemon) -> String {
    std::fs::read_to_string(&t.paths.config_file).unwrap_or_default()
}

const RESTART: &str = "restart the daemon to apply (`hutan daemon restart`)";

#[tokio::test(flavor = "multi_thread")]
async fn config_ls_lists_every_setting_as_a_table_and_as_json() {
    let t = TestDaemon::start().await;
    let out = t.hutan(&["config", "ls"]).await.ok();
    let header = out.stdout.lines().next().unwrap();
    for column in ["KEY", "VALUE", "DEFAULT", "SET", "APPLIES", "PENDING"] {
        assert!(header.contains(column), "{header}");
    }
    let row = out
        .stdout
        .lines()
        .find(|line| line.starts_with("notify.enabled"))
        .unwrap_or_else(|| panic!("no notify.enabled row in\n{}", out.stdout));
    assert_eq!(
        row.split_whitespace().collect::<Vec<_>>(),
        ["notify.enabled", "true", "true", "-", "live", "-"]
    );
    assert!(
        out.stdout
            .contains(&format!("file: {}", t.paths.config_file.display()))
    );
    assert!(!out.stdout.contains(RESTART), "nothing is pending");

    let json = t.hutan(&["config", "ls", "--json"]).await.ok().json();
    assert_eq!(json["restart_pending"], false);
    let entries = json["entries"].as_array().unwrap();
    assert!(entries.len() > 40);
    let restore = entries
        .iter()
        .find(|e| e["key"] == "daemon.restore_sessions")
        .unwrap();
    assert_eq!(restore["kind"]["type"], "enum");
    assert_eq!(restore["applies"], "restart");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn config_set_get_and_reset_edit_the_file_and_say_when_a_restart_is_needed() {
    let t = TestDaemon::start_toml("# mine\n[notify]\nenabled = true # on\n", |_| {}).await;

    // A live setting: in use at once, nothing to restart.
    let out = t
        .hutan(&["config", "set", "notify.enabled", "false"])
        .await
        .ok();
    assert_eq!(out.stdout.trim_end(), "set: notify.enabled = false");
    assert_eq!(file(&t), "# mine\n[notify]\nenabled = false # on\n");
    assert_eq!(
        t.hutan(&["config", "get", "notify.enabled"])
            .await
            .ok()
            .stdout
            .trim_end(),
        "false"
    );
    let json = t
        .hutan(&["config", "get", "notify.enabled", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(json["value"], false);
    assert_eq!(json["set_in_file"], true);
    assert_eq!(json["pending_restart"], false);

    // Values are TOML; bare text is a string.
    t.hutan(&["config", "set", "tasks.max_parallel", "2"])
        .await
        .ok();
    t.hutan(&[
        "config",
        "set",
        "notify.kinds",
        "[\"task_failed\", \"task_done\"]",
    ])
    .await
    .ok();
    t.hutan(&["config", "set", "assistant.model", "\"some/model\""])
        .await
        .ok();
    let text = file(&t);
    assert!(text.contains("max_parallel = 2"), "{text}");
    assert!(
        text.contains("kinds = [\"task_failed\", \"task_done\"]"),
        "{text}"
    );
    assert!(text.contains("model = \"some/model\""), "{text}");

    // A setting that needs a restart says so, in the set and in the later reads.
    let out = t
        .hutan(&["config", "set", "daemon.restore_sessions", "stopped"])
        .await
        .ok();
    assert!(
        out.stdout
            .contains("set: daemon.restore_sessions = stopped"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains(RESTART), "{}", out.stdout);
    let get = t
        .hutan(&["config", "get", "daemon.restore_sessions"])
        .await
        .ok();
    assert!(get.stdout.starts_with("stopped"), "{}", get.stdout);
    assert!(get.stdout.contains(RESTART));
    let ls = t.hutan(&["config", "ls"]).await.ok();
    assert!(ls.stdout.contains(RESTART));
    assert_eq!(
        t.hutan(&["config", "ls", "--json"]).await.ok().json()["restart_pending"],
        true
    );

    // Reset removes the key: the default, which is what the daemon runs, so nothing is pending.
    let out = t
        .hutan(&["config", "reset", "daemon.restore_sessions"])
        .await
        .ok();
    assert_eq!(
        out.stdout.trim_end(),
        "reset: daemon.restore_sessions = respawn (the default)"
    );
    assert!(!file(&t).contains("restore_sessions"));
    assert!(!file(&t).contains("[daemon]"), "{}", file(&t));
    t.hutan(&["config", "reset", "tasks.max_parallel"])
        .await
        .ok();
    assert!(!file(&t).contains("max_parallel"));
    assert!(file(&t).starts_with("# mine\n[notify]\n"));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_servers_are_added_edited_and_removed_with_config() {
    let t = TestDaemon::start_toml("# mine\n[notify]\nenabled = true\n", |_| {}).await;

    let out = t
        .hutan(&[
            "config",
            "set",
            "mcp.docs",
            "{command = \"npx\", args = [\"-y\", \"docs\"]}",
        ])
        .await
        .ok();
    assert!(
        out.stdout
            .starts_with("set: mcp.docs = {\"command\":\"npx\",\"args\":[\"-y\",\"docs\"]}"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains(RESTART), "{}", out.stdout);
    assert!(file(&t).contains("[mcp.docs]\ncommand = \"npx\"\nargs = [\"-y\", \"docs\"]\n"));
    assert!(file(&t).starts_with("# mine\n[notify]\n"));

    // Edit: a remote server in its place.
    t.hutan(&[
        "config",
        "set",
        "mcp.docs",
        "{url = \"https://x.test/mcp\", transport = \"sse\"}",
    ])
    .await
    .ok();
    assert!(!file(&t).contains("npx"), "{}", file(&t));
    assert!(file(&t).contains("transport = \"sse\""));

    // Refused: not both, and a name outside the rule.
    for (args, wanted) in [
        (
            &[
                "config",
                "set",
                "mcp.both",
                "{command = \"x\", url = \"https://a\"}",
            ][..],
            "not both",
        ),
        (
            &["config", "set", "mcp.Bad", "{command = \"x\"}"],
            "lowercase",
        ),
    ] {
        let out = t.hutan(args).await;
        assert_eq!(out.code, 1, "{args:?}: {out:?}");
        assert!(out.stderr.contains(wanted), "{args:?}: {}", out.stderr);
    }

    // Remove.
    let out = t.hutan(&["config", "reset", "mcp.docs"]).await.ok();
    assert!(
        out.stdout.starts_with("removed: mcp.docs"),
        "{}",
        out.stdout
    );
    assert!(!file(&t).contains("mcp"), "{}", file(&t));
    assert!(file(&t).starts_with("# mine\n[notify]\n"));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn config_refuses_what_the_daemon_refuses() {
    let t = TestDaemon::start().await;
    for (args, wanted) in [
        (
            &["config", "set", "tasks.max_parallel", "0"][..],
            "at least 1",
        ),
        (
            &["config", "set", "tasks.max_parallel", "many"],
            "whole number",
        ),
        (
            &["config", "set", "control.agents", "sometimes"],
            "ask, allow, read_only",
        ),
        (
            &["config", "set", "nope.nothing", "1"],
            "not a daemon setting",
        ),
        (
            &["config", "set", "codex.bin", "/definitely/not/here"],
            "not usable",
        ),
        (&["config", "reset", "nope.nothing"], "not a daemon setting"),
        (&["config", "get", "nope.nothing"], "not a setting"),
    ] {
        let out = t.hutan(args).await;
        assert_eq!(out.code, 1, "{args:?}: {out:?}");
        assert!(out.stderr.contains(wanted), "{args:?}: {}", out.stderr);
    }
    assert_eq!(file(&t), "", "nothing was written");
    // A negative number is a value, not an option.
    let out = t
        .hutan(&["config", "set", "notify.min_turn_secs", "-1"])
        .await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("at least 0"), "{}", out.stderr);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn assistant_reset_names_the_scope_it_resets() {
    let t = TestDaemon::start().await;
    let out = t.hutan(&["assistant", "reset"]).await.ok();
    assert_eq!(
        out.stdout.trim_end(),
        "the global assistant inherits its backend again"
    );
    let json = t.hutan(&["assistant", "reset", "--json"]).await.ok().json();
    assert_eq!(json["scope"], "global");
    assert_eq!(json["workspace_id"], serde_json::Value::Null);
    let out = t
        .hutan(&["assistant", "reset", "--ws", "no-such-workspace"])
        .await;
    assert_eq!(out.code, 1, "{out:?}");
    t.stop().await;
}
