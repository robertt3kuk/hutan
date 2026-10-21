//! `hutan ps | top | kill | agent interrupt`: the resource manager (protocol 0.18,
//! `docs/resources.md`). `ps` prints the process tree of every live session (or one) with CPU
//! and memory, marks the processes that agents reported, and shows `hutand` itself; with
//! `--watch` it subscribes and prints again at every sample, which is the only time the daemon
//! samples. `top` is `ps --watch` with the busiest first.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::IsTerminal as _;

use hutan_client::Client;
use hutan_core::SessionId;
use hutan_protocol::messages::{Event, SessionInfo};
use hutan_protocol::resources::{
    AgentInterrupted, InterruptVia, ProcessInfo, ProcessSignaled, ResourceSample, SessionResources,
    SignalName,
};
use tokio::sync::broadcast::error::RecvError;

use super::Ctx;
use super::sessions::resolve_target;
use crate::args::{AgentInterruptArgs, KillArgs, PsArgs, SignalArg, TopArgs};
use crate::error::CliError;

/// Clears the screen and homes the cursor between two prints of `--watch` on a terminal.
const CLEAR: &str = "\x1b[H\x1b[2J";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    Pid,
    Cpu,
}

impl Ctx {
    pub(super) async fn ps(&self, args: PsArgs) -> Result<(), CliError> {
        self.watch_or_once(args.target, args.watch, args.interval, Order::Pid)
            .await
    }

    pub(super) async fn top(&self, args: TopArgs) -> Result<(), CliError> {
        self.watch_or_once(args.target, true, args.interval, Order::Cpu)
            .await
    }

    async fn watch_or_once(
        &self,
        target: Option<String>,
        watch: bool,
        interval: u32,
        order: Order,
    ) -> Result<(), CliError> {
        let client = self.resources_client().await?;
        let sessions = self.session_list(&client, None).await?.sessions;
        let only = match &target {
            Some(target) => Some(resolve_target(&sessions, target)?.id),
            None => None,
        };
        if !watch {
            let sample = client
                .resources_list(only)
                .await
                .map_err(|e| self.fail(e))?;
            return self.print_sample(&sample, &sessions, only, order, false);
        }
        // Before the subscription, so no sample slips by.
        let mut events = client.events();
        let first = client
            .resources_subscribe(Some(interval))
            .await
            .map_err(|e| self.fail(e))?;
        let tty = std::io::stdout().is_terminal() && !self.json;
        self.print_sample(&first.sample, &sessions, only, order, tty)?;
        loop {
            match events.recv().await {
                Ok((_, Event::ResourcesUpdated { sample })) => {
                    // Titles and new sessions can change between samples.
                    let sessions = self.session_list(&client, None).await?.sessions;
                    self.print_sample(&sample, &sessions, only, order, tty)?;
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return Ok(()),
            }
        }
    }

    async fn resources_client(&self) -> Result<Client, CliError> {
        let client = self.connect().await?;
        if !client.supports_resources() {
            return Err(CliError::Failed(
                "this daemon has no resource manager (protocol 0.18); restart it with a newer hutand"
                    .to_owned(),
            ));
        }
        Ok(client)
    }

    fn print_sample(
        &self,
        sample: &ResourceSample,
        sessions: &[SessionInfo],
        only: Option<SessionId>,
        order: Order,
        clear: bool,
    ) -> Result<(), CliError> {
        let mut shown = sample.clone();
        if let Some(only) = only {
            shown.sessions.retain(|s| s.session_id == only);
        }
        if clear {
            print!("{CLEAR}");
        }
        self.print(&shown, || render(&shown, sessions, order))
    }

    pub(super) async fn kill(&self, args: KillArgs) -> Result<(), CliError> {
        let client = self.resources_client().await?;
        let session = self.resolve(&client, &args.target).await?;
        match args.pid {
            None => {
                let info = client
                    .session_kill(session.id)
                    .await
                    .map_err(|e| self.fail(e))?;
                self.print(&info, || format!("killed session {}", info.id))
            }
            Some(pid) => {
                let signal = args.signal.map_or(SignalName::Term, signal_name);
                let done: ProcessSignaled = client
                    .process_signal(session.id, pid, signal)
                    .await
                    .map_err(|e| self.fail(e))?;
                self.print(&done, || {
                    format!(
                        "sent {} to pid {} of session {}",
                        done.signal.as_str().to_uppercase(),
                        done.pid,
                        done.session_id
                    )
                })
            }
        }
    }

    pub(super) async fn agent_interrupt(&self, args: AgentInterruptArgs) -> Result<(), CliError> {
        let client = self.resources_client().await?;
        let session = self.resolve(&client, &args.target).await?;
        let done = client
            .agent_interrupt(session.id, args.node, args.force)
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&done, || interrupted_text(&done))
    }
}

fn signal_name(arg: SignalArg) -> SignalName {
    match arg {
        SignalArg::Term => SignalName::Term,
        SignalArg::Kill => SignalName::Kill,
        SignalArg::Int => SignalName::Int,
        SignalArg::Hup => SignalName::Hup,
        SignalArg::Stop => SignalName::Stop,
        SignalArg::Cont => SignalName::Cont,
    }
}

