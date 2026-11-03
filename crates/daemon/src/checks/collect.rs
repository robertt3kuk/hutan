//! [`ChecksService`]: reads the facts the detectors need and answers `agents.findings`.
//!
//! Like `agents.review` and `agents.stats` it builds the answer on request from what the daemon
//! records — the registry's sessions, the journal and the turn table, the checkpoints, the audit
//! tables, the state log, the tasks and the insight reducer — and keeps nothing a restart would
//! lose except what is cheap to read again. What it keeps is a cache per chat: the findings of
//! its journal, valid while the journal's last sequence, its turns and its checkpoints are the
//! ones they were read at. A turn that has ended is read once; a new event of a running turn
//! only re-reads that turn.
//!
//! Bounded: at most [`SESSIONS_MAX`] sessions of the scope are read, the newest
//! [`TURNS_ANALYSED`] turns of each, [`EVENTS_MAX`] events of a turn, [`TEST_PATCHES_MAX`] test
//! files' diffs of a turn; four chats are read at a time.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hutan_core::agent::{AgentEvent, AgentEventKind, ToolKind};
use hutan_core::checkpoint::{CheckpointInfo, CheckpointKind};
use hutan_core::insight::AgentInsight;
use hutan_core::task::DeliveryState;
use hutan_core::{SessionId, WorkspaceId};
use hutan_protocol::checkpoint::{
    CheckpointDiffRequest, CheckpointListRequest, DiffAgainst, LIST_MAX_LIMIT,
};
use hutan_protocol::checks::{
    AgentsFindings, AgentsFindingsDismiss, AgentsFindingsRequest, AgentsFindingsRestore,
    CoverageGap, Finding, FindingDismissed, FindingRestored, SessionCoverage, is_finding_id,
};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{AgentState, SessionInfo, SessionListFilter, TerminalAgentState};
use hutan_protocol::review::NotRecorded;
use hutan_protocol::task::TaskListRequest;
use hutan_storage::{AgentStateRow, ChatTurnRow, ChatTurnStatus, Storage};
use tokio::sync::Semaphore;
use tracing::warn;

use super::detectors::{self, JournalCoverage};
use super::facts::{
    BlockedSpan, ChangedFile, JournalFacts, LiveFacts, PlacementFact, RequestFact, SessionMeta,
    TaskFact, ToolFailure, TurnFacts, TurnFile, TurnFiles, VerifyFact,
};
use super::{AnswerInputs, Draft, TURNS_ANALYSED, Thresholds, assemble};
use crate::agents_review::{
    FilesInput, files_input, journal_files, last_assistant_message, merge_files, review_commands,
    tool_runs,
};
use crate::checkpoints::turns::TurnCheckpoints;
use crate::insight::InsightHub;
use crate::registry::{RegistryHandle, now_ms};
use crate::signs::{FileRole, file_role, split_patch};
use crate::state_log::StateLog;
use crate::tasks::TaskService;

/// The sessions of a scope one answer reads (live ones first, then the newest).
pub const SESSIONS_MAX: usize = 60;
/// The events of one turn that are read.
const EVENTS_MAX: u32 = 20_000;
/// The test files of one turn whose diff is read.
const TEST_PATCHES_MAX: usize = 8;
/// The characters of one test file's patch that are read.
const PATCH_CHARS_MAX: usize = 128 * 1024;
/// The requests of one chat the long-wait detector reads (the newest).
const REQUESTS_MAX: usize = 100;
/// Chats read at once.
const PARALLEL: usize = 4;
/// Chats whose findings are kept.
const CACHE_MAX: usize = 256;

/// What a service is made of.
#[derive(Debug)]
pub struct ChecksInputs {
    pub registry: RegistryHandle,
    pub storage: Storage,
    pub insight: Arc<InsightHub>,
    pub tasks: Arc<TaskService>,
    pub state_log: Arc<StateLog>,
    pub thresholds: Thresholds,
}

/// What the journal analysis of one chat found, with what it was read at.
#[derive(Debug)]
struct Analysis {
    fingerprint: u64,
    /// By turn id: what was read of a turn that ended, and what it was read at.
    turns: HashMap<String, (u64, TurnFacts)>,
    drafts: Vec<Draft>,
    gaps: Vec<CoverageGap>,
    /// When the newest finished turn with file changes ended.
    changed_at_ms: Option<u64>,
    /// When the chat's newest turn completed, if it is the newest and none runs now.
    done_since_ms: Option<u64>,
    /// The files the chat's insight lists, as it names them.
    files: Option<Vec<String>>,
}

