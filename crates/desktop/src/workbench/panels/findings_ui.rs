//! What the **Checks** tab of Mawas's rail and the **Findings** section of the Context panel share
//! (CHECKS-C; protocol 0.72 `agents.findings`, 0.70 `agents.verify`): one read of the daemon's
//! findings, the rows with their actions, and the *Verify* step.
//!
//! - **The daemon decides.** A row is a [`Finding`] as the daemon wrote it: title, the *exact* or
//!   *heuristic* label, its evidence on expand. Nothing here recognises anything.
//! - **Actions.** *Open* shows the session, *Review* attaches the turn's review to Mawas's
//!   composer, *Verify* shows the command the daemon resolved and only then runs it, *Dismiss*
//!   is the user's (`agents.findings.dismiss`; the daemon refuses an agent), *Ask Mawas* puts a
//!   prepared question in the composer — nothing is sent.
//! - **Cheap.** A read happens when a surface is on screen and the agents changed, at most every
//!   two seconds ([`REFRESH`]); a hidden surface does nothing.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};
use hutan_client::Connection;
use hutan_core::{SessionId, WorkspaceId};
use hutan_protocol::checks::{
    AgentsFindings, AgentsFindingsRequest, EvidenceKind, Finding, FindingConfidence,
    FindingSeverity,
};
use hutan_protocol::verify::{
    AgentsVerifyPlanRequest, AgentsVerifyRequest, VerifyPlan, VerifyRefusal, VerifyRun,
    VerifyStatus,
};

use crate::icons::Icon;
use crate::theme::{Theme, Tokens};
use crate::workbench::actions::{AskMawas, OpenSession, ReviewAt, ReviewTurn};
use crate::workbench::regions::Regions;
use crate::workbench::render::probe;
use crate::workbench::ui::{Button, readable_on};
use crate::workbench::view::Region;

/// The shortest pause between two reads caused by the agents changing.
pub(super) const REFRESH: Duration = Duration::from_secs(2);
/// The lines of a run's output a row shows.
const OUTPUT_LINES: usize = 8;

/// What a read of the findings is about: a workspace (the Focus), one session, or everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Scope {
    pub workspace: Option<WorkspaceId>,
    pub session: Option<SessionId>,
}

/// How far a *Verify* of one session has come.
#[derive(Debug, Clone)]
pub(super) enum VerifyStep {
    /// The daemon is resolving the command.
    Planning,
    /// The command it resolved, waiting for the user's go.
    Ready(VerifyPlan),
    /// The command runs.
    Running(VerifyPlan),
    /// The run is over.
    Done(Box<VerifyRun>),
    Failed(String),
}

/// One surface's read of the findings and what it shows of them.
#[derive(Default)]
pub(super) struct FindingsState {
    pub(super) answer: Option<AgentsFindings>,
    for_scope: Option<Scope>,
    /// What the store looked like when the answer was read.
    revision: (u64, usize, usize),
    /// The answer is out of date (a dismissal, a run): read again at once.
    stale: bool,
    loading: bool,
    retry: bool,
    last_load: Option<Instant>,
    /// The daemon has no findings (it predates protocol 0.72).
    pub(super) unsupported: bool,
    pub(super) error: Option<String>,
    /// Findings whose evidence is open.
    open: HashSet<String>,
    pub(super) verify: HashMap<SessionId, VerifyStep>,
    /// The row each *Verify* started from, by finding id (`None`: the Context panel's own
    /// button). Its plan and its result show there and nowhere else.
    verify_from: HashMap<SessionId, Option<String>>,
    /// The Checks tab lists what the user dismissed too, each with *Restore*.
    pub(super) show_dismissed: bool,
}

impl FindingsState {
    /// The answer, when it is the one for `scope`.
    pub(super) fn current(&self, scope: Scope) -> Option<&AgentsFindings> {
        self.answer
            .as_ref()
            .filter(|_| self.for_scope == Some(scope))
    }

    /// The findings not dismissed, worst first (the daemon's order), of the answer for `scope`.
    pub(super) fn open_findings(&self, scope: Scope) -> Vec<&Finding> {
        self.current(scope)
            .map(|answer| answer.findings.iter().filter(|f| !f.dismissed).collect())
            .unwrap_or_default()
    }

    /// The findings the user dismissed, listed when [`show_dismissed`](Self::show_dismissed) is
    /// on, of the answer for `scope`.
    pub(super) fn dismissed_findings(&self, scope: Scope) -> Vec<&Finding> {
        self.current(scope)
            .map(|answer| answer.findings.iter().filter(|f| f.dismissed).collect())
            .unwrap_or_default()
    }

