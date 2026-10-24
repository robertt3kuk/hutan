//! Instruction files (AGENT-INSTRUCTIONS, `docs/agents.md` § Instruction files): which files
//! Codex, Claude Code and OMP read in a project and in the user's own folders, which agent
//! reads which, and the plan of the *Share one file with every agent* flow.
//!
//! Everything here is pure: the caller (`workbench::instructions`) asks the daemon what is in
//! the project (`fs.list`, `fs.find`, `fs.read`) and stats the user-level candidates, then hands
//! the facts to [`discover`]. The rules are the harnesses' own documentation, as of
//! 2026-10-25, quoted in `docs/agents.md`:
//!
//! - **Codex** (developers.openai.com/codex/guides/agents-md): `$CODEX_HOME` (`~/.codex`)
//!   `AGENTS.override.md`, else `AGENTS.md`, the first non-empty one; then, from the project
//!   root down to the folder it starts in, one file per folder: `AGENTS.override.md`, else
//!   `AGENTS.md`, else a name of `project_doc_fallback_filenames`. Codex does not follow `@`
//!   imports.
//! - **Claude Code** (code.claude.com/docs/en/memory): `~/.claude/CLAUDE.md`, the managed
//!   policy file, `./CLAUDE.md` or `./.claude/CLAUDE.md`, `./CLAUDE.local.md`; a subfolder's
//!   `CLAUDE.md` when the agent works in it; `AGENTS.md` (and `.claude/AGENTS.md`) only when no
//!   `CLAUDE.md` / `CLAUDE.local.md` exists on the path (the default of *Project instructions*,
//!   v2.1.277 and later); `@path` imports are expanded (four hops).
//! - **OMP** (`omp://context-files.md`): `~/.omp/agent/AGENTS.md` (and, when
//!   `enabledProviders` names them, `~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`; one user file
//!   survives, the native one first); in the project, per folder depth the highest-priority
//!   provider wins: `.omp/AGENTS.md` (100) over `.claude/CLAUDE.md` (80) over the standalone
//!   `AGENTS.md` and `CLAUDE.md` (10, both kept); `.omp/RULES.md` is sticky; `@` imports are
//!   expanded (five hops); deeper `AGENTS.md` files are listed for the agent to read.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The harnesses whose instruction files Hutan shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Harness {
    Codex,
    Claude,
    Omp,
}

impl Harness {
    pub const ALL: [Harness; 3] = [Harness::Codex, Harness::Claude, Harness::Omp];

    /// The agent id Hutan uses (`hutan new --agent`).
    pub fn id(self) -> &'static str {
        match self {
            Harness::Codex => "codex",
            Harness::Claude => "claude",
            Harness::Omp => "omp",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Harness::Codex => "Codex",
            Harness::Claude => "Claude Code",
            Harness::Omp => "OMP",
        }
    }
}

/// How a harness reads a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reads {
    /// It is part of the instructions the harness starts with.
    Always,
    /// Through an `@` import in another file the harness reads (`via`, project-relative).
    Import { via: String },
    /// Only when the agent works in the file's folder (Claude Code, OMP) or is started in it
    /// (Codex).
    OnDemand,
    /// The file is there, but another file at the same level wins (`by`).
    Shadowed { by: String },
}

impl Reads {
    /// Whether the file's text reaches the agent without extra conditions.
    pub fn is_read(&self) -> bool {
        matches!(self, Reads::Always | Reads::Import { .. })
    }
}

/// One harness and how it reads a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reader {
    pub harness: Harness,
    pub reads: Reads,
}

