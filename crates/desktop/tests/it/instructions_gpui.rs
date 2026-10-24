#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Instruction files (AGENT-INSTRUCTIONS): which agent reads which file, per the harnesses'
//! own rules (fixture folders for each harness's home, never the user's), the `@` imports, the
//! share plan, and — over a real daemon — the Instructions panel.

use std::path::{Path, PathBuf};

use crate::common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::panels::InstructionsPanel;
use hutan_desktop::workbench::{Region, Workbench, actions};
use hutan_settings::{SettingsPaths, SettingsStore};

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
    // Every agent reads it, but Claude Code only by itself: the share flow still adds the import.
    assert!(!d.shared());
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
            (
                Harness::Claude,
                Reads::Shadowed {
                    by: "CLAUDE.md".to_owned()
                }
            ),
            (Harness::Omp, Reads::Always)
        ]
    );
    assert_eq!(
        both.file("AGENTS.md").unwrap().summary(),
        "Read by Codex, OMP · Claude Code reads CLAUDE.md instead"
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
                Harness::Claude,
                Reads::Shadowed {
                    by: ".claude/CLAUDE.md".to_owned()
                }
            ),
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

// ---- the panel over a real daemon ----

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let (view, cx) = open_with(cx, settings, daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    (view, cx)
}

/// Opens `dir` as the project the window works in, with fixture harness folders in `home`.
fn project(view: &Entity<Workbench>, cx: &mut VisualTestContext, dir: &Path, home: &Path) {
    let homes = HarnessHomes::under(home);
    view.update(cx, |wb, cx| wb.set_instruction_homes(Some(homes), cx));
    let dir = dir.to_owned();
    view.update_in(cx, |wb, window, cx| {
        wb.open_project_path(dir.clone(), window, cx);
    });
    wait_until(cx, "the project to be the active one", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).files_root().as_deref() == Some(dir.as_path())
        })
    });
}

fn write(root: &Path, relative: &str, text: &str) -> PathBuf {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    path
}

/// The Instructions panel, shown and read at least `reads` times.
fn panel(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    reads: u64,
) -> Entity<InstructionsPanel> {
    cx.dispatch_action(actions::OpenPanel("instructions".into()));
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<InstructionsPanel>("instructions"))
        .expect("the Instructions panel is drawn");
    wait_until(cx, "the instruction files to be read", |cx| {
        panel.read_with(cx, |p, _| {
            p.reads() >= reads && !p.reading() && p.discovery().is_some()
        })
    });
    panel
}

fn listed(panel: &Entity<InstructionsPanel>, cx: &VisualTestContext) -> Vec<String> {
    panel.read_with(cx, |p, _| p.files().into_iter().map(|f| f.label).collect())
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, "the control to be drawn", |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
    });
    let at = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap()
        .center();
    cx.simulate_click(at, Modifiers::none());
    draw(cx);
}

fn fixture_home(dir: &Path) -> PathBuf {
    let home = dir.join("home");
    write(&home, ".claude/CLAUDE.md", "# mine\n");
    write(&home, ".codex/AGENTS.md", "# codex\n");
    home
}

