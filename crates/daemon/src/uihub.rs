//! Routing of the UI control requests (`ui.*`, protocol 0.21, ADR 0016).
//!
//! The desktop publishes its action catalog, settings catalog and layout with `ui.publish`;
//! the hub caches the last publish of every window and nothing else (layout stays desktop
//! state, ADR 0010). `ui.run` and `ui.set_setting` are sent to the window as the event
//! `ui.command` — to its one connection, outside the event sequence, so it is never replayed —
//! and the caller waits for the window's `ui.command_result`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::Event;
use hutan_protocol::ui::{
    COMMAND_TIMEOUT_MS, LayoutSummary, SET_SETTING_ACTION, UiActionList, UiCommandResult,
    UiLayoutResult, UiPublish, UiRan, UiRun, UiSetSetting, UiTarget,
};
use serde_json::{Value, json};
use tokio::sync::{oneshot, watch};
use tracing::{debug, warn};

use crate::bus::EventBus;
use crate::conn::{ConnHandle, ConnId};

/// Windows one connection may publish; a desktop has one.
const MAX_WINDOWS_PER_CONN: usize = 16;

type Answer = Result<Value, ErrorBody>;

#[derive(Debug)]
struct Window {
    conn: ConnHandle,
    publish: Arc<UiPublish>,
    /// Orders windows for "the one that was active most recently": stamped when a publish
    /// reports the window active (or, failing that, on every publish).
    stamp: u64,
}

#[derive(Debug)]
struct Pending {
    conn: ConnId,
    reply: oneshot::Sender<Answer>,
}

#[derive(Debug, Default)]
struct Inner {
    windows: HashMap<(ConnId, String), Window>,
    pending: HashMap<u64, Pending>,
    clock: u64,
}

/// What the daemon knows about the windows of its desktops.
#[derive(Debug)]
pub struct UiHub {
    bus: Arc<EventBus>,
    timeout: Duration,
    next_request: AtomicU64,
    inner: Mutex<Inner>,
    /// How many windows have published: a window that did is built and synchronised with the
    /// daemon, so the daemon may ask it to do things (the menu bar item hands off through it).
    windows: watch::Sender<usize>,
}

/// The window a request goes to.
struct Target {
    key: (ConnId, String),
    conn: ConnHandle,
}

impl UiHub {
    /// Whether a window of a desktop is the one the user is working in now (its last publish
    /// said `window_active`): what a user hook with `when_focused = false` waits out.
    pub fn any_window_active(&self) -> bool {
        self.lock()
            .windows
            .values()
            .any(|w| w.publish.layout.window_active)
    }

    pub fn new(bus: Arc<EventBus>) -> Arc<Self> {
        Self::with_timeout(bus, Duration::from_millis(COMMAND_TIMEOUT_MS))
    }

