//! The Automation page of the settings sheet: the user's hooks (`[[hooks]]` of `daemon.toml`,
//! protocol 0.49, `docs/integrations.md` § User hooks), what they last did, *Run test* with a
//! sample payload, and the log of runs.
//!
//! The daemon runs the hooks and keeps the numbers; this only asks (`hooks.list`, `hooks.log`,
//! `hooks.test`) and draws what it answers. It cannot add or change a hook: they are written by
//! hand in `daemon.toml`, which is the point (an agent cannot touch them either).

use std::collections::HashMap;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, div,
    px,
};
use hutan_protocol::hooks::{
    HookEvent, HookInfo, HookOutcome, HookRun, HooksList, HooksLogRequest, HooksTested,
};

use super::panels::common::now_ms;
use super::render::probe;
use super::settings_labels::words;
use super::settings_rows::{note, sheet_row};
use super::ui::Button;
use super::view::{Region, Workbench};
use crate::sessions::agents_text::age_text;
use crate::theme::DensityExt as _;

/// Runs of the log the page shows.
const LOG_ROWS: u32 = 20;
/// Lines of a run's output the page shows.
const OUTPUT_LINES: usize = 4;

/// What the Automation page knows: the daemon's answers.
#[derive(Debug, Default)]
pub struct HooksPage {
    pub list: Option<HooksList>,
    /// Newest first.
    pub log: Vec<HookRun>,
    /// The daemon has no hooks (older than protocol 0.49).
    pub unsupported: bool,
    pub error: Option<String>,
    /// The hook whose test is running.
    pub testing: Option<u32>,
    /// The last test of each hook, with the payload it was given.
    pub tested: HashMap<u32, HooksTested>,
}

/// What a hook runs on, in words next to its name in the file.
fn event_help(event: HookEvent) -> &'static str {
    match event {
        HookEvent::TurnFinished => "an agent's turn ended",
        HookEvent::TurnFailed => "an agent's turn ended with an error",
        HookEvent::ApprovalRequested => "an agent asks for a permission",
        HookEvent::AttentionChanged => "a session started or stopped needing you",
        HookEvent::SessionExited => "a session's process ended",
        HookEvent::TaskDone => "a task finished",
        HookEvent::FinishDone => "you finished a checkout (commit, merge, push)",
    }
}

/// `ok`, `exit 4`, `timed out`, `did not start`.
pub(crate) fn outcome_text(run: &HookRun) -> String {
    match run.outcome {
        HookOutcome::Ok => "ok".to_owned(),
        HookOutcome::Failed => match (run.exit_code, run.signal) {
            (Some(code), _) => format!("exit code {code}"),
            (None, Some(signal)) => format!("ended by signal {signal}"),
            (None, None) => "failed".to_owned(),
        },
        HookOutcome::TimedOut => "timed out".to_owned(),
        HookOutcome::SpawnFailed => "did not start".to_owned(),
    }
}

fn duration_text(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

/// The first lines of what a run printed.
fn output_text(run: &HookRun) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for (label, text) in [("", &run.stdout), ("stderr: ", &run.stderr)] {
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            lines.push(format!("{label}{line}"));
        }
    }
    if lines.is_empty() {
        return None;
    }
    let more = lines.len().saturating_sub(OUTPUT_LINES);
    lines.truncate(OUTPUT_LINES);
    if more > 0 {
        lines.push(format!("… {more} more lines"));
    }
    Some(lines.join("\n"))
}

/// One line about a run: how it went, how long it took, and when.
fn run_line(run: &HookRun, now: u64) -> String {
    let mut line = format!(
        "{} in {}, {}",
        outcome_text(run),
        duration_text(run.duration_ms),
        age_text(now, run.started_at_ms)
    );
    if run.test {
        line.push_str(" (test)");
    }
    if let Some(error) = &run.error {
        line.push_str(&format!(" — {error}"));
    }
    line
}

/// What the hook is for, and where it applies.
fn hook_help(hook: &HookInfo) -> String {
    let mut parts = vec![format!("Runs when {}.", event_help(hook.on))];
    let mut filters = Vec::new();
    for (label, value) in [
        ("harness", &hook.harness),
        ("project", &hook.project),
        ("workspace", &hook.workspace),
    ] {
        if let Some(value) = value {
            filters.push(format!("{label} {value}"));
        }
    }
    if !filters.is_empty() {
        parts.push(format!("Only for {}.", filters.join(", ")));
    }
    if !hook.when_focused {
        parts.push("Skipped while Hutan is the window you are working in.".to_owned());
    }
    parts.join(" ")
}

fn command_text(hook: &HookInfo) -> String {
    let line = hook.run.join(" ");
    if hook.shell {
        format!("Shell: {line}")
    } else {
        line
    }
}

