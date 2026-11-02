//! The **Checks** tab of Mawas's rail and *Check with Mawas* (CHECKS-C; protocol 0.72
//! `agents.findings`; `docs/assistant.md` § Checks, `docs/spikes/2026-11-02-checks.md` §5).
//!
//! The tab lists what the daemon's deterministic checks found wrong, scoped by the Focus like the
//! other tabs, grouped by severity and, inside a group, by session. Each row's actions are the
//! shared ones of [`super::findings_ui`]. A line under the list says what the checks could not
//! see (a terminal agent without the integration reports no tools); the tab's badge is the number
//! of open alerts in scope. It reads when it is on screen and the agents changed, at most every
//! two seconds; a hidden rail does nothing.
//!
//! *Check with Mawas* (a workspace's or a session's ⋯) opens Home, sets the Focus to the
//! workspace and puts a prepared ask — with the findings in it — in the composer. Nothing is
//! sent.

use gpui::{
    AnyElement, App, Context, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px,
};
use hutan_core::{SessionId, WorkspaceId};
use hutan_protocol::checks::{AgentsFindings, AgentsFindingsRequest, Finding, FindingSeverity};

use super::assistant::AssistantPanel;
use super::findings_ui::{self, FindingsState, RAIL, Scope, Surface};
use crate::settings::{AssistantSettings, RailTab};
use crate::workbench::chat::Look;
use crate::workbench::render::probe;
use crate::workbench::view::Region;

/// The findings a prepared ask lists.
const ASK_FINDINGS: usize = 8;

/// A prepared ask waiting for Mawas's chat to be there.
#[derive(Debug, Clone)]
pub(super) struct PendingAsk {
    /// The Focus to set; `None`: leave it.
    pub focus: Option<WorkspaceId>,
    pub text: String,
}

fn state_of(panel: &mut AssistantPanel) -> &mut FindingsState {
    &mut panel.checks
}

impl AssistantPanel {
    /// What the Checks tab reads for: the workspace of the Focus (every workspace without one).
    fn checks_scope(&self, cx: &App) -> Scope {
        Scope {
            workspace: self.rail_scope(cx),
            session: None,
        }
    }

    /// The open alerts in scope, the number the tab shows (zero until read).
    pub(super) fn checks_badge(&self, cx: &App) -> usize {
        self.checks
            .open_findings(self.checks_scope(cx))
            .iter()
            .filter(|f| f.severity == FindingSeverity::Alert)
            .count()
    }

    /// The findings the tab lists, once read for the scope shown now.
    pub fn checks_findings(&self, cx: &App) -> Option<&AgentsFindings> {
        self.checks.current(self.checks_scope(cx))
    }

    /// The rows the tab draws, top to bottom (row `n` is the region `Finding(_, n)`).
    pub fn checks_rows(&self, cx: &App) -> Vec<Finding> {
        grouped(self.checks.open_findings(self.checks_scope(cx)))
            .into_iter()
            .flat_map(|(_, group)| group.into_iter().cloned())
            .collect()
    }

    /// Where the *Verify* of `session` stands, in words (tests wait on it): `planning`,
    /// `ready: <command>` (with ` (refused)` when the daemon would not run it), `running:
    /// <command>`, `done: <status> exit <code>: <command>` or `failed: <why>`.
    pub fn checks_verify_step(&self, session: SessionId) -> Option<String> {
        use super::findings_ui::VerifyStep;
        self.checks.verify.get(&session).map(|step| match step {
            VerifyStep::Planning => "planning".to_owned(),
            VerifyStep::Ready(plan) => format!(
                "ready: {}{}",
                plan.command,
                if plan.refused.is_some() {
                    " (refused)"
                } else {
                    ""
                }
            ),
            VerifyStep::Running(plan) => format!("running: {}", plan.command),
            VerifyStep::Done(run) => format!(
                "done: {} exit {}: {}",
                run.status.as_str(),
                run.exit_code
                    .map_or_else(|| "-".to_owned(), |c| c.to_string()),
                run.command
            ),
            VerifyStep::Failed(why) => format!("failed: {why}"),
        })
    }

    /// Reads the findings once the Checks tab is on screen, and again when the agents changed
    /// (at most every two seconds). A hidden rail and the other tabs do no work.
    pub(super) fn ensure_checks(&mut self, cx: &mut Context<Self>) {
        let wanted = self.rail_open
            && self.env.settings.get::<AssistantSettings>().rail_tab == RailTab::Checks;
        if !wanted || !self.connected(cx) {
            return;
        }
        let scope = self.checks_scope(cx);
        let (revision, connection) = {
            let store = self.env.sessions.read(cx);
            (
                (
                    store.revision(),
                    store.sessions().len(),
                    store.approvals().len() + store.questions().len(),
                ),
                store.connection().clone(),
            )
        };
        self.checks
            .ensure(state_of, cx, &connection, scope, revision);
    }