impl Reader {
    /// The sentence of the glyph's tooltip.
    pub fn describe(&self) -> String {
        let name = self.harness.name();
        match &self.reads {
            Reads::Always => format!("{name} reads this file"),
            Reads::Import { via } => format!("{name} reads this file through the @import in {via}"),
            Reads::OnDemand => match self.harness {
                Harness::Codex => {
                    format!("{name} reads this file when it is started in this folder")
                }
                _ => format!("{name} reads this file when the agent works in this folder"),
            },
            Reads::Shadowed { by } => format!("{name} reads {by} instead of this file"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Project,
    User,
}

/// One instruction file that exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionFile {
    pub scope: Scope,
    /// Absolute.
    pub path: PathBuf,
    /// What the row says: the path relative to the project (`AGENTS.md`,
    /// `services/api/AGENTS.md`), or the user-level path with `~` (`~/.claude/CLAUDE.md`).
    pub label: String,
    /// Bytes.
    pub size: u64,
    /// A symbolic link: Hutan never writes through it.
    pub symlink: bool,
    /// One entry per harness that reads the file or would, in [`Harness::ALL`] order; a
    /// harness that does not read it at all has none.
    pub readers: Vec<Reader>,
}

impl InstructionFile {
    pub fn reader(&self, harness: Harness) -> Option<&Reader> {
        self.readers.iter().find(|r| r.harness == harness)
    }

    /// The harnesses that read the file (not shadowed, not only on demand).
    pub fn read_by(&self) -> Vec<Harness> {
        self.readers
            .iter()
            .filter(|r| r.reads.is_read())
            .map(|r| r.harness)
            .collect()
    }
}

/// What the harnesses' own folders are: Codex's, Claude Code's, OMP's, and the home they hang
/// from. Tests hand over folders of their own, so that the user's real ones are never looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessHomes {
    pub home: PathBuf,
    /// `$CODEX_HOME`, else `~/.codex`.
    pub codex: PathBuf,
    /// `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
    pub claude: PathBuf,
    /// `$PI_CODING_AGENT_DIR`, else `~/.omp/agent`.
    pub omp: PathBuf,
    /// Where an organization's managed `CLAUDE.md` lives; `None` for a test.
    pub managed: Option<PathBuf>,
}

impl HarnessHomes {
    /// The folders as the harnesses resolve them for this process.
    pub fn from_env() -> Option<Self> {
        let home = PathBuf::from(std::env::var_os("HOME").filter(|h| !h.is_empty())?);
        let pick = |var: &str, default: PathBuf| {
            std::env::var_os(var)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or(default)
        };
        Some(Self {
            codex: pick("CODEX_HOME", home.join(".codex")),
            claude: pick("CLAUDE_CONFIG_DIR", home.join(".claude")),
            omp: pick("PI_CODING_AGENT_DIR", home.join(".omp").join("agent")),
            managed: Some(PathBuf::from(
                "/Library/Application Support/ClaudeCode/CLAUDE.md",
            )),
            home,
        })
    }

    /// The default folders below `home` (nothing managed): what a test of its own builds.
    pub fn under(home: &Path) -> Self {
        Self {
            home: home.to_owned(),
            codex: home.join(".codex"),
            claude: home.join(".claude"),
            omp: home.join(".omp").join("agent"),
            managed: None,
        }
    }
}

/// What the harnesses' own configuration changes about which files they read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HarnessConfig {
    /// Codex's `project_doc_fallback_filenames`: names it reads in a folder without an
    /// `AGENTS.md`.
    pub codex_fallbacks: Vec<String>,
    /// OMP's `enabledProviders`: the other harnesses' user files it reads too.
    pub omp_reads_claude_user: bool,
    pub omp_reads_codex_user: bool,
}

impl HarnessConfig {
    /// Reads the two settings, read-only and best effort: a file that is missing or not what
    /// it should be leaves the default (`docs/agents.md` § Instruction files).
    pub fn load(homes: &HarnessHomes) -> Self {
        let mut config = Self::default();
        if let Ok(text) = std::fs::read_to_string(homes.codex.join("config.toml")) {
            config.codex_fallbacks = codex_fallbacks(&text);
        }
        for name in ["config.yml", "config.yaml"] {
            if let Ok(text) = std::fs::read_to_string(homes.omp.join(name)) {
                let enabled = omp_enabled_providers(&text);
                let all = enabled.iter().any(|p| p == "*" || p == "all");
                config.omp_reads_claude_user = all || enabled.iter().any(|p| p == "claude");
                config.omp_reads_codex_user = all || enabled.iter().any(|p| p == "codex");
                break;
            }
        }
        config
    }
}