impl Workbench {
    /// What the daemon last said about the hooks.
    pub fn hooks_page(&self) -> &HooksPage {
        &self.hooks_page
    }

    /// Reads the hooks and the log (`hooks.list`, `hooks.log`).
    pub fn refresh_hooks(&mut self, cx: &mut Context<Self>) {
        if !self.daemon_connected(cx) {
            return;
        }
        let connection = self.connection(cx);
        if !connection.supports_hooks() {
            self.hooks_page.unsupported = true;
            return;
        }
        cx.spawn(async move |this, cx| {
            let list = connection.hooks_list().await;
            let log = connection
                .hooks_log(HooksLogRequest {
                    hook: None,
                    limit: Some(LOG_ROWS),
                })
                .await;
            this.update(cx, |this, cx| {
                match (list, log) {
                    (Ok(list), Ok(log)) => {
                        this.hooks_page.list = Some(list);
                        this.hooks_page.log = log.runs;
                        this.hooks_page.error = None;
                    }
                    (Err(error), _) | (_, Err(error)) => {
                        this.hooks_page.error = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// *Run test*: runs hook `index` with a sample payload and shows how it went.
    pub fn test_hook(&mut self, index: u32, cx: &mut Context<Self>) {
        if self.hooks_page.testing.is_some() || !self.daemon_connected(cx) {
            return;
        }
        self.hooks_page.testing = Some(index);
        self.hooks_page.error = None;
        cx.notify();
        let connection = self.connection(cx);
        cx.spawn(async move |this, cx| {
            let tested = connection.hooks_test(index).await;
            this.update(cx, |this, cx| {
                this.hooks_page.testing = None;
                match tested {
                    Ok(tested) => {
                        this.hooks_page.tested.insert(index, tested);
                    }
                    Err(error) => this.hooks_page.error = Some(error.to_string()),
                }
                this.refresh_hooks(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The rows of the Automation page.
    pub(super) fn render_hooks(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = self.theme;
        let tokens = self.density.ui();
        let mut out = vec![self.section_heading(words::SEC_HOOKS, words::SEC_HOOKS_HELP)];
        if !self.daemon_connected(cx) {
            out.push(
                note(words::DAEMON_NOT_RUNNING, &theme, &tokens)
                    .py(px(tokens.spacing * 2.0))
                    .into_any_element(),
            );
            return out;
        }
        if self.hooks_page.unsupported {
            out.push(
                note(words::HOOKS_UNSUPPORTED, &theme, &tokens)
                    .py(px(tokens.spacing * 2.0))
                    .into_any_element(),
            );
            return out;
        }
        if let Some(error) = self.hooks_page.error.clone() {
            out.push(
                div()
                    .id("hooks-error")
                    .py(px(tokens.spacing))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.danger)
                    .child(SharedString::from(error))
                    .into_any_element(),
            );
        }
        let now = now_ms();
        let Some(list) = self.hooks_page.list.clone() else {
            out.push(
                note(words::HOOKS_LOADING, &theme, &tokens)
                    .py(px(tokens.spacing * 2.0))
                    .into_any_element(),
            );
            return out;
        };
        if list.hooks.is_empty() {
            out.push(
                note(words::HOOKS_NONE, &theme, &tokens)
                    .id("hooks-none")
                    .py(px(tokens.spacing * 2.0))
                    .into_any_element(),
            );
        }
        for hook in &list.hooks {
            out.push(self.render_hook(hook, now, cx));
        }
        if !list.hooks.is_empty() {
            out.push(
                note(
                    words::hooks_limits(list.running, list.max_concurrent, list.queue_capacity),
                    &theme,
                    &tokens,
                )
                .id("hooks-limits")
                .py(px(tokens.spacing))
                .into_any_element(),
            );
        }

        out.push(self.section_heading(words::SEC_HOOKS_LOG, words::SEC_HOOKS_LOG_HELP));
        if self.hooks_page.log.is_empty() {
            out.push(
                note(words::HOOKS_LOG_EMPTY, &theme, &tokens)
                    .id("hooks-log-empty")
                    .py(px(tokens.spacing * 2.0))
                    .into_any_element(),
            );
        }
        let log = self.hooks_page.log.clone();
        for run in &log {
            let hook = list.hooks.iter().find(|h| h.index == run.hook);
            let title = format!(
                "{} · hook {}{}",
                run.event.as_str(),
                run.hook + 1,
                hook.and_then(|h| h.name.as_ref())
                    .map(|name| format!(" ({name})"))
                    .unwrap_or_default()
            );
            out.push(sheet_row(
                SharedString::from(format!("hooks-run-{}", run.id)),
                title,
                run_line(run, now),
                output_under(run, &theme, &tokens),
                div().into_any_element(),
                &theme,
                &tokens,
            ));
        }
        out.push(
            div()
                .flex()
                .justify_end()
                .py(px(tokens.spacing * 2.0))
                .child(
                    Button::new("hooks-refresh", words::HOOKS_REFRESH, &theme, &tokens)
                        .compact()
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_hooks(cx))),
                )
                .into_any_element(),
        );
        out
    }

    /// One hook: what it is for, its command, how it last went, and *Run test*.
    fn render_hook(&mut self, hook: &HookInfo, now: u64, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let index = hook.index;
        let title = match &hook.name {
            Some(name) => format!("{} · {name}", hook.on.as_str()),
            None => hook.on.as_str().to_owned(),
        };
        let mut under: Vec<AnyElement> = vec![
            div()
                .id(SharedString::from(format!("hook-command-{index}")))
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.fg)
                .child(SharedString::from(command_text(hook)))
                .into_any_element(),
        ];
        let last = match &hook.last_run {
            Some(run) => words::hook_last_run(&run_line(run, now)),
            None => words::HOOK_NEVER_RAN.to_owned(),
        };
        under.push(
            note(last, &theme, &tokens)
                .id(SharedString::from(format!("hook-last-{index}")))
                .into_any_element(),
        );
        let counts = words::hook_counts(hook.runs, hook.failures, hook.dropped, hook.skipped);
        if let Some(counts) = counts {
            under.push(
                note(counts, &theme, &tokens)
                    .id(SharedString::from(format!("hook-counts-{index}")))
                    .into_any_element(),
            );
        }
        if let Some(tested) = self.hooks_page.tested.get(&index) {
            under.push(
                note(
                    words::hook_test_result(&run_line(&tested.run, now)),
                    &theme,
                    &tokens,
                )
                .id(SharedString::from(format!("hook-test-{index}")))
                .into_any_element(),
            );
            under.extend(output_under(&tested.run, &theme, &tokens));
        } else if let Some(run) = &hook.last_run {
            under.extend(output_under(run, &theme, &tokens));
        }
        let testing = self.hooks_page.testing == Some(index);
        let busy = self.hooks_page.testing.is_some();
        let label = if testing {
            words::HOOK_TESTING
        } else {
            words::HOOK_RUN_TEST
        };
        let mut test = Button::new(
            SharedString::from(format!("hook-test-run-{index}")),
            label,
            &theme,
            &tokens,
        )
        .compact()
        .disabled(busy)
        .on_click(cx.listener(move |this, _, _, cx| this.test_hook(index, cx)));
        // The first hook's button is the one the window tests click.
        if index == 0 {
            test = test.child(probe(&self.regions, Region::Settings("hook-test-first")));
        }
        sheet_row(
            SharedString::from(format!("hook-{index}")),
            title,
            hook_help(hook),
            under,
            test.into_any_element(),
            &theme,
            &tokens,
        )
    }
}

/// What a run printed, as small mono text under its row; nothing when it printed nothing.
fn output_under(
    run: &HookRun,
    theme: &crate::theme::Theme,
    tokens: &crate::theme::Tokens,
) -> Vec<AnyElement> {
    output_text(run)
        .map(|text| {
            div()
                .id(SharedString::from(format!("hook-output-{}", run.id)))
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .child(SharedString::from(text))
                .into_any_element()
        })
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(outcome: HookOutcome) -> HookRun {
        HookRun {
            id: 1,
            hook: 0,
            event: HookEvent::TurnFinished,
            test: false,
            started_at_ms: 1_000,
            duration_ms: 212,
            outcome,
            exit_code: Some(4),
            signal: None,
            session_id: None,
            stdout: "one\n\ntwo\n".to_owned(),
            stderr: "bad\n".to_owned(),
            output_truncated: false,
            error: None,
        }
    }

    #[test]
    fn a_run_is_one_line_and_its_output_a_few() {
        let mut failed = run(HookOutcome::Failed);
        assert_eq!(run_line(&failed, 61_000), "exit code 4 in 212 ms, 1m ago");
        assert_eq!(output_text(&failed).unwrap(), "one\ntwo\nstderr: bad");
        failed.test = true;
        failed.stdout = "a\nb\nc\nd\ne\nf\n".to_owned();
        assert!(run_line(&failed, 1_000).ends_with("(test)"));
        let shown = output_text(&failed).unwrap();
        assert_eq!(shown.lines().count(), OUTPUT_LINES + 1);
        assert!(shown.ends_with("… 3 more lines"), "{shown}");
        failed.stdout.clear();
        failed.stderr.clear();
        assert!(output_text(&failed).is_none());
        let mut late = run(HookOutcome::TimedOut);
        late.error = Some("killed after 30 s".to_owned());
        assert!(run_line(&late, 2_000).contains("timed out in 212 ms"));
        assert!(run_line(&late, 2_000).ends_with("— killed after 30 s"));
        assert_eq!(outcome_text(&run(HookOutcome::Ok)), "ok");
    }
}