    /// Reads the findings for `scope` unless the last answer is for it and the agents did not
    /// change (`revision`), at most every [`REFRESH`].
    pub(super) fn ensure<T: 'static>(
        &mut self,
        access: fn(&mut T) -> &mut FindingsState,
        cx: &mut Context<T>,
        connection: &Connection,
        scope: Scope,
        revision: (u64, usize, usize),
    ) {
        if self.loading || self.unsupported {
            return;
        }
        if !connection.supports_agents_findings() {
            self.unsupported = true;
            return;
        }
        let same = self.for_scope == Some(scope);
        if self.answer.is_some() && same && !self.stale && self.revision == revision {
            return;
        }
        if let Some(last) = self.last_load
            && same
            && !self.stale
            && last.elapsed() < REFRESH
        {
            if !self.retry {
                self.retry = true;
                let wait = REFRESH.saturating_sub(last.elapsed());
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wait).await;
                    this.update(cx, |this, cx| {
                        access(this).retry = false;
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            return;
        }
        self.loading = true;
        self.stale = false;
        self.last_load = Some(Instant::now());
        let include_dismissed = self.show_dismissed;
        let connection = connection.clone();
        cx.spawn(async move |this, cx| {
            let answer = connection
                .agents_findings(AgentsFindingsRequest {
                    workspace_id: scope.workspace,
                    session_id: scope.session,
                    include_dismissed,
                    ..Default::default()
                })
                .await;
            this.update(cx, |this, cx| {
                let state = access(this);
                state.loading = false;
                state.for_scope = Some(scope);
                state.revision = revision;
                match answer {
                    Ok(answer) => {
                        state.error = None;
                        state.answer = Some(answer);
                    }
                    Err(error) => {
                        tracing::warn!("cannot read the findings: {error}");
                        state.error = Some(format!("Could not read the findings: {error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// *Dismiss*: the finding goes at once; the daemon keeps the dismissal and the list is read
    /// again.
    pub(super) fn dismiss<T: 'static>(
        &mut self,
        access: fn(&mut T) -> &mut FindingsState,
        cx: &mut Context<T>,
        connection: &Connection,
        id: String,
    ) {
        if let Some(answer) = self.answer.as_mut() {
            if self.show_dismissed {
                if let Some(finding) = answer.findings.iter_mut().find(|f| f.id == id) {
                    finding.dismissed = true;
                }
            } else {
                answer.findings.retain(|finding| finding.id != id);
                answer.dismissed_hidden += 1;
            }
        }
        self.open.remove(&id);
        cx.notify();
        let connection = connection.clone();
        cx.spawn(async move |this, cx| {
            let result = connection.agents_findings_dismiss(id).await;
            this.update(cx, |this, cx| {
                let state = access(this);
                state.stale = true;
                if let Err(error) = result {
                    state.error = Some(format!("Could not dismiss the finding: {error}"));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// *Restore*: the finding is listed again at once; the daemon forgets the dismissal and the
    /// list is read again.
    pub(super) fn restore<T: 'static>(
        &mut self,
        access: fn(&mut T) -> &mut FindingsState,
        cx: &mut Context<T>,
        connection: &Connection,
        id: String,
    ) {
        if let Some(answer) = self.answer.as_mut()
            && let Some(finding) = answer.findings.iter_mut().find(|f| f.id == id)
        {
            finding.dismissed = false;
        }
        cx.notify();
        let connection = connection.clone();
        cx.spawn(async move |this, cx| {
            let result = connection.agents_findings_restore(id).await;
            this.update(cx, |this, cx| {
                let state = access(this);
                state.stale = true;
                if let Err(error) = result {
                    state.error = Some(format!("Could not restore the finding: {error}"));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// *Show dismissed* / *Hide dismissed*: the next read asks for the dismissed findings too
    /// (or no longer does).
    pub(super) fn set_show_dismissed<T: 'static>(&mut self, cx: &mut Context<T>, shown: bool) {
        if self.show_dismissed != shown {
            self.show_dismissed = shown;
            self.stale = true;
            cx.notify();
        }
    }

    fn toggle(&mut self, id: &str) {
        if !self.open.remove(id) {
            self.open.insert(id.to_owned());
        }
    }

    /// *Verify*: asks the daemon which command it would run; nothing runs yet.
    pub(super) fn plan_verify<T: 'static>(
        &mut self,
        access: fn(&mut T) -> &mut FindingsState,
        cx: &mut Context<T>,
        connection: &Connection,
        session: SessionId,
        from: Option<String>,
    ) {
        if matches!(
            self.verify.get(&session),
            Some(VerifyStep::Planning | VerifyStep::Running(_))
        ) {
            return;
        }
        self.verify.insert(session, VerifyStep::Planning);
        self.verify_from.insert(session, from);
        cx.notify();
        let connection = connection.clone();
        cx.spawn(async move |this, cx| {
            let plan = connection
                .agents_verify_plan(AgentsVerifyPlanRequest {
                    session_id: session,
                    command: None,
                })
                .await;
            this.update(cx, |this, cx| {
                let step = match plan {
                    Ok(plan) => VerifyStep::Ready(plan),
                    Err(error) => VerifyStep::Failed(error.to_string()),
                };
                access(this).verify.insert(session, step);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The user's go after the command was shown: runs it (`force` when its own turn runs and
    /// the user says so), then reads the findings again.
    pub(super) fn run_verify<T: 'static>(
        &mut self,
        access: fn(&mut T) -> &mut FindingsState,
        cx: &mut Context<T>,
        connection: &Connection,
        session: SessionId,
        force: bool,
    ) {
        let Some(VerifyStep::Ready(plan)) = self.verify.get(&session).cloned() else {
            return;
        };
        self.verify.insert(session, VerifyStep::Running(plan));
        cx.notify();
        let connection = connection.clone();
        cx.spawn(async move |this, cx| {
            let run = connection
                .agents_verify(AgentsVerifyRequest {
                    session_id: session,
                    command: None,
                    force,
                })
                .await;
            this.update(cx, |this, cx| {
                let state = access(this);
                state.stale = true;
                let step = match run {
                    Ok(run) => VerifyStep::Done(Box::new(run)),
                    Err(error) => VerifyStep::Failed(error.to_string()),
                };
                state.verify.insert(session, step);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Takes a plan (or a result) off the row.
    pub(super) fn forget_verify(&mut self, session: SessionId) {
        self.verify.remove(&session);
        self.verify_from.remove(&session);
    }

    /// Where the *Verify* of `session` stands, in words (tests wait on it): `planning`,
    /// `ready: <command>` (with ` (refused)` when the daemon would not run it), `running:
    /// <command>`, `done: <status> exit <code>: <command>` or `failed: <why>`.
    pub(super) fn step_text(&self, session: SessionId) -> Option<String> {
        self.verify.get(&session).map(|step| match step {
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

    /// The step of `session`'s *Verify* when it started from the row of finding `id`.
    pub(super) fn step_on_row(&self, session: SessionId, id: &str) -> Option<&VerifyStep> {
        match self.verify_from.get(&session) {
            Some(Some(from)) if from == id => self.verify.get(&session),
            _ => None,
        }
    }

    /// The steps no row of `shown` carries: the one the Context panel's own button started, and
    /// those whose row is gone (the finding they started from went away, as findings do).
    pub(super) fn unplaced(&self, shown: &[&Finding]) -> Vec<(SessionId, &VerifyStep)> {
        self.verify
            .iter()
            .filter(|(session, _)| match self.verify_from.get(session) {
                Some(Some(from)) => !shown.iter().any(|f| &f.id == from),
                _ => true,
            })
            .map(|(session, step)| (*session, step))
            .collect()
    }

    /// A number that changes when what a surface draws of `scope` changes: the rows, the open
    /// evidence and the state of every *Verify*.
    pub(super) fn signature(&self, scope: Scope) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        if let Some(answer) = self.current(scope) {
            for finding in &answer.findings {
                finding.id.hash(&mut hasher);
                finding.dismissed.hash(&mut hasher);
                self.open.contains(&finding.id).hash(&mut hasher);
            }
            answer.dismissed_hidden.hash(&mut hasher);
        }
        self.show_dismissed.hash(&mut hasher);
        self.answer.is_some().hash(&mut hasher);
        self.unsupported.hash(&mut hasher);
        let mut runs = 0u64;
        for (session, step) in &self.verify {
            let mut each = std::collections::hash_map::DefaultHasher::new();
            session.to_string().hash(&mut each);
            self.step_text(*session).hash(&mut each);
            self.verify_from.get(session).hash(&mut each);
            matches!(step, VerifyStep::Done(_)).hash(&mut each);
            runs = runs.wrapping_add(each.finish());
        }
        runs.hash(&mut hasher);
        hasher.finish()
    }
}

/// The names of the regions a surface's rows record, so tests click them by name.
#[derive(Debug)]
pub(super) struct Names {
    pub row: &'static str,
    pub open: &'static str,
    pub review: &'static str,
    pub verify: &'static str,
    pub run: &'static str,
    pub run_anyway: &'static str,
    pub cancel: &'static str,
    pub dismiss: &'static str,
    pub ask: &'static str,
    pub step: &'static str,
    pub head: &'static str,
    pub severity: &'static str,
    pub restore: &'static str,
}

/// The Checks tab of the rail.
pub(super) const RAIL: Names = Names {
    row: "checks-row",
    open: "checks-open",
    review: "checks-review",
    verify: "checks-verify",
    run: "checks-verify-run",
    run_anyway: "checks-verify-anyway",
    cancel: "checks-verify-cancel",
    dismiss: "checks-dismiss",
    ask: "checks-ask",
    step: "checks-verify-step",
    head: "checks-head",
    severity: "checks-severity",
    restore: "checks-restore",
};

/// The Findings section of the Context panel.
pub(super) const CONTEXT: Names = Names {
    row: "findings-row",
    open: "findings-open",
    review: "findings-review",
    verify: "findings-verify",
    run: "findings-verify-run",
    run_anyway: "findings-verify-anyway",
    cancel: "findings-verify-cancel",
    dismiss: "findings-dismiss",
    ask: "findings-ask",
    step: "findings-verify-step",
    head: "findings-head",
    severity: "findings-severity",
    restore: "findings-restore",
};

/// What a row is drawn with.
pub(super) struct Surface<'a> {
    pub theme: &'a Theme,
    pub tokens: &'a Tokens,
    pub regions: &'a Regions,
    pub names: &'static Names,
    pub connection: &'a Connection,
    /// The titles of the sessions the findings are about ([`titles`]).
    pub titles: &'a HashMap<SessionId, String>,
    /// The row names its session (the rail lists many; the Context panel one).
    pub show_session: bool,
    pub now_ms: u64,
}

/// The words of a severity and its colour.
fn severity_word(severity: FindingSeverity, theme: &Theme) -> (&'static str, gpui::Hsla) {
    // The word is text on whatever the surface it sits on: the colour of the state, stepped
    // until it reads at AA (4.5:1) on every ground of the chrome.
    let grounds = [
        theme.bg,
        theme.sidebar,
        theme.surface,
        theme.surface_hover,
        theme.sidebar_accent,
        theme.right_sidebar,
        theme.right_sidebar_accent,
    ];
    let (word, color) = match severity {
        FindingSeverity::Alert => ("Alert", theme.danger),
        FindingSeverity::Warn => ("Warn", theme.warning),
        FindingSeverity::Info => ("Info", theme.muted),
    };
    (word, readable_on(color, &grounds, 4.5))
}

fn evidence_word(kind: EvidenceKind) -> &'static str {
    match kind {
        EvidenceKind::Command => "command",
        EvidenceKind::File => "file",
        EvidenceKind::Diff => "diff",
        EvidenceKind::Event => "event",
        EvidenceKind::Stat => "number",
        EvidenceKind::Verify => "test run",
    }
}

/// The question *Ask Mawas* puts in the composer about one finding.
pub(super) fn ask_text(finding: &Finding, session_titles: &[String]) -> String {
    let mut text = format!(
        "Check this finding of Hutan's checks and tell me whether it is real and what to do about it: \"{}\" ({} detector, {}, {}",
        finding.title,
        finding.detector,
        severity_word_plain(finding.severity),
        finding.confidence.as_str()
    );
    match session_titles {
        [] => {}
        [title] => text.push_str(&format!(", in the session \"{title}\"")),
        titles => text.push_str(&format!(
            ", in the sessions {}",
            titles
                .iter()
                .map(|title| format!("\"{title}\""))
                .collect::<Vec<_>>()
                .join(" and ")
        )),
    }
    text.push(')');
    if let Some(session) = finding.scope.session_id {
        text.push_str(&format!(
            ". Use agents_findings with session_id {session} for its evidence and agents_review for the turn; say what is exact and what is a guess."
        ));
    } else {
        text.push_str(&format!(
            ". Use agents_findings with workspace_id {} for its evidence; say what is exact and what is a guess.",
            finding.scope.workspace_id
        ));
    }
    text
}

fn severity_word_plain(severity: FindingSeverity) -> &'static str {
    match severity {
        FindingSeverity::Alert => "an alert",
        FindingSeverity::Warn => "a warning",
        FindingSeverity::Info => "for information",
    }
}

fn dot(text: impl Into<SharedString>, theme: &Theme, tokens: &Tokens) -> gpui::Div {
    div()
        .text_size(px(tokens.secondary_font_size))
        .text_color(theme.muted)
        .child(text.into())
}

/// The rows of `findings`, one per finding, with their actions; the row numbers (of the regions
/// and ids) count from `base`, so several groups can follow each other.
pub(super) fn rows<T: 'static>(
    findings: &[&Finding],
    base: usize,
    state: &FindingsState,
    access: fn(&mut T) -> &mut FindingsState,
    surface: &Surface<'_>,
    cx: &mut Context<T>,
) -> Vec<AnyElement> {
    findings
        .iter()
        .enumerate()
        .map(|(index, finding)| row(finding, base + index, state, access, surface, cx))
        .collect()
}

/// The session a row acts on: the finding's own, or — for one about a checkout several agents
/// share, which has none — the first agent its evidence names.
pub(super) fn target_session(finding: &Finding) -> Option<SessionId> {
    finding.scope.session_id.or_else(|| {
        finding
            .evidence
            .iter()
            .find_map(|e| e.reference.as_deref()?.parse().ok())
    })
}

fn row<T: 'static>(
    finding: &Finding,
    index: usize,
    state: &FindingsState,
    access: fn(&mut T) -> &mut FindingsState,
    surface: &Surface<'_>,
    cx: &mut Context<T>,
) -> AnyElement {
    let (theme, tokens, names) = (surface.theme, surface.tokens, surface.names);
    let session = target_session(finding);
    let involved_titles = involved_titles(finding, surface.titles);
    let (word, color) = severity_word(finding.severity, theme);
    let open = state.open.contains(&finding.id);
    let confidence = match finding.confidence {
        FindingConfidence::Exact => "exact",
        FindingConfidence::Heuristic => "heuristic",
    };
    let id = finding.id.clone();
    // The severity is the first thing of the row, at the top-left whatever the title's length:
    // a title that wraps goes on under the title, never under the word.
    let head = vec![
        div()
            .flex_none()
            .text_color(color)
            .relative()
            .child(SharedString::from(word))
            .child(probe(
                surface.regions,
                Region::Finding(names.severity, index),
            ))
            .into_any_element(),
        div()
            .flex_1()
            .min_w_0()
            .child(SharedString::from(finding.title.clone()))
            .into_any_element(),
        dot(confidence, theme, tokens)
            .flex_none()
            .into_any_element(),
    ];
    let meta = meta_text(
        finding,
        surface.titles,
        surface.show_session,
        surface.now_ms,
    );

    let mut body = div()
        .id((names.row, index))
        .relative()
        .flex()
        .flex_col()
        .gap(px(tokens.spacing))
        .child(
            div()
                .id((names.step, index))
                .relative()
                .flex()
                .items_start()
                .gap(px(tokens.spacing))
                .text_size(px(tokens.font_size))
                .cursor_pointer()
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| {
                        access(this).toggle(&id);
                        cx.notify();
                    }
                }))
                .children(head)
                .child(probe(surface.regions, Region::Finding(names.head, index))),
        )
        .child(dot(meta, theme, tokens));

    if open {
        for line in evidence_lines(finding, surface.titles) {
            body = body.child(dot(line, theme, tokens).pl(px(tokens.spacing * 2.0)));
        }
        if finding.evidence.is_empty() {
            body = body.child(
                dot(
                    "The daemon recorded no evidence line for it.",
                    theme,
                    tokens,
                )
                .pl(px(tokens.spacing * 2.0)),
            );
        }
    }

    // The actions.
    let mut actions = div()
        .flex()
        .flex_wrap()
        .gap(px(tokens.spacing))
        .child(
            Button::new((names.open, index), "Open", theme, tokens)
                .ghost()
                .compact()
                .icon(Icon::ActionOpenSession)
                .tooltip("Open the session")
                .on_click({
                    move |_, window, cx| {
                        if let Some(session) = session {
                            window.dispatch_action(Box::new(OpenSession(session.to_string())), cx);
                        }
                    }
                })
                .disabled(session.is_none())
                .child(probe(surface.regions, Region::Finding(names.open, index))),
        )
        .child(
            Button::new((names.review, index), "Review", theme, tokens)
                .ghost()
                .compact()
                .icon(Icon::ActionAddContext)
                .tooltip("Attach the turn's review to Mawas's message")
                .on_click({
                    let turn = finding.scope.turn_id;
                    move |_, window, cx| {
                        if let Some(session) = session {
                            window.dispatch_action(
                                Box::new(ReviewTurn(ReviewAt {
                                    session,
                                    turn,
                                    number: None,
                                })),
                                cx,
                            );
                        }
                    }
                })
                .disabled(session.is_none())
                .child(probe(surface.regions, Region::Finding(names.review, index))),
        );
    if let Some(session) = session
        && surface.connection.supports_agents_verify()
    {
        let connection = surface.connection.clone();
        let from = id.clone();
        actions = actions.child(
            Button::new((names.verify, index), "Verify", theme, tokens)
                .ghost()
                .compact()
                .tooltip(
                    "Run the project's tests in the session's folder; the command is shown first",
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    access(this).plan_verify(access, cx, &connection, session, Some(from.clone()));
                }))
                .child(probe(surface.regions, Region::Finding(names.verify, index))),
        );
    }
    actions = actions
        .child({
            let connection = surface.connection.clone();
            let id = id.clone();
            Button::new((names.dismiss, index), "Dismiss", theme, tokens)
                .ghost()
                .compact()
                .tooltip("Hide this finding; it stays hidden")
                .on_click(cx.listener(move |this, _, _, cx| {
                    access(this).dismiss(access, cx, &connection, id.clone());
                }))
                .child(probe(
                    surface.regions,
                    Region::Finding(names.dismiss, index),
                ))
        })
        .child({
            let text = ask_text(finding, &involved_titles);
            Button::new((names.ask, index), "Ask Mawas", theme, tokens)
                .ghost()
                .compact()
                .tooltip("Put a question about this finding in Mawas's composer")
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(AskMawas(text.clone())), cx);
                })
                .child(probe(surface.regions, Region::Finding(names.ask, index)))
        });
    body = body.child(actions);

    // A *Verify* shows its plan and its result on the row it started from, and only there.
    if let Some(session) = session
        && let Some(step) = state.step_on_row(session, &finding.id)
    {
        body = body.child(verify_step(
            session, step, index, state, access, surface, cx,
        ));
    }
    body.child(probe(surface.regions, Region::Finding(names.row, index)))
        .into_any_element()
}

/// Where a *Verify* of the row's session stands: the command first, the result after.
pub(super) fn verify_step<T: 'static>(
    session: SessionId,
    step: &VerifyStep,
    index: usize,
    _state: &FindingsState,
    access: fn(&mut T) -> &mut FindingsState,
    surface: &Surface<'_>,
    cx: &mut Context<T>,
) -> AnyElement {
    let (theme, tokens, names) = (surface.theme, surface.tokens, surface.names);
    let mut line = div()
        .id((names.step, 1000 + index))
        .relative()
        .flex()
        .flex_col()
        .gap(px(tokens.spacing))
        .pl(px(tokens.spacing * 2.0))
        .text_size(px(tokens.secondary_font_size));
    match step {
        VerifyStep::Planning => {
            line = line.child(dot("Looking for the test command…", theme, tokens));
        }
        VerifyStep::Ready(plan) => {
            line = line.child(div().child(SharedString::from(format!(
                "Would run `{}` in {} ({})",
                plan.command,
                plan.cwd.display(),
                plan.why
            ))));
            let mut buttons = div().flex().flex_wrap().gap(px(tokens.spacing));
            match plan.refused {
                None => {
                    let connection = surface.connection.clone();
                    buttons = buttons.child(
                        Button::new((names.run, index), "Run", theme, tokens)
                            .compact()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                access(this).run_verify(access, cx, &connection, session, false);
                            }))
                            .child(probe(surface.regions, Region::Finding(names.run, index))),
                    );
                }
                Some(VerifyRefusal::Running) => {
                    buttons = buttons.child(dot(
                        "A run of this session is already in progress.",
                        theme,
                        tokens,
                    ));
                }
                Some(VerifyRefusal::TurnRunning) => {
                    let connection = surface.connection.clone();
                    buttons = buttons
                        .child(dot("The session's own turn is running.", theme, tokens))
                        .child(
                            Button::new((names.run_anyway, index), "Run anyway", theme, tokens)
                                .compact()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    access(this).run_verify(access, cx, &connection, session, true);
                                }))
                                .child(probe(
                                    surface.regions,
                                    Region::Finding(names.run_anyway, index),
                                )),
                        );
                }
            }
            buttons = buttons.child(
                Button::new((names.cancel, index), "Cancel", theme, tokens)
                    .ghost()
                    .compact()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        access(this).forget_verify(session);
                        cx.notify();
                    }))
                    .child(probe(surface.regions, Region::Finding(names.cancel, index))),
            );
            line = line.child(buttons);
        }
        VerifyStep::Running(plan) => {
            line = line.child(dot(format!("Running `{}`…", plan.command), theme, tokens));
        }
        VerifyStep::Done(run) => {
            line = line.child(
                div()
                    .text_color(if run.status == VerifyStatus::Passed {
                        theme.success
                    } else {
                        theme.danger
                    })
                    .child(SharedString::from(run_headline(run))),
            );
            let tail: Vec<&str> = run.output_tail.lines().collect();
            let from = tail.len().saturating_sub(OUTPUT_LINES);
            if !tail.is_empty() {
                line = line
                    .child(dot(tail[from..].join("\n"), theme, tokens).font_family("monospace"));
            }
            line = line.child(
                Button::new((names.cancel, index), "Close", theme, tokens)
                    .ghost()
                    .compact()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        access(this).forget_verify(session);
                        cx.notify();
                    }))
                    .child(probe(surface.regions, Region::Finding(names.cancel, index))),
            );
        }
        VerifyStep::Failed(why) => {
            line = line
                .child(dot(format!("Not verified: {why}"), theme, tokens))
                .child(
                    Button::new((names.cancel, index), "Close", theme, tokens)
                        .ghost()
                        .compact()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            access(this).forget_verify(session);
                            cx.notify();
                        }))
                        .child(probe(surface.regions, Region::Finding(names.cancel, index))),
                );
        }
    }
    line.child(probe(surface.regions, Region::Finding(names.step, index)))
        .into_any_element()
}