#[gpui::test]
fn the_panel_lists_the_files_with_the_agents_that_read_them(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    write(&root, "AGENTS.md", "# rules\n");
    write(&root, "CLAUDE.md", "# claude\n");
    write(&root, "services/api/AGENTS.md", "# api\n");
    let root = root.canonicalize().unwrap();
    let home = fixture_home(dir.path());
    project(&view, cx, &root, &home);

    let panel = panel(&view, cx, 1);
    assert_eq!(
        listed(&panel, cx),
        [
            "AGENTS.md",
            "CLAUDE.md",
            "services/api/AGENTS.md",
            "~/.codex/AGENTS.md",
            "~/.claude/CLAUDE.md"
        ]
    );
    panel.read_with(cx, |p, _| {
        let d = p.discovery().unwrap();
        assert_eq!(
            d.file("AGENTS.md").unwrap().summary(),
            "Read by Codex, OMP · Claude Code reads CLAUDE.md instead"
        );
        assert_eq!(
            d.file("CLAUDE.md").unwrap().read_by(),
            [Harness::Claude, Harness::Omp]
        );
        assert_eq!(
            d.file("services/api/AGENTS.md").unwrap().summary(),
            "Codex, OMP when working in this folder"
        );
        assert_eq!(
            d.file("~/.claude/CLAUDE.md").unwrap().read_by(),
            [Harness::Claude]
        );
        // The user's files are the user's: nothing in them was touched.
        assert!(!d.shared());
    });
    assert_eq!(
        std::fs::read_to_string(home.join(".claude/CLAUDE.md")).unwrap(),
        "# mine\n"
    );
}

#[gpui::test]
fn a_click_opens_a_project_file_in_a_tab_and_never_a_user_file(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let agents = write(&root, "AGENTS.md", "# rules\n");
    let root = root.canonicalize().unwrap();
    let home = fixture_home(dir.path());
    project(&view, cx, &root, &home);
    let panel = panel(&view, cx, 1);
    assert_eq!(listed(&panel, cx)[0], "AGENTS.md");

    click(&view, cx, Region::InstructionRow(0));
    wait_until(cx, "AGENTS.md in a file tab", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.focused_file_view()
                .is_some_and(|v| v.read(cx).buffer().read(cx).path().ends_with("AGENTS.md"))
        })
    });
    let opened = view.read_with(cx, |wb, cx| {
        wb.focused_file_view()
            .unwrap()
            .read(cx)
            .buffer()
            .read(cx)
            .path()
            .to_owned()
    });
    assert_eq!(
        PathBuf::from(opened).canonicalize().unwrap(),
        agents.canonicalize().unwrap()
    );
}

#[gpui::test]
fn an_empty_project_offers_to_create_agents_md(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = dir.path().join("empty-home");
    std::fs::create_dir_all(&home).unwrap();
    project(&view, cx, &root, &home);
    let panel = panel(&view, cx, 1);
    assert!(listed(&panel, cx).is_empty());

    click(&view, cx, Region::Instructions("create"));
    wait_until(cx, "AGENTS.md to be created", |_| {
        root.join("AGENTS.md").exists()
    });
    assert_eq!(
        std::fs::read_to_string(root.join("AGENTS.md")).unwrap(),
        STARTER
    );
    wait_until(cx, "the panel to list it", |cx| {
        listed(&panel, cx) == ["AGENTS.md"]
    });
    // The toast undoes it.
    assert!(view.read_with(cx, |wb, _| wb.instructions_toast().is_some()));
    click(&view, cx, Region::InstructionsToast("undo"));
    wait_until(cx, "AGENTS.md to be gone", |_| {
        !root.join("AGENTS.md").exists()
    });
}

