#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Instruction files (AGENT-INSTRUCTIONS): which agent reads which file, per the harnesses'
//! own rules (fixture folders for each harness's home, never the user's), the `@` imports, the
//! share plan, and — over a real daemon — the Instructions panel.

use std::path::{Path, PathBuf};

use hutan_desktop::instructions::{
    Change, Discovery, Existing, Facts, Harness, HarnessConfig, HarnessHomes, ProjectFile, Reads,
    STARTER, Scope, ShareInput, SharePlan, UserFile, codex_fallbacks, discover, imports_of,
    omp_enabled_providers, plan_share, preview, project_names, user_candidates,
};

// ---- fixtures ----

fn file(rel: &str, text: &str) -> ProjectFile {
    let dir = rel.rsplit_once('/').map_or("", |(d, _)| d);
    ProjectFile {
        rel: rel.to_owned(),
        size: text.len() as u64,
        symlink: false,
        imports: imports_of(text, dir),
    }
}

fn homes() -> HarnessHomes {
    HarnessHomes::under(Path::new("/home/test"))
}

fn facts(project: Vec<ProjectFile>, user: &[&str], config: HarnessConfig) -> Facts {
    let homes = homes();
    Facts {
        root: PathBuf::from("/work/proj"),
        project,
        user: user
            .iter()
            .map(|rel| UserFile {
                path: PathBuf::from("/home/test").join(rel),
                size: 10,
            })
            .collect(),
        homes,
        config,
    }
}

fn readers(discovery: &Discovery, label: &str) -> Vec<(Harness, Reads)> {
    discovery
        .file(label)
        .unwrap_or_else(|| panic!("{label} is not listed: {discovery:#?}"))
        .readers
        .iter()
        .map(|r| (r.harness, r.reads.clone()))
        .collect()
}

fn labels(files: &[hutan_desktop::instructions::InstructionFile]) -> Vec<&str> {
    files.iter().map(|f| f.label.as_str()).collect()
}

// ---- discovery ----

#[test]
fn agents_md_alone_is_read_by_all_three() {
    let d = discover(&facts(
        vec![file("AGENTS.md", "# rules\n")],
        &[],
        HarnessConfig::default(),
    ));
    assert_eq!(
        readers(&d, "AGENTS.md"),
        [
            (Harness::Codex, Reads::Always),
            (Harness::Claude, Reads::Always),
            (Harness::Omp, Reads::Always),
        ]
    );
    assert!(d.shared());
}

#[test]
fn claude_md_hides_agents_md_from_claude_code_until_it_imports_it() {
    let both = discover(&facts(
        vec![
            file("AGENTS.md", "# rules\n"),
            file("CLAUDE.md", "# mine\n"),
        ],
        &[],
        HarnessConfig::default(),
    ));
    // Codex reads AGENTS.md; Claude Code reads CLAUDE.md instead; OMP reads both (same level).
    assert_eq!(
        readers(&both, "AGENTS.md"),
        [
            (Harness::Codex, Reads::Always),
            (Harness::Omp, Reads::Always)
        ]
    );
    assert_eq!(
        readers(&both, "CLAUDE.md"),
        [
            (Harness::Claude, Reads::Always),
            (Harness::Omp, Reads::Always)
        ]
    );
    assert!(!both.shared());

    let imported = discover(&facts(
        vec![
            file("AGENTS.md", "# rules\n"),
            file("CLAUDE.md", "@AGENTS.md\n"),
        ],
        &[],
        HarnessConfig::default(),
    ));
    assert_eq!(
        readers(&imported, "AGENTS.md"),
        [
            (Harness::Codex, Reads::Always),
            (
                Harness::Claude,
                Reads::Import {
                    via: "CLAUDE.md".to_owned()
                }
            ),
            (Harness::Omp, Reads::Always),
        ]
    );
    assert!(imported.shared());
}

#[test]
fn the_dot_claude_file_and_the_local_file_count_as_claude_md() {
    let d = discover(&facts(
        vec![
            file("AGENTS.md", "x"),
            file(".claude/CLAUDE.md", "y"),
            file("CLAUDE.local.md", "z"),
        ],
        &[],
        HarnessConfig::default(),
    ));
    assert_eq!(
        labels(&d.project),
        ["AGENTS.md", ".claude/CLAUDE.md", "CLAUDE.local.md"]
    );
    // Claude Code reads the two CLAUDE files and not AGENTS.md; OMP prefers the Claude folder.
    assert_eq!(
        readers(&d, "AGENTS.md"),
        [
            (Harness::Codex, Reads::Always),
            (
                Harness::Omp,
                Reads::Shadowed {
                    by: ".claude/CLAUDE.md".to_owned()
                }
            ),
        ]
    );
    assert_eq!(
        readers(&d, ".claude/CLAUDE.md"),
        [
            (Harness::Claude, Reads::Always),
            (Harness::Omp, Reads::Always)
        ]
    );
    assert_eq!(
        readers(&d, "CLAUDE.local.md"),
        [(Harness::Claude, Reads::Always)]
    );
}

