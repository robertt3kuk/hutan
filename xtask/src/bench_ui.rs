//! Benchmarks of the desktop's heavy surfaces (B3 with panels, B7–B10 of `docs/perf.md`).
//!
//! Each scenario seeds a daemon in a temporary `HUTAN_HOME` (projects as real `git init`
//! folders, terminals, chats played by `xtask fake-codex`: no model, no network), starts the
//! release desktop on a seeded layout with `HUTAN_BENCH_UI` naming what the window does by
//! itself (`hutan_desktop::bench_ui`), and reads the frame and operation timings it traces
//! (`hutan_desktop::frames`).

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use hutan_core::{AgentEventKind, SessionId};
use hutan_protocol::messages::{
    CreatedBy, Request, SessionCreate, SessionInfo, SessionResult, SessionSpec, TerminalSpec,
    WorkspaceCreate, WorkspaceInfo, WorkspaceOpen, WorkspaceOpened,
};
use serde_json::{Value, json};
use tokio::time::sleep;

use crate::bench::{
    DISPLAY_STALLED, MIB, Metrics, REFRESH_INTERVAL_MS, Stack, display_refresh_interval_ms,
    is_event, window_active,
};
use crate::proc;
use crate::seed;
use crate::stats::percentile;

/// Journal events of the B7 chat: 9 per unit of `fake-codex`'s `bench:fill`.
pub const TRANSCRIPT_UNITS: usize = 560;
/// Events a unit of `bench:fill` writes to the journal.
const EVENTS_PER_UNIT: u64 = 9;

/// The daemon configuration of the scenarios with chats: `[codex]` is this very executable
/// playing `fake-codex`, and the harnesses the user has installed are never looked for.
pub fn daemon_toml() -> Result<String> {
    let exe = std::env::current_exe().context("finding the xtask executable")?;
    let nowhere = "/nonexistent/hutan-bench";
    Ok(format!(
        "[codex]\nbin = {exe:?}\nargs = [\"fake-codex\"]\n\n\
         [harness.omp]\nbin = \"{nowhere}/omp\"\ncli = \"{nowhere}/omp\"\n\n\
         [harness.claude]\nbin = \"{nowhere}/claude\"\ncli = \"{nowhere}/claude\"\n\n\
         [terminal]\nenv_inherit = \"daemon\"\n",
    ))
}

/// A stack whose daemon plays chats with `fake-codex`.
pub async fn start_stack(bins: &crate::bench::Bins) -> Result<Stack<'_>> {
    Stack::start_with(bins, Some(&daemon_toml()?)).await
}

// ---- seeding -------------------------------------------------------------------------------

/// A project folder of the scenario: a real git repository so the sidebar shows what it shows
/// for the user's repositories.
pub struct Project {
    pub info: WorkspaceInfo,
    pub dir: PathBuf,
}

/// Makes `count` repositories under the stack's home and opens each as a project.
pub async fn open_projects(stack: &Stack<'_>, count: usize) -> Result<Vec<Project>> {
    let root = stack.path("projects");
    std::fs::create_dir_all(&root)?;
    let mut projects = Vec::with_capacity(count);
    for n in 0..count {
        let dir = root.join(format!("proj-{n:02}"));
        std::fs::create_dir_all(&dir)?;
        let status = Command::new("git")
            .args(["init", "-q"])
            .arg(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .stdin(Stdio::null())
            .status()
            .context("running git init")?;
        if !status.success() {
            bail!("git init {} failed", dir.display());
        }
        let opened: WorkspaceOpened = stack
            .client
            .call(Request::WorkspaceOpen(WorkspaceOpen {
                path: dir.clone(),
                created_by: Some(CreatedBy::User),
            }))
            .await?;
        projects.push(Project {
            info: opened.workspace,
            dir,
        });
    }
    Ok(projects)
}

/// Groups projects into user workspaces: `groups[i]` lists indexes into `projects`.
pub async fn make_workspaces(
    stack: &Stack<'_>,
    projects: &[Project],
    groups: &[Vec<usize>],
) -> Result<Vec<WorkspaceInfo>> {
    let mut made = Vec::new();
    for (n, group) in groups.iter().enumerate() {
        made.push(
            stack
                .client
                .workspace_create(WorkspaceCreate {
                    name: format!("Workspace {}", n + 1),
                    paths: Vec::new(),
                    project_ids: group.iter().map(|&i| projects[i].info.id).collect(),
                    created_by: Some(CreatedBy::User),
                })
                .await?,
        );
    }
    Ok(made)
}

/// A terminal session in `project` that prints a few lines and then waits like a shell.
pub async fn terminal(stack: &Stack<'_>, project: &Project, title: &str) -> Result<SessionInfo> {
    let script = format!("printf '{title}\\nready\\n$ '; exec cat");
    let result: SessionResult = stack
        .client
        .call(Request::SessionCreate(SessionCreate {
            workspace_id: Some(project.info.id),
            spec: SessionSpec::Terminal(TerminalSpec {
                command: vec!["sh".into(), "-c".into(), script],
                cwd: Some(project.dir.clone()),
                title: Some(title.to_owned()),
                ..TerminalSpec::default()
            }),
        }))
        .await?;
    Ok(result.session)
}

/// A chat session (`xtask fake-codex` behind it) in `project`.
pub async fn chat(stack: &Stack<'_>, project: &Project, title: &str) -> Result<SessionInfo> {
    Ok(stack
        .client
        .chat_create(hutan_protocol::chat::ChatCreate {
            contain: None,
            workspace_id: Some(project.info.id),
            cwd: project.dir.clone(),
            agent: "codex".to_owned(),
            model: None,
            approval_policy: None,
            sandbox: None,
            title: Some(title.to_owned()),
            mode: None,
            provider_id: None,
            system_prompt: None,
            account: None,
            safety: None,
        })
        .await?)
}

/// Sends `text` to a chat and waits until its turn ended.
pub async fn send_and_wait(stack: &Stack<'_>, session: SessionId, text: &str) -> Result<()> {
    let before = stack.client.chat_history(session, None, 1).await?.last_seq;
    stack.client.chat_send(session, text).await?;
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let page = stack.client.chat_history(session, None, 8).await?;
        let ended = page.last_seq > before
            && page.events.iter().any(|e| {
                e.seq > before
                    && matches!(
                        e.kind,
                        AgentEventKind::TurnCompleted { .. } | AgentEventKind::TurnCancelled
                    )
            });
        if ended {
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!("the turn of {session} did not end");
        }
        sleep(Duration::from_millis(100)).await;
    }
}