#[derive(Debug)]
pub struct ChecksService {
    registry: RegistryHandle,
    storage: Storage,
    insight: Arc<InsightHub>,
    tasks: Arc<TaskService>,
    state_log: Arc<StateLog>,
    thresholds: Thresholds,
    cache: Mutex<HashMap<SessionId, Arc<Analysis>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // No critical section leaves the cache half updated across a panic point.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn internal(error: impl std::fmt::Display) -> ErrorBody {
    ErrorBody::new(ErrorCode::Internal, error.to_string())
}

fn hashed(parts: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    parts.hash(&mut hasher);
    hasher.finish()
}

/// A session that is an agent: a chat that is not an assistant or the digest, or a terminal
/// session an agent reported in or was recognised in.
fn is_agent(info: &SessionInfo) -> bool {
    match &info.chat {
        Some(chat) => !chat.is_assistant() && !chat.digest,
        None => info.agent.is_some() || info.terminal_agent.is_some(),
    }
}

fn harness_of(info: &SessionInfo) -> Option<String> {
    info.chat
        .as_ref()
        .map(|chat| chat.agent.clone())
        .or_else(|| info.terminal_agent.as_ref().map(|a| a.app.clone()))
        .or_else(|| info.agent.as_ref().map(|a| a.agent.clone()))
}

