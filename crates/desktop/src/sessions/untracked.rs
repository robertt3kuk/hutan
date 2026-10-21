//! Agents that run in a terminal without telling Hutan (`UX-ATTENTION-CONTROLS`): the daemon
//! counts a terminal as an agent only once an integration reported there, so a `claude` started
//! by hand is invisible to the attention queue. This finds such an agent in the process trees
//! the daemon lists (`resources.list`) so the terminal can say so, quietly, and offer to track
//! it (`agent.report`, no state: Hutan shows it as *State unavailable*, never as idle).
//!
//! GPUI-free and unit tested. A name match is evidence, not proof; the note says "looks like".

use std::collections::HashMap;
use std::path::Path;

use hutan_core::SessionId;
use hutan_protocol::resources::{ProcessInfo, ResourceSample};

/// The agent programs Hutan has integrations for, by executable name.
pub const KNOWN_AGENTS: [&str; 3] = ["claude", "codex", "omp"];

/// Programs that run an agent's script, so that `node …/claude` counts as `claude`.
const LAUNCHERS: [&str; 4] = ["node", "bun", "deno", "python3"];

/// An agent program found in a terminal session that has not reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Untracked {
    pub agent: &'static str,
    /// The process that looks like the agent.
    pub pid: u32,
}

/// The known agent `process` is, by its executable name or by the program its command line runs.
fn agent_of(process: &ProcessInfo) -> Option<&'static str> {
    let known = |word: &str| {
        let stem = Path::new(word)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(word);
        KNOWN_AGENTS
            .into_iter()
            .find(|agent| agent.eq_ignore_ascii_case(stem))
    };
    if let Some(agent) = known(&process.name) {
        return Some(agent);
    }
    let mut words = process.command.split_whitespace();
    let first = words.next()?;
    if let Some(agent) = known(first) {
        return Some(agent);
    }
    let launcher = Path::new(first).file_name().and_then(|n| n.to_str())?;
    if LAUNCHERS.contains(&launcher) {
        // `node /…/claude/cli.js` runs the agent; options before the script are skipped.
        return words
            .filter(|word| !word.starts_with('-'))
            .take(2)
            .find_map(known);
    }
    None
}

/// The first known agent among `processes`, in their order (parents before children).
pub fn find_agent(processes: &[ProcessInfo]) -> Option<Untracked> {
    processes.iter().find_map(|process| {
        agent_of(process).map(|agent| Untracked {
            agent,
            pid: process.pid,
        })
    })
}

/// The agents found in `sample`, by session.
pub fn scan(sample: &ResourceSample) -> HashMap<SessionId, Untracked> {
    sample
        .sessions
        .iter()
        .filter_map(|session| {
            find_agent(&session.processes).map(|found| (session.session_id, found))
        })
        .collect()
}

/// Whether a command line a shell is about to run starts a known agent (`claude --resume`,
/// `~/bin/codex`): the moment worth looking at the process tree.
pub fn is_agent_command(command: &str) -> bool {
    command.split_whitespace().next().is_some_and(|word| {
        Path::new(word)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| KNOWN_AGENTS.contains(&name))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_protocol::resources::ProcessState;

    fn process(pid: u32, name: &str, command: &str) -> ProcessInfo {
        ProcessInfo {
            pid,
            ppid: 1,
            name: name.to_owned(),
            command: command.to_owned(),
            state: ProcessState::Sleeping,
            cpu_percent: None,
            rss_bytes: 0,
            threads: 1,
            started_at_ms: None,
            agent_node: None,
        }
    }

    #[test]
    fn an_agent_is_found_by_its_executable_or_the_script_a_launcher_runs() {
        let found = |processes: &[ProcessInfo]| find_agent(processes).map(|u| (u.agent, u.pid));
        assert_eq!(
            found(&[process(10, "zsh", "-zsh"), process(11, "claude", "claude")]),
            Some(("claude", 11))
        );
        assert_eq!(
            found(&[process(
                12,
                "node",
                "node --no-warnings /opt/bin/codex.js --yolo"
            )]),
            Some(("codex", 12))
        );
        assert_eq!(
            found(&[process(13, "bun", "/usr/local/bin/bun run /x/omp")]),
            Some(("omp", 13))
        );
        assert_eq!(
            found(&[process(14, "sh", "/Users/x/bin/claude --resume")]),
            Some(("claude", 14)),
            "by the first word of the command line"
        );
    }

    #[test]
    fn a_shell_an_editor_and_lookalikes_are_not_agents() {
        for (name, command) in [
            ("zsh", "-zsh"),
            ("vim", "vim claude-notes.md"),
            ("node", "node server.js"),
            ("claudette", "claudette"),
            ("cargo", "cargo run -p codex-fixture"),
        ] {
            assert_eq!(find_agent(&[process(1, name, command)]), None, "{name}");
        }
    }

    #[test]
    fn a_command_line_starts_an_agent_by_its_first_word() {
        assert!(is_agent_command("claude --resume"));
        assert!(is_agent_command("~/bin/codex"));
        assert!(!is_agent_command("cargo test claude"));
        assert!(!is_agent_command(""));
    }
}