/// How many journal events a chat has.
async fn journal_events(stack: &Stack<'_>, session: SessionId) -> Result<u64> {
    let mut total = 0u64;
    let mut before = None;
    loop {
        let page = stack.client.chat_history(session, before, 1000).await?;
        total += page.events.len() as u64;
        match page.events.first() {
            Some(first) if page.has_more => before = Some(first.seq),
            _ => return Ok(total),
        }
    }
}

// ---- reading the trace -----------------------------------------------------------------

/// `(start, end)` wall-clock microseconds of the phase `name`.
fn phase(events: &[Value], name: &str) -> Option<(u64, u64)> {
    let at = |state: &str| {
        events
            .iter()
            .find(|e| is_event(e, "phase") && e["name"] == name && e["state"] == state)
            .and_then(|e| e["t_us"].as_u64())
    };
    Some((at("start")?, at("end")?))
}

fn within(event: &Value, window: (u64, u64)) -> bool {
    event["t_us"]
        .as_u64()
        .is_some_and(|t| window.0 <= t && t <= window.1)
}

/// The values of `field` (µs, as ms) of the events called `ev` inside `windows`.
fn series(events: &[Value], ev: &str, field: &str, windows: &[(u64, u64)]) -> Vec<f64> {
    events
        .iter()
        .filter(|e| is_event(e, ev))
        .filter(|e| windows.iter().any(|w| within(e, *w)))
        .filter_map(|e| e[field].as_f64())
        .map(|us| us / 1000.0)
        .collect()
}

/// Frame intervals inside `windows`, without the first frame of each: its interval runs back
/// to the last frame before the phase, whatever the window did in between.
fn intervals(events: &[Value], windows: &[(u64, u64)]) -> Vec<f64> {
    windows
        .iter()
        .flat_map(|w| {
            series(events, "frame", "interval_us", &[*w])
                .into_iter()
                .skip(1)
        })
        .filter(|ms| *ms > 0.0)
        .collect()
}

/// `kind` operations: `(total, hutan)` in ms, hutan being the handlers plus the frame's own work
/// (what B4a calls the Hutan path: no wait for the display).
fn ops(events: &[Value], kind: &str, windows: &[(u64, u64)]) -> Vec<(f64, f64)> {
    events
        .iter()
        .filter(|e| is_event(e, "op") && e["kind"] == kind)
        .filter(|e| {
            windows
                .iter()
                .any(|w| e["start_us"].as_u64().is_some_and(|t| w.0 <= t && t <= w.1))
        })
        .filter_map(|e| {
            let us = |f: &str| e[f].as_f64();
            Some((
                us("total_us")? / 1000.0,
                (us("handle_us")? + us("work_us")?) / 1000.0,
            ))
        })
        .collect()
}

fn put_stats(metrics: &mut Metrics, prefix: &str, values: &[f64]) {
    for (suffix, p) in [("p50", 50.0), ("p95", 95.0), ("p99", 99.0), ("max", 100.0)] {
        if let Some(v) = percentile(values, p) {
            metrics.insert(format!("{prefix}_{suffix}_ms"), v);
        }
    }
    metrics.insert(format!("{prefix}_n"), values.len() as f64);
}