#[test]
fn omp_prefers_its_own_file_then_claudes_at_the_same_level() {
    let d = discover(&facts(
        vec![
            file(".omp/AGENTS.md", "a"),
            file(".omp/RULES.md", "r"),
            file(".claude/CLAUDE.md", "c"),
            file("AGENTS.md", "g"),
            file("CLAUDE.md", "m"),
        ],
        &[],
        HarnessConfig::default(),
    ));
    assert_eq!(
        readers(&d, ".omp/AGENTS.md"),
        [(Harness::Omp, Reads::Always)]
    );
    assert_eq!(
        readers(&d, ".omp/RULES.md"),
        [(Harness::Omp, Reads::Always)]
    );
    for label in [".claude/CLAUDE.md", "AGENTS.md", "CLAUDE.md"] {
        let omp = d.file(label).unwrap().reader(Harness::Omp).unwrap();
        assert_eq!(
            omp.reads,
            Reads::Shadowed {
                by: ".omp/AGENTS.md".to_owned()
            },
            "{label}"
        );
    }
    // Without the native file the Claude folder's wins.
    let d = discover(&facts(
        vec![file(".claude/CLAUDE.md", "c"), file("AGENTS.md", "g")],
        &[],
        HarnessConfig::default(),
    ));
    assert_eq!(
        d.file("AGENTS.md")
            .unwrap()
            .reader(Harness::Omp)
            .unwrap()
            .reads,
        Reads::Shadowed {
            by: ".claude/CLAUDE.md".to_owned()
        }
    );
}

#[test]
fn codex_takes_the_override_then_agents_md_then_its_fallback_names() {
    let config = HarnessConfig {
        codex_fallbacks: vec!["TEAM_GUIDE.md".to_owned()],
        ..HarnessConfig::default()
    };
    let d = discover(&facts(
        vec![
            file("AGENTS.override.md", "o"),
            file("AGENTS.md", "a"),
            file("TEAM_GUIDE.md", "t"),
        ],
        &[],
        config.clone(),
    ));
    assert_eq!(
        readers(&d, "AGENTS.override.md"),
        [(Harness::Codex, Reads::Always)]
    );
    assert_eq!(
        d.file("AGENTS.md")
            .unwrap()
            .reader(Harness::Codex)
            .unwrap()
            .reads,
        Reads::Shadowed {
            by: "AGENTS.override.md".to_owned()
        }
    );
    assert_eq!(
        d.file("TEAM_GUIDE.md")
            .unwrap()
            .reader(Harness::Codex)
            .unwrap()
            .reads,
        Reads::Shadowed {
            by: "AGENTS.override.md".to_owned()
        }
    );
    // The fallback is read when there is no AGENTS.md; an empty AGENTS.md is skipped.
    let mut empty = file("AGENTS.md", "");
    empty.size = 0;
    let d = discover(&facts(
        vec![empty, file("TEAM_GUIDE.md", "t")],
        &[],
        config.clone(),
    ));
    assert_eq!(
        readers(&d, "TEAM_GUIDE.md"),
        [(Harness::Codex, Reads::Always)]
    );
    assert!(project_names(&config).contains(&"TEAM_GUIDE.md".to_owned()));
}

#[test]
fn nested_files_are_read_on_demand() {
    let d = discover(&facts(
        vec![
            file("AGENTS.md", "root"),
            file("services/api/AGENTS.md", "api"),
            file("services/api/CLAUDE.md", "api claude"),
            file("web/CLAUDE.local.md", "w"),
            file("node_modules/.hidden/AGENTS.md", "ignored"),
        ],
        &[],
        HarnessConfig::default(),
    ));
    assert_eq!(
        labels(&d.project),
        [
            "AGENTS.md",
            "services/api/AGENTS.md",
            "services/api/CLAUDE.md",
            "web/CLAUDE.local.md"
        ]
    );
    assert_eq!(
        readers(&d, "services/api/AGENTS.md"),
        [
            (Harness::Codex, Reads::OnDemand),
            (Harness::Omp, Reads::OnDemand)
        ]
    );
    assert_eq!(
        readers(&d, "services/api/CLAUDE.md"),
        [(Harness::Claude, Reads::OnDemand)]
    );
    assert_eq!(
        readers(&d, "web/CLAUDE.local.md"),
        [(Harness::Claude, Reads::OnDemand)]
    );
}