/// The folders the session may change: its own, the extra ones and the checkouts of its turns.
fn roots_of(info: &SessionInfo, checkouts: &[CheckpointInfo]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for root in checkouts
        .iter()
        .filter_map(|c| c.checkout.clone())
        .chain(info.cwd.clone())
        .chain(info.folder.clone())
        .chain(info.extra_dirs.iter().map(|dir| dir.path.clone()))
    {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

fn meta_of(info: &SessionInfo, roots: Vec<PathBuf>) -> SessionMeta {
    SessionMeta {
        id: info.id,
        workspace_id: info.workspace_id,
        title: info.title.clone(),
        harness: harness_of(info),
        chat: info.chat.is_some(),
        live: info.state.is_live(),
        created_ms: info.created_at_ms,
        roots,
    }
}

impl ChecksService {
    pub fn new(inputs: ChecksInputs) -> Arc<Self> {
        Arc::new(Self {
            registry: inputs.registry,
            storage: inputs.storage,
            insight: inputs.insight,
            tasks: inputs.tasks,
            state_log: inputs.state_log,
            thresholds: inputs.thresholds,
            cache: Mutex::new(HashMap::new()),
        })
    }

    async fn blocking<T: Send + 'static>(
        &self,
        read: impl FnOnce(&Storage) -> Result<T, hutan_storage::StorageError> + Send + 'static,
    ) -> Result<T, ErrorBody> {
        let storage = self.storage.clone();
        tokio::task::spawn_blocking(move || read(&storage))
            .await
            .map_err(internal)?
            .map_err(|error| internal(format!("reading what the findings are built from: {error}")))
    }

    /// `agents.findings`.
    pub async fn findings(
        self: &Arc<Self>,
        req: AgentsFindingsRequest,
    ) -> Result<AgentsFindings, ErrorBody> {
        let now = now_ms();
        let infos = self
            .registry
            .list_sessions(SessionListFilter::default())
            .await?
            .sessions;
        let workspaces = self.blocking(|s| s.workspaces().list()).await?;
        if let Some(asked) = req.workspace_id
            && !workspaces.iter().any(|w| w.id == asked)
        {
            return Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!("no workspace {asked}"),
            ));
        }
        let parent: HashMap<WorkspaceId, Option<WorkspaceId>> =
            workspaces.iter().map(|w| (w.id, w.parent_id)).collect();
        let in_workspace = |workspace: WorkspaceId| {
            req.workspace_id.is_none_or(|asked| {
                workspace == asked || parent.get(&workspace).copied().flatten() == Some(asked)
            })
        };
        if let Some(asked) = req.session_id {
            let Some(info) = infos.iter().find(|i| i.id == asked) else {
                return Err(ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("session {asked} not found"),
                ));
            };
            if !is_agent(info) {
                return Err(ErrorBody::new(
                    ErrorCode::BadRequest,
                    format!(
                        "session {asked} is not an agent: it is a shell, an assistant or the digest"
                    ),
                ));
            }
            if !in_workspace(info.workspace_id) {
                return Err(ErrorBody::new(
                    ErrorCode::BadRequest,
                    format!("session {asked} is not in workspace {:?}", req.workspace_id),
                ));
            }
        }

        // The sessions of the scope: the live ones first, then the newest.
        let mut scope: Vec<&SessionInfo> = infos
            .iter()
            .filter(|i| is_agent(i))
            .filter(|i| match req.session_id {
                Some(asked) => i.id == asked,
                None => in_workspace(i.workspace_id),
            })
            .collect();
        scope.sort_by(|a, b| {
            b.state
                .is_live()
                .cmp(&a.state.is_live())
                .then(b.created_at_ms.cmp(&a.created_at_ms))
        });
        let mut not_recorded = vec![NotRecorded::new(
            format!("turns older than the newest {TURNS_ANALYSED} of each chat"),
            "the detectors read the newest turns of a chat only",
        )];
        if scope.len() > SESSIONS_MAX {
            not_recorded.push(NotRecorded::new(
                format!("{} sessions of the scope", scope.len() - SESSIONS_MAX),
                format!(
                    "only the newest {SESSIONS_MAX} sessions are read; name a workspace or a session"
                ),
            ));
            scope.truncate(SESSIONS_MAX);
        }
        if !self.state_log.enabled() {
            not_recorded.push(NotRecorded::new(
                "how long terminal agents waited",
                "the state log is off (`[agents] state_log` in daemon.toml)",
            ));
        }

        // Every live agent's files, for the overlap of two sessions; the scope's journals.
        let chats: Vec<SessionInfo> = scope
            .iter()
            .filter(|i| i.chat.is_some())
            .map(|i| (*i).clone())
            .collect();
        let analyses = self.analyses(chats).await;
        let live_infos: Vec<&SessionInfo> = {
            let mut live: Vec<&SessionInfo> = scope
                .iter()
                .copied()
                .filter(|i| i.state.is_live())
                .collect();
            let in_scope: Vec<SessionId> = scope.iter().map(|i| i.id).collect();
            live.extend(
                infos
                    .iter()
                    .filter(|i| is_agent(i) && i.state.is_live() && !in_scope.contains(&i.id)),
            );
            live
        };
        // What the integrations of terminal agents report, and the live sessions outside the
        // scope for the overlap of two sessions.
        let mut wanted: Vec<&SessionInfo> =
            scope.iter().copied().filter(|i| i.chat.is_none()).collect();
        for info in &live_infos {
            if !analyses.contains_key(&info.id) && !wanted.iter().any(|w| w.id == info.id) {
                wanted.push(info);
            }
        }
        let insights = self.insights(&wanted).await;

        let ctx = self.context(now, &infos).await?;
        let mut findings: Vec<Finding> = Vec::new();
        let mut coverage: Vec<SessionCoverage> = Vec::new();
        let mut live_facts: Vec<LiveFacts> = Vec::new();
        for info in &live_infos {
            let analysis = analyses.get(&info.id).and_then(|a| a.as_ref().ok());
            let files = match analysis {
                Some(analysis) => analysis.files.clone(),
                None => files_of(&insights, info.id),
            };
            live_facts.push(self.live_facts(info, analysis, files, &ctx));
        }
        let live_ids: Vec<SessionId> = live_infos.iter().map(|i| i.id).collect();
        for info in &scope {
            let analysis = analyses.get(&info.id);
            if let Some(Ok(analysis)) = analysis {
                findings.extend(analysis.drafts.iter().cloned().map(|mut draft| {
                    draft.workspace = info.workspace_id;
                    draft.finish()
                }));
            }
            let live = match live_ids.iter().position(|id| *id == info.id) {
                Some(at) => live_facts[at].clone(),
                None => {
                    let analysis = analysis.and_then(|a| a.as_ref().ok());
                    let files = analysis
                        .and_then(|a| a.files.clone())
                        .or_else(|| files_of(&insights, info.id));
                    self.live_facts(info, analysis, files, &ctx)
                }
            };
            findings.extend(
                detectors::live_drafts(&live, &self.thresholds)
                    .into_iter()
                    .map(Draft::finish),
            );
            let terminal_insight = if analysis.is_none() {
                insights
                    .get(&info.id)
                    .filter(|insight| insight.as_ref().is_ok_and(reports_something))
            } else {
                None
            };
            if let Some(insight) = terminal_insight {
                // What its integration reports feeds the detectors that read the insight.
                let journal = JournalFacts {
                    meta: meta_of(info, roots_of(info, &[])),
                    turns: Vec::new(),
                    turns_total: 0,
                    insight: insight.clone(),
                };
                findings.extend(
                    detectors::journal_drafts(&journal, &self.thresholds)
                        .into_iter()
                        .map(Draft::finish),
                );
            }
            let terminal_gaps = terminal_insight
                .map(|insight| detectors::insight_gaps(insight, "terminal agent"))
                .unwrap_or_default();
            let journal = match analysis {
                Some(Ok(analysis)) => JournalCoverage::Read(&analysis.gaps),
                Some(Err(why)) => JournalCoverage::Unreadable(why.as_str()),
                None if terminal_insight.is_some() => {
                    JournalCoverage::TerminalReported(&terminal_gaps)
                }
                None => JournalCoverage::TerminalAgent,
            };
            coverage.push(detectors::coverage(&live, journal));
        }
        // Across sessions: the findings that concern a session of the scope.
        let in_scope: Vec<SessionId> = scope.iter().map(|i| i.id).collect();
        let mut ordered: Vec<&LiveFacts> = live_facts.iter().collect();
        ordered.sort_by_key(|l| !in_scope.contains(&l.meta.id));
        findings.extend(
            detectors::workspace_drafts(&ordered)
                .into_iter()
                .filter(|d| d.members.iter().any(|m| in_scope.contains(m)))
                .map(Draft::finish),
        );

        let dismissed = self.blocking(|s| s.dismissed_findings().all()).await?;
        Ok(assemble(AnswerInputs {
            now_ms: now,
            findings,
            dismissed: &dismissed,
            req: &req,
            coverage,
            not_recorded,
        }))
    }

    /// `agents.findings.dismiss`.
    pub async fn dismiss(&self, req: AgentsFindingsDismiss) -> Result<FindingDismissed, ErrorBody> {
        if !is_finding_id(&req.id) {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!(
                    "`{}` is not the id of a finding (chk_ and 16 hex digits)",
                    req.id
                ),
            ));
        }
        let id = req.id.clone();
        let at = now_ms();
        let dismissed_at_ms = self
            .blocking(move |s| s.dismissed_findings().dismiss(&id, at))
            .await?;
        Ok(FindingDismissed {
            id: req.id,
            dismissed_at_ms,
        })
    }

    /// `agents.findings.restore`: the dismissal is forgotten, the finding is listed again.
    pub async fn restore(&self, req: AgentsFindingsRestore) -> Result<FindingRestored, ErrorBody> {
        if !is_finding_id(&req.id) {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!(
                    "`{}` is not the id of a finding (chk_ and 16 hex digits)",
                    req.id
                ),
            ));
        }
        let id = req.id.clone();
        let restored = self
            .blocking(move |s| s.dismissed_findings().restore(&id))
            .await?;
        Ok(FindingRestored {
            id: req.id,
            restored,
        })
    }

    // Context ---------------------------------------------------------------------------------

    /// What every session's live facts are read from, read once.
    async fn context(&self, now: u64, infos: &[SessionInfo]) -> Result<Context, ErrorBody> {
        let agents: Vec<SessionId> = infos.iter().filter(|i| is_agent(i)).map(|i| i.id).collect();
        let chats: Vec<SessionId> = infos
            .iter()
            .filter(|i| is_agent(i) && i.chat.is_some())
            .map(|i| i.id)
            .collect();
        let log_on = self.state_log.enabled();
        let rows = self
            .blocking(move |s| {
                let mut requests: HashMap<SessionId, Vec<RequestFact>> = HashMap::new();
                let mut last_event: HashMap<SessionId, u64> = HashMap::new();
                for id in chats {
                    let mut facts: Vec<RequestFact> = Vec::new();
                    for row in s.approvals().list(Some(id))? {
                        facts.push(RequestFact {
                            id: row.id.to_string(),
                            kind: "approval",
                            title: row.title,
                            asked_ms: row.requested_at_ms,
                            resolved_ms: row.resolved_at_ms,
                        });
                    }
                    for row in s.questions().list(Some(id))? {
                        let plan = hutan_agents_view::presentation::is_plan_title(&row.title);
                        facts.push(RequestFact {
                            id: row.id.to_string(),
                            kind: if plan { "plan" } else { "question" },
                            title: row.title,
                            asked_ms: row.requested_at_ms,
                            resolved_ms: row.resolved_at_ms,
                        });
                    }
                    facts.sort_by_key(|f| std::cmp::Reverse(f.asked_ms));
                    facts.truncate(REQUESTS_MAX);
                    requests.insert(id, facts);
                    if let Some(item) = s.chat().items(id, None, 1)?.last() {
                        last_event.insert(id, item.ts_ms);
                    }
                }
                let state_rows = if log_on {
                    s.agent_state_log()
                        .in_window(now.saturating_sub(14 * 86_400_000), now)?
                } else {
                    Vec::new()
                };
                let repo_roots: HashMap<PathBuf, PathBuf> = s
                    .worktrees()
                    .list()?
                    .into_iter()
                    .map(|w| (w.path, w.repo_root))
                    .collect();
                let verify: HashMap<SessionId, VerifyFact> = s
                    .verify_runs()
                    .newest_for_sessions(&agents)?
                    .into_iter()
                    .map(|row| {
                        (
                            row.session_id,
                            VerifyFact {
                                run_id: row.id,
                                last_line: row
                                    .output_tail
                                    .lines()
                                    .rev()
                                    .find(|l| !l.trim().is_empty())
                                    .map(str::to_owned),
                                command: row.command,
                                status: row.status,
                                exit_code: row.exit_code,
                                started_ms: row.started_ms,
                                ended_ms: row.ended_ms,
                                requested_by: row.requested_by,
                            },
                        )
                    })
                    .collect();
                Ok((requests, last_event, state_rows, repo_roots, verify))
            })
            .await?;
        let (requests, last_event, state_rows, repo_roots, verify) = rows;
        let mut states: HashMap<SessionId, Vec<AgentStateRow>> = HashMap::new();
        for row in state_rows {
            states.entry(row.session_id).or_default().push(row);
        }
        Ok(Context {
            now,
            requests,
            last_event,
            states,
            repo_roots,
            tasks: self.tasks.list(TaskListRequest::default()).tasks,
            state_log: log_on,
            verify,
        })
    }

    /// The live facts of a session.
    fn live_facts(
        &self,
        info: &SessionInfo,
        analysis: Option<&Arc<Analysis>>,
        files: Option<Vec<String>>,
        ctx: &Context,
    ) -> LiveFacts {
        let roots = roots_of(info, &[]);
        let meta = meta_of(info, roots);
        let chat = info.chat.is_some();
        let placement = info.placement.as_ref().map(|p| PlacementFact {
            root: p.root.clone(),
            shared_with: p.shared_with.clone(),
        });
        let requests = ctx.requests.get(&info.id).cloned().unwrap_or_default();
        let pending_request = info
            .chat
            .as_ref()
            .is_some_and(|c| c.pending_approvals > 0 || c.pending_questions > 0)
            || requests
                .iter()
                .any(|r| r.resolved_ms.is_none() && meta.live);
        let working_since_ms = (!chat)
            .then(|| {
                info.agent
                    .as_ref()
                    .filter(|a| a.state == AgentState::Working)
                    .map(|a| a.updated_at_ms)
                    .or_else(|| {
                        info.terminal_agent
                            .as_ref()
                            .filter(|a| a.state == TerminalAgentState::Working)
                            .map(|a| a.since_ms)
                    })
            })
            .flatten();
        let task = ctx
            .tasks
            .iter()
            .filter(|t| {
                t.delivery == DeliveryState::AwaitingReview && t.sessions.contains(&info.id)
            })
            .map(|t| TaskFact {
                title: t.title.clone(),
                awaiting_since_ms: t
                    .delivery_history
                    .iter()
                    .rev()
                    .find(|r| r.state == DeliveryState::AwaitingReview)
                    .map(|r| r.at_ms)
                    .or(t.ended_at_ms)
                    .unwrap_or(t.created_at_ms),
            })
            .max_by_key(|t| t.awaiting_since_ms);
        let cwd = info.cwd.clone().or_else(|| info.folder.clone());
        let changed_files = files.map(|paths| {
            paths
                .iter()
                .map(|path| {
                    changed_file(
                        path,
                        cwd.as_deref(),
                        info.placement.as_ref().map(|p| p.root.as_path()),
                        &ctx.repo_roots,
                    )
                })
                .collect()
        });
        LiveFacts {
            meta,
            now_ms: ctx.now,
            turn_running: info.chat.as_ref().is_some_and(|c| c.turn_active),
            last_event_ms: ctx.last_event.get(&info.id).copied(),
            working_since_ms,
            pending_request,
            requests,
            blocked_spans: blocked_spans(
                ctx.states.get(&info.id).map_or(&[], Vec::as_slice),
                ctx.now,
            ),
            state_log: ctx.state_log,
            task,
            changed_at_ms: analysis.and_then(|a| a.changed_at_ms),
            placement,
            files: changed_files,
            verify: ctx.verify.get(&info.id).cloned(),
            done_since_ms: if chat {
                analysis.and_then(|a| a.done_since_ms)
            } else {
                info.agent
                    .as_ref()
                    .filter(|a| a.state == AgentState::Idle)
                    .map(|a| a.updated_at_ms)
                    .or_else(|| {
                        info.terminal_agent
                            .as_ref()
                            .filter(|a| a.state == TerminalAgentState::Idle)
                            .map(|a| a.since_ms)
                    })
            },
        }
    }

    // Journals --------------------------------------------------------------------------------

    /// The analyses of `chats`, four at a time: by session, the error in words when a journal
    /// could not be read.
    async fn analyses(
        self: &Arc<Self>,
        chats: Vec<SessionInfo>,
    ) -> HashMap<SessionId, Result<Arc<Analysis>, String>> {
        let permits = Arc::new(Semaphore::new(PARALLEL));
        let mut set = tokio::task::JoinSet::new();
        for info in chats {
            let this = self.clone();
            let permits = permits.clone();
            set.spawn(async move {
                // The semaphore is never closed.
                let _permit = permits.acquire_owned().await.ok();
                (info.id, this.analyse(&info).await)
            });
        }
        let mut out = HashMap::new();
        while let Some(done) = set.join_next().await {
            match done {
                Ok((id, result)) => {
                    out.insert(id, result);
                }
                Err(error) => warn!(%error, "reading a chat for its findings failed"),
            }
        }
        out
    }

    /// The insight of each of `sessions` (a terminal agent's is what its integration reports;
    /// a chat outside the scope has the one its journal gives), or why there is none.
    async fn insights(
        &self,
        sessions: &[&SessionInfo],
    ) -> HashMap<SessionId, Result<AgentInsight, String>> {
        let mut out = HashMap::new();
        for info in sessions {
            let insight = match self.registry.insight_target(info.id).await {
                Ok(target) => self
                    .insight
                    .snapshot(info.id, &target)
                    .await
                    .map_err(|e| e.message),
                Err(error) => Err(error.message),
            };
            out.insert(info.id, insight);
        }
        out
    }

    /// The findings of one chat's journal, from the cache while nothing changed.
    async fn analyse(&self, info: &SessionInfo) -> Result<Arc<Analysis>, String> {
        let id = info.id;
        let (rows, last_seq) = self
            .blocking(move |s| Ok((s.chat().turns(id)?, s.chat().last_seq(id)?)))
            .await
            .map_err(|e| e.message)?;
        let checkpoints = self.registry.chats().checkpoints().clone();
        let cp_infos: Vec<CheckpointInfo> = match checkpoints
            .list(CheckpointListRequest {
                session_id: Some(id),
                workspace_id: None,
                limit: Some(LIST_MAX_LIMIT),
                before: None,
            })
            .await
        {
            Ok(list) => list.checkpoints,
            Err(error) => {
                warn!(session = %id, error = %error.message, "the checkpoints could not be listed for the findings");
                Vec::new()
            }
        };
        let fingerprint = hashed((
            last_seq,
            rows.len(),
            rows.last().map(|r| r.status as u8),
            cp_infos
                .iter()
                .map(|c| {
                    (
                        c.id.to_string(),
                        c.state.as_str(),
                        c.files_changed,
                        c.added,
                        c.removed,
                    )
                })
                .collect::<Vec<_>>(),
        ));
        let previous = lock(&self.cache).get(&id).cloned();
        if let Some(previous) = &previous
            && previous.fingerprint == fingerprint
        {
            return Ok(previous.clone());
        }

        let roots = roots_of(info, &cp_infos);
        let meta = meta_of(info, roots);
        let total = rows.len();
        let skipped = total.saturating_sub(TURNS_ANALYSED);
        let mut turns: Vec<TurnFacts> = Vec::new();
        let mut kept: HashMap<String, (u64, TurnFacts)> = HashMap::new();
        for (index, row) in rows.iter().enumerate().skip(skipped) {
            let cp = cp_infos
                .iter()
                .find(|c| {
                    c.kind == CheckpointKind::Turn
                        && c.turn_id.is_some_and(|t| t.to_string() == row.turn_id)
                })
                .cloned();
            let turn_fp = hashed((
                row.status as u8,
                row.ended_at_ms,
                cp.as_ref()
                    .map(|c| (c.state.as_str(), c.files_changed, c.added, c.removed)),
            ));
            let reusable = previous
                .as_ref()
                .and_then(|p| p.turns.get(&row.turn_id))
                .filter(|(fp, facts)| *fp == turn_fp && facts.status != ChatTurnStatus::Running);
            let facts = match reusable {
                Some((_, facts)) => facts.clone(),
                None => {
                    self.read_turn(
                        info,
                        &meta,
                        &checkpoints,
                        row,
                        u32::try_from(index + 1).unwrap_or(u32::MAX),
                        cp,
                    )
                    .await?
                }
            };
            if facts.status != ChatTurnStatus::Running {
                kept.insert(row.turn_id.clone(), (turn_fp, facts.clone()));
            }
            turns.push(facts);
        }

        let insight = match self.registry.insight_target(id).await {
            Ok(target) => self
                .insight
                .snapshot(id, &target)
                .await
                .map_err(|e| e.message),
            Err(error) => Err(error.message),
        };
        let files = insight.as_ref().ok().map(insight_paths);
        let journal = JournalFacts {
            meta,
            turns,
            turns_total: total,
            insight,
        };
        let changed_at_ms = journal
            .turns
            .iter()
            .filter(|t| t.status != ChatTurnStatus::Running && !t.files.files().is_empty())
            .map(TurnFacts::at_ms)
            .max();
        // The agent said it was done when its newest turn completed and none runs now.
        let done_since_ms = journal
            .turns
            .last()
            .filter(|t| t.status == ChatTurnStatus::Completed)
            .and_then(|t| t.ended_ms);
        let analysis = Arc::new(Analysis {
            fingerprint,
            turns: kept,
            drafts: detectors::journal_drafts(&journal, &self.thresholds),
            gaps: detectors::journal_gaps(&journal),
            changed_at_ms,
            done_since_ms,
            files,
        });
        let mut cache = lock(&self.cache);
        if cache.len() >= CACHE_MAX
            && !cache.contains_key(&id)
            && let Some(victim) = cache.keys().next().copied()
        {
            cache.remove(&victim);
        }
        cache.insert(id, analysis.clone());
        Ok(analysis)
    }

    /// One turn of a chat, reduced to what the detectors read.
    async fn read_turn(
        &self,
        info: &SessionInfo,
        meta: &SessionMeta,
        checkpoints: &Arc<TurnCheckpoints>,
        row: &ChatTurnRow,
        number: u32,
        cp: Option<CheckpointInfo>,
    ) -> Result<TurnFacts, String> {
        let session = info.id;
        let turn_key = row.turn_id.clone();
        let items = self
            .blocking(move |s| s.chat().items_of_turn(session, &turn_key, EVENTS_MAX))
            .await
            .map_err(|e| e.message)?;
        let events: Vec<AgentEvent> = items
            .iter()
            .filter_map(|item| serde_json::from_str(&item.event_json).ok())
            .collect();
        let runs = tool_runs(&events);
        let tool_failures: Vec<ToolFailure> = runs
            .iter()
            .filter(|run| run.kind != ToolKind::Command)
            .filter_map(|run| {
                Some(ToolFailure {
                    name: run.name.clone(),
                    text: run.failure.clone()?,
                })
            })
            .collect();
        let journal = journal_files(&events);
        let checkout = cp.as_ref().and_then(|c| c.checkout.clone());
        let root = checkout
            .as_deref()
            .or(meta.roots.first().map(PathBuf::as_path));
        let cp_id = cp.as_ref().map(|c| c.id);
        let files = match files_input(checkpoints, cp).await {
            FilesInput::Checkpoint { diff, .. } => {
                let seen = merge_files(&diff.files, &journal, root);
                TurnFiles::Checkpoint(self.with_patches(checkpoints, cp_id, seen, &events).await)
            }
            FilesInput::Unchanged { .. } => {
                let seen = merge_files(&[], &journal, root);
                TurnFiles::Checkpoint(self.with_patches(checkpoints, cp_id, seen, &events).await)
            }
            FilesInput::Missing(why) => {
                if journal.is_empty() {
                    TurnFiles::Missing(why)
                } else {
                    let seen = merge_files(&[], &journal, root);
                    TurnFiles::Journal(self.with_patches(checkpoints, None, seen, &events).await)
                }
            }
        };
        Ok(TurnFacts {
            turn_id: row.turn_id.parse().ok(),
            number,
            status: row.status,
            started_ms: row.started_at_ms,
            ended_ms: row.ended_at_ms,
            error: row.error.clone(),
            final_message: last_assistant_message(&events),
            commands: review_commands(&runs),
            tool_failures,
            files,
        })
    }

    /// The files with the patch of each test file (the checkpoint's diff, else what the
    /// journal's file changes carried).
    async fn with_patches(
        &self,
        checkpoints: &Arc<TurnCheckpoints>,
        checkpoint: Option<hutan_core::CheckpointId>,
        seen: Vec<crate::signs::SeenFile>,
        events: &[AgentEvent],
    ) -> Vec<TurnFile> {
        let mut files = Vec::with_capacity(seen.len());
        let mut read = 0;
        for file in seen {
            let mut patch = None;
            if file_role(&file.path) == Some(FileRole::Test) && read < TEST_PATCHES_MAX {
                read += 1;
                if let Some(checkpoint) = checkpoint
                    && !file.journal_only
                    && let Ok(diff) = checkpoints
                        .diff(CheckpointDiffRequest {
                            checkpoint_id: checkpoint,
                            against: DiffAgainst::End,
                            path: Some(file.path.clone()),
                            stat: false,
                        })
                        .await
                {
                    patch = diff.patch;
                }
                if patch.is_none() {
                    patch = journal_patch(events, &file.path);
                }
            }
            files.push(TurnFile {
                seen: file,
                patch: patch.map(|p| cut(p, PATCH_CHARS_MAX)),
            });
        }
        files
    }
}