    /// A hub that waits `timeout` for a window to answer (tests shorten it).
    pub fn with_timeout(bus: Arc<EventBus>, timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            bus,
            timeout,
            next_request: AtomicU64::new(1),
            inner: Mutex::new(Inner::default()),
            windows: watch::Sender::new(0),
        })
    }

    /// How many windows have published (and not closed), as it changes.
    pub fn windows(&self) -> watch::Receiver<usize> {
        self.windows.subscribe()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `ui.publish`: replaces what the window published before.
    pub fn publish(&self, conn: &ConnHandle, publish: UiPublish) -> Result<(), ErrorBody> {
        let window_id = publish.layout.window_id.clone();
        if window_id.trim().is_empty() {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "`layout.window_id` must not be empty",
            ));
        }
        let mut inner = self.lock();
        let key = (conn.id(), window_id);
        let known = inner
            .windows
            .keys()
            .filter(|(c, _)| *c == conn.id())
            .count();
        if known >= MAX_WINDOWS_PER_CONN && !inner.windows.contains_key(&key) {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("a connection may publish at most {MAX_WINDOWS_PER_CONN} windows"),
            ));
        }
        let was_active = inner
            .windows
            .get(&key)
            .is_some_and(|w| w.publish.layout.window_active);
        let previous = inner.windows.get(&key).map(|w| w.stamp);
        let stamp = if publish.layout.window_active && !was_active || previous.is_none() {
            inner.clock += 1;
            inner.clock
        } else {
            previous.unwrap_or(0)
        };
        inner.windows.insert(
            key,
            Window {
                conn: conn.clone(),
                publish: Arc::new(publish),
                stamp,
            },
        );
        self.windows.send_replace(inner.windows.len());
        Ok(())
    }

    /// Forgets the windows of a closed connection and fails the commands it still owed.
    pub fn connection_closed(&self, conn: ConnId) {
        let pending: Vec<Pending> = {
            let mut inner = self.lock();
            inner.windows.retain(|(c, _), _| *c != conn);
            self.windows.send_replace(inner.windows.len());
            let ids: Vec<u64> = inner
                .pending
                .iter()
                .filter(|(_, p)| p.conn == conn)
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| inner.pending.remove(&id))
                .collect()
        };
        for pending in pending {
            let _ = pending.reply.send(Err(ErrorBody::new(
                ErrorCode::InvalidState,
                "the window closed before it answered",
            )));
        }
    }

    fn target(&self, window: Option<&str>) -> Result<(Target, Arc<UiPublish>), ErrorBody> {
        let inner = self.lock();
        let chosen = match window {
            Some(id) => inner
                .windows
                .iter()
                .find(|((_, w), _)| w == id)
                .ok_or_else(|| {
                    ErrorBody::new(
                        ErrorCode::NotFound,
                        format!("no window `{id}`; `hutan ui layout` shows the open ones"),
                    )
                })?,
            None => inner
                .windows
                .iter()
                .max_by_key(|((conn, _), w)| (w.stamp, conn.0))
                .ok_or_else(no_window)?,
        };
        Ok((
            Target {
                key: chosen.0.clone(),
                conn: chosen.1.conn.clone(),
            },
            chosen.1.publish.clone(),
        ))
    }

    /// `ui.actions`.
    pub fn actions(&self, req: &UiTarget) -> Result<UiActionList, ErrorBody> {
        let (target, publish) = self.target(req.window_id.as_deref())?;
        Ok(UiActionList {
            window_id: target.key.1,
            actions: publish.actions.clone(),
            settings: publish.settings.clone(),
        })
    }

    /// `ui.layout`.
    pub fn layout(&self, req: &UiTarget) -> Result<UiLayoutResult, ErrorBody> {
        let (_, publish) = self.target(req.window_id.as_deref())?;
        Ok(UiLayoutResult {
            layout: publish.layout.clone(),
        })
    }

    /// The layout of every window, most recently active first.
    pub fn layouts(&self) -> Vec<LayoutSummary> {
        let inner = self.lock();
        let mut windows: Vec<&Window> = inner.windows.values().collect();
        windows.sort_by_key(|w| std::cmp::Reverse(w.stamp));
        windows.iter().map(|w| w.publish.layout.clone()).collect()
    }

    /// `ui.run`: an action of the window's catalog.
    pub async fn run(&self, req: UiRun) -> Result<UiRan, ErrorBody> {
        let (target, publish) = self.target(req.window_id.as_deref())?;
        if !publish.actions.iter().any(|a| a.id == req.action) {
            return Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!(
                    "unknown action `{}`; `hutan ui actions` lists what window `{}` offers",
                    req.action, target.key.1
                ),
            ));
        }
        let args = match req.args {
            None | Some(Value::Null) => json!({}),
            Some(args @ Value::Object(_)) => args,
            Some(_) => {
                return Err(ErrorBody::new(
                    ErrorCode::BadRequest,
                    "`args` must be a JSON object",
                ));
            }
        };
        self.command(target, req.action, args).await
    }

    /// `ui.set_setting`: validated against the catalog here (the key exists and takes the
    /// layer), against the schema by the desktop.
    pub async fn set_setting(&self, req: UiSetSetting) -> Result<UiRan, ErrorBody> {
        let (target, publish) = self.target(req.window_id.as_deref())?;
        let Some(setting) = publish
            .settings
            .iter()
            .find(|s| s.namespace == req.namespace && s.key == req.key)
        else {
            return Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!(
                    "unknown setting `{}.{}`; `hutan ui actions` lists the settings of window `{}`",
                    req.namespace, req.key, target.key.1
                ),
            ));
        };
        if !setting.layers.contains(&req.layer) {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!(
                    "`{}.{}` cannot be written to layer `{}`; it takes: {}",
                    req.namespace,
                    req.key,
                    req.layer,
                    setting.layers.join(", ")
                ),
            ));
        }
        let args = json!({
            "namespace": req.namespace,
            "key": req.key,
            "value": req.value,
            "layer": req.layer,
        });
        self.command(target, SET_SETTING_ACTION.to_owned(), args)
            .await
    }

    async fn command(
        &self,
        target: Target,
        action: String,
        args: Value,
    ) -> Result<UiRan, ErrorBody> {
        let request_id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = oneshot::channel();
        self.lock().pending.insert(
            request_id,
            Pending {
                conn: target.key.0,
                reply,
            },
        );
        let event = Event::UiCommand {
            request_id,
            window_id: target.key.1.clone(),
            action: action.clone(),
            args,
        };
        // To this window only, outside the replay ring (`EventBus::publish_to`).
        let sent = self.bus.publish_to(&target.conn, &event).unwrap_or(false);
        if !sent {
            self.lock().pending.remove(&request_id);
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                "the window's connection is gone",
            ));
        }
        debug!(request_id, window = %target.key.1, %action, "ui.command sent");
        let outcome = tokio::time::timeout(self.timeout, answer).await;
        match outcome {
            Ok(Ok(Ok(result))) => Ok(UiRan {
                window_id: target.key.1,
                result,
            }),
            Ok(Ok(Err(error))) => Err(error),
            Ok(Err(_)) => Err(ErrorBody::new(
                ErrorCode::InvalidState,
                "the window closed before it answered",
            )),
            Err(_) => {
                self.lock().pending.remove(&request_id);
                warn!(request_id, %action, "the window did not answer a ui.command");
                Err(ErrorBody::new(
                    ErrorCode::Internal,
                    "the window did not answer",
                ))
            }
        }
    }

    /// `ui.command_result`: from the connection the command went to.
    pub fn command_result(&self, conn: ConnId, result: UiCommandResult) -> Result<(), ErrorBody> {
        let pending = {
            let mut inner = self.lock();
            match inner.pending.get(&result.request_id) {
                Some(p) if p.conn == conn => inner.pending.remove(&result.request_id),
                _ => None,
            }
        };
        let Some(pending) = pending else {
            return Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!(
                    "no command {} is waiting for this connection (answered, or timed out)",
                    result.request_id
                ),
            ));
        };
        let answer = match (result.ok, result.error) {
            (_, Some(error)) => Err(error),
            (Some(ok), None) => Ok(ok),
            (None, None) => Ok(json!({})),
        };
        // The caller may have stopped waiting; nothing to do about that.
        let _ = pending.reply.send(answer);
        Ok(())
    }
}

fn no_window() -> ErrorBody {
    ErrorBody::new(
        ErrorCode::InvalidState,
        "no Hutan window is open: the desktop is not connected",
    )
}
