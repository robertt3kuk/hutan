//! The names the daemon gives terminal sessions its user did not name (UX-SESSION-NAMES).
//!
//! One policy for every client: `hutan ls`, the sidebar, the tab strip, the jump palette and
//! Resources all show `SessionInfo.title`. A session the user renamed keeps that name. Any
//! other terminal is called after **where it is** — the folder it is in now,
//! the branch when sessions in the same folder are on different ones, the command it is running — and a title the program set is used only when it says more
//! than a path: fish's `~/h/u/proj` and zsh's `user@host:~/proj` name nothing the folder does
//! not. A terminal with a coding agent in it (AGENT-TERM-1) is named by the agent's own title —
//! the terminal title without the program's mark and spinner — and never by the raw title. Sessions of one workspace that would read the same are told apart by a number in the
//! order they were created (`proj`, `proj 2`).

use std::path::{Path, PathBuf};

/// A command line is cut to this many characters in a name.
const COMMAND_CHARS: usize = 32;
/// A title the program set is cut to this many characters.
const TITLE_CHARS: usize = 48;

/// What the policy needs to know about one session of a workspace.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Order of creation: ties on the time are broken by position in the slice.
    pub created_at_ms: u64,
    pub naming: Naming,
}

#[derive(Debug, Clone)]
pub enum Naming {
    /// A name nobody here may change: the user's own, or a session that is not a terminal.
    Fixed(String),
    Auto(Facts),
}

/// What the daemon knows about the place of an automatically named terminal.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// Where the shell is now (OSC 7), else where it started.
    pub folder: Option<PathBuf>,
    pub branch: Option<String>,
    /// The last title the program set (OSC 0/2).
    pub osc_title: Option<String>,
    /// A coding agent runs in the terminal (`SessionInfo.terminal_agent`): the title is its,
    /// with the program's mark and spinner in front of it, and only `agent_title` is used.
    pub has_agent: bool,
    /// What the agent calls its session (`terminal_agent.title`).
    pub agent_title: Option<String>,
    /// The program's name: what a terminal is called when nothing better is known.
    pub default_title: String,
    /// The command line running in the foreground (OSC 133), secrets removed.
    pub running_command: Option<String>,
}

/// What a name is made of, before sessions are told apart.
struct Parts {
    label: String,
    /// Set when the label is the program's own title: no branch or command follows it.
    own_title: bool,
    branch: Option<String>,
    command: Option<String>,
}

/// The names of the `entries`: `Some` for the automatically named ones, in the same order.
pub fn derive(entries: &[Entry], home: Option<&Path>) -> Vec<Option<String>> {
    let parts: Vec<Option<Parts>> = entries
        .iter()
        .map(|entry| match &entry.naming {
            Naming::Fixed(_) => None,
            Naming::Auto(facts) => Some(parts_of(facts, home)),
        })
        .collect();
    // A branch is named only where it tells two sessions of the same folder apart.
    let shows_branch = |label: &str| {
        let mut branches = parts
            .iter()
            .flatten()
            .filter(|p| !p.own_title && p.label == label)
            .map(|p| &p.branch);
        branches
            .next()
            .is_some_and(|first| branches.any(|other| other != first))
    };
    let identities: Vec<String> = entries
        .iter()
        .zip(&parts)
        .map(|(entry, parts)| match (&entry.naming, parts) {
            (Naming::Fixed(title), _) => title.clone(),
            (Naming::Auto(_), Some(p)) => identity(p, shows_branch(&p.label)),
            (Naming::Auto(_), None) => String::new(),
        })
        .collect();
    let rank = |i: usize| (entries[i].created_at_ms, i);
    (0..entries.len())
        .map(|i| {
            let p = parts[i].as_ref()?;
            let earlier = (0..entries.len())
                .filter(|&j| j != i && rank(j) < rank(i) && identities[j] == identities[i])
                .count();
            let mut name = identities[i].clone();
            if earlier > 0 {
                name.push_str(&format!(" {}", earlier + 1));
            }
            if let Some(command) = &p.command {
                name.push_str(" · ");
                name.push_str(command);
            }
            Some(name)
        })
        .collect()
}

