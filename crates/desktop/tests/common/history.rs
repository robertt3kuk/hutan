//! Synthetic Claude Code / Codex / OMP transcripts in temporary roots (the shapes of the
//! `hutan-index` fixtures), a `daemon.toml` that points the index at them, and stand-in
//! agents for `PATH`. Nothing here ever touches the real `~/.claude`, `~/.codex` or `~/.omp`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::TempDir;

pub const CLAUDE_ID: &str = "5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60";
pub const CODEX_ID: &str = "0198aaaa-bbbb-cccc-dddd-eeeeffff0000";
pub const OMP_ID: &str = "0f4c7d52";
/// A Claude session whose id (it has an apostrophe) is refused by the resume-command rules, so
/// its hits carry no `resume_argv`.
pub const UNRESUMABLE_ID: &str = "bad'id";
/// A Claude session whose recorded directory no longer exists.
pub const GONE_ID: &str = "gone-dir-1";
pub const GONE_CWD: &str = "/nonexistent/hutan-test/vanished";
/// A Claude session in `project_a` that started five hours ago and was last used ten minutes
/// ago: first by "last updated", last by "created".
pub const LATE_ID: &str = "late-start-1";

pub struct HistoryEnv {
    _tmp: TempDir,
    /// The temporary directory, canonical (`/private/var/…`, as FSEvents reports it).
    pub base: PathBuf,
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub omp: PathBuf,
    /// Working directories the fixtures record (real directories, so Resume can start there).
    pub project_a: PathBuf,
    pub project_b: PathBuf,
    /// Stand-in agents (`claude`, `codex`, `omp`) live here.
    pub bin: PathBuf,
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// `2026-10-15T09:00:00.000Z` for Unix milliseconds.
pub fn iso(ms: u64) -> String {
    let secs = ms / 1000;
    let (year, month, day) = civil_from_days(i64::try_from(secs / 86_400).unwrap());
    let rem = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        ms % 1000
    )
}

pub fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

const HOUR: u64 = 3_600_000;
const DAY: u64 = 24 * HOUR;

impl HistoryEnv {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let env = Self {
            claude: base.join("sources/claude"),
            codex: base.join("sources/codex"),
            omp: base.join("sources/omp"),
            project_a: base.join("projects/app-a"),
            project_b: base.join("projects/app-b"),
            bin: base.join("bin"),
            base,
            _tmp: tmp,
        };
        for dir in [
            &env.claude,
            &env.codex,
            &env.omp,
            &env.project_a,
            &env.project_b,
            &env.bin,
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        env
    }

    /// A `daemon.toml` that points every source at the temporary roots.
    pub fn daemon_toml(&self, enabled: bool) -> String {
        format!(
            "[history]\nenabled = {enabled}\n[history.roots]\nclaude = \"{}\"\ncodex = \"{}\"\nomp = \"{}\"\n",
            self.claude.display(),
            self.codex.display(),
            self.omp.display()
        )
    }