/// The first line of a finished run: how it ended, with the exit code and the time.
pub(super) fn run_headline(run: &VerifyRun) -> String {
    let how = match run.status {
        VerifyStatus::Passed => "Tests passed".to_owned(),
        VerifyStatus::Failed => "Tests failed".to_owned(),
        VerifyStatus::TimedOut => "Tests timed out".to_owned(),
        VerifyStatus::Cancelled => "The run was cancelled".to_owned(),
        VerifyStatus::Error => format!(
            "The run could not start: {}",
            run.error.as_deref().unwrap_or("no reason given")
        ),
        VerifyStatus::Running => "Running".to_owned(),
    };
    let code = run
        .exit_code
        .map(|code| format!(" (exit {code})"))
        .unwrap_or_default();
    let time = run
        .duration_ms
        .map(|ms| format!(" in {:.1} s", ms as f64 / 1000.0))
        .unwrap_or_default();
    format!("{how}{code}{time}: `{}`", run.command)
}

/// The first row number of the runs that belong to no row.
pub(super) const ORPHAN_BASE: usize = 5000;

/// The *Verify* steps of sessions that none of `shown` is about any more (the finding the run
/// started from went away, as findings do): the result stays on screen until the user closes it.
pub(super) fn orphan_steps<T: 'static>(
    shown: &[&Finding],
    state: &FindingsState,
    access: fn(&mut T) -> &mut FindingsState,
    surface: &Surface<'_>,
    title_of: &dyn Fn(SessionId) -> String,
    cx: &mut Context<T>,
) -> Vec<AnyElement> {
    let mut orphans = state.unplaced(shown);
    orphans.sort_by_key(|(session, _)| session.to_string());
    orphans
        .into_iter()
        .enumerate()
        .map(|(n, (session, step))| {
            div()
                .flex()
                .flex_col()
                .gap(px(surface.tokens.spacing))
                .child(
                    div()
                        .text_size(px(surface.tokens.font_size))
                        .child(SharedString::from(format!(
                            "Tests of {}",
                            title_of(session)
                        ))),
                )
                .child(verify_step(
                    session,
                    step,
                    ORPHAN_BASE + n,
                    state,
                    access,
                    surface,
                    cx,
                ))
                .into_any_element()
        })
        .collect()
}