fn interrupted_text(done: &AgentInterrupted) -> String {
    let who = match &done.node_id {
        Some(node) => format!("agent {node} of session {}", done.session_id),
        None => format!("the main agent of session {}", done.session_id),
    };
    let pid = done.pid.unwrap_or(0);
    let how = match done.via {
        InterruptVia::Turn => "its turn was asked to stop".to_owned(),
        InterruptVia::Thread => "its thread's turn was asked to stop".to_owned(),
        InterruptVia::Keystroke => "^C was typed into its terminal".to_owned(),
        InterruptVia::Sigint => format!("SIGINT to pid {pid}"),
        InterruptVia::SigintTerm => format!("SIGINT, then SIGTERM, to pid {pid}"),
    };
    format!("interrupted {who}: {how}")
}

// Rendering -----------------------------------------------------------------------------------

fn cpu_text(cpu: Option<f64>) -> String {
    cpu.map_or_else(|| "-".to_owned(), |cpu| format!("{cpu:.1}"))
}

fn mem_text(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let value = bytes as f64;
    if value < KIB * KIB {
        format!("{:.0}K", value / KIB)
    } else if value < KIB * KIB * KIB {
        format!("{:.1}M", value / (KIB * KIB))
    } else {
        format!("{:.2}G", value / (KIB * KIB * KIB))
    }
}

/// The processes of one session as `(depth, process)`: parents before children, siblings by pid
/// or, for [`Order::Cpu`], busiest first.
fn tree_rows(processes: &[ProcessInfo], order: Order) -> Vec<(usize, &ProcessInfo)> {
    let pids: HashSet<u32> = processes.iter().map(|p| p.pid).collect();
    let mut children: BTreeMap<u32, Vec<&ProcessInfo>> = BTreeMap::new();
    let mut tops: Vec<&ProcessInfo> = Vec::new();
    for process in processes {
        if pids.contains(&process.ppid) && process.ppid != process.pid {
            children.entry(process.ppid).or_default().push(process);
        } else {
            tops.push(process);
        }
    }
    let sort = |list: &mut Vec<&ProcessInfo>| match order {
        Order::Pid => list.sort_by_key(|p| p.pid),
        Order::Cpu => list.sort_by(|a, b| {
            b.cpu_percent
                .unwrap_or(0.0)
                .total_cmp(&a.cpu_percent.unwrap_or(0.0))
                .then(a.pid.cmp(&b.pid))
        }),
    };
    sort(&mut tops);
    for list in children.values_mut() {
        sort(list);
    }
    let mut rows = Vec::with_capacity(processes.len());
    let mut seen = HashSet::new();
    let mut stack: Vec<(usize, &ProcessInfo)> = tops.into_iter().rev().map(|p| (0, p)).collect();
    while let Some((depth, process)) = stack.pop() {
        if !seen.insert(process.pid) {
            continue;
        }
        rows.push((depth, process));
        if let Some(kids) = children.get(&process.pid) {
            stack.extend(kids.iter().rev().map(|p| (depth + 1, *p)));
        }
    }
    rows
}

fn process_line(depth: usize, process: &ProcessInfo, agent: Option<&str>) -> String {
    let marker = agent.map_or_else(String::new, |label| format!("  ◆ agent {label}"));
    format!(
        "  {:>7} {:>6} {:>8} {:<8} {}{}{}",
        process.pid,
        cpu_text(process.cpu_percent),
        mem_text(process.rss_bytes),
        format!("{:?}", process.state).to_lowercase(),
        "  ".repeat(depth),
        process.command,
        marker
    )
}

fn session_block(
    session: &SessionResources,
    info: Option<&SessionInfo>,
    order: Order,
    out: &mut String,
) {
    let title = info.map_or("", |s| s.title.as_str());
    out.push_str(&format!(
        "{} {title}  ({} process(es), cpu {}%, {}){}\n",
        session.session_id,
        session.totals.processes,
        cpu_text(Some(session.totals.cpu_percent)),
        mem_text(session.totals.rss_bytes),
        if session.truncated {
            "  [list truncated]"
        } else {
            ""
        }
    ));
    out.push_str("      PID   CPU%      RSS STATE     COMMAND\n");
    let label_of: HashMap<&str, &str> = session
        .agents
        .iter()
        .map(|a| (a.node_id.as_str(), a.label.as_str()))
        .collect();
    for (depth, process) in tree_rows(&session.processes, order) {
        let agent = process
            .agent_node
            .as_deref()
            .map(|node| label_of.get(node).copied().unwrap_or(node));
        out.push_str(&process_line(depth, process, agent));
        out.push('\n');
    }
    let without_pid: Vec<String> = session
        .agents
        .iter()
        .filter(|a| a.pid.is_none())
        .map(|a| {
            format!(
                "{} ({}, {})",
                a.label,
                format!("{:?}", a.kind).to_lowercase(),
                format!("{:?}", a.state).to_lowercase()
            )
        })
        .collect();
    if !without_pid.is_empty() {
        out.push_str(&format!(
            "  agents without a process id: {}\n",
            without_pid.join(", ")
        ));
    }
}