/// The driver waits at most this long for the next frame (`bench_ui::Driver::frame`): a phase
/// that draws continuously and whose median frame interval reaches it was not being presented.
const FRAME_WAIT_CAP_MS: f64 = 90.0;

/// The display facts every UI scenario records, and whether the run must be repeated: the
/// window was not active, or, for a phase that draws continuously, not presented at all (slow
/// frames are a result, not a stall: they stay far below the driver's wait for a frame).
fn display(metrics: &mut Metrics, events: &[Value], continuous: &[f64]) -> Result<()> {
    let interval_ms = display_refresh_interval_ms(events)?;
    metrics.insert(REFRESH_INTERVAL_MS.into(), interval_ms);
    metrics.insert("b4_refresh_hz".into(), 1000.0 / interval_ms);
    let active = window_active(events);
    metrics.insert("b4_window_active".into(), active);
    // Covered at any moment between the start and the end of the scenario: frames stop.
    let hidden = events
        .iter()
        .filter(|e| is_event(e, "visibility") && e["visible"] == false)
        .count()
        + events
            .iter()
            .filter(|e| {
                (is_event(e, "bench_start") || is_event(e, "bench_done")) && e["visible"] == false
            })
            .count();
    metrics.insert("b4_window_hidden_events".into(), hidden as f64);
    let stalled = hidden > 0
        || active != 1.0
        || percentile(continuous, 50.0).is_some_and(|p50| p50 >= FRAME_WAIT_CAP_MS);
    if stalled {
        eprintln!(
            "window active: {active}; hidden {hidden} time(s); median continuous frame interval {:?} ms ({} frames)",
            percentile(continuous, 50.0),
            continuous.len()
        );
    }
    metrics.insert(DISPLAY_STALLED.into(), f64::from(u8::from(stalled)));
    Ok(())
}

const DONE_PATIENCE: Duration = Duration::from_secs(180);

async fn wait_done(stack: &mut Stack<'_>) -> Result<()> {
    stack
        .wait_trace("bench_done", DONE_PATIENCE, |e| is_event(e, "bench_done"))
        .await?;
    // The last frames of the final phase are traced right after.
    sleep(Duration::from_millis(500)).await;
    stack.check_desktop()
}

/// Waits until the desktop is connected, then a little longer, and reads its memory.
async fn settled_rss(stack: &mut Stack<'_>) -> Result<f64> {
    stack
        .wait_trace("the desktop connecting", Duration::from_secs(60), |e| {
            e["ev"] == "startup" && e["stage"] == "connected"
        })
        .await?;
    sleep(Duration::from_millis(1200)).await;
    let pid = stack.desktop()?.pid();
    Ok(proc::usage(pid)?.rss_bytes as f64 / MIB)
}

// ---- B3 with panels ------------------------------------------------------------------------

/// Layout of B3p: the sessions panel left, Agents right, Resources at the bottom (all shown) and
/// a terminal next to the Tasks panel in the center.
fn panels_layout(terminal: &SessionInfo) -> Value {
    seed::custom_layout(
        seed::dock("sessions", 260.0, true),
        seed::dock("agents", 300.0, true),
        seed::dock("resources", 200.0, true),
        seed::split(
            "horizontal",
            vec![
                seed::pane_of(1, vec![seed::terminal_item(&terminal.id.to_string())], 0),
                seed::pane_of(2, vec![seed::panel_item("tasks")], 0),
            ],
        ),
        2,
    )
}

