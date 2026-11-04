//! Automatic shell integration over real PTYs: zsh, bash and fish started the way the daemon
//! starts them (login shell or explicit program) with a throwaway `HOME`. Shells that are not
//! installed are skipped with a message. Extra bash builds can be tested with
//! `HUTAN_TEST_BASH=/path/to/bash`.
#![allow(clippy::expect_used, clippy::panic)] // tests assert by panicking

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hutan_protocol::messages::TerminalSize;
use hutan_protocol::render::TerminalSnapshot;
use hutan_terminal::shell_integration;
use hutan_terminal::{PromptMark, RenderTracker, Terminal, TerminalConfig, TerminalMeta};
use parking_lot::Mutex;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::timeout;

const LIMIT: Duration = Duration::from_secs(20);
/// The user's prompt is `PROMPT` plus a space; the screen text drops trailing blanks.
const PROMPT: &str = "HUTAN-TEST-PROMPT>";

fn text(snapshot: &TerminalSnapshot) -> String {
    snapshot
        .lines
        .iter()
        .map(|row| {
            row.runs
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn wait_for(term: &Terminal, what: &str, check: impl Fn(&str) -> bool) {
    let mut damage = term.damage();
    let mut tracker = RenderTracker::new();
    let result = timeout(LIMIT, async {
        loop {
            let screen = text(&term.snapshot(&mut tracker));
            if check(&screen) {
                return;
            }
            if damage.changed().await.is_err() {
                return;
            }
        }
    })
    .await;
    let screen = text(&term.snapshot(&mut RenderTracker::new()));
    assert!(
        result.is_ok() && check(&screen),
        "{what}: not seen; screen:\n{screen}"
    );
}

/// Everything the sniffer reported, in order.
#[derive(Clone, Default)]
struct Collected(Arc<Mutex<Vec<TerminalMeta>>>);

impl Collected {
    fn start(term: &Terminal) -> Self {
        let collected = Self::default();
        let mut meta = term.metadata();
        let sink = collected.0.clone();
        tokio::spawn(async move {
            loop {
                match meta.recv().await {
                    Ok(event) => sink.lock().push(event),
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return,
                }
            }
        });
        collected
    }

    fn marks(&self) -> Vec<PromptMark> {
        self.0
            .lock()
            .iter()
            .filter_map(|event| match event {
                TerminalMeta::PromptMark(mark) => Some(*mark),
                _ => None,
            })
            .collect()
    }

    fn cwds(&self) -> Vec<PathBuf> {
        self.0
            .lock()
            .iter()
            .filter_map(|event| match event {
                TerminalMeta::Cwd(path) => Some(path.clone()),
                _ => None,
            })
            .collect()
    }

    async fn wait_prompts(&self, ends: usize) {
        let deadline = Instant::now() + LIMIT;
        while self.count(PromptMark::B) < ends {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for prompt end #{ends}; marks so far: {:?}",
                self.marks()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn count(&self, kind: PromptMark) -> usize {
        self.marks().iter().filter(|m| **m == kind).count()
    }
}

use PromptMark::{A, B, C};

fn d(code: i32) -> PromptMark {
    PromptMark::D {
        exit_code: Some(code),
    }
}

/// Prompt, `true`, prompt, `false`, prompt, `cd`, prompt.
fn full_sequence() -> Vec<PromptMark> {
    vec![A, B, C, d(0), A, B, C, d(1), A, B, C, d(0), A, B]
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Zsh,
    Bash,
    Fish,
}

struct Scenario {
    kind: Kind,
    shell: PathBuf,
    /// Empty = the daemon's login-shell path (`$SHELL -l`).
    argv: Vec<String>,
    /// zsh: put the user's startup files in a custom `ZDOTDIR`.
    custom_zdotdir: bool,
    /// fish: give the shell an explicit `XDG_DATA_DIRS`.
    explicit_xdg: bool,
    /// The user's prompt is rebuilt by a framework hook that also clobbers `$?`.
    prompt_framework: bool,
    /// Startup files that must have run, in order, exactly once.
    expected_rc: Vec<&'static str>,
    /// Session env additions, and whether `TerminalConfig::shell_integration` is on.
    extra_env: Vec<(&'static str, &'static str)>,
    integration: bool,
}

impl Scenario {
    fn new(kind: Kind, shell: &str, argv: &[&str], expected_rc: &[&'static str]) -> Self {
        Self {
            kind,
            shell: PathBuf::from(shell),
            argv: argv.iter().map(|a| (*a).to_owned()).collect(),
            custom_zdotdir: false,
            explicit_xdg: false,
            prompt_framework: false,
            expected_rc: expected_rc.to_vec(),
            extra_env: Vec::new(),
            integration: true,
        }
    }
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, content).expect("write");
}

fn write_user_files(s: &Scenario, home: &Path, zdotdir: &Path) {
    let log = r#"echo NAME >> "$HUTAN_TEST_LOG""#;
    let log = |name: &str| log.replace("NAME", name);
    match s.kind {
        Kind::Zsh => {
            let prompt = if s.prompt_framework {
                "fw_precmd() { false; PS1='HUTAN-TEST-PROMPT> ' }\nprecmd_functions+=(fw_precmd)\n"
            } else {
                "PS1='HUTAN-TEST-PROMPT> '\n"
            };
            for name in ["zshenv", "zprofile", "zlogin"] {
                write(&zdotdir.join(format!(".{name}")), &(log(name) + "\n"));
            }
            write(
                &zdotdir.join(".zshrc"),
                &format!(
                    "{}\nexport HUTAN_TEST_COUNT=$((${{HUTAN_TEST_COUNT:-0}}+1))\n{prompt}",
                    log("zshrc")
                ),
            );
            if s.custom_zdotdir {
                // Read by a shim that forgot the user's ZDOTDIR.
                for name in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
                    write(&home.join(name), &(log("decoy") + "\n"));
                }
            }
        }
        Kind::Bash => {
            write(
                &home.join(".bash_profile"),
                &format!("{}\n. \"$HOME/.bashrc\"\n", log("profile")),
            );
            let prompt = if s.prompt_framework {
                "fw_prompt() { false; PS1='HUTAN-TEST-PROMPT> '; }\nPROMPT_COMMAND=fw_prompt\n"
            } else {
                "PS1='HUTAN-TEST-PROMPT> '\n"
            };
            write(
                &home.join(".bashrc"),
                &format!(
                    "{}\nexport HUTAN_TEST_COUNT=$((${{HUTAN_TEST_COUNT:-0}}+1))\n{prompt}",
                    log("bashrc")
                ),
            );
        }
        Kind::Fish => write(
            &home.join(".config/fish/config.fish"),
            &format!(
                "{}\nset -gx HUTAN_TEST_COUNT (math \"0$HUTAN_TEST_COUNT + 1\")\n\
                 function fish_prompt\n    echo -n '{PROMPT} '\nend\n",
                log("config")
            ),
        ),
    }
}

fn inherited(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

async fn run(s: Scenario) {
    let tmp = tempfile::tempdir().expect("tempdir");
    // A canonical root keeps `$PWD` identical to the path the test computed.
    let root = tmp.path().canonicalize().expect("canonicalize");
    let home = root.join("home");
    let zdotdir = if s.custom_zdotdir {
        root.join("zd")
    } else {
        home.clone()
    };
    let target = home.join("a b é");
    fs::create_dir_all(&target).expect("target dir");
    // The session starts in a symlink to `home`; the shell must report the path it was given.
    let start = root.join("start-link");
    std::os::unix::fs::symlink(&home, &start).expect("symlink");
    let log = root.join("rc.log");
    write_user_files(&s, &home, &zdotdir);
    let hutan_home = root.join("hutan-home");
    let integration_dir = shell_integration::install(&hutan_home).expect("install");

    let xdg_original = root.join("xdg-original");
    let mut env: BTreeMap<String, String> = BTreeMap::from([
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("SHELL".into(), s.shell.to_string_lossy().into_owned()),
        ("HUTAN_TEST_LOG".into(), log.to_string_lossy().into_owned()),
        (
            "XDG_CONFIG_HOME".into(),
            home.join(".config").to_string_lossy().into_owned(),
        ),
        (
            "XDG_DATA_HOME".into(),
            home.join(".local/share").to_string_lossy().into_owned(),
        ),
        (
            "XDG_CACHE_HOME".into(),
            home.join(".cache").to_string_lossy().into_owned(),
        ),
        (
            "HISTFILE".into(),
            home.join(".hist").to_string_lossy().into_owned(),
        ),
    ]);
    if s.custom_zdotdir {
        env.insert("ZDOTDIR".into(), zdotdir.to_string_lossy().into_owned());
    }
    if s.explicit_xdg {
        env.insert(
            "XDG_DATA_DIRS".into(),
            format!("{}:/usr/share", xdg_original.display()),
        );
    }
    for (key, value) in &s.extra_env {
        env.insert((*key).to_owned(), (*value).to_owned());
    }
    let expected_zdotdir = env
        .get("ZDOTDIR")
        .cloned()
        .unwrap_or_else(|| inherited("ZDOTDIR"));
    let expected_xdg = env
        .get("XDG_DATA_DIRS")
        .cloned()
        .unwrap_or_else(|| inherited("XDG_DATA_DIRS"));

    let term = Terminal::spawn(TerminalConfig {
        argv: s.argv.clone(),
        cwd: Some(start.clone()),
        env,
        size: TerminalSize {
            cols: 400,
            rows: 40,
            pixel_width: 0,
            pixel_height: 0,
        },
        shell_integration: s.integration,
        shell_integration_dir: Some(integration_dir),
        ..TerminalConfig::default()
    })
    .expect("spawn shell");
    let collected = Collected::start(&term);

    wait_for(&term, "custom prompt", |screen| screen.contains(PROMPT)).await;
    let marking = s.integration && !s.extra_env.contains(&("HUTAN_SHELL_INTEGRATION", "0"));
    // zsh loads the integration on its first prompt, after a prompt framework's hook already
    // rebuilt PS1, so in that setup the first prompt carries no marks (documented limitation).
    let first_prompt_marked = !(s.prompt_framework && s.kind == Kind::Zsh);
    let prompts = |n: usize| n - usize::from(!first_prompt_marked);
    if marking && first_prompt_marked {
        collected.wait_prompts(1).await;
    }
    term.write_input(b"true\n").expect("write");
    if marking {
        collected.wait_prompts(prompts(2)).await;
    }
    term.write_input(b"false\n").expect("write");
    if marking {
        collected.wait_prompts(prompts(3)).await;
    }
    let cd = format!("cd '{}'\n", target.display());
    term.write_input(cd.as_bytes()).expect("write");
    if marking {
        collected.wait_prompts(prompts(4)).await;
    } else {
        wait_for(&term, "cd done", |screen| {
            screen.matches(PROMPT).count() >= 4
        })
        .await;
    }

    if marking {
        let marks = collected.marks();
        if s.prompt_framework && s.kind == Kind::Zsh {
            assert_eq!(marks, full_sequence()[2..], "marks: {marks:?}");
        } else {
            assert_eq!(marks, full_sequence(), "marks: {marks:?}");
        }
        assert_eq!(
            collected.cwds().first(),
            Some(&start),
            "cwds: {:?}",
            collected.cwds()
        );
        assert_eq!(
            collected.cwds().last(),
            Some(&target),
            "cwds: {:?}",
            collected.cwds()
        );
    } else {
        assert_eq!(
            collected.marks(),
            Vec::new(),
            "opted out: no marks expected"
        );
    }

    // What a child process of the shell sees.
    let version = if marking {
        shell_integration::VERSION
    } else {
        ""
    };
    let count = if s.kind == Kind::Fish || s.kind == Kind::Bash || s.kind == Kind::Zsh {
        "1"
    } else {
        ""
    };
    let probe = "sh -c 'echo \"ZD=[$ZDOTDIR] XDG=[$XDG_DATA_DIRS] VER=[$HUTAN_SHELL_INTEGRATION_VERSION] \
                 COUNT=[$HUTAN_TEST_COUNT] LEAK=[$HUTAN_ORIG_ZDOTDIR$HUTAN_ORIG_XDG_DATA_DIRS\
                 $HUTAN_XDG_DATA_DIRS_UNSET$HUTAN_BASH_INJECT$HUTAN_BASH_RCFILE]\"'\n";
    term.write_input(probe.as_bytes()).expect("write");
    let expected = format!(
        "ZD=[{expected_zdotdir}] XDG=[{expected_xdg}] VER=[{version}] COUNT=[{count}] LEAK=[]"
    );
    wait_for(&term, "child environment", |screen| {
        screen.contains(&expected)
    })
    .await;

    term.write_input(b"exit\n").expect("write");
    let _ = timeout(LIMIT, term.wait_exit())
        .await
        .expect("exit in time");

    let ran = fs::read_to_string(&log).unwrap_or_default();
    let ran: Vec<&str> = ran.lines().collect();
    assert_eq!(
        ran, s.expected_rc,
        "user startup files must run once, in order"
    );
}

fn have(path: &str) -> bool {
    Path::new(path).exists()
}

fn skip(what: &str) {
    eprintln!("skipping: {what} is not installed");
}

fn bash_candidates() -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let extra = std::env::var("HUTAN_TEST_BASH").ok();
    for path in ["/bin/bash", "/opt/homebrew/bin/bash", "/usr/local/bin/bash"]
        .into_iter()
        .map(str::to_owned)
        .chain(extra)
    {
        if let Ok(real) = fs::canonicalize(&path)
            && !found
                .iter()
                .any(|f| fs::canonicalize(f).is_ok_and(|c| c == real))
        {
            found.push(path);
        }
    }
    found
}

const FISH: &str = "/opt/homebrew/bin/fish";

fn fish_path() -> Option<&'static str> {
    [FISH, "/usr/local/bin/fish", "/usr/bin/fish"]
        .into_iter()
        .find(|p| have(p))
}

#[tokio::test]
async fn zsh_login_shell_with_custom_zdotdir() {
    if !have("/bin/zsh") {
        return skip("zsh");
    }
    let mut s = Scenario::new(
        Kind::Zsh,
        "/bin/zsh",
        &[],
        &["zshenv", "zprofile", "zshrc", "zlogin"],
    );
    s.custom_zdotdir = true;
    run(s).await;
}

#[tokio::test]
async fn zsh_login_shell_without_zdotdir() {
    if !have("/bin/zsh") {
        return skip("zsh");
    }
    run(Scenario::new(
        Kind::Zsh,
        "/bin/zsh",
        &[],
        &["zshenv", "zprofile", "zshrc", "zlogin"],
    ))
    .await;
}

#[tokio::test]
async fn zsh_explicit_program_with_prompt_framework() {
    if !have("/bin/zsh") {
        return skip("zsh");
    }
    let mut s = Scenario::new(Kind::Zsh, "/bin/zsh", &["/bin/zsh"], &["zshenv", "zshrc"]);
    s.prompt_framework = true;
    run(s).await;
}

#[tokio::test]
async fn bash_login_shell() {
    let bashes = bash_candidates();
    if bashes.is_empty() {
        return skip("bash");
    }
    for bash in bashes {
        eprintln!("bash login shell: {bash}");
        run(Scenario::new(
            Kind::Bash,
            &bash,
            &[],
            &["profile", "bashrc"],
        ))
        .await;
    }
}

#[tokio::test]
async fn bash_explicit_program_with_prompt_framework() {
    let bashes = bash_candidates();
    if bashes.is_empty() {
        return skip("bash");
    }
    for bash in bashes {
        eprintln!("bash explicit program: {bash}");
        let mut s = Scenario::new(Kind::Bash, &bash, &[&bash], &["bashrc"]);
        s.prompt_framework = true;
        run(s).await;
    }
}

#[tokio::test]
async fn fish_login_shell() {
    let Some(fish) = fish_path() else {
        return skip("fish");
    };
    let mut s = Scenario::new(Kind::Fish, fish, &[], &["config"]);
    s.explicit_xdg = true;
    run(s).await;
}

#[tokio::test]
async fn fish_keeps_an_unset_xdg_data_dirs_unset() {
    let Some(fish) = fish_path() else {
        return skip("fish");
    };
    run(Scenario::new(Kind::Fish, fish, &[fish], &["config"])).await;
}

#[tokio::test]
async fn opt_out_by_environment_leaves_shells_untouched() {
    if have("/bin/zsh") {
        let mut s = Scenario::new(
            Kind::Zsh,
            "/bin/zsh",
            &[],
            &["zshenv", "zprofile", "zshrc", "zlogin"],
        );
        s.custom_zdotdir = true;
        s.extra_env = vec![("HUTAN_SHELL_INTEGRATION", "0")];
        run(s).await;
    }
    for bash in bash_candidates() {
        let mut s = Scenario::new(Kind::Bash, &bash, &[], &["profile", "bashrc"]);
        s.extra_env = vec![("HUTAN_SHELL_INTEGRATION", "0")];
        run(s).await;
    }
}

#[tokio::test]
async fn opt_out_by_config_leaves_shells_untouched() {
    if have("/bin/zsh") {
        let mut s = Scenario::new(
            Kind::Zsh,
            "/bin/zsh",
            &[],
            &["zshenv", "zprofile", "zshrc", "zlogin"],
        );
        s.integration = false;
        run(s).await;
    }
    for bash in bash_candidates() {
        let mut s = Scenario::new(Kind::Bash, &bash, &[], &["profile", "bashrc"]);
        s.integration = false;
        run(s).await;
    }
}

#[tokio::test]
async fn other_programs_get_no_injection_env() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let integration_dir = shell_integration::install(tmp.path()).expect("install");
    let term = Terminal::spawn(TerminalConfig {
        argv: vec!["/usr/bin/env".to_owned()],
        env: BTreeMap::from([
            // Stale leftovers of an outer Hutan terminal must not reach the program either.
            ("HUTAN_SHELL_INTEGRATION_VERSION".into(), "stale".into()),
            ("HUTAN_ORIG_ZDOTDIR".into(), "/stale".into()),
        ]),
        size: TerminalSize {
            cols: 400,
            rows: 500,
            pixel_width: 0,
            pixel_height: 0,
        },
        shell_integration_dir: Some(integration_dir),
        ..TerminalConfig::default()
    })
    .expect("spawn env");
    let status = timeout(LIMIT, term.wait_exit())
        .await
        .expect("exit in time");
    assert!(status.success());
    let screen = text(&term.snapshot(&mut RenderTracker::new()));
    assert!(
        screen.contains("TERM_PROGRAM=hutan"),
        "env output:\n{screen}"
    );
    for forbidden in [
        "HUTAN_SHELL_INTEGRATION",
        "HUTAN_ORIG",
        "HUTAN_BASH",
        "HUTAN_XDG",
        "shell-integration",
    ] {
        assert!(!screen.contains(forbidden), "{forbidden} leaked:\n{screen}");
    }
}

// ---- agents' own OpenTelemetry: the shell functions (TEL-2, ADR 0026) ---------------------------

/// A stand-in `hutan` that answers `telemetry env <agent>` the way the real one does: lines of
/// `env NAME=value` and `arg VALUE`, or nothing (a conflict, an agent that is not enabled). It
/// logs what it was asked.
const FAKE_HUTAN: &str = r#"#!/bin/sh
echo "$*" >> "$OUT/hutan.log"
[ "$1 $2" = "telemetry env" ] || exit 0
[ -e "$OUT/quiet" ] && exit 0
case $3 in
  claude) printf 'env FAKE_A=1\nenv FAKE_B=x y z\n' ;;
  codex) printf 'env FAKE_C=tok\narg -c\narg otel.exporter={a="b c"}\n' ;;
