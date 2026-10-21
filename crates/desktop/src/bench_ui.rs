//! Scripted UI scenarios for the desktop benchmarks B7–B10 (`cargo run -p xtask -- bench`).
//!
//! `HUTAN_BENCH_UI=<scenario.json>` (with `HUTAN_TRACE_LATENCY=1`, see [`crate::frames`]) makes
//! the window drive itself through one scenario, through the paths a user's hands take: GPUI
//! keystrokes, mouse and scroll-wheel events dispatched to the window, and the actions the keys
//! are bound to. The scenario's `phase` events frame what the harness measures; `frame` and
//! `op` events (see [`crate::frames`]) are the measurements.
//!
//! | `scenario` | what happens | metrics |
//! |---|---|---|
//! | `transcript` | opens a chat session from its id, then scrolls its transcript up to the top of what is loaded (paging earlier events in) and back | `open` op (B7 first paint), frames of the `scroll` phase |
//! | `stream` | clicks the composer of the chat that is open, then types while the harness streams a reply into it | `key` ops and frames of the `type` phase (B8) |
//! | `sidebar` | clicks the sessions filter, types a query and erases it, then drags a project onto a workspace | `key` ops of the `filter` phase, frames of the `drag` phase (B9) |
//! | `idle` | nothing: the frames drawn while the window idles (diagnosis of B3) | frames of the `idle` phase |
//! | `switch` | jumps between the tabs with ⌘1…⌘9 and to sessions with ⌘P | `switch` and `jump` ops (B10) |
//! | `editor_open` | opens each file in a tab (`workbench::OpenFile`), waits for its highlighted viewport, pauses for a memory sample | `open` op per file (B11a/b first paint), `editor_highlighted` (B11c), `editor_rss_point` (B11d) |
//! | `editor_type` | opens a file, puts the caret in its middle and types into it | `key` ops of the `type` phase (B12a: the Hutan path) |
//! | `editor_scroll` | opens a file, then scrolls it down and back up with the wheel | frames of the `scroll_down` / `scroll_up` phases (B13), `editor_shaped` |
//!
//! The windows must be in front and the display awake, as for the keystroke benchmarks.

use std::time::{Duration, Instant};

use gpui::{
    AsyncWindowContext, Context, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PlatformInput, Point, ScrollDelta, ScrollWheelEvent, TouchPhase, Window, point,
    px,
};
use hutan_terminal_view::trace;
use serde::Deserialize;

use crate::frames::{self, Op};
use crate::workbench::actions::{FileTarget, OpenFile, OpenSession};
use crate::workbench::{Region, Workbench};

const POLL: Duration = Duration::from_millis(2);
/// How long to wait for anything the scenario needs to appear before giving up.
const PATIENCE: Duration = Duration::from_secs(60);

/// One scenario, as the harness writes it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "scenario", rename_all = "snake_case")]
pub enum Scenario {
    Transcript {
        /// The chat session to open.
        session: String,
        #[serde(default)]
        start_delay_ms: u64,
        /// Frames of scrolling each way.
        frames: u32,
        /// Pixels per frame.
        step_px: f32,
    },
    Stream {
        #[serde(default)]
        start_delay_ms: u64,
        interval_ms: u64,
        #[serde(default)]
        jitter_ms: u64,
        keys: usize,
    },
    Sidebar {
        #[serde(default)]
        start_delay_ms: u64,
        /// Keys typed into the filter (then erased with as many backspaces).
        filter: Vec<String>,
        interval_ms: u64,
        #[serde(default)]
        jitter_ms: u64,
        /// The project row that is dragged.
        project: String,
        /// The workspace row it is dropped on.
        workspace: String,
        /// Frames the drag takes.
        frames: u32,
    },
    /// Nothing happens: the frames drawn while the window idles are what is measured.
    Idle {
        #[serde(default)]
        start_delay_ms: u64,
        seconds: u64,
    },
    Switch {
        #[serde(default)]
        start_delay_ms: u64,
        /// `cmd-3` presses a key; `jump:text` opens ⌘P, types `text` and presses Enter.
        steps: Vec<String>,
        interval_ms: u64,
        #[serde(default)]
        jitter_ms: u64,
    },
    EditorOpen {
        #[serde(default)]
        start_delay_ms: u64,
        /// Absolute paths below a project, opened one after the other.
        files: Vec<String>,
        /// Seconds to wait after each file is highlighted (the harness samples memory then).
        #[serde(default)]
        pause_s: u64,
    },
    EditorType {
        #[serde(default)]
        start_delay_ms: u64,
        file: String,
        keys: usize,
        interval_ms: u64,
        #[serde(default)]
        jitter_ms: u64,
    },
    EditorScroll {
        #[serde(default)]
        start_delay_ms: u64,
        file: String,
        /// Frames of scrolling each way.
        frames: u32,
        /// Pixels per frame.
        step_px: f32,
    },
}