    fn write(&self, path: &Path, lines: &[String]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, lines.join("\n") + "\n").unwrap();
    }

    /// Five sessions; the first three (Claude, Codex, OMP) have the word `hutantest` in a user
    /// message.
    ///
    /// - Claude, `project_a`, an hour old: several rows say `stall`
    /// - Codex, `project_b`, two hours old
    /// - OMP, `project_a`, from 2020 (outside every date filter but `all`)
    /// - Claude `bad'id`, `project_b`: `pangolin hutanfallback`, no resume command
    /// - Claude `gone-dir-1`: `zebrafish hutangone`, its directory does not exist
    pub fn write_fixtures(&self) {
        let now = now_ms();
        let a = self.project_a.display();
        let b = self.project_b.display();
        let (t1, t2) = (iso(now - HOUR), iso(now - HOUR + 5_000));
        self.write(
            &self.claude.join(format!("-app-a/{CLAUDE_ID}.jsonl")),
            &[
                format!(r#"{{"type":"user","timestamp":"{t1}","cwd":"{a}","sessionId":"{CLAUDE_ID}","message":{{"role":"user","content":[{{"type":"text","text":"why does the quokka scheduler stall under load hutantest"}}]}}}}"#),
                format!(r#"{{"type":"assistant","timestamp":"{t2}","cwd":"{a}","message":{{"role":"assistant","content":[{{"type":"text","text":"Checking the scheduler stall."}},{{"type":"tool_use","id":"t1","name":"Bash","input":{{"command":"grep -rn stall crates/scheduler/src"}}}}]}}}}"#),
                format!(r#"{{"type":"user","timestamp":"{t2}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"crates/scheduler/src/queue.rs:42: // stall when the queue is full"}}]}}}}"#),
                format!(r#"{{"type":"ai-title","aiTitle":"Scheduler stall investigation","sessionId":"{CLAUDE_ID}"}}"#),
            ],
        );
        let t = iso(now - 2 * HOUR);
        self.write(
            &self.codex.join(format!(
                "2026/10/15/rollout-2026-10-15T10-00-00-{CODEX_ID}.jsonl"
            )),
            &[
                format!(r#"{{"timestamp":"{t}","type":"session_meta","payload":{{"id":"{CODEX_ID}","cwd":"{b}"}}}}"#),
                format!(r#"{{"timestamp":"{t}","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"add retry with jitter to the capybara client hutantest"}}]}}}}"#),
                format!(r#"{{"timestamp":"{t}","type":"response_item","payload":{{"type":"function_call","name":"shell","arguments":"{{\"command\":[\"bash\",\"-lc\",\"cargo test -p capybara\"]}}","call_id":"c1"}}}}"#),
                format!(r#"{{"timestamp":"{t}","type":"response_item","payload":{{"type":"function_call_output","call_id":"c1","output":"test result: ok. 4 passed"}}}}"#),
            ],
        );
        let old = iso(now - 400 * DAY);
        self.write(
            &self.omp.join(format!("-app-a/2020-01-02T11-00-00-000Z_{OMP_ID}.jsonl")),
            &[
                format!(r#"{{"type":"title","v":1,"title":"Axolotl cache design","updatedAt":"{old}","pad":"      "}}"#),
                format!(r#"{{"type":"session","version":3,"id":"{OMP_ID}","timestamp":"{old}","cwd":"{a}"}}"#),
                format!(r#"{{"type":"message","timestamp":"{old}","message":{{"role":"user","content":[{{"type":"text","text":"sketch the axolotl eviction policy hutantest"}}]}}}}"#),
                format!(r#"{{"type":"message","timestamp":"{old}","message":{{"role":"assistant","content":[{{"type":"text","text":"Use LRU with a size cap."}}]}}}}"#),
            ],
        );
        let t = iso(now - 3 * HOUR);
        self.write(
            &self.claude.join(format!("-app-b/{UNRESUMABLE_ID}.jsonl")),
            &[format!(
                r#"{{"type":"user","timestamp":"{t}","cwd":"{b}","message":{{"role":"user","content":[{{"type":"text","text":"remember the pangolin plan hutanfallback"}}]}}}}"#
            )],
        );
        let t = iso(now - 4 * HOUR);
        self.write(
            &self.claude.join(format!("-gone/{GONE_ID}.jsonl")),
            &[format!(
                r#"{{"type":"user","timestamp":"{t}","cwd":"{GONE_CWD}","message":{{"role":"user","content":[{{"type":"text","text":"count the zebrafish hutangone"}}]}}}}"#
            )],
        );
    }

    /// The session [`LATE_ID`]: `late bloomer pelican` five hours ago, the answer ten minutes ago.
    pub fn write_late_session(&self) {
        let now = now_ms();
        let a = self.project_a.display();
        let (t1, t2) = (iso(now - 5 * HOUR), iso(now - 10 * 60_000));
        self.write(
            &self.claude.join(format!("-app-a/{LATE_ID}.jsonl")),
            &[
                format!(r#"{{"type":"user","timestamp":"{t1}","cwd":"{a}","sessionId":"{LATE_ID}","message":{{"role":"user","content":[{{"type":"text","text":"late bloomer pelican"}}]}}}}"#),
                format!(r#"{{"type":"assistant","timestamp":"{t2}","cwd":"{a}","message":{{"role":"assistant","model":"claude-test-model","content":[{{"type":"text","text":"Pelicans bloom late."}}]}}}}"#),
            ],
        );
    }

    /// `count` small Codex sessions in `project_b` (`filler-0` …), one minute apart, newest
    /// first; used to make the list scroll.
    pub fn write_filler_sessions(&self, count: usize) {
        let now = now_ms();
        let b = self.project_b.display();
        for n in 0..count {
            let id = format!("0198bbbb-0000-0000-0000-{n:012}");
            let t = iso(now - 6 * HOUR - (n as u64) * 60_000);
            self.write(
                &self.codex.join(format!("2026/10/14/rollout-2026-10-14T01-00-{n:02}-{id}.jsonl")),
                &[
                    format!(r#"{{"timestamp":"{t}","type":"session_meta","payload":{{"id":"{id}","cwd":"{b}"}}}}"#),
                    format!(r#"{{"timestamp":"{t}","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"filler session number {n}"}}]}}}}"#),
                ],
            );
        }
    }

    /// Scripts named `claude`, `codex` and `omp` in [`Self::bin`] that say how they were started
    /// and then wait; put that directory first on `PATH` before the daemon starts so a resumed
    /// session never runs the real agent.
    pub fn install_stand_ins(&self) {
        for name in ["claude", "codex", "omp"] {
            let script = self.bin.join(name);
            fs::write(
                &script,
                format!("#!/bin/sh\necho \"FAKE-{name} $* IN $(pwd)\"\nexec cat\n"),
            )
            .unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
            settle(&script);
        }
    }

    /// A stand-in login shell that puts `bin` on `PATH`, like a user's shell configuration does,
    /// and prints its environment as `<shell> -l -c '/usr/bin/env -0'` is asked to; returns its
    /// path (for the `terminal.shell` setting, so Resume finds the stand-in agents the way real
    /// ones are found).
    pub fn install_login_shell(&self) -> PathBuf {
        let shell = self.base.join("login-shell");
        fs::write(
            &shell,
            format!(
                "#!/bin/sh\nexport PATH=\"{}:$PATH\"\nexec /usr/bin/env -0\n",
                self.bin.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).unwrap();
        settle(&shell);
        shell
    }
}

/// Runs a freshly written script once and waits for it. macOS vets the first execution of a new
/// executable, which can take tens of seconds on a loaded machine; doing that here keeps it out
/// of the timeouts of what the test is about.
fn settle(path: &std::path::Path) {
    let _ = std::process::Command::new(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}
