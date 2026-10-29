//! The client-side model of daemon state: what [`Connection::state`](crate::Connection::state)
//! publishes and how events fold into it. Pure data and logic, no I/O.

use std::collections::BTreeMap;
use std::time::Duration;

use hutan_core::{
    AgentEvent, AgentNode, AgentObservation, Epoch, SessionId, Task, TaskId, TerminalId,
    WorkspaceId,
};
use hutan_protocol::blocks::Block;
use hutan_protocol::chat::{ApprovalInfo, QuestionInfo};
use hutan_protocol::messages::{Event, SessionInfo, SessionSnapshot, StopBy, WorkspaceInfo};
use hutan_protocol::notify::NotificationIntent;
use hutan_protocol::resources::ResourceSample;

use crate::identity::DaemonIdentity;
use crate::start_failure::StartFailure;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// The first connection attempt is in progress.
    Connecting,
    /// Handshaken and synchronised: the rest of [`StoreState`] is current.
    Connected,
    /// The connection was lost (or could not be made); the next attempt starts in `retry_in`.
    /// The rest of [`StoreState`] is the last known view.
    Reconnecting { attempt: u32, retry_in: Duration },
    /// [`Connection::shutdown`](crate::Connection::shutdown) ran; nothing will change anymore.
    Stopped,
}

/// Latest known daemon state.
#[derive(Debug, Clone, PartialEq)]
pub struct StoreState {
    pub connection: ConnectionState,
    /// Epoch of the daemon this state describes; `None` before the first synchronisation.
    pub epoch: Option<Epoch>,
    /// The daemon's manual order (`position`); Home usually comes first. Sort modes, pins and
    /// filters are the UI's business, from the fields of [`WorkspaceInfo`].
    pub workspaces: Vec<WorkspaceInfo>,
    /// Creation order (`created_at_ms`, then id).
    pub sessions: Vec<SessionInfo>,
    /// Permission requests of agent chats waiting for an answer, oldest first (capability
    /// `chat.codex`). The daemon keeps them across client disconnects.
    pub approvals: Vec<ApprovalInfo>,
    /// Questions of agents waiting for an answer, oldest first (capability `chat.questions`);
    /// kept by the daemon like approvals.
    pub questions: Vec<QuestionInfo>,
    /// The observation tree of every session that has agents (capability `agent.tree`):
    /// the main agent and its subagents.
    pub trees: BTreeMap<SessionId, AgentObservation>,
    /// Why the daemon is not there, while it is not: the failure of the last autostart, or the
    /// start-error record a daemon left next to the socket. `None` while connected.
    pub daemon_start: Option<StartFailure>,
    /// Starting the daemon failed the same way [`AUTOSTART_GIVE_UP`](crate::AUTOSTART_GIVE_UP)
    /// times in a row: the connection no longer starts it by itself (it still dials) until
    /// [`Connection::start_daemon_now`](crate::Connection::start_daemon_now) (*Retry*) or a
    /// daemon that answers.
    pub autostart_halted: bool,
    /// The daemon this state describes (its `welcome`, protocol 0.42); the last one while
    /// disconnected, `None` before the first connection.
    pub daemon: Option<DaemonIdentity>,
    /// The daemon announced a stop someone asked for (`daemon.stopping` without `restart`,
    /// protocol 0.42): who. The connection then starts no daemon by itself until
    /// [`Connection::start_daemon_now`](crate::Connection::start_daemon_now) or
    /// [`Connection::set_autostart`](crate::Connection::set_autostart); `None` again once a
    /// daemon connects.
    pub stopped_by: Option<StopBy>,
}