esac
"#;

/// A stand-in agent that records the variables and the arguments it was started with.
const FAKE_AGENT: &str = r#"#!/bin/sh
name=$(basename "$0")
{
  printf 'A=[%s] B=[%s] C=[%s]\n' "$FAKE_A" "$FAKE_B" "$FAKE_C"
  for a in "$@"; do printf 'arg=[%s]\n' "$a"; done
} > "$OUT/$name.out"
echo "RAN-$name"
"#;

struct Launching {
    kind: Kind,
    shell: PathBuf,
    /// What the shell is started with (an explicit program: no login files, so `PATH` stays).
    argv: Vec<String>,
    /// The terminal has `HUTAN_OTLP_ENDPOINT` (the receiver listened when it started).
    otlp: bool,
    /// The user's own startup file defines a `codex` function.
    users_codex: bool,
}

fn executable(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    write(path, text);
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn appended(path: &Path, text: &str) {
    use std::io::Write as _;
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .expect("append");
}

async fn type_and_wait(term: &Terminal, line: &str, what: &str, seen: impl Fn(&str) -> bool) {
    term.write_input(format!("{line}\n").as_bytes())
        .expect("write");
    wait_for(term, what, seen).await;
}

async fn launching(l: Launching) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let home = root.join("home");
    let bin = root.join("bin");
    let out = root.join("out");
    fs::create_dir_all(&out).expect("out");
    executable(&bin.join("hutan"), FAKE_HUTAN);
    executable(&bin.join("claude"), FAKE_AGENT);
    executable(&bin.join("codex"), FAKE_AGENT);
    let s = Scenario::new(l.kind, &l.shell.to_string_lossy(), &[], &[]);
    write_user_files(&s, &home, &home);
    if l.users_codex {
        let (rc, function) = match l.kind {
            Kind::Zsh => (home.join(".zshrc"), "codex() { echo USERS-CODEX; }\n"),
            Kind::Bash => (home.join(".bashrc"), "codex() { echo USERS-CODEX; }\n"),
            Kind::Fish => (
                home.join(".config/fish/config.fish"),
                "function codex\n    echo USERS-CODEX\nend\n",
            ),
        };
        appended(&rc, function);
    }
    let hutan_home = root.join("hutan-home");
    let integration_dir = shell_integration::install(&hutan_home).expect("install");
    let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display());
    let mut env: BTreeMap<String, String> = BTreeMap::from([
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("SHELL".into(), l.shell.to_string_lossy().into_owned()),
        ("PATH".into(), path),
        ("OUT".into(), out.to_string_lossy().into_owned()),
        (
            "HUTAN_TEST_LOG".into(),
            root.join("rc.log").to_string_lossy().into_owned(),
        ),
        (
            "XDG_CONFIG_HOME".into(),
            home.join(".config").to_string_lossy().into_owned(),
        ),
        (
            "HISTFILE".into(),
            home.join(".hist").to_string_lossy().into_owned(),
        ),
        (
            "HUTAN_BIN".into(),
            bin.join("hutan").to_string_lossy().into_owned(),
        ),
    ]);
    if l.otlp {
        env.insert(
            "HUTAN_OTLP_ENDPOINT".into(),
            "http://127.0.0.1:50123/s/0123456789abcdef0123456789abcdef".into(),
        );
        env.insert(
            "HUTAN_OTLP_TOKEN".into(),
            "0123456789abcdef0123456789abcdef".into(),
        );
    }
    let term = Terminal::spawn(TerminalConfig {
        argv: l.argv.clone(),
        cwd: Some(home.clone()),
        env,
        size: TerminalSize {
            cols: 400,
            rows: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
        shell_integration: true,
        shell_integration_dir: Some(integration_dir),
        ..TerminalConfig::default()
    })
    .expect("spawn shell");
    wait_for(&term, "custom prompt", |screen| screen.contains(PROMPT)).await;

    let read = |name: &str| fs::read_to_string(out.join(name)).unwrap_or_default();
    let asked = || read("hutan.log");

    // `claude` with an argument that has a space: the variables are in front of that one launch.
    type_and_wait(&term, "claude --flag 'a b'", "claude ran", |screen| {
        screen.matches("RAN-claude").count() == 1
    })
    .await;
    if l.otlp {
        assert_eq!(
            read("claude.out"),
            "A=[1] B=[x y z] C=[]\narg=[--flag]\narg=[a b]\n"
        );
        assert_eq!(asked(), "telemetry env claude\n");
    } else {
        // No receiver when the terminal started: no function, the program as it is.
        assert_eq!(
            read("claude.out"),
            "A=[] B=[] C=[]\narg=[--flag]\narg=[a b]\n"
        );
        assert_eq!(asked(), "");
        return finish_shell(term).await;
    }
    // ...and the shell itself has none of them.
    type_and_wait(
        &term,
        "echo \"SHELL_A=[$FAKE_A]\"",
        "no variable left",
        |screen| screen.contains("SHELL_A=[]"),
    )
    .await;

    if l.users_codex {
        // A function of the user's is never replaced, and Hutan is not even asked.
        type_and_wait(&term, "codex exec 'x y'", "the user's codex", |screen| {
            screen.contains("USERS-CODEX")
        })
        .await;
        assert_eq!(asked(), "telemetry env claude\n");
        assert!(!out.join("codex.out").exists());
    } else {
        // Arguments in front of the user's, whole, in order.
        type_and_wait(&term, "codex exec 'x y'", "codex ran", |screen| {
            screen.matches("RAN-codex").count() == 1
        })
        .await;
        assert_eq!(
            read("codex.out"),
            "A=[] B=[] C=[tok]\narg=[-c]\narg=[otel.exporter={a=\"b c\"}]\narg=[exec]\narg=[x y]\n"
        );
        // Nothing from Hutan (a conflict, an agent not enabled): the agent as it is.
        fs::write(out.join("quiet"), "").expect("quiet");
        type_and_wait(&term, "claude again", "claude ran again", |screen| {
            screen.matches("RAN-claude").count() == 2
        })
        .await;
        assert_eq!(read("claude.out"), "A=[] B=[] C=[]\narg=[again]\n");
    }
    finish_shell(term).await;
}