/// B3p: 20 projects, 3 workspaces, 60 sessions, a few tasks; the sidebar and the Tasks,
/// Resources and Agents panels open; 30 s of idling.
pub async fn idle_panels(stack: &mut Stack<'_>) -> Result<Metrics> {
    let projects = open_projects(stack, 20).await?;
    make_workspaces(
        stack,
        &projects,
        &[vec![0, 1, 2, 3], vec![4, 5, 6, 7], vec![8, 9, 10, 11]],
    )
    .await?;
    let mut sessions = Vec::new();
    for n in 0..56 {
        let project = &projects[n % projects.len()];
        sessions.push(terminal(stack, project, &format!("term-{n:02}")).await?);
    }
    for (n, project) in projects.iter().take(4).enumerate() {
        chat(stack, project, &format!("agent-{n}")).await?;
    }
    let mut previous = None;
    for n in 0..6 {
        let task = stack
            .client
            .task_create(hutan_protocol::task::TaskCreate {
                workspace_id: projects[0].info.id,
                title: format!("Task {n}: tighten the retry path"),
                prompt: None,
                depends_on: previous.into_iter().collect(),
                placement: None,
                run: None,
                auto_start: false,
                after_policy: hutan_core::AfterPolicy::Done,
            })
            .await?;
        previous = Some(task.id);
    }
    sleep(Duration::from_secs(2)).await;
    let first = sessions.first().context("no sessions")?;
    // `HUTAN_BENCH_IDLE_FRAMES=1` traces the frames of the window while it idles (diagnosis:
    // the trace costs a little, so the number is not B3p's).
    let frames = std::env::var_os("HUTAN_BENCH_IDLE_FRAMES").is_some();
    let scenario = json!({ "scenario": "idle", "start_delay_ms": 12_000, "seconds": 20 });
    stack.launch_layout(
        &panels_layout(first),
        frames,
        None,
        frames.then_some(&scenario),
    )?;
    let started = Instant::now();
    while stack.clients().await? < 2 {
        stack.check_desktop()?;
        if started.elapsed() > Duration::from_secs(30) {
            bail!("the desktop never connected");
        }
        sleep(Duration::from_millis(100)).await;
    }
    sleep(crate::bench::SETTLE).await;
    stack.check_desktop()?;
    let mut metrics = Metrics::new();
    crate::bench::idle_cpu(stack, "b3p", &mut metrics).await?;
    // A visible window is woken by the display link ~20 times a second even when nothing
    // changes (docs/perf.md, B3); an occluded one, or one on a sleeping display, is not, and
    // measures nothing.
    let wakeups = metrics
        .get("b3p_desktop_wakeups_per_s")
        .copied()
        .unwrap_or(0.0);
    metrics.insert(DISPLAY_STALLED.into(), f64::from(u8::from(wakeups < 5.0)));
    Ok(metrics)
}

// ---- B7 ------------------------------------------------------------------------------------

const SCROLL_FRAMES: u32 = 700;
const SCROLL_STEP_PX: f32 = 400.0;

/// B7: a chat with 5 000 journal events is opened in a window that shows a terminal, then
/// scrolled to the top of the journal (which pages every earlier event in) and back.
pub async fn transcript(stack: &mut Stack<'_>) -> Result<Metrics> {
    let projects = open_projects(stack, 1).await?;
    let project = &projects[0];
    let term = terminal(stack, project, "term-0").await?;
    let chat = chat(stack, project, "journal").await?;
    send_and_wait(stack, chat.id, &format!("bench:fill {TRANSCRIPT_UNITS}")).await?;
    let events = journal_events(stack, chat.id).await?;
    if events < TRANSCRIPT_UNITS as u64 * EVENTS_PER_UNIT {
        bail!("the journal has {events} events, expected at least 5000");
    }
    let layout = seed::custom_layout(
        seed::dock("sessions", 240.0, true),
        seed::dock("inspector", 300.0, true),
        seed::hidden_dock("resources", 200.0),
        seed::pane_of(1, vec![seed::terminal_item(&term.id.to_string())], 0),
        1,
    );
    let scenario = json!({
        "scenario": "transcript",
        "session": chat.id.to_string(),
        "start_delay_ms": 3000,
        "frames": SCROLL_FRAMES,
        "step_px": SCROLL_STEP_PX,
    });
    stack.launch_layout(&layout, true, None, Some(&scenario))?;
    let before = settled_rss(stack).await?;
    stack
        .wait_trace("bench_start", DONE_PATIENCE, |e| is_event(e, "bench_start"))
        .await?;
    // `--sample`: ten seconds of the scroll, after the open.
    sleep(Duration::from_secs(2)).await;
    let desktop = stack.desktop()?.pid();
    let sampled = stack.sample(desktop, 10, "transcript-desktop");
    wait_done(stack).await?;
    if let Some(mut child) = sampled {
        let _ = child.wait();
    }
    let after = proc::usage(stack.desktop()?.pid())?.rss_bytes as f64 / MIB;

    let trace = stack.trace();
    let open = phase(&trace, "open").context("no open phase")?;
    let scroll: Vec<(u64, u64)> = ["scroll_up", "scroll_down"]
        .iter()
        .filter_map(|name| phase(&trace, name))
        .collect();
    let mut metrics = Metrics::new();
    let opened = ops(&trace, "open", &[open]);
    let Some(&(total, hutan)) = opened.first() else {
        bail!("the chat was never painted");
    };
    metrics.insert("b7a_open_ms".into(), total);
    metrics.insert("b7a_open_hutan_ms".into(), hutan);
    for field in ["handle_us", "wait_us", "work_us"] {
        if let Some(v) = series(&trace, "op", field, &[open]).first() {
            metrics.insert(format!("b7a_open_{}_ms", field.trim_end_matches("_us")), *v);
        }
    }
    let frames = intervals(&trace, &scroll);
    put_stats(&mut metrics, "b7b_scroll_frame", &frames);
    metrics.insert(
        "b7b_scroll_frame_p95_ms".into(),
        percentile(&frames, 95.0).context("no scroll frames")?,
    );
    put_stats(
        &mut metrics,
        "b7b_scroll_work",
        &series(&trace, "frame", "work_us", &scroll),
    );
    metrics.insert("b7c_rss_before_mb".into(), before);
    metrics.insert("b7c_rss_after_mb".into(), after);
    metrics.insert("b7c_rss_growth_mb".into(), after - before);
    metrics.insert("b7_journal_events".into(), events as f64);
    if let Some(loaded) = trace.iter().find(|e| is_event(e, "chat_loaded")) {
        metrics.insert(
            "b7_entries_loaded".into(),
            loaded["entries"].as_f64().unwrap_or(0.0),
        );
        metrics.insert(
            "b7_earlier_left".into(),
            f64::from(u8::from(loaded["has_earlier"] == true)),
        );
    }
    display(&mut metrics, &trace, &frames)?;
    Ok(metrics)
}

