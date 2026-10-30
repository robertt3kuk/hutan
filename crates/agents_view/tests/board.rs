//! The reducer as its second caller uses it (`docs/architecture.md`: the daemon links this crate,
//! with no window): only the public boundary, protocol types in, one record per agent out. The
//! unit tests of `presentation` hold the reducer's rules; these hold what a caller may rely on.

use std::collections::{BTreeMap, HashMap, HashSet};

use hutan_agents_view::attention::Unread;
use hutan_agents_view::models::ModelNames;
use hutan_agents_view::presentation::{
    AgentPresentation, Facts, Group, Link, ReasonKind, Role, Scope, State, build,
};
use hutan_core::agent::{PermissionKind, PermissionOption, PermissionOptionKind};
use hutan_core::{ApprovalId, SessionId, SessionKind, SessionState, WorkspaceId};
use hutan_protocol::chat::ApprovalInfo;
use hutan_protocol::messages::{
    AgentReport, AgentState, SessionInfo, WorkspaceInfo, WorkspaceKind,
};

fn workspace(name: &str, kind: WorkspaceKind) -> WorkspaceInfo {
    WorkspaceInfo {
        id: WorkspaceId::new(),
        name: name.into(),
        roots: Vec::new(),
        kind,
        pinned: false,
        position: 0,
        created_by: Default::default(),
        last_active_ms: 0,
        live_sessions: 0,
        total_sessions: 0,
        parent_id: None,
        project: None,
    }
}

fn session(workspace: &WorkspaceInfo, title: &str) -> SessionInfo {
    SessionInfo {
        forked_from: None,
        extra_dirs: Vec::new(),
        containment: None,
        terminal_agent: None,
        id: SessionId::new(),
        workspace_id: workspace.id,
        kind: SessionKind::Terminal,
        title: title.into(),
        state: SessionState::Running,
        command: Vec::new(),
        cwd: None,
        terminal_id: None,
        created_at_ms: 1,
        agent: None,
        restored: false,
        chat: None,
        placement: None,
        folder: None,
        recovery: None,
        muted: false,
    }
}

fn agent(workspace: &WorkspaceInfo, title: &str, state: AgentState, at: u64) -> SessionInfo {
    let mut info = session(workspace, title);
    info.agent = Some(AgentReport {
        source: "omp-extension".into(),
        agent: "omp".into(),
        state,
        message: None,
        agent_session_id: None,
        updated_at_ms: at,
        blocked_sub: None,
    });
    info
}

fn approval(session: SessionId, title: &str, at: u64) -> ApprovalInfo {
    ApprovalInfo {
        id: ApprovalId::new(),
        session_id: session,
        turn_id: None,
        kind: PermissionKind::Command,
        title: title.into(),
        detail: None,
        options: vec![PermissionOption {
            id: "accept".into(),
            label: "Allow once".into(),
            kind: PermissionOptionKind::AllowOnce,
            provider: None,
        }],
        requested_at_ms: at,
        target: None,
        suggestion: None,
    }
}

/// What a caller with no window passes: everything only a window knows is empty.
fn daemon_view(
    link: Link,
    workspaces: &[WorkspaceInfo],
    sessions: &[SessionInfo],
    approvals: &[ApprovalInfo],
) -> AgentPresentation {
    build(&Facts {
        link,
        workspaces,
        sessions,
        approvals,
        questions: &[],
        trees: &BTreeMap::new(),
        unread: &BTreeMap::new(),
        reviews: &HashSet::new(),
        changes_to_review: &HashSet::new(),
        snoozed: &HashSet::new(),
        answered_approvals: &HashMap::new(),
        answered_questions: &HashSet::new(),
        model_names: &ModelNames::default(),
        revision: 1,
    })
}