#[test]
fn user_files_list_with_the_agents_that_read_them() {
    let d = discover(&facts(
        Vec::new(),
        &[
            ".codex/AGENTS.md",
            ".claude/CLAUDE.md",
            ".omp/agent/AGENTS.md",
            ".omp/agent/RULES.md",
        ],
        HarnessConfig::default(),
    ));
    assert_eq!(
        labels(&d.user),
        [
            "~/.codex/AGENTS.md",
            "~/.claude/CLAUDE.md",
            "~/.omp/agent/AGENTS.md",
            "~/.omp/agent/RULES.md"
        ]
    );
    assert!(d.user.iter().all(|f| f.scope == Scope::User));
    assert_eq!(
        readers(&d, "~/.codex/AGENTS.md"),
        [(Harness::Codex, Reads::Always)]
    );
    assert_eq!(
        readers(&d, "~/.claude/CLAUDE.md"),
        [(Harness::Claude, Reads::Always)]
    );
    assert_eq!(
        readers(&d, "~/.omp/agent/AGENTS.md"),
        [(Harness::Omp, Reads::Always)]
    );
    // OMP reads Claude Code's and Codex's user files only when enabledProviders names them,
    // and keeps one user file: its own wins.
    let config = HarnessConfig {
        omp_reads_claude_user: true,
        omp_reads_codex_user: true,
        ..HarnessConfig::default()
    };
    let d = discover(&facts(
        Vec::new(),
        &[
            ".codex/AGENTS.md",
            ".claude/CLAUDE.md",
            ".omp/agent/AGENTS.md",
        ],
        config.clone(),
    ));
    assert_eq!(
        d.file("~/.claude/CLAUDE.md")
            .unwrap()
            .reader(Harness::Omp)
            .unwrap()
            .reads,
        Reads::Shadowed {
            by: "~/.omp/agent/AGENTS.md".to_owned()
        }
    );
    let d = discover(&facts(
        Vec::new(),
        &[".codex/AGENTS.md", ".claude/CLAUDE.md"],
        config,
    ));
    assert_eq!(
        readers(&d, "~/.claude/CLAUDE.md"),
        [
            (Harness::Claude, Reads::Always),
            (Harness::Omp, Reads::Always)
        ]
    );
    assert_eq!(
        d.file("~/.codex/AGENTS.md")
            .unwrap()
            .reader(Harness::Omp)
            .unwrap()
            .reads,
        Reads::Shadowed {
            by: "~/.claude/CLAUDE.md".to_owned()
        }
    );
    assert_eq!(user_candidates(&homes()).len(), 5);
}

#[test]
fn harness_configs_are_read_from_their_files() {
    assert_eq!(
        codex_fallbacks(
            "project_doc_fallback_filenames = [\"TEAM_GUIDE.md\", \"a/b.md\", \".agents.md\"]\n"
        ),
        ["TEAM_GUIDE.md", ".agents.md"]
    );
    assert!(codex_fallbacks("not toml [").is_empty());
    assert_eq!(
        omp_enabled_providers("enabledProviders:\n  - claude\n  - \"github\"\nother: 1\n"),
        ["claude", "github"]
    );
    assert_eq!(
        omp_enabled_providers("enabledProviders: [codex]\n"),
        ["codex"]
    );

    let dir = tempfile::tempdir().unwrap();
    let homes = HarnessHomes::under(dir.path());
    std::fs::create_dir_all(&homes.codex).unwrap();
    std::fs::create_dir_all(&homes.omp).unwrap();
    std::fs::write(
        homes.codex.join("config.toml"),
        "project_doc_fallback_filenames = [\"TEAM_GUIDE.md\"]\n",
    )
    .unwrap();
    std::fs::write(
        homes.omp.join("config.yml"),
        "enabledProviders:\n  - claude\n",
    )
    .unwrap();
    let config = HarnessConfig::load(&homes);
    assert_eq!(config.codex_fallbacks, ["TEAM_GUIDE.md"]);
    assert!(config.omp_reads_claude_user && !config.omp_reads_codex_user);
    // Nothing there: the defaults.
    assert_eq!(
        HarnessConfig::load(&HarnessHomes::under(&dir.path().join("none"))),
        HarnessConfig::default()
    );
}

// ---- the share plan ----

fn existing(text: &str) -> Option<Existing> {
    Some(Existing {
        text: text.to_owned(),
        symlink: false,
    })
}

fn writes(plan: SharePlan) -> Vec<(String, Change)> {
    match plan {
        SharePlan::Writes(w) => w.into_iter().map(|w| (w.rel, w.change)).collect(),
        other => panic!("expected writes, got {other:?}"),
    }
}