/// The first row number of the dismissed findings (their rows and *Restore* buttons).
pub(super) const DISMISSED_BASE: usize = 7000;

/// The rows of the dismissed `findings`, each with *Restore*: the title in muted words, the
/// agents it was about and when it was last seen. Numbered from [`DISMISSED_BASE`].
pub(super) fn dismissed_rows<T: 'static>(
    findings: &[&Finding],
    access: fn(&mut T) -> &mut FindingsState,
    surface: &Surface<'_>,
    cx: &mut Context<T>,
) -> Vec<AnyElement> {
    let (theme, tokens, names) = (surface.theme, surface.tokens, surface.names);
    findings
        .iter()
        .enumerate()
        .map(|(n, finding)| {
            let index = DISMISSED_BASE + n;
            let (word, color) = severity_word(finding.severity, theme);
            let titles = involved_titles(finding, surface.titles);
            let mut meta = Vec::new();
            if !titles.is_empty() {
                meta.push(titles.join(" · "));
            }
            meta.push(hutan_client::time::relative_text(
                surface.now_ms,
                finding.last_at_ms,
            ));
            let connection = surface.connection.clone();
            let id = finding.id.clone();
            div()
                .id((names.row, index))
                .relative()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing))
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(tokens.spacing))
                        .text_size(px(tokens.font_size))
                        .child(div().flex_none().text_color(color).child(word))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(theme.muted)
                                .child(SharedString::from(finding.title.clone())),
                        ),
                )
                .child(dot(meta.join(" · "), theme, tokens))
                .child(
                    div().flex().gap(px(tokens.spacing)).child(
                        Button::new((names.restore, index), "Restore", theme, tokens)
                            .ghost()
                            .compact()
                            .tooltip("List this finding again")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                access(this).restore(access, cx, &connection, id.clone());
                            }))
                            .child(probe(
                                surface.regions,
                                Region::Finding(names.restore, index),
                            )),
                    ),
                )
                .child(probe(surface.regions, Region::Finding(names.row, index)))
                .into_any_element()
        })
        .collect()
}

