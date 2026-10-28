//! Coding agents in terminal sessions (protocol 0.55, capability `terminal.agent`,
//! `docs/integrations.md` § Agents that report in the terminal): which agent runs in a
//! terminal, what it does and what it calls its session.
//!
//! Three sources feed one [`TerminalAgent`], none of which needs an installed hook:
//! - **OSC 7501**, the status OMP writes about itself ([`from_status`]): the strongest;
//! - **the program in the terminal's foreground**, recognised by its executable or the script
//!   it runs ([`probe`]): the icon shows before the first report, and for harnesses that report
//!   nothing in the terminal (Claude Code, Codex);
//! - **the terminal title** ([`with_title`]): the agent's own name for its session and, for a
//!   program that reports nothing, the braille spinner a working harness puts in front of it.
//!
//! Everything here is pure (the process table comes in through [`ProcessSource`]), so the rules
//! are tested without a clock or a process.

use std::path::Path;

use hutan_protocol::messages::{TerminalAgent, TerminalAgentSource, TerminalAgentState};
use hutan_terminal::{AgentPhase, AgentStatus};

use crate::resources::ProcessSource;
use crate::session_names::is_path_title;

/// The source name of the tree node OSC 7501 reports are folded into.
pub const OSC_SOURCE: &str = "osc7501";
/// The app of a report that names none and follows no earlier one.
const UNNAMED_APP: &str = "agent";
/// Longest agent title kept, in characters.
const MAX_TITLE_CHARS: usize = 120;
/// Most processes below a terminal's shell that a probe looks at.
const MAX_PROBED: usize = 64;
/// Wrappers that run a program given as an argument: `env`, runtimes and package runners.
const LAUNCHERS: &[&str] = &[
    "env", "sudo", "command", "exec", "time", "nohup", "node", "bun", "deno", "npx", "bunx",
    "pnpm", "yarn", "python", "python3", "uv", "uvx", "pipx", "sh", "bash", "zsh",
];
/// The harnesses recognised by program name: executable (extension removed) → app.
const PROGRAMS: &[(&str, &str)] = &[
    ("omp", "omp"),
    ("claude", "claude"),
    ("claude-code", "claude"),
    ("codex", "codex"),
    ("gemini", "gemini"),
    ("opencode", "opencode"),
    ("aider", "aider"),
];
/// Package directories whose script is a harness (`node …/@anthropic-ai/claude-code/cli.js`).
const PACKAGES: &[(&str, &str)] = &[
    ("claude-code", "claude"),
    ("codex", "codex"),
    ("oh-my-pi", "omp"),
    ("gemini-cli", "gemini"),
    ("opencode-ai", "opencode"),
];

fn app_named(name: &str) -> Option<&'static str> {
    let stem = name
        .strip_suffix(".js")
        .or_else(|| name.strip_suffix(".mjs"))
        .unwrap_or(name);
    PROGRAMS
        .iter()
        .find(|(program, _)| program.eq_ignore_ascii_case(stem))
        .map(|(_, app)| *app)
}

fn file_name(arg: &str) -> &str {
    Path::new(arg)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(arg)
}

/// The harness a command line runs, when it is a known one: its own executable, or the first
/// argument that is not a flag of a wrapper (`env FOO=1 omp`, `npx claude`, `node …/claude-code/cli.js`).
pub fn app_of(args: &[String]) -> Option<&'static str> {
    let first = file_name(args.first()?);
    if let Some(app) = app_named(first) {
        return Some(app);
    }
    if !LAUNCHERS
        .iter()
        .any(|launcher| launcher.eq_ignore_ascii_case(first))
    {
        return None;
    }
    args.iter()
        .skip(1)
        .filter(|arg| !arg.starts_with('-') && !arg.contains('='))
        .take(3)
        .find_map(|arg| {
            app_named(file_name(arg)).or_else(|| {
                Path::new(arg).components().find_map(|part| {
                    let part = part.as_os_str().to_str()?;
                    PACKAGES
                        .iter()
                        .find(|(package, _)| *package == part)
                        .map(|(_, app)| *app)
                })
            })
        })
}

/// What a look at a terminal's process tree found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    /// The harness at or below the terminal's process, if any: the first process of the tree,
    /// parents before children, that [`app_of`] knows.
    pub app: Option<&'static str>,
    /// Something runs below the terminal's process (a shell with a job in the foreground).
    pub busy: bool,
}

/// Looks at the processes at and below `root`, a terminal's process.
pub fn probe(source: &dyn ProcessSource, root: u32) -> Probe {
    let below = source.descendants(root, MAX_PROBED);
    let busy = !below.is_empty();
    let mut pids = vec![root];
    pids.extend(below);
    let app = pids.into_iter().find_map(|pid| {
        source
            .arguments(pid)
            .as_deref()
            .and_then(app_of)
            .or_else(|| {
                source
                    .inspect(pid)
                    .and_then(|snapshot| app_named(&snapshot.name))
            })
    });
    Probe { app, busy }
}