fn identity(parts: &Parts, with_branch: bool) -> String {
    match (&parts.branch, with_branch) {
        (Some(branch), true) => format!("{} ({branch})", parts.label),
        _ => parts.label.clone(),
    }
}

fn parts_of(facts: &Facts, home: Option<&Path>) -> Parts {
    let title = if facts.has_agent {
        // The raw title carries the agent's mark and a spinner that changes every frame: only
        // the agent's own name counts, and without one the session is named by its place.
        facts.agent_title.as_deref()
    } else {
        facts.osc_title.as_deref()
    };
    if let Some(title) = title.map(str::trim)
        && !title.is_empty()
        && !is_path_title(title)
        && title != facts.default_title
    {
        return Parts {
            label: cut(title, TITLE_CHARS),
            own_title: true,
            branch: None,
            command: None,
        };
    }
    Parts {
        label: folder_label(facts, home).unwrap_or_else(|| facts.default_title.clone()),
        own_title: false,
        branch: facts.branch.clone(),
        command: facts
            .running_command
            .as_deref()
            .map(|command| cut(command.lines().next().unwrap_or("").trim(), COMMAND_CHARS))
            .filter(|command| !command.is_empty()),
    }
}

/// The name of the folder; `~` for the home folder, `/` for the root.
fn folder_label(facts: &Facts, home: Option<&Path>) -> Option<String> {
    let folder = facts.folder.as_deref()?;
    if home == Some(folder) {
        return Some("~".to_owned());
    }
    match folder.file_name() {
        Some(name) => Some(name.to_string_lossy().into_owned()),
        None => folder.has_root().then(|| "/".to_owned()),
    }
}

/// A title that only says a directory: `~`, `~/h/u/proj`, `/usr/local`, `user@host:~/proj`.
pub fn is_path_title(title: &str) -> bool {
    let title = title.trim();
    // `user@host:` in front of the path (zsh and bash defaults).
    let path = match title.split_once(':') {
        Some((who, rest)) if !who.contains(char::is_whitespace) && who.contains('@') => rest,
        _ => title,
    };
    !path.contains(char::is_whitespace) && (path.starts_with('~') || path.starts_with('/'))
}