/// The agents a finding is about: its own session, or — for one about a checkout or a file that
/// several share, which has none — every session its evidence names (only those whose title is
/// known: a turn's id parses like a session's).
fn involved(finding: &Finding, titles: &HashMap<SessionId, String>) -> Vec<SessionId> {
    if let Some(session) = finding.scope.session_id {
        return vec![session];
    }
    let mut out: Vec<SessionId> = Vec::new();
    for evidence in &finding.evidence {
        if let Some(id) = evidence
            .reference
            .as_deref()
            .and_then(|r| r.parse::<SessionId>().ok())
            .filter(|id| titles.contains_key(id))
            && !out.contains(&id)
        {
            out.push(id);
        }
    }
    out
}

/// The titles of [`involved`], in the order the finding names them.
fn involved_titles(finding: &Finding, titles: &HashMap<SessionId, String>) -> Vec<String> {
    involved(finding, titles)
        .iter()
        .filter_map(|id| titles.get(id).cloned())
        .collect()
}

/// The line under a row's title: every agent the finding is about (two that share a checkout,
/// two that changed one file; one agent only where the list does not already say whose it is),
/// how many times it happened, and when it was last seen.
pub(super) fn meta_text(
    finding: &Finding,
    titles: &HashMap<SessionId, String>,
    show_session: bool,
    now_ms: u64,
) -> String {
    let agents = involved_titles(finding, titles);
    let mut meta = Vec::new();
    if agents.len() > 1 || (show_session && !agents.is_empty()) {
        meta.push(agents.join(" · "));
    }
    if finding.count > 1 {
        meta.push(format!("{} times", finding.count));
    }
    meta.push(hutan_client::time::relative_text(
        now_ms,
        finding.last_at_ms,
    ));
    meta.join(" · ")
}

