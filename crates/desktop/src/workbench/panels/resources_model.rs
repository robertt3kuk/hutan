//! The view model of the resources panel (M13-D): a daemon sample turned into the lines the panel
//! draws (the `hutan` row, a row per session, its process tree, agents without a process), with
//! sorting, filtering and folding applied, and the actions each line offers. No GPUI here, so the
//! rules are unit tested.

use std::collections::{HashMap, HashSet};

use hutan_core::{NodeKind, NodeState, SessionId, SessionKind};
use hutan_protocol::messages::{Request, SessionRef};
use hutan_protocol::resources::{
    AgentInterrupt, AgentProcess, ProcessInfo, ProcessSignal, ProcessState, ResourceSample,
    ResourceTotals, SessionResources, SignalName,
};

use crate::sessions::PlacementBadge;
use crate::settings::ResourcesSort;

/// The harness whose subagents are threads the daemon can interrupt one by one (Codex); for the
/// others a subagent can only be stopped through its process.
const THREAD_HARNESS: &str = "codex";

/// At most this many process names are listed in a confirmation.
const LISTED: usize = 12;

/// Something the user folded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fold {
    /// A session's whole process tree.
    Session(SessionId),
    /// The descendants of a process.
    Process(SessionId, u32),
}

/// What the panel knows about a session besides its processes (from the sessions store).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionMeta {
    pub title: String,
    pub workspace: String,
    /// The session's working folder (last path component), if it has one.
    pub folder: String,
    pub kind: SessionKind,
    /// The chat agent of a harness session (`codex`, `omp`, …).
    pub chat_agent: Option<String>,
    pub placement: Option<PlacementBadge>,
}

/// Whether an agent node can be interrupted from here, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Interrupt {
    /// `agent.interrupt`; `force` when the node is a process that can be SIGTERMed after the
    /// SIGINT.
    Allowed { force: bool },
    /// Disabled; the text says why.
    Unsupported(String),
}

/// What M13-C lets the desktop do to an agent node (see `docs/resources.md` § Actions): the main
/// agent always (a chat's turn, or ^C in a terminal); a subagent that is a process of the session
/// (SIGINT, optionally SIGTERM after); a Codex subagent thread; nothing else.
pub fn interrupt_support(
    meta: Option<&SessionMeta>,
    kind: NodeKind,
    pid: Option<u32>,
) -> Interrupt {
    match kind {
        NodeKind::Main => Interrupt::Allowed { force: false },
        NodeKind::Sub if pid.is_some() => Interrupt::Allowed { force: true },
        NodeKind::Sub
            if meta.is_some_and(|m| {
                m.kind == SessionKind::Harness && m.chat_agent.as_deref() == Some(THREAD_HARNESS)
            }) =>
        {
            Interrupt::Allowed { force: false }
        }
        NodeKind::Sub => Interrupt::Unsupported(
            "this agent has no process id and is not a thread the harness can interrupt; \
             interrupt the main agent instead"
                .to_owned(),
        ),
    }
}

/// An agent node of a session.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentMark {
    pub node_id: String,
    /// The node's name, else its type, else its id.
    pub label: String,
    pub kind: NodeKind,
    pub state: NodeState,
    pub interrupt: Interrupt,
}

impl AgentMark {
    fn of(agent: &AgentProcess, meta: Option<&SessionMeta>) -> Self {
        Self {
            node_id: agent.node_id.clone(),
            label: agent.label.clone(),
            kind: agent.kind,
            state: agent.state,
            interrupt: interrupt_support(meta, agent.kind, agent.pid),
        }
    }
}

/// One process, or `hutand`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessLine {
    pub pid: u32,
    /// Indent level inside the session's tree; the tree's root is 0.
    pub depth: usize,
    pub name: String,
    pub command: String,
    pub state: ProcessState,
    pub cpu: Option<f64>,
    pub rss: u64,
    pub threads: u32,
    /// The agent node that reported this process.
    pub agent: Option<AgentMark>,
    /// Has descendants that fold into this line.
    pub foldable: bool,
    pub collapsed: bool,
    /// Descendants the fold hides, or all of them while not folded.
    pub descendants: usize,
}

/// One session.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionLine {
    pub session: SessionId,
    pub title: String,
    pub workspace: String,
    pub folder: String,
    pub placement: Option<PlacementBadge>,
    pub totals: ResourceTotals,
    pub collapsed: bool,
    /// Processes were left out (the daemon's limits).
    pub truncated: bool,
}

/// An agent node that is not a process of the session: a Codex subagent thread, or an agent that
/// reported no pid.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentLine {
    pub session: SessionId,
    pub mark: AgentMark,
}

/// One line of the table.
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    /// `hutand` itself.
    Daemon(ProcessLine),
    Session(SessionLine),
    Process {
        session: SessionId,
        line: ProcessLine,
    },
    Agent(AgentLine),
    /// A remark: the daemon left processes out.
    Note(String),
}