fn cut(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto(folder: &str, branch: Option<&str>) -> Facts {
        Facts {
            folder: Some(PathBuf::from(folder)),
            branch: branch.map(str::to_owned),
            default_title: "fish".to_owned(),
            ..Facts::default()
        }
    }

    fn entry(created_at_ms: u64, facts: Facts) -> Entry {
        Entry {
            created_at_ms,
            naming: Naming::Auto(facts),
        }
    }

    fn names(entries: &[Entry]) -> Vec<String> {
        derive(entries, Some(Path::new("/Users/me")))
            .into_iter()
            .map(Option::unwrap_or_default)
            .collect()
    }

    #[test]
    fn path_titles_are_recognised() {
        for title in [
            "~",
            "~/h/u/proj-demo",
            "/usr/local/bin",
            "me@host:~/proj",
            "me@host:/tmp",
        ] {
            assert!(is_path_title(title), "{title}");
        }
        for title in ["vim main.rs", "fish", "cargo test", "build: ~/x y", "a@b.c"] {
            assert!(!is_path_title(title), "{title}");
        }
    }

    #[test]
    fn three_sessions_in_one_folder_read_differently() {
        let facts = auto("/Users/me/ux/proj-demo", Some("main"));
        let entries = [
            entry(1, facts.clone()),
            entry(2, facts.clone()),
            entry(3, facts),
        ];
        assert_eq!(names(&entries), ["proj-demo", "proj-demo 2", "proj-demo 3"]);
    }

    #[test]
    fn a_path_title_is_never_the_name() {
        let mut facts = auto("/Users/me/ux/proj-demo", None);
        facts.osc_title = Some("~/h/u/proj-demo".to_owned());
        assert_eq!(names(&[entry(1, facts.clone())]), ["proj-demo"]);
        facts.osc_title = Some("me@host:~/ux/proj-demo".to_owned());
        assert_eq!(names(&[entry(1, facts)]), ["proj-demo"]);
    }

    #[test]
    fn a_real_title_is_kept_and_the_program_name_is_not_one() {
        let mut facts = auto("/Users/me/proj", None);
        facts.osc_title = Some("vim main.rs".to_owned());
        facts.running_command = Some("vim main.rs".to_owned());
        assert_eq!(names(&[entry(1, facts.clone())]), ["vim main.rs"]);
        facts.osc_title = Some("fish".to_owned());
        assert_eq!(names(&[entry(1, facts)]), ["proj · vim main.rs"]);
    }

    #[test]
    fn an_agent_names_its_session_by_its_own_title_not_the_terminal_title() {
        let mut facts = auto("/Users/me/proj", None);
        facts.has_agent = true;
        facts.osc_title = Some("π ⠋ 👷 WORKER: worker".to_owned());
        facts.agent_title = Some("👷 WORKER: worker".to_owned());
        facts.running_command = Some("omp".to_owned());
        assert_eq!(names(&[entry(1, facts.clone())]), ["👷 WORKER: worker"]);
        // Without a title of its own the raw one is never the name: the place is.
        facts.agent_title = None;
        assert_eq!(names(&[entry(1, facts.clone())]), ["proj · omp"]);
        // Two agents with the same title are told apart like any namesakes.
        facts.agent_title = Some("Fix the build".to_owned());
        let entries = [entry(1, facts.clone()), entry(2, facts)];
        assert_eq!(names(&entries), ["Fix the build", "Fix the build 2"]);
    }

    #[test]
    fn the_running_command_follows_the_name_and_leaves_the_number_alone() {
        let facts = auto("/Users/me/proj", None);
        let mut running = facts.clone();
        running.running_command = Some("cargo test --workspace".to_owned());
        let entries = [entry(1, facts), entry(2, running)];
        assert_eq!(names(&entries), ["proj", "proj 2 · cargo test --workspace"]);
    }

    #[test]
    fn the_branch_shows_only_where_it_tells_sessions_apart() {
        let one = [entry(1, auto("/w/proj", Some("main")))];
        assert_eq!(names(&one), ["proj"]);
        let two = [
            entry(1, auto("/w/proj", Some("main"))),
            entry(2, auto("/w/proj", Some("feat"))),
            entry(3, auto("/w/other", Some("main"))),
        ];
        assert_eq!(names(&two), ["proj (main)", "proj (feat)", "other"]);
    }

    #[test]
    fn home_root_and_unknown_folders_have_names() {
        let mut unknown = auto("/x", None);
        unknown.folder = None;
        let entries = [
            entry(1, auto("/Users/me", None)),
            entry(2, auto("/", None)),
            entry(3, unknown),
        ];
        assert_eq!(names(&entries), ["~", "/", "fish"]);
    }

    #[test]
    fn a_named_session_keeps_its_name_and_counts_for_the_numbering() {
        let entries = [
            Entry {
                created_at_ms: 1,
                naming: Naming::Fixed("proj".to_owned()),
            },
            entry(2, auto("/w/proj", None)),
        ];
        let derived = derive(&entries, None);
        assert_eq!(derived[0], None);
        assert_eq!(derived[1].as_deref(), Some("proj 2"));
    }

    #[test]
    fn long_commands_are_cut() {
        let mut facts = auto("/w/proj", None);
        facts.running_command = Some("x".repeat(80));
        let name = names(&[entry(1, facts)]).remove(0);
        assert_eq!(name.chars().count(), "proj · ".chars().count() + 33);
        assert!(name.ends_with('…'));
    }
}