#[test]
fn share_with_only_claude_md_moves_its_text_to_agents_md() {
    let w = writes(plan_share(&ShareInput {
        claude: existing("# Claude rules\n\nBe brief.\n"),
        ..ShareInput::default()
    }));
    assert_eq!(
        w,
        [
            (
                "AGENTS.md".to_owned(),
                Change::Create {
                    text: "# Claude rules\n\nBe brief.\n".to_owned()
                }
            ),
            (
                "CLAUDE.md".to_owned(),
                Change::Replace {
                    before: "# Claude rules\n\nBe brief.\n".to_owned(),
                    after: "@AGENTS.md\n".to_owned()
                }
            ),
        ]
    );
}

#[test]
fn share_with_only_agents_md_adds_the_import_file() {
    let w = writes(plan_share(&ShareInput {
        agents: existing("# rules\n"),
        ..ShareInput::default()
    }));
    assert_eq!(
        w,
        [(
            "CLAUDE.md".to_owned(),
            Change::Create {
                text: "@AGENTS.md\n".to_owned()
            }
        )]
    );
}

#[test]
fn share_with_neither_creates_both() {
    let w = writes(plan_share(&ShareInput::default()));
    assert_eq!(
        w,
        [
            (
                "AGENTS.md".to_owned(),
                Change::Create {
                    text: STARTER.to_owned()
                }
            ),
            (
                "CLAUDE.md".to_owned(),
                Change::Create {
                    text: "@AGENTS.md\n".to_owned()
                }
            ),
        ]
    );
}

#[test]
fn share_with_both_keeps_what_claude_md_said_below_the_import() {
    let w = writes(plan_share(&ShareInput {
        agents: existing("# shared\n"),
        claude: existing("# only for Claude\r\nuse plan mode\r\n"),
        ..ShareInput::default()
    }));
    assert_eq!(
        w,
        [(
            "CLAUDE.md".to_owned(),
            Change::Replace {
                before: "# only for Claude\r\nuse plan mode\r\n".to_owned(),
                after: "@AGENTS.md\r\n\r\n# only for Claude\r\nuse plan mode\r\n".to_owned()
            }
        )]
    );
    // The same text in both: the import replaces it, nothing is lost.
    let w = writes(plan_share(&ShareInput {
        agents: existing("# same\n"),
        claude: existing("# same\n"),
        ..ShareInput::default()
    }));
    assert_eq!(
        w[0].1,
        Change::Replace {
            before: "# same\n".to_owned(),
            after: "@AGENTS.md\n".to_owned()
        }
    );
}

#[test]
fn share_edits_the_dot_claude_file_with_a_relative_import() {
    let w = writes(plan_share(&ShareInput {
        agents: existing("# shared\n"),
        dot_claude: existing("# c\n"),
        ..ShareInput::default()
    }));
    assert_eq!(w[0].0, ".claude/CLAUDE.md");
    assert_eq!(
        w[0].1,
        Change::Replace {
            before: "# c\n".to_owned(),
            after: "@../AGENTS.md\n\n# c\n".to_owned()
        }
    );
}

#[test]
fn share_leaves_what_is_already_shared_or_not_ours() {
    assert!(matches!(
        plan_share(&ShareInput {
            agents: existing("a"),
            claude: existing("@AGENTS.md\n\nmore\n"),
            ..ShareInput::default()
        }),
        SharePlan::Done { .. }
    ));
    assert!(matches!(
        plan_share(&ShareInput {
            agents: existing("a"),
            claude: Some(Existing {
                text: "a".to_owned(),
                symlink: true
            }),
            ..ShareInput::default()
        }),
        SharePlan::Done { .. }
    ));
    assert!(matches!(
        plan_share(&ShareInput {
            agents: Some(Existing {
                text: "a".to_owned(),
                symlink: true
            }),
            ..ShareInput::default()
        }),
        SharePlan::Blocked { .. }
    ));
    assert!(matches!(
        plan_share(&ShareInput {
            claude: existing(&"x".repeat(300 * 1024)),
            ..ShareInput::default()
        }),
        SharePlan::Blocked { .. }
    ));
    // Relative imports of .claude/CLAUDE.md would break when its text moves.
    assert!(matches!(
        plan_share(&ShareInput {
            dot_claude: existing("see @rules.md\n"),
            ..ShareInput::default()
        }),
        SharePlan::Blocked { .. }
    ));
}

#[test]
fn the_preview_says_exactly_what_is_written() {
    let SharePlan::Writes(plan) = plan_share(&ShareInput {
        claude: existing("# Claude rules\n"),
        ..ShareInput::default()
    }) else {
        panic!("writes")
    };
    let text = preview(&plan, 8);
    assert!(text.contains("Create AGENTS.md"), "{text}");
    assert!(text.contains("Change CLAUDE.md"), "{text}");
    assert!(text.contains("    # Claude rules"), "{text}");
    assert!(text.contains("    @AGENTS.md"), "{text}");
    assert!(text.contains("~/.claude"), "{text}");
}