// ---- B8 ------------------------------------------------------------------------------------

const STREAM_RATE: u64 = 200;
const STREAM_SECONDS: u64 = 24;
const TYPE_SECONDS: u64 = 20;
const TYPE_INTERVAL_MS: u64 = 100;

/// B8: a reply streams into the open chat at 200 deltas/s while 200 keys are typed into its
/// composer over 20 s.
pub async fn streaming(stack: &mut Stack<'_>) -> Result<Metrics> {
    let projects = open_projects(stack, 1).await?;
    let chat = chat(stack, &projects[0], "stream").await?;
    // A little history above the stream, as in a real conversation.
    send_and_wait(stack, chat.id, "bench:fill 20").await?;
    let layout = seed::custom_layout(
        seed::dock("sessions", 240.0, true),
        seed::dock("inspector", 300.0, true),
        seed::hidden_dock("resources", 200.0),
        seed::pane_of(1, vec![seed::chat_item(&chat.id.to_string())], 0),
        1,
    );
    let keys = (TYPE_SECONDS * 1000 / TYPE_INTERVAL_MS) as usize;
    let scenario = json!({
        "scenario": "stream",
        "start_delay_ms": 2000,
        "interval_ms": TYPE_INTERVAL_MS,
        "jitter_ms": 17,
        "keys": keys,
    });
    stack.launch_layout(&layout, true, None, Some(&scenario))?;
    stack
        .wait_trace("bench_ready", Duration::from_secs(60), |e| {
            is_event(e, "bench_ready")
        })
        .await?;
    stack
        .client
        .chat_send(
            chat.id,
            format!("bench:stream {STREAM_RATE} {STREAM_SECONDS}"),
        )
        .await?;
    wait_done(stack).await?;

    let trace = stack.trace();
    let typing = phase(&trace, "type").context("no type phase")?;
    let mut metrics = Metrics::new();
    let frames = intervals(&trace, &[typing]);
    put_stats(&mut metrics, "b8a_frame", &frames);
    metrics.insert(
        "b8a_frame_p95_ms".into(),
        percentile(&frames, 95.0).context("no frames while streaming")?,
    );
    metrics.insert(
        "b8b_frame_max_ms".into(),
        percentile(&frames, 100.0).unwrap_or(0.0),
    );
    put_stats(
        &mut metrics,
        "b8a_work",
        &series(&trace, "frame", "work_us", &[typing]),
    );
    let keys = ops(&trace, "key", &[typing]);
    if keys.len() < 150 {
        bail!("only {} of the typed keys were painted", keys.len());
    }
    put_stats(
        &mut metrics,
        "b8c_key",
        &keys.iter().map(|k| k.1).collect::<Vec<_>>(),
    );
    put_stats(
        &mut metrics,
        "b8c_key_total",
        &keys.iter().map(|k| k.0).collect::<Vec<_>>(),
    );
    display(&mut metrics, &trace, &frames)?;
    Ok(metrics)
}

// ---- B9 ------------------------------------------------------------------------------------

const SIDEBAR_PROJECTS: usize = 40;
const SIDEBAR_SESSIONS: usize = 300;
const FILTER_KEYS: &str = "term-1";
const FILTER_INTERVAL_MS: u64 = 120;
const DRAG_FRAMES: u32 = 90;