impl Scenario {
    pub fn parse(json: &str) -> Result<Self, String> {
        let scenario: Scenario = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let keys: Vec<&str> = match &scenario {
            Scenario::Sidebar { filter, .. } => filter.iter().map(String::as_str).collect(),
            Scenario::Switch { steps, .. } => steps
                .iter()
                .filter(|s| !s.starts_with("jump:"))
                .map(String::as_str)
                .collect(),
            _ => Vec::new(),
        };
        for key in keys {
            if key.is_empty() {
                return Err("empty key".to_owned());
            }
            Keystroke::parse(key).map_err(|e| format!("key {key:?}: {e}"))?;
        }
        Ok(scenario)
    }
}

/// Starts the scenario named by `HUTAN_BENCH_UI`, if any. Returns whether there is one.
pub fn start(window: &mut Window, cx: &mut Context<Workbench>) -> bool {
    let Some(path) = std::env::var_os("HUTAN_BENCH_UI").filter(|p| !p.is_empty()) else {
        return false;
    };
    if !trace::enabled() {
        tracing::error!("HUTAN_BENCH_UI needs HUTAN_TRACE_LATENCY=1");
        return false;
    }
    let scenario = match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|json| Scenario::parse(&json))
    {
        Ok(scenario) => scenario,
        Err(err) => {
            tracing::error!("HUTAN_BENCH_UI {}: {err}", path.to_string_lossy());
            return false;
        }
    };
    // A window that is covered (or on a sleeping display) is not asked for frames: whatever the
    // scenario measures then is not Hutan, and the harness repeats the run.
    window
        .observe_window_visibility(|visibility, _, _| {
            trace::event_with(
                "visibility",
                &format!(r#""visible":{}"#, visibility.is_visible()),
            );
        })
        .detach();
    cx.spawn_in(window, async move |this, cx| {
        let mut driver = Driver { cx };
        driver.run(scenario, &this).await;
    })
    .detach();
    true
}

struct Driver<'a> {
    cx: &'a mut AsyncWindowContext,
}