impl Line {
    /// The session the line belongs to.
    pub fn session(&self) -> Option<SessionId> {
        match self {
            Line::Session(line) => Some(line.session),
            Line::Process { session, .. } => Some(*session),
            Line::Agent(line) => Some(line.session),
            Line::Daemon(_) | Line::Note(_) => None,
        }
    }

    /// What an inline error is attached to.
    pub fn target(&self) -> Option<Target> {
        match self {
            Line::Session(line) => Some(Target::Session(line.session)),
            Line::Process { session, line } => Some(Target::Process(*session, line.pid)),
            Line::Agent(line) => Some(Target::Agent(line.session, line.mark.node_id.clone())),
            Line::Daemon(_) | Line::Note(_) => None,
        }
    }

    /// The text a screen reader (and a test) reads for the line: name, pid, command.
    pub fn label(&self) -> String {
        match self {
            Line::Daemon(line) | Line::Process { line, .. } => line.name.clone(),
            Line::Session(line) => line.title.clone(),
            Line::Agent(line) => line.mark.label.clone(),
            Line::Note(text) => text.clone(),
        }
    }
}

/// What an inline error or a busy marker belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    Session(SessionId),
    Process(SessionId, u32),
    Agent(SessionId, String),
}

/// What the user chooses to do; the panel sends it to the daemon (or the workbench asks first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `process.signal`.
    Signal {
        session: SessionId,
        pid: u32,
        signal: SignalName,
    },
    /// `session.stop`.
    StopSession(SessionId),
    /// `session.kill`.
    KillSession(SessionId),
    /// `agent.interrupt`; no node is the main agent.
    InterruptAgent {
        session: SessionId,
        node: Option<String>,
        force: bool,
    },
}

impl Command {
    pub fn target(&self) -> Target {
        match self {
            Command::Signal { session, pid, .. } => Target::Process(*session, *pid),
            Command::StopSession(session) | Command::KillSession(session) => {
                Target::Session(*session)
            }
            Command::InterruptAgent { session, node, .. } => {
                Target::Agent(*session, node.clone().unwrap_or_default())
            }
        }
    }

    /// Whether the workbench asks before sending it (a SIGKILL cannot be undone, and stopping
    /// a session ends it).
    pub fn needs_confirmation(&self) -> bool {
        matches!(
            self,
            Command::StopSession(_)
                | Command::KillSession(_)
                | Command::Signal {
                    signal: SignalName::Kill,
                    ..
                }
        )
    }

    /// The daemon request that carries the command out.
    pub fn request(&self) -> Request {
        match self {
            Command::Signal {
                session,
                pid,
                signal,
            } => Request::ProcessSignal(ProcessSignal {
                session_id: *session,
                pid: *pid,
                signal: *signal,
            }),
            Command::StopSession(session) => Request::SessionStop(SessionRef {
                session_id: *session,
            }),
            Command::KillSession(session) => Request::SessionKill(SessionRef {
                session_id: *session,
            }),
            Command::InterruptAgent {
                session,
                node,
                force,
            } => Request::AgentInterrupt(AgentInterrupt {
                session_id: *session,
                node_id: node.clone(),
                force: *force,
            }),
        }
    }
}

/// One entry of a context menu.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub label: String,
    pub command: Option<Command>,
    /// Greyed out; the text says why.
    pub disabled: Option<String>,
}

impl Item {
    fn run(label: &str, command: Command) -> Self {
        Self {
            label: label.to_owned(),
            command: Some(command),
            disabled: None,
        }
    }
}

/// The agent entries of an agent node.
fn agent_items(session: SessionId, mark: &AgentMark) -> Vec<Item> {
    let node = (mark.kind == NodeKind::Sub).then(|| mark.node_id.clone());
    match &mark.interrupt {
        Interrupt::Allowed { force } => {
            let mut items = vec![Item::run(
                "Interrupt Agent",
                Command::InterruptAgent {
                    session,
                    node: node.clone(),
                    force: false,
                },
            )];
            if *force {
                items.push(Item::run(
                    "Interrupt Agent, Then Terminate",
                    Command::InterruptAgent {
                        session,
                        node,
                        force: true,
                    },
                ));
            }
            items
        }
        Interrupt::Unsupported(reason) => vec![Item {
            label: "Interrupt Agent".to_owned(),
            command: None,
            disabled: Some(reason.clone()),
        }],
    }
}