/// B9: 300 sessions in 40 projects (nine of them in three workspaces); a filter is typed and
/// erased, then a project is dragged onto another workspace.
pub async fn sidebar(stack: &mut Stack<'_>) -> Result<Metrics> {
    let projects = open_projects(stack, SIDEBAR_PROJECTS).await?;
    let workspaces = make_workspaces(
        stack,
        &projects,
        &[vec![0, 1, 2], vec![3, 4, 5], vec![6, 7, 8]],
    )
    .await?;
    let mut first = None;
    for n in 0..SIDEBAR_SESSIONS {
        // The projects of the workspaces stay light (one session each) so that the rows the
        // drag needs are in view; the other projects share the rest.
        let index = if n < 9 {
            n
        } else {
            9 + (n - 9) % (SIDEBAR_PROJECTS - 9)
        };
        let session = terminal(stack, &projects[index], &format!("term-{n:03}")).await?;
        first.get_or_insert(session);
    }
    let first = first.context("no sessions")?;
    sleep(Duration::from_secs(2)).await;
    let layout = seed::custom_layout(
        seed::dock("sessions", 300.0, true),
        seed::dock("inspector", 300.0, true),
        seed::hidden_dock("resources", 200.0),
        seed::pane_of(1, vec![seed::terminal_item(&first.id.to_string())], 0),
        1,
    );
    let filter: Vec<String> = FILTER_KEYS
        .chars()
        .map(|c| {
            if c == '-' {
                "-".to_owned()
            } else {
                c.to_string()
            }
        })
        .collect();
    let scenario = json!({
        "scenario": "sidebar",
        "start_delay_ms": 3000,
        "filter": filter,
        "interval_ms": FILTER_INTERVAL_MS,
        "jitter_ms": 17,
        "project": projects[0].info.id.to_string(),
        "workspace": workspaces[1].id.to_string(),
        "frames": DRAG_FRAMES,
    });
    stack.launch_layout(&layout, true, None, Some(&scenario))?;
    stack
        .wait_trace("bench_start", DONE_PATIENCE, |e| is_event(e, "bench_start"))
        .await?;
    let desktop = stack.desktop()?.pid();
    let sampled = stack.sample(desktop, 10, "sidebar-desktop");
    wait_done(stack).await?;
    if let Some(mut child) = sampled {
        let _ = child.wait();
    }

    let trace = stack.trace();
    let filtering = phase(&trace, "filter").context("no filter phase")?;
    let dragging = phase(&trace, "drag").context("no drag phase")?;
    let mut metrics = Metrics::new();
    let keys = ops(&trace, "key", &[filtering]);
    if keys.len() < FILTER_KEYS.len() * 2 - 2 {
        bail!("only {} filter keys were painted", keys.len());
    }
    put_stats(
        &mut metrics,
        "b9a_key",
        &keys.iter().map(|k| k.1).collect::<Vec<_>>(),
    );
    put_stats(
        &mut metrics,
        "b9a_key_total",
        &keys.iter().map(|k| k.0).collect::<Vec<_>>(),
    );
    let frames = intervals(&trace, &[dragging]);
    put_stats(&mut metrics, "b9b_frame", &frames);
    put_stats(
        &mut metrics,
        "b9b_work",
        &series(&trace, "frame", "work_us", &[dragging]),
    );
    metrics.insert(
        "b9b_frame_p95_ms".into(),
        percentile(&frames, 95.0).context("no drag frames")?,
    );
    display(&mut metrics, &trace, &frames)?;
    Ok(metrics)
}

// ---- B10 -----------------------------------------------------------------------------------

const SWITCHES: usize = 90;

/// B10: ten terminals and five chats; the first nine are tabs of one pane (⌘1…⌘9), the other six
/// open from ⌘P.
pub async fn switching(stack: &mut Stack<'_>) -> Result<Metrics> {
    let projects = open_projects(stack, 3).await?;
    // Terminals and chats interleave so that the tabs mix kinds: tab = index + 1.
    let mut items: Vec<(String, bool, SessionId)> = Vec::new();
    let (mut terminals, mut chats) = (0usize, 0usize);
    for n in 0..15 {
        let project = &projects[n % projects.len()];
        if n % 3 == 2 {
            let title = format!("chat-{chats}");
            let session = self::chat(stack, project, &title).await?;
            send_and_wait(stack, session.id, "bench:fill 20").await?;
            items.push((title, true, session.id));
            chats += 1;
        } else {
            let title = format!("term-{terminals:02}");
            let session = terminal(stack, project, &title).await?;
            items.push((title, false, session.id));
            terminals += 1;
        }
    }
    let tabs: Vec<Value> = items
        .iter()
        .take(9)
        .map(|(_, is_chat, id)| {
            if *is_chat {
                seed::chat_item(&id.to_string())
            } else {
                seed::terminal_item(&id.to_string())
            }
        })
        .collect();
    let layout = seed::custom_layout(
        seed::dock("sessions", 240.0, true),
        seed::dock("inspector", 300.0, true),
        seed::hidden_dock("resources", 200.0),
        seed::pane_of(1, tabs, 0),
        1,
    );
    // A fixed pseudo-random walk: mostly tabs, every sixth step a jump to a session that is
    // not a tab yet or that was opened by an earlier jump.
    let mut steps = Vec::new();
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move |bound: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % bound as u64) as usize
    };
    let mut last_tab = 0;
    for n in 0..SWITCHES {
        if n % 6 == 5 {
            let (title, ..) = &items[9 + next(6)];
            steps.push(format!("jump:{title}"));
        } else {
            let mut tab = next(9);
            if tab == last_tab {
                tab = (tab + 1) % 9;
            }
            last_tab = tab;
            steps.push(format!("cmd-{}", tab + 1));
        }
    }
    let scenario = json!({
        "scenario": "switch",
        "start_delay_ms": 3000,
        "steps": steps,
        "interval_ms": 300,
        "jitter_ms": 17,
    });
    stack.launch_layout(&layout, true, None, Some(&scenario))?;
    wait_done(stack).await?;

    let trace = stack.trace();
    let window = phase(&trace, "switch").context("no switch phase")?;
    let mut metrics = Metrics::new();
    for (kind, name) in [("switch", "b10_tab"), ("jump", "b10_jump")] {
        let done = ops(&trace, kind, &[window]);
        put_stats(
            &mut metrics,
            name,
            &done.iter().map(|k| k.0).collect::<Vec<_>>(),
        );
        put_stats(
            &mut metrics,
            &format!("{name}_hutan"),
            &done.iter().map(|k| k.1).collect::<Vec<_>>(),
        );
    }
    let all: Vec<f64> = ["switch", "jump"]
        .iter()
        .flat_map(|kind| ops(&trace, kind, &[window]))
        .map(|k| k.0)
        .collect();
    if all.len() < SWITCHES * 9 / 10 {
        bail!("only {} of {SWITCHES} switches were painted", all.len());
    }
    metrics.insert(
        "b10_switch_p95_ms".into(),
        percentile(&all, 95.0).context("no switches")?,
    );
    put_stats(&mut metrics, "b10_switch", &all);
    display(&mut metrics, &trace, &[])?;
    Ok(metrics)
}