fn phase_state(phase: AgentPhase) -> TerminalAgentState {
    match phase {
        AgentPhase::Working => TerminalAgentState::Working,
        AgentPhase::Blocked => TerminalAgentState::Blocked,
        AgentPhase::Done => TerminalAgentState::Done,
        AgentPhase::Error => TerminalAgentState::Error,
        AgentPhase::Idle => TerminalAgentState::Idle,
    }
}

/// The agent after an OSC 7501 report. `message` is the report's text after the daemon redacted
/// and bounded it. The time of the last change of state stays while the state does.
pub fn from_status(
    prev: Option<&TerminalAgent>,
    status: &AgentStatus,
    message: Option<String>,
    now_ms: u64,
) -> TerminalAgent {
    let app = status
        .app
        .clone()
        .or_else(|| prev.map(|agent| agent.app.clone()))
        .unwrap_or_else(|| UNNAMED_APP.to_owned());
    let state = phase_state(status.phase);
    let unchanged = prev.is_some_and(|agent| agent.state == state && agent.app == app);
    TerminalAgent {
        app,
        state,
        // What a report does not carry is not what an earlier one carried.
        kind: status
            .kind
            .clone()
            .filter(|_| state == TerminalAgentState::Blocked),
        message: message.filter(|_| {
            matches!(
                state,
                TerminalAgentState::Blocked | TerminalAgentState::Error
            )
        }),
        since_ms: match prev {
            Some(agent) if unchanged => agent.since_ms,
            _ => now_ms,
        },
        title: prev.and_then(|agent| agent.title.clone()),
        source: TerminalAgentSource::Osc,
    }
}

/// The agent after the foreground program is known to be `app`: `None` when an agent that
/// reports itself (or the same program) is already known.
pub fn from_process(prev: Option<&TerminalAgent>, app: &str, now_ms: u64) -> Option<TerminalAgent> {
    match prev {
        Some(agent) if agent.source == TerminalAgentSource::Osc || agent.app == app => None,
        _ => Some(TerminalAgent {
            app: app.to_owned(),
            state: TerminalAgentState::Unknown,
            kind: None,
            message: None,
            since_ms: now_ms,
            title: None,
            source: TerminalAgentSource::Process,
        }),
    }
}

/// Whether the title starts with a braille spinner frame: the sign of a working harness.
pub fn spinner_title(raw: &str) -> bool {
    raw.trim_start()
        .chars()
        .next()
        .is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c))
}

/// Marks and spinner frames a harness puts in front of its session's name: `π`, braille
/// patterns, Claude Code's asterisks, bullets and the separators after them.
fn is_title_mark(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            'π' | '>' | '-' | '–' | '—' | '|' | '·' | '•' | '●' | '○' | '*'
        )
        || matches!(
            c,
            '✳' | '✻' | '✶' | '✢' | '✽' | '✷' | '✺' | '◐' | '◓' | '◑' | '◒'
        )
        || ('\u{2800}'..='\u{28ff}').contains(&c)
}

/// What the agent calls its session, from the terminal title it set: without the program's mark
/// and spinner (`π ⠋ Fix the build` → `Fix the build`). A title that only says the program
/// (`Claude Code`) or a directory (`~/proj`) names nothing: `None`.
pub fn clean_title(raw: &str, app: &str) -> Option<String> {
    let raw = raw.trim();
    // OMP: `π`, one glyph for its state (`>` idle, a braille frame while working, `!` waiting or
    // failed, whatever a later version draws), then the label: nothing of the label is a mark.
    let title = match raw.strip_prefix('π').map(str::trim_start) {
        Some(rest) => {
            let mut chars = rest.chars();
            match chars.next() {
                Some(glyph)
                    if !glyph.is_alphanumeric()
                        && chars.as_str().starts_with(char::is_whitespace) =>
                {
                    chars.as_str().trim()
                }
                Some(glyph) if !glyph.is_alphanumeric() && chars.as_str().is_empty() => "",
                _ => rest.trim_start_matches(is_title_mark).trim(),
            }
        }
        None => raw.trim_start_matches(is_title_mark).trim(),
    };
    if title.is_empty() || is_path_title(title) {
        return None;
    }
    let generic = [
        app,
        "claude code",
        "claude",
        "codex",
        "omp",
        "opencode",
        "gemini",
    ];
    if generic.iter().any(|name| title.eq_ignore_ascii_case(name)) {
        return None;
    }
    let mut chars = title.chars();
    let mut out: String = chars.by_ref().take(MAX_TITLE_CHARS).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    Some(out)
}