/// Transient events that are not part of [`StoreState`].
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// The daemon's epoch changed: every terminal of the old daemon is gone.
    DaemonRestarted {
        old: Epoch,
        new: Epoch,
    },
    SessionExited {
        session: SessionId,
        code: Option<i32>,
        signal: Option<i32>,
    },
    /// The session was closed (`session.closed`, protocol 0.30) and is no longer in the state:
    /// close its tabs.
    SessionClosed {
        session: SessionId,
    },
    Bell {
        terminal: TerminalId,
    },
    TerminalNotification {
        terminal: TerminalId,
        title: Option<String>,
        body: String,
    },
    /// A block of the terminal started, changed state or was truncated (`terminal.block`,
    /// protocol 0.5). Fold it into a [`BlockList`](crate::BlockList).
    TerminalBlock {
        terminal: TerminalId,
        block: Block,
    },
    /// Something deserves the user's attention (`notify.intent`, protocol 0.13).
    NotifyIntent {
        intent: Box<NotificationIntent>,
    },
    /// A notification's "Open" was chosen (`ui.open`, protocol 0.13). `at_ms` is when the
    /// daemon sent it: a client that was offline when it went out finds it replayed, and
    /// should not act on an old one.
    UiOpen {
        session: SessionId,
        workspace: Option<WorkspaceId>,
        at_ms: u64,
    },
    /// Worktrees of a workspace's repositories were created or removed (`worktree.changed`,
    /// protocol 0.14): list them again.
    WorktreesChanged {
        workspace: WorkspaceId,
    },
    /// A task was created or changed (`task.updated`, protocol 0.16): the whole task as it is
    /// now.
    TaskUpdated {
        task: Box<Task>,
    },
    /// A task was deleted (`task.removed`, protocol 0.16).
    TaskRemoved {
        id: TaskId,
    },
    /// A login or logout session of a harness ended (`account.changed`, protocol 0.17): list
    /// the accounts again.
    AccountsChanged {
        harness: String,
    },
    /// A turn ended or a harness reported a limit (`usage.changed`, protocol 0.45): the whole
    /// summary as it is now.
    UsageChanged {
        summary: Box<hutan_protocol::usage::UsageSummary>,
    },
    /// A check found a release newer than the running Hutan that was not announced yet
    /// (`update.available`, protocol 0.60).
    UpdateAvailable {
        release: Box<hutan_protocol::update::UpdateRelease>,
    },
    /// The install `update.install` started changed state (`update.install`, protocol 0.60).
    UpdateInstall {
        install: Box<hutan_protocol::update::UpdateInstall>,
    },
    /// Mawas's memory changed, or a proposal arrived or was answered (`memory.changed`,
    /// protocol 0.32): list the entries and proposals again.
    MemoryChanged {
        scope: hutan_protocol::memory::MemoryScope,
        workspace: Option<WorkspaceId>,
        scope_revision: u64,
    },
    /// A periodic sample of the processes of the live sessions (`resources.updated`, protocol
    /// 0.18), sent while [`Connection::resources_subscribe`](crate::Connection::resources_subscribe)
    /// is in force; also the first sample after a reconnect re-subscribed. Outside the event
    /// sequence: a receiver that lags just waits for the next one.
    ResourcesUpdated {
        sample: Box<ResourceSample>,
    },
    /// The daemon asks this client, as a desktop window, to run an action or write a setting
    /// (`ui.command`, protocol 0.21). Answer with `ui.command_result` carrying `request_id`; it
    /// is never replayed.
    UiCommand {
        request_id: u64,
        window_id: String,
        action: String,
        args: serde_json::Value,
    },
    /// Paths this connection watches changed (`fs.changed`, protocol 0.33, capability
    /// `fs.watch`; [`Connection::fs_watch`](crate::Connection::fs_watch)). `overflow`: more
    /// happened than is listed — check every watched path again. After a reconnect the
    /// connection watches its paths again by itself and announces an empty `overflow`, since
    /// changes made while it was away were never sent. Outside the event sequence.
    FsChanged {
        changes: Vec<hutan_protocol::fs::FsChange>,
        overflow: bool,
    },
    /// Something below a watched root changed (`fs.generation`, protocol 0.33, capability
    /// `fs.watch`; at most one a second per root): the daemon's finder index of `root` and
    /// any tree of it a client keeps are stale. Sent to connections that watch a path below
    /// `root`. Outside the event sequence.
    FsGeneration {
        root: std::path::PathBuf,
        generation: u64,
    },
    /// How far a running `fs.search` of this connection got (`fs.search.progress`, protocol
    /// 0.33, capability `fs`; [`Connection::fs_search`](crate::Connection::fs_search)): the
    /// files looked at and the hits found so far, a few a second. Outside the event sequence.
    FsSearchProgress {
        search_id: String,
        files_searched: u64,
        hits: u32,
    },
    /// The insight of the chat this connection focused (`client.focus`) changed
    /// (`agent.insight_updated`, protocol 0.59, capability `agent.insight`): the whole
    /// snapshot; replace what you hold when its `seq` is larger. At most one a second per
    /// session. Outside the event sequence: a client that missed one asks `agent.insight`.
    AgentInsight {
        session_id: hutan_core::SessionId,
        insight: Box<hutan_core::insight::AgentInsight>,
    },
    /// A language server changed state (`lsp.state`, protocol 0.39, capability `lsp`): the
    /// whole entry as it is now. Sent to every connection.
    LspState {
        server: Box<hutan_protocol::lsp::LspServerInfo>,
    },
    /// A server replaced the diagnostics of `path` (`lsp.diagnostics`, protocol 0.39), a
    /// document this connection has open; an empty list clears the file. Outside the event
    /// sequence: a client that missed one asks `lsp.diagnostics`.
    LspDiagnostics {
        path: std::path::PathBuf,
        version: Option<i32>,
        diagnostics: Vec<hutan_protocol::lsp::LspDiagnostic>,
        truncated: bool,
    },
}