    /// The Checks tab: the findings by severity, then by session.
    pub(super) fn render_checks(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let scope = self.checks_scope(cx);
        let mut sections: Vec<AnyElement> = Vec::new();
        let muted = |text: String| -> AnyElement {
            div()
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .child(SharedString::from(text))
                .into_any_element()
        };
        if self.checks.unsupported {
            sections.push(muted(
                "This daemon has no checks (it predates protocol 0.72).".to_owned(),
            ));
        } else if let Some(answer) = self.checks.current(scope) {
            let name = self.rail_scope_name(cx);
            let open = self.checks.open_findings(scope);
            let header = match (&name, open.len()) {
                (Some(name), 0) => format!("Nothing found in {name}."),
                (None, 0) => "Nothing found.".to_owned(),
                (Some(name), n) => format!("{n} found in {name}"),
                (None, n) => format!("{n} found"),
            };
            sections.push(
                div()
                    .relative()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(header))
                    .child(probe(&self.env.regions, Region::Assistant("checks-header")))
                    .into_any_element(),
            );
            let connection = self.env.sessions.read(cx).connection().clone();
            let surface = Surface {
                theme: &theme,
                tokens: &tokens,
                regions: &self.env.regions,
                names: &RAIL,
                connection: &connection,
                show_session: true,
                now_ms: hutan_client::time::now_ms(),
            };
            let mut base = 0;
            for (label, group) in grouped(open.clone()) {
                sections.push(
                    div()
                        .text_size(px(tokens.font_size))
                        .child(SharedString::from(format!("{label} · {}", group.len())))
                        .into_any_element(),
                );
                let count = group.len();
                sections.extend(findings_ui::rows(
                    &group,
                    base,
                    &self.checks,
                    state_of,
                    &surface,
                    cx,
                ));
                base += count;
            }
            let titles: std::collections::HashMap<SessionId, String> =
                self.checks
                    .verify
                    .keys()
                    .map(|id| {
                        let title = self.env.sessions.read(cx).session(*id).map_or_else(
                            || "a session that is gone".to_owned(),
                            |s| s.title.clone(),
                        );
                        (*id, title)
                    })
                    .collect();
            let title_of = |id: SessionId| titles.get(&id).cloned().unwrap_or_default();
            sections.extend(findings_ui::orphan_steps(
                &open,
                &self.checks,
                state_of,
                &surface,
                &title_of,
                cx,
            ));
            if answer.dismissed_hidden > 0 {
                sections.push(muted(format!(
                    "{} dismissed finding{} hidden.",
                    answer.dismissed_hidden,
                    if answer.dismissed_hidden == 1 {
                        ""
                    } else {
                        "s"
                    }
                )));
            }
            if answer.omitted > 0 {
                sections.push(muted(format!(
                    "{} more findings are not listed.",
                    answer.omitted
                )));
            }
            if let Some(line) = coverage_line(answer) {
                sections.push(
                    div()
                        .relative()
                        .child(muted(line))
                        .child(probe(
                            &self.env.regions,
                            Region::Assistant("checks-coverage"),
                        ))
                        .into_any_element(),
                );
            }
        } else {
            sections.push(muted(
                self.checks
                    .error
                    .clone()
                    .unwrap_or_else(|| "Checking what the agents did…".to_owned()),
            ));
        }
        if let Some(error) = self
            .checks
            .error
            .as_ref()
            .filter(|_| self.checks.current(scope).is_some())
        {
            sections.push(muted(error.clone()));
        }
        hutan_ui::scroll_y(
            "assistant-today-scroll",
            div()
                .id("assistant-today-checks")
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .gap(px(tokens.spacing * 3.0))
                .p(px(tokens.spacing * 3.0))
                .children(sections),
        )
        .into_any_element()
    }

    /// *Check with Mawas*: Home opens (the caller reveals it), the Focus goes to `workspace`,
    /// and the composer gets a prepared ask with the findings of the workspace (or of `session`)
    /// in it. The user sends it; nothing is sent here.
    pub fn check_with_mawas(
        &mut self,
        workspace: WorkspaceId,
        session: Option<SessionId>,
        cx: &mut Context<Self>,
    ) {
        self.ensure_open(cx);
        let store = self.env.sessions.read(cx);
        let connection = store.connection().clone();
        let what = match session {
            Some(id) => store.session(id).map_or_else(
                || "this session".to_owned(),
                |s| format!("the session \"{}\"", s.title),
            ),
            None => store.workspace(workspace).map_or_else(
                || "this workspace".to_owned(),
                |w| format!("the workspace {}", w.name),
            ),
        };
        cx.spawn(async move |this, cx| {
            let answer = if connection.supports_agents_findings() {
                connection
                    .agents_findings(AgentsFindingsRequest {
                        workspace_id: session.is_none().then_some(workspace),
                        session_id: session,
                        ..Default::default()
                    })
                    .await
                    .map_err(|error| error.to_string())
            } else {
                Err("this daemon has no checks (it predates protocol 0.72)".to_owned())
            };
            let text = check_ask(&what, workspace, session, &answer);
            this.update(cx, |this, cx| {
                this.pending_ask = Some(PendingAsk {
                    focus: Some(workspace),
                    text,
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// *Ask Mawas* of one finding: the prepared question waits for the composer.
    pub fn ask_mawas(&mut self, text: String, cx: &mut Context<Self>) {
        self.ensure_open(cx);
        self.pending_ask = Some(PendingAsk { focus: None, text });
        cx.notify();
    }

    /// Puts a waiting prepared ask in the composer once Mawas's chat is there, and sets its
    /// Focus. Nothing is sent.
    pub(super) fn apply_pending_ask(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_ask.clone() else {
            return;
        };
        let Some(model) = self
            .session(cx)
            .and_then(|session| self.chat_model(session))
        else {
            return;
        };
        self.pending_ask = None;
        if let Some(workspace) = pending.focus {
            self.set_focus(Some(workspace), cx);
        }
        model.update(cx, |model, cx| model.add_note(&pending.text, cx));
        self.focus_wanted = true;
        cx.notify();
    }
}

/// What the checks could not see, in a line: terminal agents (their integration reports tools,
/// files and tokens, not the commands, the turns or the final message) and the notes of what is
/// not recorded.
fn coverage_line(answer: &AgentsFindings) -> Option<String> {
    let terminals: Vec<&hutan_protocol::checks::SessionCoverage> = answer
        .coverage
        .iter()
        .filter(|c| !c.chat && !c.gaps.is_empty())
        .collect();
    let mut parts = Vec::new();
    if !terminals.is_empty() {
        let first = terminals
            .iter()
            .flat_map(|c| c.gaps.first())
            .map(|gap| gap.why.as_str())
            .next()
            .unwrap_or("not fully checked");
        parts.push(format!(
            "{} terminal agent{}: {first}",
            terminals.len(),
            if terminals.len() == 1 { "" } else { "s" }
        ));
    }
    if !answer.not_recorded.is_empty() {
        parts.push(format!(
            "Not recorded: {}",
            answer
                .not_recorded
                .iter()
                .map(|n| n.what.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    (!parts.is_empty()).then(|| parts.join(". ") + ".")
}

/// The question *Check with Mawas* prepares: what to check, the findings the daemon already has
/// (so the user sees what she will be told about) and how to go on.
fn check_ask(
    what: &str,
    workspace: WorkspaceId,
    session: Option<SessionId>,
    answer: &Result<AgentsFindings, String>,
) -> String {
    let tool = match session {
        Some(id) => format!("agents_findings with session_id {id}"),
        None => format!("agents_findings with workspace_id {workspace}"),
    };
    let mut text = format!(
        "Check {what} and tell me what is wrong, if anything: use {tool}, then agents_review for the turns that matter and agents_verify to run the tests where a claim needs it. Say what is exact and what is a guess, and what the checks could not see."
    );
    match answer {
        Ok(answer) => {
            let open: Vec<&Finding> = answer.findings.iter().filter(|f| !f.dismissed).collect();
            if open.is_empty() {
                text.push_str("\n\nThe checks found nothing so far.");
            } else {
                text.push_str(&format!(
                    "\n\nThe checks found {} so far ({} alert{}, {} warning{}):",
                    open.len(),
                    answer.counts.alert,
                    if answer.counts.alert == 1 { "" } else { "s" },
                    answer.counts.warn,
                    if answer.counts.warn == 1 { "" } else { "s" },
                ));
                for finding in open.iter().take(ASK_FINDINGS) {
                    text.push_str(&format!(
                        "\n- {} ({}, {})",
                        finding.title,
                        finding.detector,
                        finding.confidence.as_str()
                    ));
                }
                if open.len() > ASK_FINDINGS {
                    text.push_str(&format!("\n- … and {} more", open.len() - ASK_FINDINGS));
                }
            }
        }
        Err(error) => {
            text.push_str(&format!(
                "\n\nThe findings could not be read here: {error}."
            ));
        }
    }
    text
}

/// The findings by severity (worst first), and inside a severity by session in the order the
/// daemon gave; empty groups are left out.
fn grouped(open: Vec<&Finding>) -> Vec<(&'static str, Vec<&Finding>)> {
    [
        (FindingSeverity::Alert, "Alerts"),
        (FindingSeverity::Warn, "Warnings"),
        (FindingSeverity::Info, "For information"),
    ]
    .into_iter()
    .filter_map(|(severity, label)| {
        let mut group: Vec<&Finding> = open
            .iter()
            .copied()
            .filter(|f| f.severity == severity)
            .collect();
        if group.is_empty() {
            return None;
        }
        let mut sessions: Vec<Option<SessionId>> = Vec::new();
        for finding in &group {
            if !sessions.contains(&finding.scope.session_id) {
                sessions.push(finding.scope.session_id);
            }
        }
        group.sort_by_key(|f| {
            sessions
                .iter()
                .position(|s| *s == f.scope.session_id)
                .unwrap_or(usize::MAX)
        });
        Some((label, group))
    })
    .collect()
}