// ---- B11, B13 ------------------------------------------------------------------------------

/// Rust source of about `bytes` bytes: items like a real crate's (doc comments, structs,
/// impls, `match`, strings), every one distinct, so highlighting and shaping do real work.
fn rust_source(bytes: usize) -> String {
    let mut text = String::with_capacity(bytes + 1024);
    let mut n = 0usize;
    while text.len() < bytes {
        text.push_str(&format!(
            "/// Item {n}: what it holds and why.\n\
             #[derive(Debug, Clone, PartialEq)]\n\
             pub struct Item{n} {{\n    pub id: u64,\n    pub name: String,\n    pub tags: Vec<&'static str>,\n}}\n\n\
             impl Item{n} {{\n    pub fn new(id: u64) -> Self {{\n        Self {{ id, name: format!(\"item-{{id}}-{n}\"), tags: vec![\"a\", \"b\"] }}\n    }}\n\n\
             \x20   pub fn score(&self, x: i64) -> i64 {{\n        match x % 3 {{\n            0 => x * {n} + 1,\n            1 => -x,\n            _ => self.id as i64 ^ x,\n        }}\n    }}\n}}\n\n"
        ));
        n += 1;
    }
    text
}

/// A project with the files the editor scenarios open, and a terminal shown in the center.
async fn editor_stack(
    stack: &mut Stack<'_>,
    sizes: &[(&str, usize)],
) -> Result<(Vec<String>, Value)> {
    let projects = open_projects(stack, 1).await?;
    let project = &projects[0];
    let term = terminal(stack, project, "term-0").await?;
    let mut files = Vec::new();
    for (name, bytes) in sizes {
        let path = project.dir.join(name);
        std::fs::write(&path, rust_source(*bytes))?;
        files.push(path.canonicalize()?.display().to_string());
    }
    let layout = seed::custom_layout(
        seed::dock("sessions", 240.0, true),
        seed::hidden_dock("inspector", 300.0),
        seed::hidden_dock("resources", 200.0),
        seed::pane_of(1, vec![seed::terminal_item(&term.id.to_string())], 0),
        1,
    );
    Ok((files, layout))
}

/// B11a-d: a 1 MB file is opened (first paint, highlighted viewport, memory), then a 10 MB one.
pub async fn editor_open(stack: &mut Stack<'_>) -> Result<Metrics> {
    let (files, layout) = editor_stack(
        stack,
        &[("one_mb.rs", 1_000_000), ("ten_mb.rs", 10_000_000)],
    )
    .await?;
    let scenario = json!({
        "scenario": "editor_open",
        "start_delay_ms": 3000,
        "files": files,
        "pause_s": 3,
    });
    stack.launch_layout(&layout, true, None, Some(&scenario))?;
    let before = settled_rss(stack).await?;
    stack
        .wait_trace("the 1 MB file", DONE_PATIENCE, |e| {
            is_event(e, "editor_rss_point") && e["file"] == 0
        })
        .await?;
    // The tree is in once the viewport was highlighted; a moment for the allocator to settle.
    sleep(Duration::from_millis(1500)).await;
    let with_one = proc::usage(stack.desktop()?.pid())?.rss_bytes as f64 / MIB;
    wait_done(stack).await?;

    let trace = stack.trace();
    let mut metrics = Metrics::new();
    for (n, metric) in [(0usize, "b11a_open_1mb"), (1, "b11b_open_10mb")] {
        let window = phase(&trace, &format!("open_{n}")).context("no open phase")?;
        let opened = ops(&trace, "open", &[window]);
        let Some(&(total, hutan)) = opened.first() else {
            bail!("file {n} was never painted");
        };
        metrics.insert(format!("{metric}_ms"), total);
        metrics.insert(format!("{metric}_hutan_ms"), hutan);
    }
    for e in trace.iter().filter(|e| is_event(e, "editor_highlighted")) {
        let ms = e["after_first_paint_ms"].as_f64().unwrap_or(f64::NAN);
        match e["file"].as_u64() {
            Some(0) => metrics.insert("b11c_highlighted_after_ms".into(), ms),
            Some(1) => metrics.insert("b11_highlighted_10mb_after_ms".into(), ms),
            _ => None,
        };
    }
    metrics.insert("b11d_rss_before_mb".into(), before);
    metrics.insert("b11d_rss_with_1mb_mb".into(), with_one);
    metrics.insert("b11d_rss_growth_mb".into(), with_one - before);
    display(&mut metrics, &trace, &[])?;
    Ok(metrics)
}