/// The entries of a process row: the signals, then its agent's.
pub fn process_items(session: SessionId, line: &ProcessLine) -> Vec<Item> {
    let signal = |label: &str, signal| {
        Item::run(
            label,
            Command::Signal {
                session,
                pid: line.pid,
                signal,
            },
        )
    };
    let mut items = vec![
        signal("Interrupt (SIGINT)", SignalName::Int),
        signal("Terminate (SIGTERM)", SignalName::Term),
        signal("Kill (SIGKILL)…", SignalName::Kill),
        if line.state == ProcessState::Stopped {
            signal("Continue (SIGCONT)", SignalName::Cont)
        } else {
            signal("Stop (SIGSTOP)", SignalName::Stop)
        },
    ];
    if let Some(agent) = &line.agent {
        items.extend(agent_items(session, agent));
    }
    items
}

/// The entries of a session row.
pub fn session_items(line: &SessionLine) -> Vec<Item> {
    vec![
        Item::run("Stop Session…", Command::StopSession(line.session)),
        Item::run("Kill Session…", Command::KillSession(line.session)),
    ]
}

/// The entries of an agent row.
pub fn agent_line_items(line: &AgentLine) -> Vec<Item> {
    agent_items(line.session, &line.mark)
}

/// What the workbench shows before it sends a [`Command`] that [`needs_confirmation`].
///
/// [`needs_confirmation`]: Command::needs_confirmation
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    pub title: String,
    pub message: String,
    pub confirm: &'static str,
}

/// The question for `command`, naming what it ends, from the lines on screen. `None` for a
/// command that is sent without asking.
pub fn confirmation(command: &Command, lines: &[Line]) -> Option<Confirmation> {
    if !command.needs_confirmation() {
        return None;
    }
    let title_of = |session: &SessionId| {
        lines
            .iter()
            .find_map(|line| match line {
                Line::Session(s) if s.session == *session => Some(s.title.clone()),
                _ => None,
            })
            .unwrap_or_else(|| session.to_string())
    };
    match command {
        Command::StopSession(session) => Some(Confirmation {
            title: "Stop session?".to_owned(),
            message: format!(
                "“{}” is running. Stopping sends SIGHUP to its processes, and SIGKILL after 3 s                  if they stay.",
                title_of(session)
            ),
            confirm: "Stop",
        }),
        Command::KillSession(session) => {
            let title = title_of(session);
            let names: Vec<String> = lines
                .iter()
                .filter_map(|line| match line {
                    Line::Process { session: s, line } if s == session => {
                        Some(format!("{} ({})", line.name, line.pid))
                    }
                    _ => None,
                })
                .collect();
            let listed = names
                .iter()
                .take(LISTED)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let more = names.len().saturating_sub(LISTED);
            let what = match names.len() {
                0 => "its processes".to_owned(),
                1 => format!("1 process: {listed}"),
                n => format!(
                    "{n} processes: {listed}{}",
                    if more > 0 {
                        format!(" and {more} more")
                    } else {
                        String::new()
                    }
                ),
            };
            Some(Confirmation {
                title: "Kill session?".to_owned(),
                message: format!(
                    "“{title}” ends now. SIGKILL goes to {what}. They get no chance to save or \
                     clean up."
                ),
                confirm: "Kill Session",
            })
        }
        Command::Signal {
            session,
            pid,
            signal: SignalName::Kill,
        } => {
            let line = lines.iter().find_map(|line| match line {
                Line::Process { session: s, line } if s == session && line.pid == *pid => {
                    Some(line)
                }
                _ => None,
            });
            let name = line.map_or_else(|| "the process".to_owned(), |l| l.name.clone());
            let children = line.map_or(0, |l| l.descendants);
            let tail = match children {
                0 => String::new(),
                1 => " Its 1 child keeps running.".to_owned(),
                n => format!(" Its {n} descendants keep running."),
            };
            Some(Confirmation {
                title: "Kill process?".to_owned(),
                message: format!("{name} (pid {pid}) gets SIGKILL and cannot clean up.{tail}"),
                confirm: "Kill",
            })
        }
        _ => None,
    }
}

/// How the lines are chosen and ordered.
#[derive(Debug, Clone, Copy)]
pub struct Options<'a> {
    pub sort: ResourcesSort,
    /// The opposite order of `sort`.
    pub reversed: bool,
    /// Case-insensitive text matched against names, commands, pids, agent labels and the
    /// session's title, workspace and folder.
    pub filter: &'a str,
    pub folds: &'a HashSet<Fold>,
}

/// Turns a sample into the lines of the table. `meta` supplies what the sessions store knows.
pub fn lines(
    sample: &ResourceSample,
    meta: &dyn Fn(SessionId) -> Option<SessionMeta>,
    options: &Options<'_>,
) -> Vec<Line> {
    let filter = options.filter.trim().to_lowercase();
    let mut out = Vec::new();
    let daemon = process_line(&sample.daemon, 0, None, 0, false);
    if matches_process(&daemon, &filter) {
        out.push(Line::Daemon(daemon));
    }

    let mut order: Vec<&SessionResources> = sample.sessions.iter().collect();
    order.sort_by(|a, b| {
        let by = match options.sort {
            ResourcesSort::Cpu => b.totals.cpu_percent.total_cmp(&a.totals.cpu_percent),
            ResourcesSort::Memory => b.totals.rss_bytes.cmp(&a.totals.rss_bytes),
            ResourcesSort::Name => title_of(a, meta)
                .to_lowercase()
                .cmp(&title_of(b, meta).to_lowercase()),
        };
        if options.reversed { by.reverse() } else { by }
    });
    for session in order {
        session_lines(session, meta, options, &filter, &mut out);
    }
    if sample.truncated {
        out.push(Line::Note(
            "The daemon lists at most 4096 processes; some are left out.".to_owned(),
        ));
    }
    out
}