fn confirm_text(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> String {
    wait_until(cx, "the sheet", |cx| {
        view.read_with(cx, |wb, _| wb.modal_open())
    });
    view.read_with(cx, |wb, _| wb.modal().unwrap().message.to_string())
}

#[gpui::test]
fn sharing_writes_what_the_sheet_showed_and_undo_puts_it_back(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    write(&root, "CLAUDE.md", "# Claude rules\n\nBe brief.\n");
    let root = root.canonicalize().unwrap();
    let home = fixture_home(dir.path());
    project(&view, cx, &root, &home);
    let panel = panel(&view, cx, 1);
    let user_before: Vec<String> = [".claude/CLAUDE.md", ".codex/AGENTS.md"]
        .iter()
        .map(|f| std::fs::read_to_string(home.join(f)).unwrap())
        .collect();

    click(&view, cx, Region::Instructions("share"));
    let sheet = confirm_text(&view, cx);
    assert!(sheet.contains("Create AGENTS.md"), "{sheet}");
    assert!(sheet.contains("Change CLAUDE.md"), "{sheet}");
    assert!(sheet.contains("    # Claude rules"), "{sheet}");
    assert!(sheet.contains("    @AGENTS.md"), "{sheet}");
    // Nothing was written before the sheet's answer.
    assert!(!root.join("AGENTS.md").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("CLAUDE.md")).unwrap(),
        "# Claude rules\n\nBe brief.\n"
    );

    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "both files to be written", |_| {
        root.join("AGENTS.md").exists()
            && std::fs::read_to_string(root.join("CLAUDE.md")).is_ok_and(|t| t == "@AGENTS.md\n")
    });
    assert_eq!(
        std::fs::read_to_string(root.join("AGENTS.md")).unwrap(),
        "# Claude rules\n\nBe brief.\n"
    );
    // The user-level files are untouched.
    let user_after: Vec<String> = [".claude/CLAUDE.md", ".codex/AGENTS.md"]
        .iter()
        .map(|f| std::fs::read_to_string(home.join(f)).unwrap())
        .collect();
    assert_eq!(user_before, user_after);
    // The panel now shows one file for everybody, and the toast says so.
    wait_until(cx, "the panel to be shared", |cx| {
        panel.read_with(cx, |p, _| p.discovery().is_some_and(Discovery::shared))
    });
    let toast = view.read_with(cx, |wb, _| {
        wb.instructions_toast().map(|t| t.text.to_string())
    });
    assert_eq!(
        toast.as_deref(),
        Some("Shared one file with every agent · AGENTS.md and CLAUDE.md")
    );
    // Undo restores the project exactly.
    click(&view, cx, Region::InstructionsToast("undo"));
    wait_until(cx, "the undo", |_| !root.join("AGENTS.md").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("CLAUDE.md")).unwrap(),
        "# Claude rules\n\nBe brief.\n"
    );
    wait_until(cx, "the panel to follow", |cx| {
        panel.read_with(cx, |p, _| {
            p.discovery().is_some_and(|d| !d.has_project("AGENTS.md"))
        })
    });
}

#[gpui::test]
fn cancelling_the_sheet_writes_nothing_and_a_shared_project_offers_nothing(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    write(&root, "AGENTS.md", "# rules\n");
    let root = root.canonicalize().unwrap();
    let home = fixture_home(dir.path());
    project(&view, cx, &root, &home);
    panel(&view, cx, 1);

    click(&view, cx, Region::Instructions("share"));
    let sheet = confirm_text(&view, cx);
    assert!(sheet.contains("Create CLAUDE.md"), "{sheet}");
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    draw(cx);
    assert!(!root.join("CLAUDE.md").exists());
    assert!(view.read_with(cx, |wb, _| wb.instructions_toast().is_none()));

    // With CLAUDE.md importing AGENTS.md there is nothing to share.
    write(&root, "CLAUDE.md", "@AGENTS.md\n");
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<InstructionsPanel>("instructions"))
        .unwrap();
    panel.update(cx, |p, cx| p.refresh(cx));
    wait_until(cx, "the panel to see the import", |cx| {
        panel.read_with(cx, |p, _| p.discovery().is_some_and(Discovery::shared))
    });
    draw(cx);
    assert!(view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::Instructions("share")).is_none()
    }));
}

#[test]
fn a_linked_claude_md_counts_as_shared() {
    let mut claude = file("CLAUDE.md", "# rules\n");
    claude.symlink = true;
    let d = discover(&facts(
        vec![file("AGENTS.md", "# rules\n"), claude],
        &[],
        HarnessConfig::default(),
    ));
    assert_eq!(
        d.file("AGENTS.md")
            .unwrap()
            .reader(Harness::Claude)
            .unwrap()
            .reads,
        Reads::Linked {
            via: "CLAUDE.md".to_owned()
        }
    );
    assert!(d.shared());
}