const EDITOR_SCROLL_FRAMES: u32 = 600;
const EDITOR_SCROLL_STEP_PX: f32 = 120.0;

/// B13: a 1 MB file scrolled down and back with the wheel, one step per frame.
pub async fn editor_scroll(stack: &mut Stack<'_>) -> Result<Metrics> {
    let (files, layout) = editor_stack(stack, &[("one_mb.rs", 1_000_000)]).await?;
    let scenario = json!({
        "scenario": "editor_scroll",
        "start_delay_ms": 3000,
        "file": files[0],
        "frames": EDITOR_SCROLL_FRAMES,
        "step_px": EDITOR_SCROLL_STEP_PX,
    });
    stack.launch_layout(&layout, true, None, Some(&scenario))?;
    wait_done(stack).await?;
    let trace = stack.trace();
    let scroll: Vec<(u64, u64)> = ["scroll_down", "scroll_up"]
        .iter()
        .filter_map(|name| phase(&trace, name))
        .collect();
    let frames = intervals(&trace, &scroll);
    let mut metrics = Metrics::new();
    put_stats(&mut metrics, "b13_scroll_frame", &frames);
    metrics.insert(
        "b13_scroll_frame_p95_ms".into(),
        percentile(&frames, 95.0).context("no scroll frames")?,
    );
    put_stats(
        &mut metrics,
        "b13_scroll_work",
        &series(&trace, "frame", "work_us", &scroll),
    );
    if let Some(shaped) = trace.iter().find(|e| is_event(e, "editor_shaped")) {
        let lines = shaped["lines"].as_f64().unwrap_or(0.0);
        let frames_n = shaped["frames"].as_f64().unwrap_or(1.0).max(1.0);
        metrics.insert("b13_lines_shaped_per_frame".into(), lines / frames_n);
        metrics.insert(
            "b13_rows_in_view".into(),
            shaped["rows"].as_f64().unwrap_or(0.0),
        );
    }
    display(&mut metrics, &trace, &frames)?;
    Ok(metrics)
}

// ---- B12a ----------------------------------------------------------------------------------

const EDITOR_TYPE_KEYS: usize = 300;
const EDITOR_TYPE_INTERVAL_MS: u64 = 60;

/// B12a: 300 keys typed into the middle of a 1 MB Rust file with syntax on, one every 60 ms.
pub async fn editor_type(stack: &mut Stack<'_>) -> Result<Metrics> {
    let (files, layout) = editor_stack(stack, &[("one_mb.rs", 1_000_000)]).await?;
    let scenario = json!({
        "scenario": "editor_type",
        "start_delay_ms": 3000,
        "file": files[0],
        "keys": EDITOR_TYPE_KEYS,
        "interval_ms": EDITOR_TYPE_INTERVAL_MS,
        "jitter_ms": 17,
    });
    stack.launch_layout(&layout, true, None, Some(&scenario))?;
    wait_done(stack).await?;
    let trace = stack.trace();
    let typing = phase(&trace, "type").context("no type phase")?;
    let keys = ops(&trace, "key", &[typing]);
    if keys.len() < EDITOR_TYPE_KEYS * 3 / 4 {
        bail!("only {} of the typed keys were painted", keys.len());
    }
    let mut metrics = Metrics::new();
    put_stats(
        &mut metrics,
        "b12a_key",
        &keys.iter().map(|k| k.1).collect::<Vec<_>>(),
    );
    put_stats(
        &mut metrics,
        "b12b_key_total",
        &keys.iter().map(|k| k.0).collect::<Vec<_>>(),
    );
    let frames = intervals(&trace, &[typing]);
    put_stats(
        &mut metrics,
        "b12a_work",
        &series(&trace, "frame", "work_us", &[typing]),
    );
    display(&mut metrics, &trace, &frames)?;
    Ok(metrics)
}
