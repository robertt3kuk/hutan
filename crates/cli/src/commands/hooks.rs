//! `hutan hooks list|test|log`: the user's `[[hooks]]` of `daemon.toml`, what they last did, and
//! a way to try one (protocol 0.49, capability `hooks`; `docs/integrations.md` § User hooks).
//!
//! Hooks are numbered from 1 here, as a person counts; the wire counts from 0.

use hutan_client::Client;
use hutan_protocol::hooks::{
    HookInfo, HookOutcome, HookRun, HooksList, HooksLogRequest, HooksTested,
};

use super::Ctx;
use super::sessions::{age_text, now_ms};
use crate::args::{HooksCommand, HooksLogArgs, HooksTestArgs};
use crate::error::CliError;

/// Most characters of a hook's command in the list.
const COMMAND_WIDTH: usize = 44;

impl Ctx {
    pub(super) async fn hooks(&self, command: HooksCommand) -> Result<(), CliError> {
        let client = self.connect().await?;
        if !client.supports_hooks() {
            return Err(CliError::Failed(
                "this hutand has no user hooks (it is older than protocol 0.49); restart it with \
                 `hutan daemon restart`"
                    .to_owned(),
            ));
        }
        match command {
            HooksCommand::List => self.hooks_list(&client).await,
            HooksCommand::Test(args) => self.hooks_test(&client, args).await,
            HooksCommand::Log(args) => self.hooks_log(&client, args).await,
        }
    }

    async fn hooks_list(&self, client: &Client) -> Result<(), CliError> {
        let list = client.hooks_list().await.map_err(|e| self.fail(e))?;
        self.print(&list, || list_text(&list, now_ms()))
    }

    async fn hooks_test(&self, client: &Client, args: HooksTestArgs) -> Result<(), CliError> {
        let hook = self.hook_index(client, &args.hook).await?;
        let tested = client.hooks_test(hook).await.map_err(|e| self.fail(e))?;
        self.print(&tested, || test_text(&tested))?;
        match tested.run.outcome {
            HookOutcome::Ok => Ok(()),
            _ => Err(CliError::Reported),
        }
    }

