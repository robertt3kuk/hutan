use serde_json::json;

use super::*;

fn fixture(text: &str) -> Value {
    serde_json::from_str(text).expect("a fixture is JSON")
}

fn edit() -> Value {
    fixture(include_str!(
        "../../../tests/fixtures/insight/claude-posttooluse-edit.json"
    ))
}

#[test]
fn a_claude_edit_reports_the_tool_its_time_and_the_file() {
    let report = from_hook(HookAgentArg::Claude, "PostToolUse", &edit()).expect("insight");
    assert_eq!(report.source, InsightSourceKind::Hooks);
    assert_eq!(
        report.tools,
        vec![ReportedTool {
            name: "Edit".to_owned(),
            calls: 1,
            failed: 0,
            ms: Some(38)
        }]
    );
    assert_eq!(
        report.files,
        vec![ReportedFile {
            path: "/work/app/src/lib.rs".to_owned(),
            kind: FileChangeKind::Update
        }]
    );
    assert_eq!(report.turn, None);
    // Nothing of the tool's input but the path is carried.
    let json = serde_json::to_string(&report).expect("serialises");
    assert!(
        !json.contains("old_string") && !json.contains("new_string"),
        "{json}"
    );
}

#[test]
fn a_write_that_created_the_file_adds_it_and_a_failure_changes_none() {
    let created = fixture(include_str!(
        "../../../tests/fixtures/insight/claude-posttooluse-write-create.json"
    ));
    let report = from_hook(HookAgentArg::Claude, "PostToolUse", &created).expect("insight");
    assert_eq!(report.files[0].kind, FileChangeKind::Add);

    let failure = fixture(include_str!(
        "../../../tests/fixtures/insight/claude-posttooluse-failure.json"
    ));
    let report = from_hook(HookAgentArg::Claude, "PostToolUseFailure", &failure).expect("insight");
    assert_eq!(
        (report.tools[0].failed, report.tools[0].ms),
        (1, Some(4210))
    );
    assert!(report.files.is_empty());

    // A failed edit changed nothing.
    let report = from_hook(HookAgentArg::Claude, "PostToolUseFailure", &edit()).expect("insight");
    assert!(report.files.is_empty());
}

#[test]
fn a_tool_that_does_not_change_files_names_none() {
    let read = json!({
        "hook_event_name": "PostToolUse", "tool_name": "Read",
        "tool_input": {"file_path": "/work/app/src/lib.rs"}, "duration_ms": 5
    });
    let report = from_hook(HookAgentArg::Claude, "PostToolUse", &read).expect("insight");
    assert_eq!(report.tools.len(), 1);
    assert!(report.files.is_empty());
}

#[test]
fn codex_apply_patch_names_the_files_of_its_envelope_and_has_no_duration() {
    let patch = fixture(include_str!(
        "../../../tests/fixtures/insight/codex-posttooluse-apply-patch.json"
    ));
    let report = from_hook(HookAgentArg::Codex, "PostToolUse", &patch).expect("insight");
    let files: Vec<(&str, FileChangeKind)> = report
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.kind))
        .collect();
    assert_eq!(
        files,
        vec![
            ("src/lib.rs", FileChangeKind::Update),
            ("docs/new.md", FileChangeKind::Add),
            ("old.txt", FileChangeKind::Delete),
        ]
    );
    assert_eq!(report.tools[0].ms, None);
    // Codex's Bash is a tool and no file.
    let bash = json!({"hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_input": {"command": "ls"}});
    assert!(
        from_hook(HookAgentArg::Codex, "PostToolUse", &bash)
            .expect("insight")
            .files
            .is_empty()
    );
}

#[test]
fn the_edges_of_a_turn_are_the_prompt_and_the_stop_of_the_main_agent() {
    let start = from_hook(HookAgentArg::Claude, "UserPromptSubmit", &json!({})).expect("insight");
    assert_eq!(start.turn.map(|turn| turn.phase), Some(TurnPhase::Start));
    let stop = from_hook(HookAgentArg::Claude, "Stop", &json!({})).expect("insight");
    assert_eq!(stop.turn.map(|turn| turn.phase), Some(TurnPhase::End));
    let failed = from_hook(HookAgentArg::Claude, "StopFailure", &json!({})).expect("insight");
    assert_eq!(
        failed.turn.and_then(|turn| turn.outcome),
        Some(InsightOutcome::Failed)
    );
    // A subagent stopping ends nothing of the conversation's turn.
    assert_eq!(
        from_hook(
            HookAgentArg::Claude,
            "Stop",
            &json!({"agent_id": "agent-1"})
        ),
        None
    );
    assert_eq!(
        from_hook(HookAgentArg::Claude, "SessionStart", &json!({})),
        None
    );
    assert_eq!(
        from_hook(
            HookAgentArg::Claude,
            "PreToolUse",
            &json!({"tool_name": "Bash"})
        ),
        None
    );
}

#[test]
fn the_status_line_gives_the_context_the_cost_and_the_api_time() {
    let tick = fixture(include_str!(
        "../../../tests/fixtures/insight/claude-statusline.json"
    ));
    let report = from_statusline(&tick).expect("insight");
    assert_eq!(report.source, InsightSourceKind::Statusline);
    // input + cache creation + cache read of the last request, as Claude Code's own percentage.
    assert_eq!(
        report.context,
        Some(ReportedContext {
            used: 8 + 1_200 + 50_800,
            window: Some(200_000),
            fixed: None,
            estimated: false
        })
    );
    assert_eq!(report.cost_usd, Some(0.4127));
    assert_eq!(report.api_ms, Some(21_400));
    assert!(report.tokens.is_none() && report.tools.is_empty());
}

#[test]
fn before_the_first_request_the_status_line_has_a_cost_of_zero_and_no_context() {
    let tick = fixture(include_str!(
        "../../../tests/fixtures/insight/claude-statusline-first-request.json"
    ));
    let report = from_statusline(&tick).expect("insight");
    // Nothing to estimate from: the percentage is null.
    assert_eq!(report.context, None);
    assert_eq!(report.cost_usd, Some(0.0));
    assert_eq!(report.api_ms, Some(0));
}

#[test]
fn a_percentage_without_the_usage_is_an_estimate_and_nothing_at_all_is_not_a_report() {
    let tick = json!({"context_window": {"context_window_size": 200000, "used_percentage": 12.5, "current_usage": null}});
    let report = from_statusline(&tick).expect("insight");
    assert_eq!(
        report.context,
        Some(ReportedContext {
            used: 25_000,
            window: Some(200_000),
            fixed: None,
            estimated: true
        })
    );
    assert_eq!(from_statusline(&json!({"model": {"id": "x"}})), None);
    assert_eq!(from_statusline(&json!("nonsense")), None);
    let hostile = json!({"cost": {"total_cost_usd": -1.0, "total_api_duration_ms": "soon"}});
    assert_eq!(from_statusline(&hostile), None);
}