/// Side effects of folding events into the state, for the caller to act on after publishing.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Effects {
    pub notices: Vec<Notice>,
    /// Every session update folded, in order: the session and its terminal afterwards. A
    /// restart shows up as the same session with another terminal, possibly after an update
    /// without any (the daemon publishes the restart before the new terminal exists), which is
    /// why consumers track which session owns a terminal instead of diffing the state.
    pub session_terminals: Vec<(SessionId, Option<TerminalId>)>,
    /// Terminals that disappeared together with their session.
    pub removed: Vec<TerminalId>,
    /// Chat events folded, in order (not part of [`StoreState`]; they go to
    /// [`Connection::chat_updates`](crate::Connection::chat_updates)).
    pub chat: Vec<AgentEvent>,
    /// Chat queues that changed, in order (protocol 0.36; they go to
    /// [`Connection::chat_updates`](crate::Connection::chat_updates) too).
    pub queues: Vec<hutan_protocol::chat::ChatQueue>,
    /// Checkpoints recorded or removed, in order (protocol 0.37; they go to
    /// [`Connection::chat_updates`](crate::Connection::chat_updates) too).
    pub checkpoints: Vec<crate::chat::CheckpointUpdate>,
}

impl Effects {
    pub(crate) fn extend(&mut self, other: Effects) {
        self.notices.extend(other.notices);
        self.session_terminals.extend(other.session_terminals);
        self.removed.extend(other.removed);
        self.chat.extend(other.chat);
        self.queues.extend(other.queues);
        self.checkpoints.extend(other.checkpoints);
    }
}

fn session_key(session: &SessionInfo) -> (u64, SessionId) {
    (session.created_at_ms, session.id)
}

impl StoreState {
    pub(crate) fn initial() -> Self {
        Self {
            connection: ConnectionState::Connecting,
            epoch: None,
            workspaces: Vec::new(),
            sessions: Vec::new(),
            approvals: Vec::new(),
            questions: Vec::new(),
            trees: BTreeMap::new(),
            daemon_start: None,
            autostart_halted: false,
            daemon: None,
            stopped_by: None,
        }
    }

    /// The projects in workspace `id`, in the daemon's order (protocol 0.19).
    pub fn projects_in(&self, id: WorkspaceId) -> impl Iterator<Item = &WorkspaceInfo> {
        self.workspaces
            .iter()
            .filter(move |w| w.parent_id == Some(id))
    }

    /// The workspace `id` is in: itself when it is no member of one. A session's workspace for
    /// settings and layout is `workspace_of(session.workspace_id)`.
    pub fn workspace_of(&self, id: WorkspaceId) -> WorkspaceId {
        self.workspaces
            .iter()
            .find(|w| w.id == id)
            .and_then(|w| w.parent_id)
            .unwrap_or(id)
    }