fn title_of(session: &SessionResources, meta: &dyn Fn(SessionId) -> Option<SessionMeta>) -> String {
    meta(session.session_id).map_or_else(|| unknown_title(session.session_id), |m| m.title)
}

/// What a session the store does not know (yet) is called: a few characters of its id, never the
/// whole thing (the tooltip of its row has that).
fn unknown_title(id: SessionId) -> String {
    let id = id.to_string();
    let tail: String = id
        .chars()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("Session {tail}")
}

fn contains(text: &str, filter: &str) -> bool {
    text.to_lowercase().contains(filter)
}

fn matches_process(line: &ProcessLine, filter: &str) -> bool {
    filter.is_empty()
        || contains(&line.name, filter)
        || contains(&line.command, filter)
        || line.pid.to_string().contains(filter)
        || line
            .agent
            .as_ref()
            .is_some_and(|agent| contains(&agent.label, filter))
}

fn process_line(
    info: &ProcessInfo,
    depth: usize,
    agent: Option<AgentMark>,
    descendants: usize,
    collapsed: bool,
) -> ProcessLine {
    ProcessLine {
        pid: info.pid,
        depth,
        name: info.name.clone(),
        command: info.command.clone(),
        state: info.state,
        cpu: info.cpu_percent,
        rss: info.rss_bytes,
        threads: info.threads,
        agent,
        foldable: descendants > 0,
        collapsed,
        descendants,
    }
}

fn session_lines(
    session: &SessionResources,
    meta: &dyn Fn(SessionId) -> Option<SessionMeta>,
    options: &Options<'_>,
    filter: &str,
    out: &mut Vec<Line>,
) {
    let id = session.session_id;
    let info = meta(id);
    let title = info
        .as_ref()
        .map_or_else(|| unknown_title(id), |m| m.title.clone());
    let workspace = info
        .as_ref()
        .map(|m| m.workspace.clone())
        .unwrap_or_default();
    let folder = info.as_ref().map(|m| m.folder.clone()).unwrap_or_default();
    let session_hit = !filter.is_empty()
        && [&title, &workspace, &folder]
            .into_iter()
            .any(|text| contains(text, filter));

    // The tree: children by parent pid; roots are the processes whose parent is not listed.
    let present: HashSet<u32> = session.processes.iter().map(|p| p.pid).collect();
    let mut children: HashMap<u32, Vec<&ProcessInfo>> = HashMap::new();
    let mut roots: Vec<&ProcessInfo> = Vec::new();
    for process in &session.processes {
        if process.ppid != process.pid && present.contains(&process.ppid) {
            children.entry(process.ppid).or_default().push(process);
        } else {
            roots.push(process);
        }
    }
    let agents: HashMap<u32, &AgentProcess> = session
        .agents
        .iter()
        .filter_map(|agent| agent.pid.map(|pid| (pid, agent)))
        .collect();
    let compare = |a: &&ProcessInfo, b: &&ProcessInfo| {
        let by = match options.sort {
            ResourcesSort::Cpu => b
                .cpu_percent
                .unwrap_or(-1.0)
                .total_cmp(&a.cpu_percent.unwrap_or(-1.0)),
            ResourcesSort::Memory => b.rss_bytes.cmp(&a.rss_bytes),
            ResourcesSort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        }
        .then(a.pid.cmp(&b.pid));
        if options.reversed { by.reverse() } else { by }
    };
    roots.sort_by(compare);
    for list in children.values_mut() {
        list.sort_by(compare);
    }

    // Which processes the filter keeps: the matches and everything above them.
    let parent_of: HashMap<u32, u32> = session
        .processes
        .iter()
        .filter(|p| present.contains(&p.ppid) && p.ppid != p.pid)
        .map(|p| (p.pid, p.ppid))
        .collect();
    let lined = |p: &ProcessInfo| {
        let agent = agents.get(&p.pid).map(|a| AgentMark::of(a, info.as_ref()));
        process_line(p, 0, agent, 0, false)
    };
    let keep: Option<HashSet<u32>> = if filter.is_empty() || session_hit {
        None
    } else {
        let mut keep = HashSet::new();
        for process in &session.processes {
            if matches_process(&lined(process), filter) {
                let mut at = process.pid;
                while keep.insert(at) {
                    match parent_of.get(&at) {
                        Some(parent) => at = *parent,
                        None => break,
                    }
                }
            }
        }
        Some(keep)
    };
    let agent_rows: Vec<AgentLine> = session
        .agents
        .iter()
        .filter(|agent| agent.pid.is_none_or(|pid| !present.contains(&pid)))
        .map(|agent| AgentLine {
            session: id,
            mark: AgentMark::of(agent, info.as_ref()),
        })
        .filter(|line| filter.is_empty() || session_hit || contains(&line.mark.label, filter))
        .collect();
    if keep.as_ref().is_some_and(HashSet::is_empty) && agent_rows.is_empty() {
        return;
    }

    let collapsed = filter.is_empty() && options.folds.contains(&Fold::Session(id));
    out.push(Line::Session(SessionLine {
        session: id,
        title,
        workspace,
        folder,
        placement: info.as_ref().and_then(|m| m.placement.clone()),
        totals: session.totals,
        collapsed,
        truncated: session.truncated,
    }));
    if collapsed {
        return;
    }

    // Depth-first, an explicit stack so a deep tree cannot overflow the call stack.
    let count = |pid: u32| -> usize {
        let mut total = 0;
        let mut stack = vec![pid];
        while let Some(at) = stack.pop() {
            for child in children.get(&at).into_iter().flatten() {
                total += 1;
                stack.push(child.pid);
            }
        }
        total
    };
    let mut stack: Vec<(&ProcessInfo, usize)> = roots.iter().rev().map(|p| (*p, 0)).collect();
    while let Some((process, depth)) = stack.pop() {
        if keep
            .as_ref()
            .is_some_and(|keep| !keep.contains(&process.pid))
        {
            continue;
        }
        let descendants = count(process.pid);
        let folded = filter.is_empty() && options.folds.contains(&Fold::Process(id, process.pid));
        let agent = agents
            .get(&process.pid)
            .map(|a| AgentMark::of(a, info.as_ref()));
        out.push(Line::Process {
            session: id,
            line: process_line(
                process,
                depth,
                agent,
                descendants,
                folded && descendants > 0,
            ),
        });
        if !folded {
            for child in children.get(&process.pid).into_iter().flatten().rev() {
                stack.push((child, depth + 1));
            }
        }
    }
    out.extend(agent_rows.into_iter().map(Line::Agent));
    if session.truncated {
        out.push(Line::Note(
            "Some of this session's processes are left out (limit 512).".to_owned(),
        ));
    }
}

