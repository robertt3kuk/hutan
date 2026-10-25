//! Opens one window that shows a hutand terminal: manual verification of the terminal view.
//!
//! ```text
//! cargo run -p hutan-terminal-view --example attach -- [--socket PATH] <terminal_id>
//! ```
//!
//! The terminal id is `terminal_id` from `hutan ls --json`. The window attaches, draws the
//! render stream, and sends typing, resizes, acks and scrollback requests back. GPUI runs on the
//! main thread; the protocol client lives on a Tokio runtime thread and the two talk over
//! bounded channels.

use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Duration;

use futures::channel::mpsc as ui_mpsc;
use futures::{SinkExt as _, StreamExt as _};
use gpui::{AppContext as _, Focusable as _, WindowOptions};
use hutan_client::{Client, ClientError, ConnectOptions};
use hutan_core::TerminalId;
use hutan_protocol::messages::{
    ClientInfo, ClientKind, Empty, Event, Request, Subscribed, TerminalAck, TerminalAttach,
    TerminalAttached, TerminalHistory, TerminalHistoryRows, TerminalResize, TerminalSize,
};
use hutan_terminal_view::{
    DefaultColors, Row, TerminalBackend, TerminalRender, TerminalView, TerminalViewEvent,
    TerminalViewSettings, ViewStatus,
};
use serde_json::Value;
use tokio::sync::mpsc as cmd_mpsc;

/// What the view asks of the connection.
#[derive(Debug)]
enum Command {
    Input(Vec<u8>),
    Resize(TerminalSize),
    Ack(u64),
    History { from_line: u32, count: u32 },
}

/// What the connection tells the view.
#[derive(Debug)]
enum Update {
    Render(TerminalRender),
    History { from_line: u32, rows: Vec<Row> },
    Status(ViewStatus),
}

const QUEUE: usize = 256;

/// Forwards the view's calls (UI thread) to the connection task. Queues are bounded; when the
/// connection cannot keep up the newest request is dropped rather than blocking the UI.
struct ChannelBackend {
    commands: cmd_mpsc::Sender<Command>,
}

impl ChannelBackend {
    fn send(&self, command: Command) {
        if let Err(e) = self.commands.try_send(command) {
            eprintln!("attach: dropped {e}");
        }
    }
}

impl TerminalBackend for ChannelBackend {
    fn input(&self, bytes: Vec<u8>) {
        self.send(Command::Input(bytes));
    }

    fn resize(&self, size: TerminalSize) {
        self.send(Command::Resize(size));
    }

    fn ack(&self, frame: u64) {
        self.send(Command::Ack(frame));
    }

    fn request_history(&self, from_line: u32, count: u32) {
        self.send(Command::History { from_line, count });
    }

    fn set_default_colors(&self, colors: DefaultColors) {
        // The protocol has no message for this yet (the daemon answers OSC 10/11/12 with its
        // own defaults until a client reports its colors).
        eprintln!("attach: default colors {colors:?}");
    }

    fn block_text(
        &self,
        _block_id: u64,
        _part: hutan_terminal_view::BlockPart,
    ) -> gpui::Task<Option<String>> {
        // This example does not fetch blocks, so no block is ever offered to copy.
        gpui::Task::ready(None)
    }
}

struct Args {
    socket: Option<PathBuf>,
    terminal: TerminalId,
}