/// `project_doc_fallback_filenames` of a Codex `config.toml`: plain file names only.
pub fn codex_fallbacks(config: &str) -> Vec<String> {
    let Ok(table) = config.parse::<toml::Table>() else {
        return Vec::new();
    };
    table
        .get("project_doc_fallback_filenames")
        .and_then(|v| v.as_array())
        .map(|names| {
            names
                .iter()
                .filter_map(|n| n.as_str())
                .filter(|n| plain_name(n))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The ids under `enabledProviders` of an OMP `config.yml`: a block list (`- claude`) or a
/// flow list (`[claude, github]`). Anything else yields nothing.
pub fn omp_enabled_providers(config: &str) -> Vec<String> {
    let unquote = |s: &str| s.trim().trim_matches(|c| c == '"' || c == '\'').to_owned();
    let mut lines = config.lines();
    while let Some(line) = lines.next() {
        let Some(rest) = line.strip_prefix("enabledProviders:") else {
            continue;
        };
        let rest = rest.trim();
        if let Some(inner) = rest.strip_prefix('[') {
            return inner
                .trim_end_matches(']')
                .split(',')
                .map(unquote)
                .filter(|s| !s.is_empty())
                .collect();
        }
        if !rest.is_empty() && !rest.starts_with('#') {
            // A scalar (`enabledProviders: "*"`).
            return vec![unquote(rest)];
        }
        let mut found = Vec::new();
        for item in lines {
            let trimmed = item.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            match trimmed.strip_prefix("- ") {
                Some(value) if item.starts_with(' ') || item.starts_with('-') => {
                    found.push(unquote(value));
                }
                _ => break,
            }
        }
        return found;
    }
    Vec::new()
}

/// A file name without directories.
fn plain_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && !name.contains('\\') && name != "." && name != ".."
}

/// The names the project is searched for (`fs.find`): every basename an instruction file can
/// have, the harnesses' fixed ones and Codex's fallbacks.
pub fn project_names(config: &HarnessConfig) -> Vec<String> {
    let mut names: Vec<String> = [
        "AGENTS.md",
        "AGENTS.override.md",
        "CLAUDE.md",
        "CLAUDE.local.md",
        "RULES.md",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for name in &config.codex_fallbacks {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}

/// The user-level files to look at, in the order the panel lists them.
pub fn user_candidates(homes: &HarnessHomes) -> Vec<PathBuf> {
    let mut paths = vec![
        homes.codex.join("AGENTS.override.md"),
        homes.codex.join("AGENTS.md"),
        homes.claude.join("CLAUDE.md"),
    ];
    if let Some(managed) = &homes.managed {
        paths.push(managed.clone());
    }
    paths.push(homes.omp.join("AGENTS.md"));
    paths.push(homes.omp.join("RULES.md"));
    paths
}

/// A file of the project, as the daemon reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFile {
    /// Relative to the project root, `/`-separated.
    pub rel: String,
    pub size: u64,
    pub symlink: bool,
    /// The project-relative paths its `@` imports name ([`imports_of`] of its text). Only
    /// filled for the files that can import: Claude Code's and OMP's.
    pub imports: Vec<String>,
}

/// A user-level file that exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFile {
    pub path: PathBuf,
    pub size: u64,
}

/// Everything [`discover`] needs to know.
#[derive(Debug, Clone)]
pub struct Facts {
    pub root: PathBuf,
    pub project: Vec<ProjectFile>,
    pub user: Vec<UserFile>,
    pub homes: HarnessHomes,
    pub config: HarnessConfig,
}

/// What the Instructions panel lists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Discovery {
    pub project: Vec<InstructionFile>,
    pub user: Vec<InstructionFile>,
}

impl Discovery {
    pub fn file(&self, label: &str) -> Option<&InstructionFile> {
        self.project
            .iter()
            .chain(&self.user)
            .find(|f| f.label == label)
    }

    /// The files whose label a project path is (`AGENTS.md` at the root).
    pub fn has_project(&self, rel: &str) -> bool {
        self.project.iter().any(|f| f.label == rel)
    }

    /// What *Share one file with every agent…* would find: `AGENTS.md` is read, by `CLAUDE.md`'s
    /// import or directly, by the harnesses the user runs.
    pub fn shared(&self) -> bool {
        self.project
            .iter()
            .find(|f| f.label == "AGENTS.md")
            .is_some_and(|agents| {
                Harness::ALL
                    .iter()
                    .all(|h| agents.reader(*h).is_some_and(|r| r.reads.is_read()))
            })
    }
}

/// Folder and name of a project-relative path.
fn split(rel: &str) -> (&str, &str) {
    rel.rsplit_once('/').unwrap_or(("", rel))
}

/// A folder the harnesses walk: no component starts with a dot.
fn plain_folder(dir: &str) -> bool {
    dir.split('/').all(|c| !c.starts_with('.'))
}

fn non_empty(file: &ProjectFile) -> bool {
    file.size > 0
}

fn reader(harness: Harness, reads: Reads) -> Reader {
    Reader { harness, reads }
}

/// Lists the instruction files of a project and of the user with the agents that read them.
pub fn discover(facts: &Facts) -> Discovery {
    let mut project: Vec<InstructionFile> = Vec::new();
    let by_rel = |rel: &str| facts.project.iter().find(|f| f.rel == rel);
    let fallbacks = &facts.config.codex_fallbacks;

    // ---- the project folder itself ----
    let codex_root = ["AGENTS.override.md", "AGENTS.md"]
        .into_iter()
        .map(str::to_owned)
        .chain(fallbacks.iter().cloned())
        .collect::<Vec<_>>();
    let codex_winner = codex_choice(&codex_root, "", &facts.project);
    let claude_own = ["CLAUDE.md", ".claude/CLAUDE.md", "CLAUDE.local.md"]
        .into_iter()
        .any(|rel| by_rel(rel).is_some());
    let omp_winner = omp_root_winner(&facts.project);

    // Every file the daemon found, in the order the panel shows them: the project folder's own
    // files first, then the folders below.
    let mut found: Vec<&ProjectFile> = facts
        .project
        .iter()
        .filter(|f| classify(&f.rel, fallbacks).is_some())
        .collect();
    found.sort_by_key(|f| sort_key(&f.rel, fallbacks));

    for file in found {
        let (dir, name) = split(&file.rel);
        let kind = classify(&file.rel, fallbacks);
        let Some(kind) = kind else { continue };
        let mut readers = Vec::new();
        if dir.is_empty() || dir == ".claude" || dir == ".omp" {
            // Codex: the one file of the project folder.
            if dir.is_empty() && codex_root.iter().any(|n| n == name) {
                match &codex_winner {
                    Some(win) if win == name => readers.push(reader(Harness::Codex, Reads::Always)),
                    Some(win) => {
                        readers.push(reader(Harness::Codex, Reads::Shadowed { by: win.clone() }))
                    }
                    None => {}
                }
            }
            // Claude Code.
            match kind {
                Kind::Claude | Kind::DotClaude | Kind::ClaudeLocal => {
                    readers.push(reader(Harness::Claude, Reads::Always));
                }
                Kind::Agents | Kind::DotClaudeAgents if !claude_own => {
                    readers.push(reader(Harness::Claude, Reads::Always));
                }
                _ => {}
            }
            // OMP.
            match kind {
                Kind::OmpAgents | Kind::OmpRules => {
                    readers.push(reader(Harness::Omp, Reads::Always));
                }
                Kind::DotClaude | Kind::Agents | Kind::Claude => {
                    if let Some(win) = &omp_winner {
                        if tier(kind) == win.tier {
                            readers.push(reader(Harness::Omp, Reads::Always));
                        } else if win.tier > tier(kind) {
                            readers.push(reader(
                                Harness::Omp,
                                Reads::Shadowed {
                                    by: win.label.clone(),
                                },
                            ));
                        }
                    }
                }
                _ => {}
            }
        } else {
            // A folder below the project.
            match kind {
                Kind::Agents | Kind::AgentsOverride | Kind::CodexFallback => {
                    let names: Vec<String> = ["AGENTS.override.md", "AGENTS.md"]
                        .into_iter()
                        .map(str::to_owned)
                        .chain(fallbacks.iter().cloned())
                        .collect();
                    match codex_choice(&names, dir, &facts.project) {
                        Some(win) if win == name => {
                            readers.push(reader(Harness::Codex, Reads::OnDemand));
                        }
                        Some(win) => readers.push(reader(
                            Harness::Codex,
                            Reads::Shadowed {
                                by: format!("{dir}/{win}"),
                            },
                        )),
                        None => {}
                    }
                }
                _ => {}
            }
            match kind {
                Kind::Claude | Kind::ClaudeLocal => {
                    readers.push(reader(Harness::Claude, Reads::OnDemand));
                }
                Kind::Agents if !claude_own && !has_claude_files(&facts.project, dir) => {
                    readers.push(reader(Harness::Claude, Reads::OnDemand));
                }
                _ => {}
            }
            if kind == Kind::Agents {
                readers.push(reader(Harness::Omp, Reads::OnDemand));
            }
        }
        project.push(InstructionFile {
            scope: Scope::Project,
            path: facts.root.join(&file.rel),
            label: file.rel.clone(),
            size: file.size,
            symlink: file.symlink,
            readers,
        });
    }
    add_imports(&mut project, &facts.project);
    for file in &mut project {
        file.readers.sort_by_key(|r| r.harness);
    }

    // ---- the user's own files ----
    let user = user_files(facts);
    Discovery { project, user }
}

/// What the instructions the harness starts with, from imports: a file that a file read by
/// Claude Code or OMP imports (up to four hops, the smaller of the two harnesses' limits) is
/// read through it.
fn add_imports(files: &mut [InstructionFile], found: &[ProjectFile]) {
    for harness in [Harness::Claude, Harness::Omp] {
        // Breadth first from every file the harness reads on its own.
        let mut frontier: Vec<(String, String)> = files
            .iter()
            .filter(|f| {
                f.reader(harness)
                    .is_some_and(|r| matches!(r.reads, Reads::Always))
            })
            .map(|f| (f.label.clone(), f.label.clone()))
            .collect();
        let mut seen: BTreeSet<String> = frontier.iter().map(|(l, _)| l.clone()).collect();
        for _ in 0..4 {
            let mut next = Vec::new();
            for (label, via) in &frontier {
                let Some(source) = found.iter().find(|f| f.rel == *label) else {
                    continue;
                };
                for target in &source.imports {
                    if !seen.insert(target.clone()) {
                        continue;
                    }
                    if let Some(file) = files.iter_mut().find(|f| f.label == *target)
                        && file.reader(harness).is_none()
                    {
                        file.readers
                            .push(reader(harness, Reads::Import { via: via.clone() }));
                    }
                    next.push((target.clone(), via.clone()));
                }
            }
            frontier = next;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Agents,
    AgentsOverride,
    CodexFallback,
    Claude,
    ClaudeLocal,
    DotClaude,
    DotClaudeAgents,
    OmpAgents,
    OmpRules,
}

/// What a project-relative path is, or `None` when it is no instruction file.
fn classify(rel: &str, fallbacks: &[String]) -> Option<Kind> {
    let (dir, name) = split(rel);
    match (dir, name) {
        (".claude", "CLAUDE.md") => Some(Kind::DotClaude),
        (".claude", "AGENTS.md") => Some(Kind::DotClaudeAgents),
        (".omp", "AGENTS.md") => Some(Kind::OmpAgents),
        (".omp", "RULES.md") => Some(Kind::OmpRules),
        _ if !plain_folder(dir) => None,
        (_, "AGENTS.md") => Some(Kind::Agents),
        (_, "AGENTS.override.md") => Some(Kind::AgentsOverride),
        (_, "CLAUDE.md") => Some(Kind::Claude),
        (_, "CLAUDE.local.md") => Some(Kind::ClaudeLocal),
        (_, n) if fallbacks.iter().any(|f| f == n) => Some(Kind::CodexFallback),
        _ => None,
    }
}

/// The order of the rows: the project folder first (Codex's, Claude Code's, OMP's), then the
/// folders below by path.
fn sort_key(rel: &str, fallbacks: &[String]) -> (usize, String, usize) {
    let (dir, _) = split(rel);
    let rank = match classify(rel, fallbacks) {
        Some(Kind::AgentsOverride) => 0,
        Some(Kind::Agents) => 1,
        Some(Kind::CodexFallback) => 2,
        Some(Kind::Claude) => 3,
        Some(Kind::DotClaude) => 4,
        Some(Kind::DotClaudeAgents) => 5,
        Some(Kind::ClaudeLocal) => 6,
        Some(Kind::OmpAgents) => 7,
        Some(Kind::OmpRules) => 8,
        None => 9,
    };
    let depth = if dir.is_empty() || dir == ".claude" || dir == ".omp" {
        0
    } else {
        1
    };
    let folder = if depth == 0 {
        String::new()
    } else {
        dir.to_owned()
    };
    (depth, folder, rank)
}

fn has_claude_files(found: &[ProjectFile], dir: &str) -> bool {
    ["CLAUDE.md", "CLAUDE.local.md"]
        .iter()
        .any(|n| found.iter().any(|f| f.rel == format!("{dir}/{n}")))
}

/// The file Codex takes of the folder `dir` out of `names` (in order): the first non-empty one.
fn codex_choice(names: &[String], dir: &str, found: &[ProjectFile]) -> Option<String> {
    names.iter().find_map(|name| {
        let rel = if dir.is_empty() {
            name.clone()
        } else {
            format!("{dir}/{name}")
        };
        found
            .iter()
            .find(|f| f.rel == rel && non_empty(f))
            .map(|_| name.clone())
    })
}

struct OmpWinner {
    tier: u32,
    label: String,
}

/// OMP's priority of the files at the project folder's depth.
fn tier(kind: Kind) -> u32 {
    match kind {
        Kind::OmpAgents => 100,
        Kind::DotClaude => 80,
        Kind::Agents | Kind::Claude => 10,
        _ => 0,
    }
}

fn omp_root_winner(found: &[ProjectFile]) -> Option<OmpWinner> {
    [
        (".omp/AGENTS.md", Kind::OmpAgents),
        (".claude/CLAUDE.md", Kind::DotClaude),
        ("AGENTS.md", Kind::Agents),
        ("CLAUDE.md", Kind::Claude),
    ]
    .into_iter()
    .find(|(rel, _)| found.iter().any(|f| f.rel == *rel && non_empty(f)))
    .map(|(rel, kind)| OmpWinner {
        tier: tier(kind),
        label: rel.to_owned(),
    })
}

fn user_files(facts: &Facts) -> Vec<InstructionFile> {
    let homes = &facts.homes;
    let exists = |path: &Path| facts.user.iter().find(|f| f.path == path);
    let live = |path: &Path| exists(path).is_some_and(|f| f.size > 0);
    let tilde = |path: &Path| match path.strip_prefix(&homes.home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    };
    let codex_override = homes.codex.join("AGENTS.override.md");
    let codex_main = homes.codex.join("AGENTS.md");
    let claude_main = homes.claude.join("CLAUDE.md");
    let omp_main = homes.omp.join("AGENTS.md");
    let omp_rules = homes.omp.join("RULES.md");

    // Codex: the override, else AGENTS.md.
    let codex_winner = [&codex_override, &codex_main]
        .into_iter()
        .find(|p| live(p))
        .cloned();
    // OMP keeps one user file: its own, else Claude Code's, else Codex's, the other two only
    // when `enabledProviders` names them.
    let omp_choice: Option<PathBuf> = if live(&omp_main) {
        Some(omp_main.clone())
    } else if facts.config.omp_reads_claude_user && live(&claude_main) {
        Some(claude_main.clone())
    } else if facts.config.omp_reads_codex_user {
        codex_winner.clone()
    } else {
        None
    };

    let mut files = Vec::new();
    for path in user_candidates(homes) {
        let Some(found) = exists(&path) else { continue };
        let mut readers = Vec::new();
        let is_managed = homes.managed.as_deref() == Some(path.as_path());
        if path == codex_override || path == codex_main {
            match &codex_winner {
                Some(win) if *win == path => readers.push(reader(Harness::Codex, Reads::Always)),
                Some(win) => {
                    readers.push(reader(Harness::Codex, Reads::Shadowed { by: tilde(win) }))
                }
                None => {}
            }
        }
        if path == claude_main || is_managed {
            readers.push(reader(Harness::Claude, Reads::Always));
        }
        if path == omp_main || path == omp_rules {
            readers.push(reader(Harness::Omp, Reads::Always));
        }
        let omp_candidate = (path == claude_main && facts.config.omp_reads_claude_user)
            || ((path == codex_main || path == codex_override)
                && facts.config.omp_reads_codex_user);
        if omp_candidate {
            match &omp_choice {
                Some(win) if *win == path => readers.push(reader(Harness::Omp, Reads::Always)),
                Some(win) => readers.push(reader(Harness::Omp, Reads::Shadowed { by: tilde(win) })),
                None => {}
            }
        }
        files.push(InstructionFile {
            scope: Scope::User,
            label: if is_managed {
                format!("{} (managed)", path.display())
            } else {
                tilde(&path)
            },
            path,
            size: found.size,
            symlink: false,
            readers,
        });
    }
    for file in &mut files {
        file.readers.sort_by_key(|r| r.harness);
    }
    files
}

// ---- imports --------------------------------------------------------------------------------

/// The paths the `@` imports of `text` name, resolved against `dir` (the importing file's
/// folder, project-relative, `""` for the root): `@AGENTS.md`, `@docs/a.md`, `@../AGENTS.md`.
/// Tokens inside code fences and code spans are ignored, so are `~/` and absolute paths (they
/// leave the project) and tokens that are not at the start of a line or after a space or tab
/// (an e-mail address, `git@host`). Paths that leave the project are dropped.
pub fn imports_of(text: &str, dir: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut fenced: Option<char> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(fence) = trimmed
            .strip_prefix("```")
            .map(|_| '`')
            .or_else(|| trimmed.strip_prefix("~~~").map(|_| '~'))
        {
            match fenced {
                None => fenced = Some(fence),
                Some(open) if open == fence => fenced = None,
                Some(_) => {}
            }
            continue;
        }
        if fenced.is_some() {
            continue;
        }
        let visible = without_code_spans(line);
        let chars: Vec<char> = visible.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let at_start = i == 0 || chars[i - 1] == ' ' || chars[i - 1] == '\t';
            if chars[i] == '@' && at_start {
                let mut token = String::new();
                let mut j = i + 1;
                while j < chars.len() {
                    if chars[j] == '\\' && chars.get(j + 1) == Some(&' ') {
                        token.push(' ');
                        j += 2;
                    } else if chars[j].is_whitespace() {
                        break;
                    } else {
                        token.push(chars[j]);
                        j += 1;
                    }
                }
                let token = token
                    .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '"', '\'']);
                if let Some(path) = resolve_import(dir, token) {
                    found.push(path);
                }
                i = j;
            } else {
                i += 1;
            }
        }
    }
    found
}