/// `—` without a figure, else one decimal below 100 % and none above.
pub fn format_cpu(cpu: Option<f64>) -> String {
    match cpu {
        None => "—".to_owned(),
        // A sum of deltas can come out as -0.0, which would read "-0.0%".
        Some(cpu) if cpu <= 0.0 => "0.0%".to_owned(),
        Some(cpu) if cpu >= 100.0 => format!("{cpu:.0}%"),
        Some(cpu) => format!("{cpu:.1}%"),
    }
}

/// Resident memory in MB; GB from 10 000 MB on.
pub fn format_memory(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = bytes as f64 / MB;
    if mb >= 10_000.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else if mb >= 1.0 || bytes == 0 {
        format!("{mb:.0} MB")
    } else {
        "<1 MB".to_owned()
    }
}

/// The filled share of the CPU bar: 100 % is a full bar, more is clipped.
pub fn cpu_fraction(cpu: Option<f64>) -> f32 {
    (cpu.unwrap_or(0.0) / 100.0).clamp(0.0, 1.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, ppid: u32, name: &str, cpu: Option<f64>, rss_mb: u64) -> ProcessInfo {
        ProcessInfo {
            pid,
            ppid,
            name: name.to_owned(),
            command: format!("/bin/{name} --flag"),
            state: ProcessState::Sleeping,
            cpu_percent: cpu,
            rss_bytes: rss_mb * 1024 * 1024,
            threads: 1,
            started_at_ms: None,
            agent_node: None,
        }
    }

    fn session(id: SessionId, processes: Vec<ProcessInfo>) -> SessionResources {
        let totals = ResourceTotals {
            processes: u32::try_from(processes.len()).unwrap(),
            cpu_percent: processes.iter().filter_map(|p| p.cpu_percent).sum(),
            rss_bytes: processes.iter().map(|p| p.rss_bytes).sum(),
        };
        SessionResources {
            session_id: id,
            processes,
            totals,
            agents: Vec::new(),
            truncated: false,
        }
    }

    fn sample(sessions: Vec<SessionResources>) -> ResourceSample {
        ResourceSample {
            at_ms: 1,
            sessions,
            services: Vec::new(),
            daemon: process(1000, 1, "hutand", Some(0.5), 40),
            truncated: false,
        }
    }

    fn meta_of(titles: &[(SessionId, &str)]) -> impl Fn(SessionId) -> Option<SessionMeta> + use<> {
        let titles: HashMap<SessionId, String> = titles
            .iter()
            .map(|(id, t)| (*id, (*t).to_owned()))
            .collect();
        move |id| {
            titles.get(&id).map(|title| SessionMeta {
                title: title.clone(),
                workspace: "ws".to_owned(),
                folder: "proj".to_owned(),
                kind: SessionKind::Terminal,
                chat_agent: None,
                placement: None,
            })
        }
    }

    fn options<'a>(sort: ResourcesSort, filter: &'a str, folds: &'a HashSet<Fold>) -> Options<'a> {
        Options {
            sort,
            reversed: false,
            filter,
            folds,
        }
    }

    /// `(kind, label, depth)` of every line.
    fn shape(lines: &[Line]) -> Vec<(&'static str, String, usize)> {
        lines
            .iter()
            .map(|line| match line {
                Line::Daemon(l) => ("daemon", l.name.clone(), 0),
                Line::Session(l) => ("session", l.title.clone(), 0),
                Line::Process { line, .. } => ("process", line.name.clone(), line.depth),
                Line::Agent(l) => ("agent", l.mark.label.clone(), 0),
                Line::Note(t) => ("note", t.clone(), 0),
            })
            .collect()
    }

    #[test]
    fn the_tree_is_indented_by_depth_and_siblings_follow_the_sort() {
        let a = SessionId::new();
        let sample = sample(vec![session(
            a,
            vec![
                process(10, 1, "sh", Some(0.0), 2),
                process(11, 10, "make", Some(80.0), 30),
                process(12, 10, "sleep", Some(1.0), 1),
                process(13, 11, "cc", Some(95.0), 200),
            ],
        )]);
        let folds = HashSet::new();
        let meta = meta_of(&[(a, "build")]);
        let cpu = shape(&lines(
            &sample,
            &meta,
            &options(ResourcesSort::Cpu, "", &folds),
        ));
        assert_eq!(
            cpu,
            [
                ("daemon", "hutand".to_owned(), 0),
                ("session", "build".to_owned(), 0),
                ("process", "sh".to_owned(), 0),
                ("process", "make".to_owned(), 1),
                ("process", "cc".to_owned(), 2),
                ("process", "sleep".to_owned(), 1),
            ]
        );
        let name = shape(&lines(
            &sample,
            &meta,
            &options(ResourcesSort::Name, "", &folds),
        ));
        let names: Vec<&str> = name.iter().map(|(_, n, _)| n.as_str()).collect();
        assert_eq!(names, ["hutand", "build", "sh", "make", "cc", "sleep"]);
        let memory = shape(&lines(
            &sample,
            &meta,
            &options(ResourcesSort::Memory, "", &folds),
        ));
        let names: Vec<&str> = memory.iter().map(|(_, n, _)| n.as_str()).collect();
        assert_eq!(names, ["hutand", "build", "sh", "make", "cc", "sleep"]);
        let reversed = lines(
            &sample,
            &meta,
            &Options {
                reversed: true,
                ..options(ResourcesSort::Cpu, "", &folds)
            },
        );
        let names: Vec<String> = reversed.iter().map(Line::label).collect();
        assert_eq!(names, ["hutand", "build", "sh", "sleep", "make", "cc"]);
    }

    #[test]
    fn sessions_are_ordered_by_their_totals_or_their_title() {
        let (quiet, busy) = (SessionId::new(), SessionId::new());
        let sample = sample(vec![
            session(quiet, vec![process(10, 1, "a", Some(1.0), 500)]),
            session(busy, vec![process(20, 1, "b", Some(90.0), 5)]),
        ]);
        let folds = HashSet::new();
        let meta = meta_of(&[(quiet, "zeta"), (busy, "alpha")]);
        let titles = |sort| -> Vec<String> {
            lines(&sample, &meta, &options(sort, "", &folds))
                .iter()
                .filter_map(|l| match l {
                    Line::Session(s) => Some(s.title.clone()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(titles(ResourcesSort::Cpu), ["alpha", "zeta"]);
        assert_eq!(titles(ResourcesSort::Memory), ["zeta", "alpha"]);
        assert_eq!(titles(ResourcesSort::Name), ["alpha", "zeta"]);
    }

    #[test]
    fn a_filter_keeps_matches_with_their_ancestors_and_drops_other_sessions() {
        let (a, b) = (SessionId::new(), SessionId::new());
        let sample = sample(vec![
            session(
                a,
                vec![
                    process(10, 1, "sh", None, 1),
                    process(11, 10, "cargo", None, 1),
                    process(12, 11, "rustc", None, 1),
                    process(13, 10, "sleep", None, 1),
                ],
            ),
            session(b, vec![process(20, 1, "vim", None, 1)]),
        ]);
        let folds = HashSet::new();
        let meta = meta_of(&[(a, "build"), (b, "notes")]);
        let by = |filter: &str| {
            shape(&lines(
                &sample,
                &meta,
                &options(ResourcesSort::Name, filter, &folds),
            ))
            .into_iter()
            .map(|(_, n, _)| n)
            .collect::<Vec<_>>()
        };
        // A process name: the match, its ancestors, its session; `hutand` does not match.
        assert_eq!(by("rustc"), ["build", "sh", "cargo", "rustc"]);
        // A command line (the fake commands carry `--flag`), case-insensitive.
        assert_eq!(by("VIM --FLAG"), ["notes", "vim"]);
        // A session title keeps the whole tree of that session.
        assert_eq!(by("notes"), ["notes", "vim"]);
        assert_eq!(by("build").len(), 5);
        // A pid.
        assert_eq!(by("13"), ["build", "sh", "sleep"]);
        // The daemon row filters too.
        assert_eq!(by("hutand"), ["hutand"]);
        assert!(by("nothing like this").is_empty());
    }

    #[test]
    fn folds_hide_descendants_and_a_filter_overrides_them() {
        let a = SessionId::new();
        let sample = sample(vec![session(
            a,
            vec![
                process(10, 1, "sh", None, 1),
                process(11, 10, "make", None, 1),
                process(12, 11, "cc", None, 1),
            ],
        )]);
        let meta = meta_of(&[(a, "build")]);
        let mut folds = HashSet::new();
        folds.insert(Fold::Process(a, 11));
        let l = lines(&sample, &meta, &options(ResourcesSort::Name, "", &folds));
        assert_eq!(
            l.iter().map(Line::label).collect::<Vec<_>>(),
            ["hutand", "build", "sh", "make"]
        );
        let Line::Process { line, .. } = &l[3] else {
            panic!("{l:?}")
        };
        assert!(line.foldable && line.collapsed);
        assert_eq!(line.descendants, 1);
        // The match `cc` is inside the fold: it shows anyway.
        let l = lines(&sample, &meta, &options(ResourcesSort::Name, "cc", &folds));
        assert_eq!(
            l.iter().map(Line::label).collect::<Vec<_>>(),
            ["build", "sh", "make", "cc"]
        );
        // A folded session shows its row only.
        folds.insert(Fold::Session(a));
        let l = lines(&sample, &meta, &options(ResourcesSort::Name, "", &folds));
        assert_eq!(
            l.iter().map(Line::label).collect::<Vec<_>>(),
            ["hutand", "build"]
        );
    }

    #[test]
    fn agents_mark_their_process_or_stand_alone_with_the_reason_they_cannot_be_interrupted() {
        let a = SessionId::new();
        let mut resources = session(
            a,
            vec![
                process(10, 1, "sh", None, 1),
                process(11, 10, "worker", None, 1),
            ],
        );
        let agent = |id: &str, kind, pid| AgentProcess {
            node_id: id.to_owned(),
            parent: None,
            kind,
            label: id.to_owned(),
            state: NodeState::Working,
            pid,
        };
        resources.agents = vec![
            agent("root", NodeKind::Main, Some(10)),
            agent("worker", NodeKind::Sub, Some(11)),
            agent("thread", NodeKind::Sub, None),
        ];
        let sample = sample(vec![resources]);
        let meta = meta_of(&[(a, "work")]);
        let folds = HashSet::new();
        let l = lines(&sample, &meta, &options(ResourcesSort::Name, "", &folds));
        let marks: Vec<(String, Interrupt)> = l
            .iter()
            .filter_map(|line| match line {
                Line::Process { line, .. } => line
                    .agent
                    .as_ref()
                    .map(|m| (m.label.clone(), m.interrupt.clone())),
                Line::Agent(line) => Some((line.mark.label.clone(), line.mark.interrupt.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            marks[0],
            ("root".to_owned(), Interrupt::Allowed { force: false })
        );
        assert_eq!(
            marks[1],
            ("worker".to_owned(), Interrupt::Allowed { force: true })
        );
        assert_eq!(marks[2].0, "thread");
        assert!(matches!(marks[2].1, Interrupt::Unsupported(_)));
        // Filtering by an agent's label finds its process.
        let l = lines(
            &sample,
            &meta,
            &options(ResourcesSort::Name, "root", &folds),
        );
        assert!(
            l.iter()
                .any(|line| matches!(line, Line::Process { line, .. } if line.pid == 10))
        );

        // A Codex harness session can interrupt a subagent thread.
        let codex = SessionMeta {
            title: "chat".into(),
            workspace: String::new(),
            folder: String::new(),
            kind: SessionKind::Harness,
            chat_agent: Some("codex".into()),
            placement: None,
        };
        assert_eq!(
            interrupt_support(Some(&codex), NodeKind::Sub, None),
            Interrupt::Allowed { force: false }
        );
        let omp = SessionMeta {
            chat_agent: Some("omp".into()),
            ..codex
        };
        assert!(matches!(
            interrupt_support(Some(&omp), NodeKind::Sub, None),
            Interrupt::Unsupported(_)
        ));
    }

    #[test]
    fn menus_offer_the_signals_and_the_agent_actions() {
        let a = SessionId::new();
        let mut line = process_line(&process(11, 10, "worker", None, 1), 1, None, 0, false);
        let labels = |items: &[Item]| items.iter().map(|i| i.label.clone()).collect::<Vec<_>>();
        assert_eq!(
            labels(&process_items(a, &line)),
            [
                "Interrupt (SIGINT)",
                "Terminate (SIGTERM)",
                "Kill (SIGKILL)…",
                "Stop (SIGSTOP)"
            ]
        );
        line.state = ProcessState::Stopped;
        assert_eq!(labels(&process_items(a, &line))[3], "Continue (SIGCONT)");
        line.agent = Some(AgentMark {
            node_id: "w".into(),
            label: "w".into(),
            kind: NodeKind::Sub,
            state: NodeState::Working,
            interrupt: Interrupt::Allowed { force: true },
        });
        let items = process_items(a, &line);
        assert_eq!(items.len(), 6);
        assert_eq!(
            items[4].command,
            Some(Command::InterruptAgent {
                session: a,
                node: Some("w".into()),
                force: false
            })
        );
        assert!(items[2].command.as_ref().unwrap().needs_confirmation());
        assert!(!items[1].command.as_ref().unwrap().needs_confirmation());
        assert!(
            session_items(&SessionLine {
                session: a,
                title: String::new(),
                workspace: String::new(),
                folder: String::new(),
                placement: None,
                totals: ResourceTotals::default(),
                collapsed: false,
                truncated: false,
            })
            .iter()
            .all(|item| item.command.as_ref().unwrap().needs_confirmation())
        );
        // The main agent is addressed without a node.
        let mark = AgentMark {
            node_id: "root".into(),
            label: "root".into(),
            kind: NodeKind::Main,
            state: NodeState::Idle,
            interrupt: Interrupt::Allowed { force: false },
        };
        assert!(matches!(
            agent_items(a, &mark)[0].command,
            Some(Command::InterruptAgent { node: None, .. })
        ));
    }

    #[test]
    fn a_kill_confirmation_names_what_dies() {
        let a = SessionId::new();
        let sample = sample(vec![session(
            a,
            vec![
                process(10, 1, "sh", None, 1),
                process(11, 10, "sleep", None, 1),
                process(12, 10, "sleep", None, 1),
            ],
        )]);
        let folds = HashSet::new();
        let l = lines(
            &sample,
            &meta_of(&[(a, "work")]),
            &options(ResourcesSort::Name, "", &folds),
        );
        let ask = confirmation(&Command::KillSession(a), &l).unwrap();
        assert!(ask.message.contains("“work”"), "{}", ask.message);
        assert!(ask.message.contains("3 processes"), "{}", ask.message);
        assert!(ask.message.contains("sleep (11)"), "{}", ask.message);
        let kill = Command::Signal {
            session: a,
            pid: 10,
            signal: SignalName::Kill,
        };
        let ask = confirmation(&kill, &l).unwrap();
        assert!(ask.message.contains("sh (pid 10)"), "{}", ask.message);
        assert!(ask.message.contains("2 descendants"), "{}", ask.message);
        let term = Command::Signal {
            session: a,
            pid: 10,
            signal: SignalName::Term,
        };
        assert_eq!(confirmation(&term, &l), None);
        let stop = confirmation(&Command::StopSession(a), &l).unwrap();
        assert!(stop.message.contains("“work”"), "{}", stop.message);
        assert_eq!(stop.confirm, "Stop");
    }

    #[test]
    fn figures_are_formatted_for_the_columns() {
        assert_eq!(format_cpu(None), "—");
        assert_eq!(format_cpu(Some(0.04)), "0.0%");
        assert_eq!(
            format_cpu(Some(-0.0)),
            "0.0%",
            "a sum of deltas may be negative zero"
        );
        assert_eq!(format_cpu(Some(12.34)), "12.3%");
        assert_eq!(format_cpu(Some(250.0)), "250%");
        assert_eq!(format_memory(0), "0 MB");
        assert_eq!(format_memory(100), "<1 MB");
        assert_eq!(format_memory(300 * 1024 * 1024), "300 MB");
        assert_eq!(format_memory(20_480 * 1024 * 1024), "20.0 GB");
        assert!((cpu_fraction(Some(50.0)) - 0.5).abs() < f32::EPSILON);
        assert!((cpu_fraction(Some(400.0)) - 1.0).abs() < f32::EPSILON);
        assert!(cpu_fraction(None).abs() < f32::EPSILON);
    }
}