/// What every session's live facts are read from.
#[derive(Debug)]
struct Context {
    now: u64,
    requests: HashMap<SessionId, Vec<RequestFact>>,
    last_event: HashMap<SessionId, u64>,
    states: HashMap<SessionId, Vec<AgentStateRow>>,
    /// A worktree's path → the repository's main checkout.
    repo_roots: HashMap<PathBuf, PathBuf>,
    tasks: Vec<hutan_core::task::Task>,
    state_log: bool,
    /// The newest run of Hutan's own tests of each session that has one.
    verify: HashMap<SessionId, VerifyFact>,
}

fn cut(text: String, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((at, _)) => text[..at].to_owned(),
        None => text,
    }
}

/// The patch of `path` that the journal's file changes carried: a change's own patch, or the
/// part of an aggregate patch that is about the file.
fn journal_patch(events: &[AgentEvent], path: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for event in events {
        let AgentEventKind::FileDiff { changes, patch } = &event.kind else {
            continue;
        };
        for change in changes {
            let named = change.path == path || Path::new(&change.path).ends_with(path);
            if named && let Some(patch) = &change.patch {
                parts.push(patch.clone());
            }
        }
        if let Some(patch) = patch {
            parts.extend(
                split_patch(patch)
                    .into_iter()
                    .filter(|(file, _)| file == path || Path::new(file).ends_with(path))
                    .map(|(_, part)| part.to_owned()),
            );
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// The files an insight lists, as it names them (relative to the session's folder when inside).
fn insight_paths(insight: &hutan_core::insight::AgentInsight) -> Vec<String> {
    insight
        .files
        .as_ref()
        .map(|files| files.items.iter().map(|f| f.path.clone()).collect())
        .unwrap_or_default()
}

/// The insight tells something: its integration reported tools, files, tokens or a context.
fn reports_something(insight: &AgentInsight) -> bool {
    insight.tools.is_some()
        || insight.files.is_some()
        || insight.totals.is_some()
        || insight.context.is_some()
        || !insight.turns.is_empty()
}

/// The files a session's insight lists, when it reports files at all.
fn files_of(
    insights: &HashMap<SessionId, Result<AgentInsight, String>>,
    id: SessionId,
) -> Option<Vec<String>> {
    insights
        .get(&id)
        .and_then(|insight| insight.as_ref().ok())
        .filter(|insight| insight.files.is_some())
        .map(insight_paths)
}

/// A file of a session by repository and path in it: a worktree and the main checkout of one
/// repository name the same file the same way.
fn changed_file(
    path: &str,
    cwd: Option<&Path>,
    checkout: Option<&Path>,
    repo_roots: &HashMap<PathBuf, PathBuf>,
) -> ChangedFile {
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.map_or_else(|| path.to_path_buf(), |cwd| cwd.join(path))
    };
    if let Some(checkout) = checkout
        && let Ok(inside) = absolute.strip_prefix(checkout)
    {
        let repo = repo_roots
            .get(checkout)
            .map_or(checkout, PathBuf::as_path)
            .display()
            .to_string();
        return ChangedFile {
            repo,
            path: inside.display().to_string(),
        };
    }
    ChangedFile {
        repo: String::new(),
        path: absolute.display().to_string(),
    }
}

/// The spans in which a terminal agent said it was blocked, from its rows of the state log (an
/// agent that says the state it is already in has not changed state).
fn blocked_spans(rows: &[AgentStateRow], now: u64) -> Vec<BlockedSpan> {
    let mut spans: Vec<BlockedSpan> = Vec::new();
    let mut open: Option<BlockedSpan> = None;
    for row in rows {
        if row.state == "blocked" {
            if open.is_none() {
                open = Some(BlockedSpan {
                    start_ms: row.at_ms,
                    end_ms: None,
                    kind: row.kind.clone().filter(|k| !k.is_empty()),
                });
            }
        } else if let Some(mut span) = open.take() {
            span.end_ms = Some(row.at_ms.min(now));
            spans.push(span);
        }
    }
    spans.extend(open);
    spans
}