/// The pseudo-random delay of step `n` (see `bench::Script::jitter`).
fn jitter(max_ms: u64, n: usize) -> Duration {
    if max_ms == 0 {
        return Duration::ZERO;
    }
    let phase = (n as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 11;
    Duration::from_micros(phase % (max_ms * 1000))
}

impl Driver<'_> {
    async fn sleep(&self, span: Duration) {
        self.cx.background_executor().timer(span).await;
    }

    /// Waits until `ready` holds; `false` after [`PATIENCE`].
    async fn until(&mut self, mut ready: impl FnMut(&mut Window, &mut gpui::App) -> bool) -> bool {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if self
                .cx
                .update(|window, cx| ready(window, cx))
                .unwrap_or(false)
            {
                return true;
            }
            self.sleep(POLL).await;
        }
        false
    }

    /// Waits for the next painted frame, or `cap`.
    async fn frame(&mut self, cap: Duration) {
        let seen = frames::frame_count();
        let deadline = Instant::now() + cap;
        while frames::frame_count() == seen && Instant::now() < deadline {
            self.sleep(POLL).await;
        }
    }

    /// `bench_start` / `bench_done`, with what the harness needs to judge the run: whether the
    /// window is the active one and the refresh interval of its display.
    fn frame_event(&mut self, name: &str) {
        let (active, visible) = self
            .cx
            .update(|window, _| (window.is_window_active(), window.is_visible()))
            .unwrap_or((false, false));
        let display = crate::bench::display_members(self.cx);
        trace::event_with(
            name,
            &format!(r#""window_active":{active},"visible":{visible},{display}"#),
        );
    }

    fn phase(name: &str, state: &str) {
        trace::event_with("phase", &format!(r#""name":"{name}","state":"{state}""#));
    }

    /// Dispatches an input event to the window.
    fn input(&mut self, event: PlatformInput) {
        let _ = self.cx.update(|window, cx| {
            window.dispatch_event(event, cx);
        });
    }

    /// Moves the pointer to `at` and lets a frame go by: what is under the pointer (hover, the
    /// target of a press) is decided by the frame that follows the move, as for a hand.
    async fn point_at(&mut self, at: Point<gpui::Pixels>, pressed: Option<MouseButton>) {
        self.input(PlatformInput::MouseMove(MouseMoveEvent {
            position: at,
            pressed_button: pressed,
            modifiers: Modifiers::default(),
        }));
        let _ = self.cx.update(|window, _| window.refresh());
        self.frame(Duration::from_millis(100)).await;
    }

    async fn click(&mut self, at: Point<gpui::Pixels>) {
        self.point_at(at, None).await;
        self.input(PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position: at,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        }));
        self.input(PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position: at,
            modifiers: Modifiers::default(),
            click_count: 1,
        }));
    }

    /// The painted centre of `region`, waiting for it to be drawn.
    async fn center_of(
        &mut self,
        this: &gpui::WeakEntity<Workbench>,
        region: Region,
    ) -> Option<Point<gpui::Pixels>> {
        let mut found = None;
        let this = this.clone();
        self.until(|_, cx| {
            found = this
                .read_with(cx, |workbench, _| workbench.region_bounds(region))
                .ok()
                .flatten()
                .map(|b| b.center());
            found.is_some()
        })
        .await;
        found
    }

    /// One keystroke as an operation the next frame is measured against.
    fn key(&mut self, key: &str, kind: &'static str) -> bool {
        let Ok(keystroke) = Keystroke::parse(key) else {
            return false;
        };
        let op = Op::start(kind);
        let ok = self
            .cx
            .update(|window, cx| {
                window.dispatch_keystroke(keystroke, cx);
            })
            .is_ok();
        op.dispatched();
        ok
    }

    /// Waits until every operation started so far has been painted.
    async fn settled(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while frames::pending_ops() > 0 && Instant::now() < deadline {
            self.sleep(POLL).await;
        }
    }

    async fn run(&mut self, scenario: Scenario, this: &gpui::WeakEntity<Workbench>) {
        // The daemon's state is in and the first frames are out.
        if !self
            .until(|_, _| trace::startup_seen("connected") && trace::startup_seen("first_frame"))
            .await
        {
            tracing::error!("bench_ui: the desktop never connected");
            return;
        }
        match scenario {
            Scenario::Transcript {
                session,
                start_delay_ms,
                frames,
                step_px,
            } => {
                self.sleep(Duration::from_millis(start_delay_ms)).await;
                self.transcript(session, frames, step_px, this).await;
            }
            Scenario::Stream {
                start_delay_ms,
                interval_ms,
                jitter_ms,
                keys,
            } => {
                self.stream(start_delay_ms, interval_ms, jitter_ms, keys, this)
                    .await;
            }
            Scenario::Sidebar {
                start_delay_ms,
                filter,
                interval_ms,
                jitter_ms,
                project,
                workspace,
                frames,
            } => {
                self.sleep(Duration::from_millis(start_delay_ms)).await;
                self.sidebar(
                    &filter,
                    interval_ms,
                    jitter_ms,
                    &project,
                    &workspace,
                    frames,
                    this,
                )
                .await;
            }
            Scenario::Idle {
                start_delay_ms,
                seconds,
            } => {
                self.sleep(Duration::from_millis(start_delay_ms)).await;
                self.frame_event("bench_start");
                frames::set_recording(true);
                Self::phase("idle", "start");
                self.sleep(Duration::from_secs(seconds)).await;
                Self::phase("idle", "end");
            }
            Scenario::Switch {
                start_delay_ms,
                steps,
                interval_ms,
                jitter_ms,
            } => {
                self.sleep(Duration::from_millis(start_delay_ms)).await;
                self.switch(&steps, interval_ms, jitter_ms).await;
            }
            Scenario::EditorOpen {
                start_delay_ms,
                files,
                pause_s,
            } => {
                self.sleep(Duration::from_millis(start_delay_ms)).await;
                self.frame_event("bench_start");
                frames::set_recording(true);
                for (n, file) in files.iter().enumerate() {
                    self.editor_open(n, file, this).await;
                    trace::event_with("editor_rss_point", &format!(r#""file":{n}"#));
                    self.sleep(Duration::from_secs(pause_s)).await;
                }
            }
            Scenario::EditorType {
                start_delay_ms,
                file,
                keys,
                interval_ms,
                jitter_ms,
            } => {
                self.sleep(Duration::from_millis(start_delay_ms)).await;
                self.frame_event("bench_start");
                frames::set_recording(true);
                self.editor_open(0, &file, this).await;
                self.editor_type(keys, interval_ms, jitter_ms, this).await;
            }
            Scenario::EditorScroll {
                start_delay_ms,
                file,
                frames,
                step_px,
            } => {
                self.sleep(Duration::from_millis(start_delay_ms)).await;
                self.frame_event("bench_start");
                frames::set_recording(true);
                self.editor_open(0, &file, this).await;
                self.editor_scroll(frames, step_px, this).await;
            }
        }
        frames::set_recording(false);
        self.frame_event("bench_done");
    }

    // ---- B7 ----------------------------------------------------------------------------------

    async fn transcript(
        &mut self,
        session: String,
        frames_each_way: u32,
        step_px: f32,
        this: &gpui::WeakEntity<Workbench>,
    ) {
        self.frame_event("bench_start");
        frames::set_recording(true);
        Self::phase("open", "start");
        let session_id = session.parse::<hutan_core::SessionId>().ok();
        let op = Op::start("open");
        let _ = self.cx.update(|window, cx| {
            window.dispatch_action(Box::new(OpenSession(session)), cx);
        });
        op.dispatched();
        self.settled().await;
        // A few frames with nothing going on, as the reader looks at what was opened.
        for _ in 0..10 {
            self.frame(Duration::from_millis(100)).await;
        }
        Self::phase("open", "end");

        let Some(center) = self.center_of(this, Region::Center).await else {
            return;
        };
        let target = session_id;
        for (name, direction) in [("scroll_up", 1.0f32), ("scroll_down", -1.0)] {
            Self::phase(name, "start");
            for _ in 0..frames_each_way {
                // Positive y scrolls the content down, i.e. towards earlier entries.
                self.input(PlatformInput::ScrollWheel(ScrollWheelEvent {
                    position: center,
                    delta: ScrollDelta::Pixels(point(px(0.0), px(step_px * direction))),
                    modifiers: Modifiers::default(),
                    touch_phase: TouchPhase::Moved,
                }));
                self.frame(Duration::from_millis(100)).await;
            }
            Self::phase(name, "end");
            // Earlier pages that were asked for arrive meanwhile.
            self.sleep(Duration::from_millis(300)).await;
            if direction > 0.0 {
                // How much of the journal the scroll brought in.
                let loaded = self
                    .cx
                    .update(|_, cx| {
                        this.read_with(cx, |workbench, cx| {
                            target
                                .and_then(|id| workbench.chat_model_of(id))
                                .map(|model| {
                                    let model = model.read(cx);
                                    (model.transcript().len(), model.has_earlier())
                                })
                        })
                        .ok()
                        .flatten()
                    })
                    .ok()
                    .flatten();
                if let Some((entries, more)) = loaded {
                    trace::event_with(
                        "chat_loaded",
                        &format!(r#""entries":{entries},"has_earlier":{more}"#),
                    );
                }
            }
        }
    }

    // ---- B8 ----------------------------------------------------------------------------------

    async fn stream(
        &mut self,
        start_delay_ms: u64,
        interval_ms: u64,
        jitter_ms: u64,
        keys: usize,
        this: &gpui::WeakEntity<Workbench>,
    ) {
        let Some(composer) = self.center_of(this, Region::Chat("composer")).await else {
            tracing::error!("bench_ui: no chat composer on screen");
            return;
        };
        self.click(composer).await;
        self.sleep(Duration::from_millis(200)).await;
        // The harness sends the message that starts the stream when it sees this.
        trace::event("bench_ready");
        self.sleep(Duration::from_millis(start_delay_ms)).await;
        self.frame_event("bench_start");
        frames::set_recording(true);
        Self::phase("type", "start");
        let started = Instant::now();
        let interval = Duration::from_millis(interval_ms);
        for n in 0..keys {
            let due =
                started + interval * u32::try_from(n).unwrap_or(u32::MAX) + jitter(jitter_ms, n);
            self.sleep(due.saturating_duration_since(Instant::now()))
                .await;
            let key = char::from(b'a' + (n % 26) as u8).to_string();
            if !self.key(&key, "key") {
                return;
            }
        }
        self.settled().await;
        Self::phase("type", "end");
    }

    // ---- B9 ----------------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)] // one call site; a struct would only rename them
    async fn sidebar(
        &mut self,
        filter: &[String],
        interval_ms: u64,
        jitter_ms: u64,
        project: &str,
        workspace: &str,
        drag_frames: u32,
        this: &gpui::WeakEntity<Workbench>,
    ) {
        let Some(field) = self.center_of(this, Region::SidebarControl("filter")).await else {
            tracing::error!("bench_ui: no sessions filter on screen");
            return;
        };
        self.click(field).await;
        self.sleep(Duration::from_millis(300)).await;
        self.frame_event("bench_start");
        frames::set_recording(true);
        Self::phase("filter", "start");
        let interval = Duration::from_millis(interval_ms);
        let started = Instant::now();
        let typed = filter.len();
        for (n, key) in filter.iter().enumerate() {
            let due =
                started + interval * u32::try_from(n).unwrap_or(u32::MAX) + jitter(jitter_ms, n);
            self.sleep(due.saturating_duration_since(Instant::now()))
                .await;
            if !self.key(key, "key") {
                return;
            }
        }
        self.settled().await;
        // Erasing is typing too.
        for n in 0..typed {
            self.sleep(interval + jitter(jitter_ms, typed + n)).await;
            if !self.key("backspace", "key") {
                return;
            }
        }
        self.settled().await;
        Self::phase("filter", "end");
        self.sleep(Duration::from_millis(300)).await;

        // Take the keyboard out of the filter: the drag is a mouse gesture.
        let (Ok(from_id), Ok(to_id)) = (
            project.parse::<hutan_core::WorkspaceId>(),
            workspace.parse::<hutan_core::WorkspaceId>(),
        ) else {
            tracing::error!("bench_ui: bad project or workspace id");
            return;
        };
        let (Some(from), Some(to)) = (
            self.center_of(this, Region::WorkspaceRow(from_id)).await,
            self.center_of(this, Region::WorkspaceRow(to_id)).await,
        ) else {
            tracing::error!("bench_ui: the rows of the drag are not on screen");
            return;
        };
        Self::phase("drag", "start");
        trace::event_with(
            "drag_rows",
            &format!(
                r#""from":[{},{}],"to":[{},{}]"#,
                f32::from(from.x),
                f32::from(from.y),
                f32::from(to.x),
                f32::from(to.y)
            ),
        );
        let modifiers = Modifiers::default();
        self.point_at(from, None).await;
        self.input(PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position: from,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }));
        // The press is seen by the frame that follows it; the pointer then leaves the row.
        self.frame(Duration::from_millis(100)).await;
        for step in 1..=drag_frames {
            let t = step as f32 / drag_frames as f32;
            let at = point(
                from.x + (to.x - from.x) * t,
                from.y + (to.y - from.y) * t + px(if step == 1 { 6.0 } else { 0.0 }),
            );
            self.input(PlatformInput::MouseMove(MouseMoveEvent {
                position: at,
                pressed_button: Some(MouseButton::Left),
                modifiers,
            }));
            self.frame(Duration::from_millis(100)).await;
            if step % 20 == 1 {
                let dragging = self
                    .cx
                    .update(|_, cx| cx.has_active_drag())
                    .unwrap_or(false);
                trace::event_with(
                    "drag_state",
                    &format!(r#""step":{step},"dragging":{dragging}"#),
                );
            }
        }
        self.input(PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position: to,
            modifiers,
            click_count: 1,
        }));
        // What the drop sets off: the daemon moves the project, the sidebar lays itself out again.
        for _ in 0..30 {
            self.frame(Duration::from_millis(100)).await;
        }
        Self::phase("drag", "end");
    }

    // ---- B10 ---------------------------------------------------------------------------------

    async fn switch(&mut self, steps: &[String], interval_ms: u64, jitter_ms: u64) {
        self.frame_event("bench_start");
        frames::set_recording(true);
        Self::phase("switch", "start");
        let interval = Duration::from_millis(interval_ms);
        for (n, step) in steps.iter().enumerate() {
            self.sleep(interval + jitter(jitter_ms, n)).await;
            if let Some(query) = step.strip_prefix("jump:") {
                if !self.key("cmd-p", "palette") {
                    return;
                }
                self.sleep(Duration::from_millis(150)).await;
                for ch in query.chars() {
                    let key = if ch == ' ' {
                        "space".to_owned()
                    } else {
                        ch.to_string()
                    };
                    if !self.key(&key, "palette") {
                        return;
                    }
                    self.sleep(Duration::from_millis(15)).await;
                }
                self.sleep(Duration::from_millis(150)).await;
                if !self.key("enter", "jump") {
                    return;
                }
            } else if !self.key(step, "switch") {
                return;
            }
            self.settled().await;
        }
        Self::phase("switch", "end");
    }

    // ---- B11, B13 -----------------------------------------------------------------------------

    /// The focused file tab's code view, once its buffer has text.
    fn code_view(
        this: &gpui::WeakEntity<Workbench>,
        cx: &gpui::App,
    ) -> Option<gpui::Entity<hutan_code_view::CodeView>> {
        this.read_with(cx, |workbench, cx| {
            let view = workbench.focused_file_view()?;
            let view = view.read(cx);
            view.buffer()
                .read(cx)
                .is_ready()
                .then(|| view.code().clone())
        })
        .ok()
        .flatten()
    }

    /// Opens `file` through the action, as `ui.run` and `hutan open` do; the `open` op ends with
    /// the first frame that draws its text. Then waits for the highlighted viewport.
    async fn editor_open(&mut self, n: usize, file: &str, this: &gpui::WeakEntity<Workbench>) {
        let phase = format!("open_{n}");
        Self::phase(&phase, "start");
        let op = Op::start("open");
        let _ = self.cx.update(|window, cx| {
            window.dispatch_action(
                Box::new(OpenFile(FileTarget {
                    path: file.to_owned(),
                    line: None,
                    column: None,
                })),
                cx,
            );
        });
        op.dispatched();
        self.settled().await;
        Self::phase(&phase, "end");
        let mut times = None;
        let this_ = this.clone();
        self.until(|_, cx| {
            times = Self::code_view(&this_, cx).map(|code| code.read(cx).paint_times());
            times.is_some_and(|t| t.first_highlighted.is_some())
                || Self::code_view(&this_, cx)
                    .is_some_and(|code| code.read(cx).buffer().read(cx).is_large())
        })
        .await;
        if let Some(times) = times
            && let (Some(text), Some(colored)) = (times.first_text, times.first_highlighted)
        {
            let ms = colored.saturating_duration_since(text).as_secs_f64() * 1000.0;
            trace::event_with(
                "editor_highlighted",
                &format!(r#""file":{n},"after_first_paint_ms":{ms:.3}"#),
            );
        }
    }

    /// B12a: the caret goes to the middle of the file and `keys` letters are typed, each an
    /// operation measured to the frame that shows it.
    async fn editor_type(
        &mut self,
        keys: usize,
        interval_ms: u64,
        jitter_ms: u64,
        this: &gpui::WeakEntity<Workbench>,
    ) {
        let placed = self
            .cx
            .update(|window, cx| {
                let code = Self::code_view(this, cx)?;
                let lines = code.read(cx).buffer().read(cx).text().line_count();
                code.update(cx, |code, cx| code.reveal_line(lines / 2, None, cx));
                let focus = gpui::Focusable::focus_handle(code.read(cx), cx);
                window.focus(&focus, cx);
                Some(())
            })
            .ok()
            .flatten();
        if placed.is_none() {
            tracing::error!("bench_ui: no file to type into");
            return;
        }
        self.sleep(Duration::from_millis(500)).await;
        Self::phase("type", "start");
        let started = Instant::now();
        let interval = Duration::from_millis(interval_ms);
        for n in 0..keys {
            let due =
                started + interval * u32::try_from(n).unwrap_or(u32::MAX) + jitter(jitter_ms, n);
            self.sleep(due.saturating_duration_since(Instant::now()))
                .await;
            let key = char::from(b'a' + (n % 26) as u8).to_string();
            if !self.key(&key, "key") {
                return;
            }
        }
        self.settled().await;
        Self::phase("type", "end");
    }

    async fn editor_scroll(
        &mut self,
        frames_each_way: u32,
        step_px: f32,
        this: &gpui::WeakEntity<Workbench>,
    ) {
        let Some(center) = self.center_of(this, Region::Center).await else {
            return;
        };
        let shaped = |cx: &mut AsyncWindowContext| {
            cx.update(|_, cx| {
                Self::code_view(this, cx).map(|code| code.read(cx).stats().lines_shaped)
            })
            .ok()
            .flatten()
            .unwrap_or(0)
        };
        let before = shaped(self.cx);
        for (name, direction) in [("scroll_down", -1.0f32), ("scroll_up", 1.0)] {
            Self::phase(name, "start");
            for _ in 0..frames_each_way {
                self.input(PlatformInput::ScrollWheel(ScrollWheelEvent {
                    position: center,
                    delta: ScrollDelta::Pixels(point(px(0.0), px(step_px * direction))),
                    modifiers: Modifiers::default(),
                    touch_phase: TouchPhase::Moved,
                }));
                self.frame(Duration::from_millis(100)).await;
            }
            Self::phase(name, "end");
        }
        let after = shaped(self.cx);
        let rows = self
            .cx
            .update(|_, cx| {
                Self::code_view(this, cx).and_then(|code| code.read(cx).layout().map(|l| l.rows))
            })
            .ok()
            .flatten()
            .unwrap_or(0);
        trace::event_with(
            "editor_shaped",
            &format!(
                r#""lines":{},"frames":{},"rows":{rows}"#,
                after - before,
                2 * frames_each_way
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_scenario() {
        let json = r#"{"scenario":"transcript","session":"s","frames":10,"step_px":40.0}"#;
        assert!(matches!(
            Scenario::parse(json),
            Ok(Scenario::Transcript { frames: 10, .. })
        ));
        let json = r#"{"scenario":"stream","interval_ms":50,"keys":3}"#;
        assert!(matches!(Scenario::parse(json), Ok(Scenario::Stream { .. })));
        let json = r#"{"scenario":"sidebar","filter":["a","b"],"interval_ms":50,"project":"p","workspace":"w","frames":30}"#;
        assert!(matches!(
            Scenario::parse(json),
            Ok(Scenario::Sidebar { .. })
        ));
        let json = r#"{"scenario":"switch","steps":["cmd-1","jump:foo"],"interval_ms":100}"#;
        assert!(matches!(Scenario::parse(json), Ok(Scenario::Switch { .. })));
        let json = r#"{"scenario":"editor_open","files":["/a.rs"],"pause_s":3}"#;
        assert!(matches!(
            Scenario::parse(json),
            Ok(Scenario::EditorOpen { .. })
        ));
        let json = r#"{"scenario":"editor_scroll","file":"/a.rs","frames":10,"step_px":40.0}"#;
        assert!(matches!(
            Scenario::parse(json),
            Ok(Scenario::EditorScroll { .. })
        ));
    }

    #[test]
    fn rejects_unknown_scenarios_and_bad_keys() {
        assert!(Scenario::parse(r#"{"scenario":"nope"}"#).is_err());
        let json = r#"{"scenario":"switch","steps":[""],"interval_ms":1}"#;
        assert!(Scenario::parse(json).is_err());
    }

    #[test]
    fn jitter_stays_inside_its_bound_and_is_repeatable() {
        for n in 0..100 {
            assert!(jitter(17, n) < Duration::from_millis(17));
            assert_eq!(jitter(17, n), jitter(17, n));
        }
        assert_eq!(jitter(0, 5), Duration::ZERO);
    }
}