/// The agent after the terminal's title became `raw` (`None`: cleared). For an agent known only
/// by its program the spinner decides what it does: a spinner frame is a turn in progress, and
/// the first plain title after one is the turn's end.
pub fn with_title(prev: &TerminalAgent, raw: Option<&str>, now_ms: u64) -> TerminalAgent {
    let mut next = prev.clone();
    next.title = raw.and_then(|raw| clean_title(raw, &prev.app));
    if prev.source == TerminalAgentSource::Process
        && let Some(raw) = raw
    {
        let state = match (spinner_title(raw), prev.state) {
            (true, _) => Some(TerminalAgentState::Working),
            (false, TerminalAgentState::Working) => Some(TerminalAgentState::Idle),
            (false, _) => None,
        };
        if let Some(state) = state.filter(|state| *state != prev.state) {
            next.state = state;
            next.since_ms = now_ms;
        }
    }
    next
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use hutan_terminal::procs::{ProcState, ProcessSnapshot};

    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn known_programs_are_recognised_by_executable_and_launcher() {
        for (line, app) in [
            ("omp", Some("omp")),
            ("/opt/homebrew/bin/omp --model x", Some("omp")),
            ("claude", Some("claude")),
            ("codex resume", Some("codex")),
            ("env FOO=1 omp", Some("omp")),
            ("npx -y claude", Some("claude")),
            ("bunx opencode", Some("opencode")),
            (
                "node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js",
                Some("claude"),
            ),
            (
                "node /usr/lib/node_modules/@openai/codex/bin/codex.js",
                Some("codex"),
            ),
            ("zsh", None),
            ("vim notes.md", None),
            ("node server.js", None),
            ("python3 -m http.server", None),
            ("cat omp", None),
            ("", None),
        ] {
            assert_eq!(app_of(&args(line)), app, "{line:?}");
        }
    }

    #[derive(Debug, Default)]
    struct Table {
        processes: HashMap<u32, (u32, &'static str, Vec<String>)>,
    }

    impl Table {
        fn with(processes: &[(u32, u32, &'static str, &str)]) -> Self {
            Self {
                processes: processes
                    .iter()
                    .map(|(pid, ppid, name, line)| (*pid, (*ppid, *name, args(line))))
                    .collect(),
            }
        }
    }

    impl ProcessSource for Table {
        fn descendants(&self, root: u32, limit: usize) -> Vec<u32> {
            let mut found = vec![root];
            let mut next = 0;
            while next < found.len() && found.len() <= limit {
                let parent = found[next];
                next += 1;
                let mut kids: Vec<u32> = self
                    .processes
                    .iter()
                    .filter(|(_, (ppid, ..))| *ppid == parent)
                    .map(|(pid, _)| *pid)
                    .collect();
                kids.sort_unstable();
                found.extend(kids);
            }
            found.remove(0);
            found
        }

        fn inspect(&self, pid: u32) -> Option<ProcessSnapshot> {
            let (ppid, name, _) = self.processes.get(&pid)?;
            Some(ProcessSnapshot {
                pid,
                ppid: *ppid,
                name: (*name).to_owned(),
                state: ProcState::Sleeping,
                cpu_time_ns: 0,
                rss_bytes: 0,
                threads: 1,
                started_at_ms: 0,
            })
        }

        fn arguments(&self, pid: u32) -> Option<Vec<String>> {
            self.processes.get(&pid).map(|(_, _, args)| args.clone())
        }
    }

    #[test]
    fn a_probe_finds_the_harness_below_the_shell_or_the_shell_itself() {
        let table = Table::with(&[
            (10, 1, "zsh", "-zsh"),
            (11, 10, "omp", "omp --model x"),
            (12, 11, "bun", "bun helper"),
        ]);
        assert_eq!(probe(&table, 10).app, Some("omp"));
        // A session whose own command is the harness.
        assert_eq!(probe(&table, 11).app, Some("omp"));
        let plain = Table::with(&[(10, 1, "zsh", "-zsh"), (11, 10, "vim", "vim a")]);
        assert_eq!(
            probe(&plain, 10),
            Probe {
                app: None,
                busy: true
            }
        );
        // A shell at its prompt: nothing below it.
        let idle = Table::with(&[(10, 1, "zsh", "-zsh")]);
        assert_eq!(
            probe(&idle, 10),
            Probe {
                app: None,
                busy: false
            }
        );
        // Only the kernel's name is known (arguments unreadable): the name decides.
        let mut named = Table::with(&[(10, 1, "zsh", "-zsh"), (11, 10, "codex", "")]);
        named.processes.get_mut(&11).unwrap().2.clear();
        assert_eq!(probe(&named, 10).app, Some("codex"));
    }

    fn status(phase: AgentPhase, kind: Option<&str>) -> AgentStatus {
        AgentStatus {
            phase,
            app: Some("omp".into()),
            kind: kind.map(str::to_owned),
            message: None,
        }
    }

    #[test]
    fn a_report_sets_state_and_keeps_since_while_the_state_stays() {
        let first = from_status(None, &status(AgentPhase::Working, None), None, 100);
        assert_eq!(
            (
                first.app.as_str(),
                first.state,
                first.since_ms,
                first.source
            ),
            (
                "omp",
                TerminalAgentState::Working,
                100,
                TerminalAgentSource::Osc
            )
        );
        let again = from_status(Some(&first), &status(AgentPhase::Working, None), None, 900);
        assert_eq!(again.since_ms, 100);
        let blocked = from_status(
            Some(&again),
            &status(AgentPhase::Blocked, Some("permission")),
            Some("Run it?".into()),
            1_000,
        );
        assert_eq!(
            (
                blocked.state,
                blocked.since_ms,
                blocked.kind.as_deref(),
                blocked.message.as_deref()
            ),
            (
                TerminalAgentState::Blocked,
                1_000,
                Some("permission"),
                Some("Run it?")
            )
        );
        // Neither kind nor message outlive the state they belong to.
        let done = from_status(
            Some(&blocked),
            &status(AgentPhase::Done, Some("x")),
            Some("y".into()),
            2_000,
        );
        assert_eq!((done.kind, done.message), (None, None));
    }

    #[test]
    fn a_report_without_an_app_keeps_the_known_one() {
        let first = from_status(None, &status(AgentPhase::Idle, None), None, 1);
        let mut anonymous = status(AgentPhase::Working, None);
        anonymous.app = None;
        assert_eq!(from_status(Some(&first), &anonymous, None, 2).app, "omp");
        assert_eq!(from_status(None, &anonymous, None, 2).app, "agent");
    }

    #[test]
    fn the_program_never_overrides_what_the_agent_reports() {
        let reported = from_status(None, &status(AgentPhase::Idle, None), None, 1);
        assert_eq!(from_process(Some(&reported), "claude", 2), None);
        let known = from_process(None, "claude", 2).expect("a new agent");
        assert_eq!(
            (known.state, known.source, known.since_ms),
            (TerminalAgentState::Unknown, TerminalAgentSource::Process, 2)
        );
        assert_eq!(from_process(Some(&known), "claude", 3), None);
        assert_eq!(
            from_process(Some(&known), "codex", 3).map(|a| a.app),
            Some("codex".to_owned())
        );
    }

    #[test]
    fn omp_titles_lose_the_mark_and_the_spinner() {
        for (raw, want) in [
            ("π >", None),
            ("π > fix the build", Some("fix the build")),
            ("π ⠋ fix the build", Some("fix the build")),
            ("π > 👷 WORKER: worker", Some("👷 WORKER: worker")),
            ("π ! 👷 WORKER: worker", Some("👷 WORKER: worker")),
            ("π ⠴ - dashes stay", Some("- dashes stay")),
            ("π ! ", None),
            ("π", None),
            ("π build the thing", Some("build the thing")),
            ("✳ Claude Code", None),
            ("⠂ Refactor auth", Some("Refactor auth")),
            ("π > ~/proj", None),
            ("omp", None),
            ("", None),
        ] {
            assert_eq!(clean_title(raw, "omp").as_deref(), want, "{raw:?}");
        }
        let long = format!("π > {}", "x".repeat(300));
        assert_eq!(
            clean_title(&long, "omp").map(|t| t.chars().count()),
            Some(MAX_TITLE_CHARS + 1)
        );
    }

    #[test]
    fn a_title_names_the_session_and_the_spinner_drives_a_program_only_agent() {
        let reported = from_status(None, &status(AgentPhase::Working, None), None, 1);
        let titled = with_title(&reported, Some("π ⠋ fix"), 5);
        assert_eq!(titled.title.as_deref(), Some("fix"));
        // A reporting agent's state is its own: the spinner changes nothing.
        assert_eq!(
            (titled.state, titled.since_ms),
            (TerminalAgentState::Working, 1)
        );
        assert_eq!(with_title(&titled, None, 6).title, None);

        let known = from_process(None, "claude", 10).expect("agent");
        let working = with_title(&known, Some("⠂ Refactor auth"), 20);
        assert_eq!(
            (working.state, working.since_ms, working.title.as_deref()),
            (TerminalAgentState::Working, 20, Some("Refactor auth"))
        );
        let still = with_title(&working, Some("⠐ Refactor auth"), 30);
        assert_eq!(still.since_ms, 20);
        let idle = with_title(&still, Some("✳ Refactor auth"), 40);
        assert_eq!((idle.state, idle.since_ms), (TerminalAgentState::Idle, 40));
        // A plain title says nothing of an agent that never worked.
        assert_eq!(
            with_title(&known, Some("Claude Code"), 50).state,
            TerminalAgentState::Unknown
        );
    }
}