    /// Replaces the pending approvals with what `approval.list` answered.
    pub(crate) fn install_approvals(&mut self, mut approvals: Vec<ApprovalInfo>) {
        approvals.sort_by(|a, b| {
            a.requested_at_ms
                .cmp(&b.requested_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        self.approvals = approvals;
    }

    /// Replaces the pending questions with what `question.list` answered.
    pub(crate) fn install_questions(&mut self, mut questions: Vec<QuestionInfo>) {
        questions.sort_by(|a, b| {
            a.requested_at_ms
                .cmp(&b.requested_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        self.questions = questions;
    }

    /// Replaces workspaces and sessions with `snapshot` (the daemon's view at one seq).
    pub(crate) fn install_snapshot(&mut self, snapshot: SessionSnapshot) -> Effects {
        let effects = Effects {
            session_terminals: snapshot
                .sessions
                .iter()
                .map(|s| (s.id, s.terminal_id))
                .collect(),
            ..Effects::default()
        };
        self.workspaces = snapshot.workspaces;
        self.workspaces.sort_by_key(|w| w.position);
        self.sessions = snapshot.sessions;
        self.sessions.sort_by_key(session_key);
        effects
    }

    /// Replaces the observation trees with what `agent.tree` answered.
    pub(crate) fn install_trees(&mut self, observations: Vec<AgentObservation>) {
        self.trees = observations
            .into_iter()
            .map(|observation| (observation.session_id, observation))
            .collect();
    }

    /// Folds an `agent.tree_updated` event into the session's tree. The root is the one node
    /// without a parent.
    fn apply_tree_update(
        &mut self,
        session: SessionId,
        changed: Vec<AgentNode>,
        removed: Vec<String>,
    ) {
        let observation = self
            .trees
            .entry(session)
            .or_insert_with(|| AgentObservation {
                session_id: session,
                root: String::new(),
                nodes: BTreeMap::new(),
            });
        for id in removed {
            observation.nodes.remove(&id);
        }
        for node in changed {
            observation.nodes.insert(node.node_id.clone(), node);
        }
        match observation.nodes.values().find(|n| n.parent.is_none()) {
            Some(root) => observation.root.clone_from(&root.node_id),
            None => {
                self.trees.remove(&session);
            }
        }
    }

    /// Drops everything learned from a previous daemon.
    pub(crate) fn clear(&mut self) {
        self.workspaces.clear();
        self.sessions.clear();
        self.approvals.clear();
        self.questions.clear();
        self.trees.clear();
    }

    /// Folds one event into the state. Events that only concern terminals' presentation (title)
    /// and unknown events change nothing.
    pub(crate) fn apply(&mut self, event: Event) -> Effects {
        let mut effects = Effects::default();
        match event {
            Event::WorkspaceUpdated { workspace } => {
                match self.workspaces.iter_mut().find(|w| w.id == workspace.id) {
                    Some(existing) => *existing = workspace,
                    None => self.workspaces.push(workspace),
                }
                // A move announces every changed position; the list follows them (stable, so
                // equal positions keep their arrival order).
                self.workspaces.sort_by_key(|w| w.position);
            }
            Event::WorkspaceClosed { workspace_id } => {
                self.workspaces.retain(|w| w.id != workspace_id);
                // The daemon removes a closed workspace's sessions without separate events.
                self.sessions.retain(|session| {
                    let keep = session.workspace_id != workspace_id;
                    if !keep {
                        effects.removed.extend(session.terminal_id);
                    }
                    keep
                });
                let sessions = &self.sessions;
                self.approvals
                    .retain(|a| sessions.iter().any(|s| s.id == a.session_id));
                self.questions
                    .retain(|q| sessions.iter().any(|s| s.id == q.session_id));
                self.trees
                    .retain(|id, _| sessions.iter().any(|s| s.id == *id));
            }
            Event::SessionUpdated { session } => {
                effects
                    .session_terminals
                    .push((session.id, session.terminal_id));
                match self.sessions.iter().position(|s| s.id == session.id) {
                    Some(at) => self.sessions[at] = session,
                    None => {
                        let at = self
                            .sessions
                            .partition_point(|s| session_key(s) < session_key(&session));
                        self.sessions.insert(at, session);
                    }
                }
            }
            Event::SessionExited {
                session_id,
                code,
                signal,
            } => effects.notices.push(Notice::SessionExited {
                session: session_id,
                code,
                signal,
            }),
            Event::SessionClosed { session_id } => {
                if let Some(at) = self.sessions.iter().position(|s| s.id == session_id) {
                    effects.removed.extend(self.sessions.remove(at).terminal_id);
                }
                self.approvals.retain(|a| a.session_id != session_id);
                self.questions.retain(|q| q.session_id != session_id);
                self.trees.remove(&session_id);
                effects.notices.push(Notice::SessionClosed {
                    session: session_id,
                });
            }
            Event::TerminalBell { terminal_id } => effects.notices.push(Notice::Bell {
                terminal: terminal_id,
            }),
            Event::TerminalNotification {
                terminal_id,
                title,
                body,
            } => effects.notices.push(Notice::TerminalNotification {
                terminal: terminal_id,
                title,
                body,
            }),
            Event::TerminalBlock { terminal_id, block } => {
                effects.notices.push(Notice::TerminalBlock {
                    terminal: terminal_id,
                    block,
                });
            }
            Event::TerminalTitle { .. } => {}
            Event::ApprovalRequired { approval } => {
                if !self.approvals.iter().any(|a| a.id == approval.id) {
                    self.approvals.push(*approval);
                }
            }
            Event::ApprovalResolved { approval_id, .. } => {
                self.approvals.retain(|a| a.id != approval_id);
            }
            Event::QuestionRequired { question } => {
                if !self.questions.iter().any(|q| q.id == question.id) {
                    self.questions.push(*question);
                }
            }
            Event::QuestionResolved { question_id, .. } => {
                self.questions.retain(|q| q.id != question_id);
            }
            // Chat events are not state of the sessions list: they go to whoever renders the
            // chat (`Connection::chat_updates`).
            Event::ChatEvent { event, .. } => effects.chat.push(*event),
            Event::ChatQueueChanged { queue, .. } => effects.queues.push(queue),
            // A chat's checkpoint state arrives in its `session.updated` too; checkpoints
            // themselves are read on demand (`checkpoint.list`), and their changes go to
            // whoever shows them (`Connection::chat_updates`).
            Event::CheckpointCreated { checkpoint } => effects
                .checkpoints
                .push(crate::chat::CheckpointUpdate::Created(checkpoint)),
            Event::CheckpointRemoved { ids } => effects
                .checkpoints
                .push(crate::chat::CheckpointUpdate::Removed(ids)),
            Event::CheckpointStateChanged { .. } => {}
            Event::AgentTreeUpdated {
                session_id,
                nodes_changed,
                nodes_removed,
            } => self.apply_tree_update(session_id, nodes_changed, nodes_removed),
            Event::NotifyIntent { intent } => effects.notices.push(Notice::NotifyIntent {
                intent: Box::new(intent),
            }),
            Event::UiOpen {
                session_id,
                workspace_id,
                at_ms,
            } => effects.notices.push(Notice::UiOpen {
                session: session_id,
                workspace: workspace_id,
                at_ms,
            }),
            Event::WorktreeChanged { workspace_id } => {
                effects.notices.push(Notice::WorktreesChanged {
                    workspace: workspace_id,
                });
            }
            Event::TaskUpdated { task } => effects.notices.push(Notice::TaskUpdated {
                task: Box::new(task),
            }),
            Event::TaskRemoved { id } => effects.notices.push(Notice::TaskRemoved { id }),
            Event::AccountChanged { harness } => {
                effects.notices.push(Notice::AccountsChanged { harness });
            }
            Event::UsageChanged { summary } => {
                effects.notices.push(Notice::UsageChanged { summary });
            }
            Event::UpdateAvailable { release } => {
                effects.notices.push(Notice::UpdateAvailable { release });
            }
            Event::UpdateInstallChanged { install } => {
                effects.notices.push(Notice::UpdateInstall { install });
            }
            Event::MemoryChanged {
                scope,
                workspace_id,
                scope_revision,
            } => effects.notices.push(Notice::MemoryChanged {
                scope,
                workspace: workspace_id,
                scope_revision,
            }),
            // A stream for one subscriber, outside the replay ring and the sequence.
            Event::ResourcesUpdated { sample } => {
                effects.notices.push(Notice::ResourcesUpdated { sample });
            }
            // A stream for the watching connection, outside the replay ring and the sequence.
            Event::FsChanged { changes, overflow } => {
                effects
                    .notices
                    .push(Notice::FsChanged { changes, overflow });
            }
            Event::FsGeneration { root, generation } => {
                effects
                    .notices
                    .push(Notice::FsGeneration { root, generation });
            }
            // A snapshot for the connection that focused the chat, outside the sequence.
            Event::AgentInsightUpdated {
                session_id,
                insight,
            } => effects.notices.push(Notice::AgentInsight {
                session_id,
                insight,
            }),
            // The search's progress, for whoever runs the search on this connection.
            Event::FsSearchProgress {
                search_id,
                files_searched,
                hits,
            } => effects.notices.push(Notice::FsSearchProgress {
                search_id,
                files_searched,
                hits,
            }),
            // Language servers (protocol 0.39): the servers' state and the diagnostics of the
            // open documents are no state of the sessions list; they reach whoever shows them
            // (the editor, the Problems panel) as notices.
            Event::LspState { server } => effects.notices.push(Notice::LspState { server }),
            Event::LspDiagnosticsChanged {
                path,
                version,
                diagnostics,
                truncated,
            } => effects.notices.push(Notice::LspDiagnostics {
                path,
                version,
                diagnostics,
                truncated,
            }),
            Event::UiCommand {
                request_id,
                window_id,
                action,
                args,
            } => effects.notices.push(Notice::UiCommand {
                request_id,
                window_id,
                action,
                args,
            }),
            // Protocol 0.42: a stop the user wants kept. A restart is followed by a daemon at
            // once and changes nothing here.
            Event::DaemonStopping { by, restart } => {
                if !restart {
                    self.stopped_by = Some(by);
                }
            }
        }
        effects
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use hutan_core::{SessionKind, SessionState, WorkspaceId};
    use hutan_protocol::messages::{CreatedBy, WorkspaceKind};

    use super::*;

    fn session(
        workspace_id: WorkspaceId,
        created_at_ms: u64,
        terminal: Option<u64>,
    ) -> SessionInfo {
        SessionInfo {
            forked_from: None,
            extra_dirs: Vec::new(),
            containment: None,
            terminal_agent: None,
            id: SessionId::new(),
            workspace_id,
            kind: SessionKind::Terminal,
            title: "t".into(),
            state: SessionState::Running,
            command: Vec::new(),
            cwd: None,
            terminal_id: terminal.map(TerminalId),
            created_at_ms,
            agent: None,
            restored: false,
            chat: None,
            placement: None,
            folder: None,
            recovery: None,
            muted: false,
        }
    }

    fn workspace(name: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            id: WorkspaceId::new(),
            name: name.into(),
            roots: vec![PathBuf::from("/r")],
            kind: WorkspaceKind::Project,
            pinned: false,
            position: 0,
            created_by: CreatedBy::User,
            last_active_ms: 0,
            live_sessions: 0,
            total_sessions: 0,
            parent_id: None,
            project: None,
        }
    }

    #[test]
    fn projects_are_found_by_their_workspace() {
        let mut state = StoreState::initial();
        let container = WorkspaceInfo {
            kind: WorkspaceKind::Workspace,
            ..workspace("shop")
        };
        let member = WorkspaceInfo {
            parent_id: Some(container.id),
            ..workspace("api")
        };
        let loose = workspace("notes");
        for w in [&container, &member, &loose] {
            state.apply(Event::WorkspaceUpdated {
                workspace: w.clone(),
            });
        }
        let names: Vec<_> = state
            .projects_in(container.id)
            .map(|w| w.name.as_str())
            .collect();
        assert_eq!(names, ["api"]);
        assert_eq!(state.workspace_of(member.id), container.id);
        assert_eq!(state.workspace_of(loose.id), loose.id);
        let unknown = WorkspaceId::new();
        assert_eq!(state.workspace_of(unknown), unknown);
    }

    #[test]
    fn workspaces_keep_arrival_order_and_update_in_place() {
        let mut state = StoreState::initial();
        let (a, b) = (workspace("a"), workspace("b"));
        state.apply(Event::WorkspaceUpdated {
            workspace: a.clone(),
        });
        state.apply(Event::WorkspaceUpdated {
            workspace: b.clone(),
        });
        let renamed = WorkspaceInfo {
            name: "a2".into(),
            ..a
        };
        state.apply(Event::WorkspaceUpdated {
            workspace: renamed.clone(),
        });
        assert_eq!(state.workspaces, vec![renamed, b]);
    }

    #[test]
    fn workspace_updates_reorder_the_list_by_position() {
        let mut state = StoreState::initial();
        let at = |name: &str, position| WorkspaceInfo {
            position,
            ..workspace(name)
        };
        let (home, a, b) = (at("home", 0), at("a", 1), at("b", 2));
        for w in [&b, &home, &a] {
            state.apply(Event::WorkspaceUpdated {
                workspace: w.clone(),
            });
        }
        assert_eq!(state.workspaces, vec![home.clone(), a.clone(), b.clone()]);

        // `b` moves before `a`: the daemon announces both new positions.
        let b_moved = WorkspaceInfo {
            position: 1,
            ..b.clone()
        };
        let a_moved = WorkspaceInfo {
            position: 2,
            ..a.clone()
        };
        for w in [&b_moved, &a_moved] {
            state.apply(Event::WorkspaceUpdated {
                workspace: (*w).clone(),
            });
        }
        assert_eq!(state.workspaces, vec![home.clone(), b_moved, a_moved]);

        // A snapshot installs the daemon's order too.
        let snapshot = SessionSnapshot {
            workspaces: vec![b.clone(), a.clone(), home.clone()],
            sessions: Vec::new(),
            seq: hutan_core::Seq(1),
        };
        state.install_snapshot(snapshot);
        assert_eq!(state.workspaces, vec![home, a, b]);
    }

    #[test]
    fn sessions_sort_by_creation_and_upsert_without_duplicates() {
        let mut state = StoreState::initial();
        let ws = WorkspaceId::new();
        let late = session(ws, 20, Some(2));
        let early = session(ws, 10, Some(1));
        for s in [late.clone(), early.clone(), early.clone()] {
            state.apply(Event::SessionUpdated { session: s });
        }
        assert_eq!(state.sessions, vec![early, late]);
    }

    #[test]
    fn session_updates_report_the_terminal_even_through_a_restart_gap() {
        let mut state = StoreState::initial();
        let first = session(WorkspaceId::new(), 1, Some(5));
        let gap = SessionInfo {
            terminal_id: None,
            ..first.clone()
        };
        let restarted = SessionInfo {
            terminal_id: Some(TerminalId(6)),
            ..first.clone()
        };
        let mut effects = Effects::default();
        for s in [first.clone(), gap, restarted] {
            effects.extend(state.apply(Event::SessionUpdated { session: s }));
        }
        assert_eq!(
            effects.session_terminals,
            vec![
                (first.id, Some(TerminalId(5))),
                (first.id, None),
                (first.id, Some(TerminalId(6)))
            ]
        );
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].terminal_id, Some(TerminalId(6)));
    }

    #[test]
    fn closing_a_workspace_removes_its_sessions() {
        let mut state = StoreState::initial();
        let ws = workspace("w");
        let keep = session(WorkspaceId::new(), 1, Some(1));
        let gone = session(ws.id, 2, Some(2));
        state.apply(Event::WorkspaceUpdated {
            workspace: ws.clone(),
        });
        for s in [keep.clone(), gone] {
            state.apply(Event::SessionUpdated { session: s });
        }
        let effects = state.apply(Event::WorkspaceClosed {
            workspace_id: ws.id,
        });
        assert!(state.workspaces.is_empty());
        assert_eq!(state.sessions, vec![keep]);
        assert_eq!(effects.removed, vec![TerminalId(2)]);
    }

    #[test]
    fn snapshot_install_reports_every_session_terminal_and_sorts() {
        let mut state = StoreState::initial();
        let ws = WorkspaceId::new();
        let old = session(ws, 1, Some(1));
        state.apply(Event::SessionUpdated {
            session: old.clone(),
        });
        let restarted = SessionInfo {
            terminal_id: Some(TerminalId(9)),
            ..old
        };
        let later = session(ws, 5, Some(3));
        let effects = state.install_snapshot(SessionSnapshot {
            workspaces: Vec::new(),
            sessions: vec![later.clone(), restarted.clone()],
            seq: hutan_core::Seq(4),
        });
        assert_eq!(
            effects.session_terminals,
            vec![
                (later.id, Some(TerminalId(3))),
                (restarted.id, Some(TerminalId(9)))
            ]
        );
        assert_eq!(state.sessions, vec![restarted, later]);
    }

    #[test]
    fn block_events_become_notices() {
        use hutan_protocol::blocks::BlockState;
        let mut state = StoreState::initial();
        let block = Block {
            id: 3,
            prompt_line: 10,
            command_line: Some(10),
            output_start: Some(11),
            end_line: None,
            command: Some("make".into()),
            exit_code: None,
            started_at_ms: 5,
            finished_at_ms: None,
            cwd: None,
            state: BlockState::Running,
        };
        let effects = state.apply(Event::TerminalBlock {
            terminal_id: TerminalId(4),
            block: block.clone(),
        });
        assert_eq!(
            effects.notices,
            vec![Notice::TerminalBlock {
                terminal: TerminalId(4),
                block
            }]
        );
    }

    #[test]
    fn exit_bell_and_notification_become_notices() {
        let mut state = StoreState::initial();
        let id = SessionId::new();
        let mut effects = Effects::default();
        effects.extend(state.apply(Event::SessionExited {
            session_id: id,
            code: Some(3),
            signal: None,
        }));
        effects.extend(state.apply(Event::TerminalBell {
            terminal_id: TerminalId(1),
        }));
        effects.extend(state.apply(Event::TerminalTitle {
            terminal_id: TerminalId(1),
            title: None,
        }));
        assert_eq!(
            effects.notices,
            vec![
                Notice::SessionExited {
                    session: id,
                    code: Some(3),
                    signal: None
                },
                Notice::Bell {
                    terminal: TerminalId(1)
                },
            ]
        );
    }

    #[test]
    fn notification_intents_and_ui_open_become_notices() {
        use hutan_protocol::notify::{IntentAction, IntentKind};
        let mut state = StoreState::initial();
        let session = SessionId::new();
        let workspace = WorkspaceId::new();
        let intent = NotificationIntent {
            id: "n1".into(),
            kind: IntentKind::AgentFinished,
            session_id: session,
            workspace_id: Some(workspace),
            title: "t".into(),
            body: "b".into(),
            actions: vec![IntentAction {
                id: "open".into(),
                label: "Open".into(),
            }],
            created_at_ms: 1,
            expires_at_ms: None,
        };
        let mut effects = Effects::default();
        effects.extend(state.apply(Event::NotifyIntent {
            intent: intent.clone(),
        }));
        effects.extend(state.apply(Event::UiOpen {
            session_id: session,
            workspace_id: Some(workspace),
            at_ms: 7,
        }));
        assert_eq!(
            effects.notices,
            vec![
                Notice::NotifyIntent {
                    intent: Box::new(intent)
                },
                Notice::UiOpen {
                    session,
                    workspace: Some(workspace),
                    at_ms: 7
                },
            ]
        );
    }
}