fn parse_args() -> Result<Args, String> {
    let mut socket = None;
    let mut terminal = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => {
                socket = Some(PathBuf::from(args.next().ok_or("--socket needs a path")?));
            }
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            other => {
                if terminal.is_some() {
                    return Err("only one terminal id is accepted".into());
                }
                terminal = Some(other.parse::<TerminalId>().map_err(|e| e.to_string())?);
            }
        }
    }
    Ok(Args {
        socket,
        terminal: terminal.ok_or("missing <terminal_id>")?,
    })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("attach: {message}");
            eprintln!("usage: attach [--socket PATH] <terminal_id>");
            return ExitCode::from(2);
        }
    };

    let (command_tx, command_rx) = cmd_mpsc::channel(QUEUE);
    let (update_tx, mut update_rx) = ui_mpsc::channel::<Update>(QUEUE);
    let connection = std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(e) => {
                eprintln!("attach: cannot start the runtime: {e}");
                return;
            }
        };
        runtime.block_on(async {
            let mut status_tx = update_tx.clone();
            if let Err(e) = run_connection(args, command_rx, update_tx).await {
                eprintln!("attach: {e}");
                let _ = status_tx.try_send(Update::Status(ViewStatus::Ended(format!(
                    "connection lost: {e}"
                ))));
            }
        });
    });

    let backend = Rc::new(ChannelBackend {
        commands: command_tx,
    });
    gpui_platform::application().run(move |cx| {
        hutan_terminal_view::bind_default_keys(cx);
        cx.on_window_closed(|cx, _window| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let opened = cx.open_window(WindowOptions::default(), |window, cx| {
            let view = cx.new(|cx| {
                TerminalView::new(backend.clone(), TerminalViewSettings::default(), window, cx)
            });
            window.focus(&view.focus_handle(cx), cx);
            window.set_window_title("hutan attach");
            cx.subscribe(&view, |_, event: &TerminalViewEvent, _cx| match event {
                TerminalViewEvent::TitleChanged(title) => {
                    eprintln!("attach: title = {title:?}");
                }
                TerminalViewEvent::SendBlockToAgent { block } => {
                    eprintln!("attach: send block {block} to an agent (no chat here)");
                }
                TerminalViewEvent::Notice(text) => eprintln!("attach: {text}"),
                TerminalViewEvent::ImageSaved(path) => {
                    eprintln!("attach: image saved as {}", path.display());
                }
                TerminalViewEvent::OpenLink(target) => {
                    eprintln!("attach: open {target:?} (nothing opens links here)");
                }
            })
            .detach();
            let pumped = view.clone();
            window
                .spawn(cx, async move |cx| {
                    while let Some(update) = update_rx.next().await {
                        pumped.update(cx, |view, cx| match update {
                            Update::Render(render) => view.apply_render(render, cx),
                            Update::History { from_line, rows } => {
                                view.apply_history(from_line, rows, cx)
                            }
                            Update::Status(status) => view.set_status(status, cx),
                        });
                    }
                })
                .detach();
            view
        });
        if let Err(e) = opened {
            eprintln!("attach: cannot open the window: {e}");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
    // Dropping the command sender (with the app) ends the connection task.
    let _ = connection.join();
    ExitCode::SUCCESS
}

/// Prints what arrives on the render stream: every snapshot, and frame totals once a second.
#[derive(Default)]
struct FrameLog {
    frames: u64,
    last_frame: u64,
    last_report: Option<std::time::Instant>,
}

impl FrameLog {
    fn record(&mut self, render: &TerminalRender) {
        match render {
            TerminalRender::Snapshot(s) => eprintln!(
                "attach: snapshot #{} {}x{} history={} alt={} mouse={:?} kitty={}",
                s.frame,
                s.cols,
                s.rows,
                s.history_len,
                s.modes.alt_screen,
                s.modes.mouse,
                s.modes.kitty_keyboard
            ),
            TerminalRender::Frame(f) => {
                self.frames += 1;
                self.last_frame = f.frame;
            }
        }
        let now = std::time::Instant::now();
        let due = self.frames.is_multiple_of(100)
            || self
                .last_report
                .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1));
        if self.frames > 0 && due {
            eprintln!(
                "attach: {} frames so far (last #{})",
                self.frames, self.last_frame
            );
            self.last_report = Some(now);
        }
    }
}

async fn run_connection(
    args: Args,
    mut commands: cmd_mpsc::Receiver<Command>,
    mut updates: ui_mpsc::Sender<Update>,
) -> Result<(), ClientError> {
    let terminal = args.terminal;
    let mut frames = FrameLog::default();
    let client = Client::connect(ConnectOptions {
        endpoint: args.socket.map(hutan_client::Endpoint::Local),
        autostart: None,
        client: ClientInfo {
            kind: ClientKind::Desktop,
            version: format!("attach-example/{}", env!("CARGO_PKG_VERSION")),
            origin: None,
        },
    })
    .await?;
    let mut events = client.events();
    // Subscribe to events before attaching so the session's end cannot slip by.
    let _: Subscribed = client.call(Request::EventsSubscribe(Empty {})).await?;
    // The snapshot is queued before the attach response, so the receiver must exist first.
    let mut renders = client.terminal_frames(terminal);
    let _: TerminalAttached = client
        .call(Request::TerminalAttach(TerminalAttach {
            terminal_id: terminal,
            size: None,
        }))
        .await?;

    loop {
        tokio::select! {
            render = renders.recv() => {
                let Some(render) = render else {
                    let _ = updates
                        .try_send(Update::Status(ViewStatus::Ended("connection closed".into())));
                    return Ok(());
                };
                frames.record(&render);
                if updates.send(Update::Render(render)).await.is_err() {
                    return Ok(());
                }
            }
            command = commands.recv() => {
                let Some(command) = command else { return Ok(()) };
                match command {
                    Command::Input(bytes) => client.send_input(terminal, &bytes).await?,
                    Command::Resize(size) => {
                        eprintln!("attach: resize {}x{} ({}x{} px)", size.cols, size.rows, size.pixel_width, size.pixel_height);
                        let client = client.clone();
                        tokio::spawn(async move {
                            let result: Result<Value, _> = client
                                .call(Request::TerminalResize(TerminalResize {
                                    terminal_id: terminal,
                                    size,
                                }))
                                .await;
                            if let Err(e) = result {
                                eprintln!("attach: resize failed: {e}");
                            }
                        });
                    }
                    Command::Ack(frame) => {
                        let client = client.clone();
                        tokio::spawn(async move {
                            let result: Result<Value, _> = client
                                .call(Request::TerminalAck(TerminalAck {
                                    terminal_id: terminal,
                                    frame,
                                }))
                                .await;
                            if let Err(e) = result {
                                eprintln!("attach: ack failed: {e}");
                            }
                        });
                    }
                    Command::History { from_line, count } => {
                        eprintln!("attach: history from_line={from_line} count={count}");
                        let client = client.clone();
                        let mut updates = updates.clone();
                        tokio::spawn(async move {
                            let result: Result<TerminalHistoryRows, _> = client
                                .call(Request::TerminalHistory(TerminalHistory {
                                    terminal_id: terminal,
                                    from_line,
                                    count,
                                }))
                                .await;
                            match result {
                                Ok(page) => {
                                    let _ = updates
                                        .send(Update::History {
                                            from_line: page.from_line,
                                            rows: page.rows,
                                        })
                                        .await;
                                }
                                Err(e) => eprintln!("attach: history failed: {e}"),
                            }
                        });
                    }
                }
            }
            event = events.recv() => {
                match event {
                    Ok((_, Event::SessionUpdated { session }))
                        if session.terminal_id == Some(terminal) && !session.state.is_live() =>
                    {
                        let _ = updates.try_send(Update::Status(ViewStatus::Ended(format!(
                            "session {} {:?}",
                            session.id, session.state
                        ))));
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        let _ = updates.try_send(Update::Status(ViewStatus::Ended(
                            "connection closed".into(),
                        )));
                        return Ok(());
                    }
                }
            }
        }
    }
}