    async fn hooks_log(&self, client: &Client, args: HooksLogArgs) -> Result<(), CliError> {
        let hook = match &args.hook {
            Some(target) => Some(self.hook_index(client, target).await?),
            None => None,
        };
        let log = client
            .hooks_log(HooksLogRequest {
                hook,
                limit: Some(args.limit),
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&log, || log_text(&log.runs))
    }

    /// The wire index of the hook a person named: a number from 1, or a `name`.
    async fn hook_index(&self, client: &Client, target: &str) -> Result<u32, CliError> {
        let list = client.hooks_list().await.map_err(|e| self.fail(e))?;
        resolve(&list, target)
    }
}

fn resolve(list: &HooksList, target: &str) -> Result<u32, CliError> {
    if list.hooks.is_empty() {
        return Err(CliError::Failed(
            "no hooks: add `[[hooks]]` to daemon.toml and restart the daemon (docs/integrations.md)"
                .to_owned(),
        ));
    }
    if let Ok(number) = target.parse::<u32>() {
        return match number.checked_sub(1) {
            Some(index) if (index as usize) < list.hooks.len() => Ok(index),
            _ => Err(CliError::Usage(format!(
                "there is no hook {number}: daemon.toml has {} (numbered from 1)",
                list.hooks.len()
            ))),
        };
    }
    let named: Vec<&HookInfo> = list
        .hooks
        .iter()
        .filter(|hook| hook.name.as_deref() == Some(target))
        .collect();
    match named.as_slice() {
        [hook] => Ok(hook.index),
        [] => Err(CliError::Usage(format!(
            "no hook is named `{target}`; `hutan hooks list` shows the numbers and names"
        ))),
        _ => Err(CliError::Usage(format!(
            "several hooks are named `{target}`; use a number from `hutan hooks list`"
        ))),
    }
}

fn outcome_word(run: &HookRun) -> String {
    match run.outcome {
        HookOutcome::Ok => "ok".to_owned(),
        HookOutcome::Failed => match (run.exit_code, run.signal) {
            (Some(code), _) => format!("exit {code}"),
            (None, Some(signal)) => format!("signal {signal}"),
            (None, None) => "failed".to_owned(),
        },
        HookOutcome::TimedOut => "timed out".to_owned(),
        HookOutcome::SpawnFailed => "not started".to_owned(),
    }
}

fn duration_text(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

fn command_text(hook: &HookInfo) -> String {
    let line = hook.run.join(" ");
    let mut text: String = line.chars().take(COMMAND_WIDTH).collect();
    if line.chars().count() > COMMAND_WIDTH {
        text.pop();
        text.push('…');
    }
    if hook.shell {
        format!("sh -c {text}")
    } else {
        text
    }
}

fn filters_text(hook: &HookInfo) -> String {
    let mut parts = Vec::new();
    for (key, value) in [
        ("harness", &hook.harness),
        ("project", &hook.project),
        ("workspace", &hook.workspace),
    ] {
        if let Some(value) = value {
            parts.push(format!("{key}={value}"));
        }
    }
    if !hook.when_focused {
        parts.push("unfocused only".to_owned());
    }
    if parts.is_empty() {
        "-".to_owned()
    } else {
        parts.join(" ")
    }
}

fn list_text(list: &HooksList, now_ms: u64) -> String {
    if list.hooks.is_empty() {
        return "no hooks: add `[[hooks]]` to daemon.toml and restart the daemon \
                (docs/integrations.md § User hooks)"
            .to_owned();
    }
    let mut rows = vec![[
        "#".to_owned(),
        "ON".to_owned(),
        "WHEN".to_owned(),
        "COMMAND".to_owned(),
        "LAST RUN".to_owned(),
        "RUNS".to_owned(),
    ]];
    for hook in &list.hooks {
        let last = match &hook.last_run {
            Some(run) => format!(
                "{} in {}, {} ago",
                outcome_word(run),
                duration_text(run.duration_ms),
                age_text(now_ms.saturating_sub(run.started_at_ms) / 1000),
            ),
            None => "never".to_owned(),
        };
        let mut runs = hook.runs.to_string();
        let mut notes = Vec::new();
        if hook.failures > 0 {
            notes.push(format!("{} failed", hook.failures));
        }
        if hook.dropped > 0 {
            notes.push(format!("{} dropped", hook.dropped));
        }
        if hook.skipped > 0 {
            notes.push(format!("{} skipped", hook.skipped));
        }
        if !notes.is_empty() {
            runs.push_str(&format!(" ({})", notes.join(", ")));
        }
        let on = match &hook.name {
            Some(name) => format!("{} ({name})", hook.on.as_str()),
            None => hook.on.as_str().to_owned(),
        };
        rows.push([
            (hook.index + 1).to_string(),
            on,
            filters_text(hook),
            command_text(hook),
            last,
            runs,
        ]);
    }
    let widths: Vec<usize> = (0..5)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut text = rows
        .iter()
        .map(|row| {
            let mut line = String::new();
            for (cell, width) in row.iter().zip(&widths) {
                line.push_str(&format!("{cell:<width$}  "));
            }
            line.push_str(&row[5]);
            line
        })
        .collect::<Vec<_>>()
        .join("\n");
    text.push_str(&format!(
        "\n{} running now (at most {} at once, {} waiting per hook); hooks are read when the daemon starts",
        list.running, list.max_concurrent, list.queue_capacity
    ));
    text
}

fn output_block(label: &str, text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let body: Vec<String> = text.trim_end().lines().map(|l| format!("  {l}")).collect();
    format!("\n{label}:\n{}", body.join("\n"))
}

fn run_text(run: &HookRun) -> String {
    let mut text = format!(
        "{} in {}",
        outcome_word(run),
        duration_text(run.duration_ms)
    );
    if let Some(error) = &run.error {
        text.push_str(&format!(" ({error})"));
    }
    text.push_str(&output_block("stdout", &run.stdout));
    text.push_str(&output_block("stderr", &run.stderr));
    if run.output_truncated {
        text.push_str("\n(output cut)");
    }
    text
}

fn test_text(tested: &HooksTested) -> String {
    let run = &tested.run;
    let payload = serde_json::to_string_pretty(&tested.payload).unwrap_or_default();
    format!(
        "hook {} ({}) with a sample payload: {}\n\npayload:\n{}",
        run.hook + 1,
        run.event.as_str(),
        run_text(run),
        payload
            .lines()
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn log_text(runs: &[HookRun]) -> String {
    if runs.is_empty() {
        return "no runs yet (the log lives as long as the daemon)".to_owned();
    }
    runs.iter()
        .map(|run| {
            format!(
                "#{} hook {} {}{}: {}",
                run.id,
                run.hook + 1,
                run.event.as_str(),
                if run.test { " (test)" } else { "" },
                run_text(run),
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use hutan_protocol::hooks::HookEvent;

    use super::*;

    fn info(index: u32, name: Option<&str>) -> HookInfo {
        HookInfo {
            index,
            name: name.map(str::to_owned),
            on: HookEvent::TurnFinished,
            harness: Some("claude".into()),
            project: None,
            workspace: None,
            run: vec!["afplay".into(), "/System/Library/Sounds/Glass.aiff".into()],
            shell: false,
            timeout_secs: 30,
            when_focused: false,
            runs: 3,
            failures: 1,
            dropped: 2,
            skipped: 0,
            last_run: None,
        }
    }

    fn list(hooks: Vec<HookInfo>) -> HooksList {
        HooksList {
            hooks,
            running: 0,
            max_concurrent: 4,
            queue_capacity: 8,
        }
    }

    #[test]
    fn hooks_are_numbered_from_one_or_named() {
        let hooks = list(vec![
            info(0, Some("sound")),
            info(1, None),
            info(2, Some("sound")),
        ]);
        assert_eq!(resolve(&hooks, "1").unwrap(), 0);
        assert_eq!(resolve(&hooks, "2").unwrap(), 1);
        assert!(matches!(resolve(&hooks, "0"), Err(CliError::Usage(_))));
        assert!(matches!(resolve(&hooks, "4"), Err(CliError::Usage(_))));
        assert!(
            matches!(resolve(&hooks, "sound"), Err(CliError::Usage(m)) if m.contains("several"))
        );
        assert!(matches!(resolve(&hooks, "nope"), Err(CliError::Usage(_))));
        let one = list(vec![info(0, Some("sound"))]);
        assert_eq!(resolve(&one, "sound").unwrap(), 0);
        assert!(matches!(
            resolve(&list(Vec::new()), "1"),
            Err(CliError::Failed(_))
        ));
    }

    #[test]
    fn the_list_says_what_each_hook_is_for_and_how_it_did() {
        let text = list_text(&list(vec![info(0, Some("sound"))]), 1_000);
        assert!(text.contains("turn.finished (sound)"), "{text}");
        assert!(text.contains("harness=claude unfocused only"), "{text}");
        assert!(
            text.contains("afplay /System/Library/Sounds/Glass.aiff"),
            "{text}"
        );
        assert!(text.contains("never"), "{text}");
        assert!(text.contains("3 (1 failed, 2 dropped)"), "{text}");
        assert!(list_text(&list(Vec::new()), 0).starts_with("no hooks"));
    }
}