async fn finish_shell(term: Terminal) {
    term.write_input(b"exit\n").expect("write");
    let _ = timeout(LIMIT, term.wait_exit())
        .await
        .expect("exit in time");
}

async fn each_shell(otlp: bool, users_codex: bool) {
    let mut ran = false;
    if have("/bin/zsh") {
        ran = true;
        launching(Launching {
            kind: Kind::Zsh,
            shell: PathBuf::from("/bin/zsh"),
            argv: vec!["/bin/zsh".into()],
            otlp,
            users_codex,
        })
        .await;
    } else {
        skip("zsh");
    }
    for bash in bash_candidates() {
        ran = true;
        eprintln!("bash: {bash}");
        launching(Launching {
            kind: Kind::Bash,
            shell: PathBuf::from(&bash),
            argv: vec![bash],
            otlp,
            users_codex,
        })
        .await;
    }
    if let Some(fish) = fish_path() {
        ran = true;
        launching(Launching {
            kind: Kind::Fish,
            shell: PathBuf::from(fish),
            argv: vec![fish.to_owned()],
            otlp,
            users_codex,
        })
        .await;
    } else {
        skip("fish");
    }
    if !ran {
        skip("any shell");
    }
}

#[tokio::test]
async fn the_agent_functions_put_what_hutan_says_in_front_of_that_one_launch() {
    each_shell(true, false).await;
}

#[tokio::test]
async fn a_function_of_the_users_own_is_never_replaced() {
    each_shell(true, true).await;
}

#[tokio::test]
async fn without_the_receiver_there_are_no_functions() {
    each_shell(false, false).await;
}