/// `line` with its inline code spans blanked out.
fn without_code_spans(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut inside = false;
    for c in line.chars() {
        if c == '`' {
            inside = !inside;
            out.push(' ');
        } else if inside {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// `token` of a file in `dir`, as a project-relative path; `None` when it leaves the project.
fn resolve_import(dir: &str, token: &str) -> Option<String> {
    if token.is_empty() || token.starts_with('/') || token.starts_with('~') {
        return None;
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in token.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

// ---- the share plan -------------------------------------------------------------------------

/// The text of a new `AGENTS.md`.
pub const STARTER: &str = "# AGENTS.md\n\nInstructions for the coding agents that work in this repository.\n\n## Build and test\n\n## Conventions\n";

/// Largest file the plan copies: a file above it is for the user to move by hand.
pub const SHARE_MAX_BYTES: usize = 256 * 1024;

/// A file of the project as the share plan needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Existing {
    pub text: String,
    pub symlink: bool,
}

/// What *Share one file with every agent…* looks at: the project's `AGENTS.md`, `CLAUDE.md` and
/// `.claude/CLAUDE.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShareInput {
    pub agents: Option<Existing>,
    pub claude: Option<Existing>,
    pub dot_claude: Option<Existing>,
}

/// What the plan does to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A new file (it must not exist).
    Create { text: String },
    /// An existing file whose text was `before`.
    Replace { before: String, after: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedWrite {
    /// Relative to the project root.
    pub rel: String,
    pub change: Change,
    /// One line for the sheet: what happens and why.
    pub note: String,
}

impl PlannedWrite {
    /// The file's text after the write.
    pub fn after(&self) -> &str {
        match &self.change {
            Change::Create { text } => text,
            Change::Replace { after, .. } => after,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharePlan {
    /// Every agent already reads one file.
    Done { reason: String },
    /// Hutan does not touch these files; `reason` says what to do by hand.
    Blocked { reason: String },
    /// These writes, in order.
    Writes(Vec<PlannedWrite>),
}

fn blank(text: &str) -> bool {
    text.trim().is_empty()
}

fn line_end(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// What making `AGENTS.md` the one file does to `input`.
pub fn plan_share(input: &ShareInput) -> SharePlan {
    if input.agents.as_ref().is_some_and(|a| a.symlink) {
        return SharePlan::Blocked {
            reason: "AGENTS.md is a symbolic link; Hutan does not write through links".to_owned(),
        };
    }
    for (name, file) in [
        ("CLAUDE.md", &input.claude),
        (".claude/CLAUDE.md", &input.dot_claude),
    ] {
        if file.as_ref().is_some_and(|f| f.symlink) {
            return SharePlan::Done {
                reason: format!("{name} is a symbolic link: it already shows the same text"),
            };
        }
    }
    for (name, file) in [
        ("AGENTS.md", &input.agents),
        ("CLAUDE.md", &input.claude),
        (".claude/CLAUDE.md", &input.dot_claude),
    ] {
        if file
            .as_ref()
            .is_some_and(|f| f.text.len() > SHARE_MAX_BYTES)
        {
            return SharePlan::Blocked {
                reason: format!(
                    "{name} is larger than {} KiB; move its text by hand",
                    SHARE_MAX_BYTES / 1024
                ),
            };
        }
    }
    // Claude Code takes ./CLAUDE.md, else ./.claude/CLAUDE.md (both are read when both exist;
    // the plan edits the first and leaves the second).
    let (claude_rel, claude, import) = match (&input.claude, &input.dot_claude) {
        (Some(file), _) => ("CLAUDE.md", Some(file), "@AGENTS.md"),
        (None, Some(file)) => (".claude/CLAUDE.md", Some(file), "@../AGENTS.md"),
        (None, None) => ("CLAUDE.md", None, "@AGENTS.md"),
    };
    let imports_agents =
        |file: &Existing, dir: &str| imports_of(&file.text, dir).iter().any(|p| p == "AGENTS.md");
    let claude_dir = if claude_rel == "CLAUDE.md" {
        ""
    } else {
        ".claude"
    };
    if let Some(file) = claude
        && input.agents.is_some()
        && imports_agents(file, claude_dir)
    {
        return SharePlan::Done {
            reason: format!("{claude_rel} already imports AGENTS.md"),
        };
    }
    if claude_rel == ".claude/CLAUDE.md"
        && let Some(file) = claude
        && input.agents.is_none()
        && !imports_of(&file.text, ".claude").is_empty()
    {
        return SharePlan::Blocked {
            reason: ".claude/CLAUDE.md has @imports that are relative to the .claude folder; move its text to AGENTS.md by hand and keep an import in it".to_owned(),
        };
    }

    let mut writes = Vec::new();
    let eol = claude.map_or("\n", |f| line_end(&f.text));
    let import_only = format!("{import}{eol}");
    match (&input.agents, claude) {
        (None, None) => {
            writes.push(PlannedWrite {
                rel: "AGENTS.md".to_owned(),
                change: Change::Create {
                    text: STARTER.to_owned(),
                },
                note: "new file, for you to fill in".to_owned(),
            });
            writes.push(PlannedWrite {
                rel: claude_rel.to_owned(),
                change: Change::Create { text: import_only },
                note: "new file: Claude Code reads AGENTS.md through it".to_owned(),
            });
        }
        (None, Some(file)) => {
            let (text, note) = if blank(&file.text) {
                (
                    STARTER.to_owned(),
                    "new file, for you to fill in".to_owned(),
                )
            } else {
                (
                    file.text.clone(),
                    format!("new file: the text of {claude_rel}, unchanged"),
                )
            };
            writes.push(PlannedWrite {
                rel: "AGENTS.md".to_owned(),
                change: Change::Create { text },
                note,
            });
            writes.push(PlannedWrite {
                rel: claude_rel.to_owned(),
                change: Change::Replace {
                    before: file.text.clone(),
                    after: import_only,
                },
                note: "now only the import; its text moved to AGENTS.md".to_owned(),
            });
        }
        (Some(_), None) => {
            writes.push(PlannedWrite {
                rel: claude_rel.to_owned(),
                change: Change::Create { text: import_only },
                note: "new file: Claude Code reads AGENTS.md through it".to_owned(),
            });
        }
        (Some(agents), Some(file)) => {
            let (after, note) = if blank(&file.text) || file.text.trim() == agents.text.trim() {
                (
                    import_only,
                    "now only the import; its text is the same as AGENTS.md".to_owned(),
                )
            } else {
                (
                    format!("{import}{eol}{eol}{}", file.text),
                    "the import goes first; everything it said stays below".to_owned(),
                )
            };
            writes.push(PlannedWrite {
                rel: claude_rel.to_owned(),
                change: Change::Replace {
                    before: file.text.clone(),
                    after,
                },
                note,
            });
        }
    }
    SharePlan::Writes(writes)
}

/// The words of the confirm sheet: exactly what the plan writes, file by file, with the text
/// each file gets (at most `lines` lines of it).
pub fn preview(writes: &[PlannedWrite], lines: usize) -> String {
    let mut out = String::new();
    for write in writes {
        let verb = match write.change {
            Change::Create { .. } => "Create",
            Change::Replace { .. } => "Change",
        };
        out.push_str(&format!("{verb} {} — {}\n", write.rel, write.note));
        let text = write.after();
        let all: Vec<&str> = text.lines().collect();
        for line in all.iter().take(lines) {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
        if all.len() > lines {
            out.push_str(&format!("    … {} more lines\n", all.len() - lines));
        }
        out.push('\n');
    }
    out.push_str(
        "Nothing else is written: your own files in ~/.claude, ~/.codex and ~/.omp stay as they are. \
         The change shows in Changes and git, and Undo puts it back for ten seconds.",
    );
    out
}