/// The whole sample as text: a block per session, then `hutand`. Sessions busiest first for
/// [`Order::Cpu`], in creation order otherwise.
fn render(sample: &ResourceSample, sessions: &[SessionInfo], order: Order) -> String {
    let mut shown: Vec<&SessionResources> = sample.sessions.iter().collect();
    if order == Order::Cpu {
        shown.sort_by(|a, b| b.totals.cpu_percent.total_cmp(&a.totals.cpu_percent));
    }
    let mut out = String::new();
    if shown.is_empty() {
        out.push_str("no live session has a process\n\n");
    }
    for session in shown {
        let info = sessions.iter().find(|s| s.id == session.session_id);
        session_block(session, info, order, &mut out);
        out.push('\n');
    }
    if sample.truncated {
        out.push_str("(more processes than the daemon lists; some are left out)\n");
    }
    let daemon = &sample.daemon;
    out.push_str(&format!(
        "hutand  pid {}  cpu {}%  {}\n",
        daemon.pid,
        cpu_text(daemon.cpu_percent),
        mem_text(daemon.rss_bytes)
    ));
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use hutan_core::{NodeKind, NodeState};
    use hutan_protocol::resources::{AgentProcess, ProcessState, ResourceTotals};

    use super::*;

    fn process(pid: u32, ppid: u32, command: &str, cpu: Option<f64>) -> ProcessInfo {
        ProcessInfo {
            pid,
            ppid,
            name: command.to_owned(),
            command: command.to_owned(),
            state: ProcessState::Sleeping,
            cpu_percent: cpu,
            rss_bytes: 3 * 1024 * 1024,
            threads: 1,
            started_at_ms: None,
            agent_node: None,
        }
    }

    fn sample() -> ResourceSample {
        let mut worker = process(30, 10, "cargo test", Some(180.0));
        worker.agent_node = Some("root".into());
        let session = SessionResources {
            session_id: SessionId::new(),
            processes: vec![
                process(10, 1, "-zsh", Some(0.5)),
                process(20, 10, "sleep 60", None),
                worker,
                process(31, 30, "rustc", Some(90.0)),
            ],
            totals: ResourceTotals {
                processes: 4,
                cpu_percent: 270.5,
                rss_bytes: 12 * 1024 * 1024,
            },
            agents: vec![
                AgentProcess {
                    node_id: "root".into(),
                    parent: None,
                    kind: NodeKind::Main,
                    label: "omp".into(),
                    state: NodeState::Working,
                    pid: Some(30),
                },
                AgentProcess {
                    node_id: "sub".into(),
                    parent: Some("root".into()),
                    kind: NodeKind::Sub,
                    label: "Explore".into(),
                    state: NodeState::Idle,
                    pid: None,
                },
            ],
            truncated: false,
        };
        ResourceSample {
            at_ms: 1,
            sessions: vec![session],
            services: Vec::new(),
            daemon: process(912, 1, "hutand", Some(1.0)),
            truncated: false,
        }
    }

    #[test]
    fn the_tree_is_indented_marks_agents_and_lists_the_ones_without_a_pid() {
        let text = render(&sample(), &[], Order::Pid);
        let lines: Vec<&str> = text.lines().collect();
        let row = |needle: &str| lines.iter().find(|l| l.contains(needle)).copied().unwrap();
        assert!(row("-zsh").ends_with("sleeping -zsh"), "{}", row("-zsh"));
        assert!(row("sleep 60").ends_with("sleeping   sleep 60"));
        assert!(
            row("sleep 60").contains(" -  "),
            "no figure yet: {}",
            row("sleep 60")
        );
        assert!(
            row("rustc").ends_with("sleeping     rustc"),
            "{}",
            row("rustc")
        );
        assert!(row("cargo test").ends_with("◆ agent omp"));
        assert!(text.contains("agents without a process id: Explore (sub, idle)"));
        assert!(text.ends_with("hutand  pid 912  cpu 1.0%  3.0M"), "{text}");
    }

    #[test]
    fn top_puts_the_busiest_sibling_first() {
        let sample = sample();
        let text = render(&sample, &[], Order::Cpu);
        assert!(
            text.find("cargo test").unwrap() < text.find("sleep 60").unwrap(),
            "{text}"
        );
        // Pid order keeps `sleep` (pid 20) before `cargo` (pid 30).
        let text = render(&sample, &[], Order::Pid);
        assert!(text.find("sleep 60").unwrap() < text.find("cargo test").unwrap());
    }

    #[test]
    fn sizes_are_short() {
        assert_eq!(mem_text(512), "0K");
        assert_eq!(mem_text(3 * 1024 * 1024), "3.0M");
        assert_eq!(mem_text(5 * 1024 * 1024 * 1024), "5.00G");
        assert_eq!(cpu_text(None), "-");
        assert_eq!(cpu_text(Some(12.34)), "12.3");
    }
}