/// The evidence lines of an opened row: the kind, where it comes from — a session by its title,
/// never by its id — and what was seen.
pub(super) fn evidence_lines(
    finding: &Finding,
    titles: &HashMap<SessionId, String>,
) -> Vec<String> {
    finding
        .evidence
        .iter()
        .map(|evidence| {
            let reference = match evidence.reference.as_deref() {
                // The excerpt may already name the session.
                Some(r) => match r.parse::<SessionId>().ok().and_then(|id| titles.get(&id)) {
                    Some(title) if evidence.excerpt.contains(title.as_str()) => String::new(),
                    Some(title) => format!(" ({title})"),
                    None => format!(" ({r})"),
                },
                None => String::new(),
            };
            format!(
                "{}{}: {}",
                evidence_word(evidence.kind),
                reference,
                if evidence.redacted {
                    "(withheld)".to_owned()
                } else {
                    evidence.excerpt.clone()
                }
            )
        })
        .collect()
}

/// The titles of every session `answer` names (its coverage) and of `extra`, as the sidebar
/// shows them now (`live`), else as the daemon wrote them.
pub(super) fn titles(
    answer: Option<&AgentsFindings>,
    live: &dyn Fn(SessionId) -> Option<String>,
    extra: impl IntoIterator<Item = SessionId>,
) -> HashMap<SessionId, String> {
    let mut out = HashMap::new();
    if let Some(answer) = answer {
        for coverage in &answer.coverage {
            out.insert(
                coverage.session_id,
                live(coverage.session_id).unwrap_or_else(|| coverage.title.clone()),
            );
        }
    }
    for id in extra {
        if let Some(title) = live(id) {
            out.insert(id, title);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::ui::contrast;

    #[test]
    fn severity_words_read_at_aa_on_every_ground_of_both_built_in_themes() {
        for theme in [Theme::dark(), Theme::light()] {
            let grounds = [
                theme.bg,
                theme.sidebar,
                theme.surface,
                theme.surface_hover,
                theme.sidebar_accent,
                theme.right_sidebar,
                theme.right_sidebar_accent,
            ];
            for severity in [
                FindingSeverity::Alert,
                FindingSeverity::Warn,
                FindingSeverity::Info,
            ] {
                let (word, color) = severity_word(severity, &theme);
                for ground in grounds {
                    let ratio = contrast(color, ground);
                    assert!(
                        ratio >= 4.5,
                        "{word} on a ground of the {} theme: {ratio:.2}:1",
                        if theme.dark { "dark" } else { "light" }
                    );
                }
            }
        }
    }
}