#[test]
fn a_caller_with_no_window_gets_the_groups_of_the_daemons_own_facts() {
    let project = workspace("hutan", WorkspaceKind::Project);
    let asking = agent(&project, "asks", AgentState::Working, 10);
    let working = agent(&project, "works", AgentState::Working, 20);
    let idle = agent(&project, "idle", AgentState::Idle, 30);
    // A plain shell is no agent, whatever it prints.
    let shell = session(&project, "zsh");
    let pending = approval(asking.id, "run cargo test", 40);
    let sessions = [shell, idle.clone(), working.clone(), asking.clone()];

    let board = daemon_view(
        Link::Online,
        std::slice::from_ref(&project),
        &sessions,
        std::slice::from_ref(&pending),
    );

    assert_eq!(board.records.len(), 3, "the shell is not an agent session");
    let group = |id| board.get(id).map(|r| r.group);
    assert_eq!(group(asking.id), Some(Group::Requests));
    assert_eq!(group(working.id), Some(Group::Working));
    assert_eq!(group(idle.id), Some(Group::Idle));
    // The canonical order: the group that blocks an agent first.
    assert_eq!(board.records[0].session, asking.id);
    let request = &board.records[0];
    assert_eq!(request.requests.len(), 1);
    assert_eq!(request.requests[0].title, "run cargo test");
    assert_eq!(
        request.headline().map(|r| r.kind),
        Some(ReasonKind::Approval)
    );
    assert_eq!(request.project, "hutan");
    assert_eq!(request.role, Role::Agent);
    let counts = board.counts(Scope::All);
    assert_eq!(
        (counts.sessions, counts.request_sessions, counts.requests),
        (3, 1, 1)
    );
    assert_eq!(board.requests(), 1);
}

#[test]
fn no_snapshot_yet_is_loading_with_no_records_and_an_offline_link_keeps_them() {
    let project = workspace("hutan", WorkspaceKind::Project);
    let sessions = [agent(&project, "a", AgentState::Working, 5)];
    let projects = std::slice::from_ref(&project);

    let loading = daemon_view(Link::Loading, projects, &sessions, &[]);
    assert_eq!(loading.link, Link::Loading);
    assert!(loading.records.is_empty(), "no false zero, no false rows");

    let offline = Link::Offline {
        last_confirmed_ms: Some(7),
    };
    let kept = daemon_view(offline, projects, &sessions, &[]);
    assert_eq!(kept.link, offline);
    assert_eq!(kept.records.len(), 1);
    assert_eq!(kept.revision, 1);
}

#[test]
fn what_only_a_window_knows_moves_a_session_between_groups() {
    let project = workspace("hutan", WorkspaceKind::Project);
    let mut ended = agent(&project, "ended", AgentState::Idle, 5);
    ended.state = SessionState::Exited {
        code: Some(0),
        signal: None,
    };
    let sessions = [ended.clone()];
    let projects = std::slice::from_ref(&project);

    // The daemon alone has no unread mark: a finished session is just finished.
    let alone = daemon_view(Link::Online, projects, &sessions, &[]);
    assert_eq!(alone.get(ended.id).map(|r| r.state), Some(State::Completed));
    assert_eq!(alone.get(ended.id).map(|r| r.group), Some(Group::Finished));

    // The window's unread mark makes it ready for review, with the same state.
    let unread = BTreeMap::from([(ended.id, Unread::Done)]);
    let window = build(&Facts {
        link: Link::Online,
        workspaces: projects,
        sessions: &sessions,
        approvals: &[],
        questions: &[],
        trees: &BTreeMap::new(),
        unread: &unread,
        reviews: &HashSet::new(),
        changes_to_review: &HashSet::new(),
        snoozed: &HashSet::new(),
        answered_approvals: &HashMap::new(),
        answered_questions: &HashSet::new(),
        model_names: &ModelNames::default(),
        revision: 2,
    });
    let record = window.get(ended.id);
    assert_eq!(record.map(|r| r.state), Some(State::Completed));
    assert_eq!(record.map(|r| r.group), Some(Group::Review));
}

#[test]
fn the_groups_keep_their_names_and_keys() {
    for group in Group::ALL {
        assert_eq!(Group::from_key(group.key()), Some(group));
        assert!(!group.label().is_empty());
    }
    assert_eq!(Group::from_key("nope"), None);
}
