//! The quick terminal (`terminal.quick_terminal`): a borderless panel that drops down from the top
//! of the screen the pointer is on, on a system-wide shortcut, over any app and over full-screen
//! Spaces (`docs/design.md` § Quick terminal).
//!
//! What it shows is ordinary Hutan sessions: the first one starts in Home or in the active
//! project (`terminal.quick_start_in`), ⌘T adds more as tabs of the panel, and hiding the panel
//! never ends them: [`QuickTerminal`] keeps one attachment per tab, the daemon keeps the
//! processes. ⌘⇧Return moves the active tab into the main window.
//!
//! Three parts:
//! - [`QuickTerminal`], the model: tabs, the active one, whether the panel is wanted on screen
//!   and the window that shows it. It has no GPUI window of its own to talk to and emits
//!   [`QuickEvent`] for what only the workbench can do.
//! - [`QuickPanelView`], the root view of the panel's window: the tab strip and the active
//!   terminal's view, sliding in from the top (fading under Reduce Motion).
//! - The `impl Workbench` below: the shortcut, showing and hiding (with the system side behind
//!   [`QuickPanelHost`](crate::macos::QuickPanelHost)), starting sessions, moving a tab to the
//!   main window.
//!
//! The window is a GPUI `WindowKind::PopUp`: a non-activating panel at pop-up level that joins
//! every Space and full-screen Space. Showing it makes it the key window without making Hutan
//! the active app, so the app that was in front stays in front and gets the keyboard back when
//! the panel hides. It is created when shown and closed once the exit animation ends; the
//! sessions are what is kept, not the window.

use std::time::Duration;

use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, Context, DisplayId, Entity,
    EventEmitter, FocusHandle, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, actions,
    div, point, prelude::FluentBuilder as _, px, size,
};
use hutan_core::{SessionId, SessionState};
use hutan_terminal_view::{LinkTarget, TerminalView, TerminalViewEvent, ViewMode};

use super::actions;
use super::style::{TerminalStyle, UiStyle};
use super::ui::motion::{self, Kind};
use super::ui::{Button, IconButton};
use super::view::Workbench;
use crate::icons::Icon;
use crate::macos::{HotkeyError, Shortcut};
use crate::sessions::TerminalSession;
use crate::settings::{QuickStart, TerminalSettings};
use crate::theme::DensityExt as _;

/// The key context of the panel's root: ⎋ and ⌘⇧↩ are bound in it (`actions.rs`).
pub const KEY_CONTEXT: &str = "QuickTerminal";

actions!(
    quick_terminal,
    [
        /// ⎋ in the panel: hides it (`terminal.quick_escape_hides`), unless the program in the
        /// terminal has the alternate screen, which keeps its Escape.
        HideOnEscape,
        /// ⌘⇧↩ in the panel: moves the active tab into the main window.
        MoveToMain,
    ]
);

/// One tab of the panel: a session and the attachment that keeps its terminal live while the
/// panel is hidden.
#[derive(Debug)]
pub(crate) struct QuickTab {
    pub session: SessionId,
    pub attached: Entity<TerminalSession>,
}

/// What the model asks of the workbench.
#[derive(Debug, Clone, PartialEq)]
pub enum QuickEvent {
    /// Hide the panel (⎋, the button).
    Hide,
    /// ⌘T, the + button: start a session next to the active tab.
    NewTab,
    /// ⌘W, the × of a tab.
    CloseTab(SessionId),
    /// ⌘⇧↩, the button: show the session as a tab of the main window.
    MoveToMain(SessionId),
    /// ⌘-click on a link in the panel's terminal.
    OpenLink(LinkTarget),
    /// Something to tell the user that the panel has no room for.
    Notice(String),
}

/// The state of the quick terminal. A GPUI entity so that the panel's window observes it and
/// tests read it.
#[derive(Debug, Default)]
pub struct QuickTerminal {
    tabs: Vec<QuickTab>,
    active: usize,
    /// The panel is on screen or on its way there (it may still be animating out after this
    /// turned false).
    wanted: bool,
    window: Option<WindowHandle<QuickPanelView>>,
    /// A session is being started.
    starting: bool,
    /// Why the first session could not start.
    error: Option<SharedString>,
    /// Counts the times keyboard focus is due to the active tab (a show, a tab switch).
    focus_epoch: u64,
}

impl EventEmitter<QuickEvent> for QuickTerminal {}

impl QuickTerminal {
    /// Whether the panel is wanted on screen (shown, or showing).
    pub fn visible(&self) -> bool {
        self.wanted
    }

    /// The sessions of the tabs, left to right.
    pub fn sessions(&self) -> Vec<SessionId> {
        self.tabs.iter().map(|tab| tab.session).collect()
    }

    /// The session of the tab that has the keyboard.
    pub fn active_session(&self) -> Option<SessionId> {
        self.tabs.get(self.active).map(|tab| tab.session)
    }

    /// The window that shows the panel, while there is one (it outlives [`visible`](Self::visible)
    /// by the length of the exit animation).
    pub fn window(&self) -> Option<AnyWindowHandle> {
        self.window.map(Into::into)
    }

    /// A session is being started for the panel.
    pub fn is_starting(&self) -> bool {
        self.starting
    }

    /// Why the panel has no terminal, when it does not.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Selects tab `index` and gives it the keyboard.
    pub fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tabs.len() {
            self.active = index;
            self.focus_epoch += 1;
            cx.notify();
        }
    }

    /// Selects the neighbour of the active tab, wrapping around.
    pub fn select_relative(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.tabs.len();
        if count > 1 {
            let next = if forward {
                (self.active + 1) % count
            } else {
                (self.active + count - 1) % count
            };
            self.select(next, cx);
        }
    }

    /// Adds `session` as the active tab.
    pub(crate) fn push_tab(
        &mut self,
        session: SessionId,
        attached: Entity<TerminalSession>,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.session == session) {
            self.select(index, cx);
            return;
        }
        self.tabs.push(QuickTab { session, attached });
        self.active = self.tabs.len() - 1;
        self.starting = false;
        self.error = None;
        self.focus_epoch += 1;
        cx.notify();
    }

    /// Drops the tab of `session` (its attachment goes with it, the session does not).
    pub(crate) fn remove_tab(&mut self, session: SessionId, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| tab.session == session) else {
            return false;
        };
        self.tabs.remove(index);
        // The tab to the left takes over, as in a pane.
        if self.active >= index {
            self.active = self.active.saturating_sub(1);
        }
        self.focus_epoch += 1;
        cx.notify();
        true
    }

    fn set_starting(&mut self, starting: bool, cx: &mut Context<Self>) {
        if self.starting != starting {
            self.starting = starting;
            cx.notify();
        }
    }

    fn fail(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.starting = false;
        self.error = Some(text.into());
        cx.notify();
    }

    /// A request for the workbench (the panel's buttons and keys call this).
    pub fn request(&mut self, event: QuickEvent, cx: &mut Context<Self>) {
        cx.emit(event);
    }
}

/// The root view of the panel's window.
pub struct QuickPanelView {
    quick: Entity<QuickTerminal>,
    /// One view of the terminal of each tab, built when the tab first draws.
    views: Vec<PanelTerminal>,
    focus: FocusHandle,
    /// The `focus_epoch` the keyboard was last given for.
    focused_epoch: u64,
    escape_hides: bool,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for QuickPanelView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuickPanelView")
            .field("views", &self.views.len())
            .finish_non_exhaustive()
    }
}

struct PanelTerminal {
    session: SessionId,
    view: Entity<TerminalView>,
    _events: Subscription,
}

impl QuickPanelView {
    fn new(
        quick: Entity<QuickTerminal>,
        escape_hides: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.observe(&quick, |_, _, cx| cx.notify()),
            // The pack or the terminal settings changed: the views follow, like the main
            // window's.
            cx.observe_global_in::<TerminalStyle>(window, |this, _, cx| {
                let style = TerminalStyle::get(cx);
                for terminal in &this.views {
                    terminal
                        .view
                        .update(cx, |view, cx| view.update_settings(style.clone(), cx));
                }
                cx.notify();
            }),
        ];
        Self {
            quick,
            views: Vec::new(),
            focus: cx.focus_handle(),
            focused_epoch: 0,
            escape_hides,
            _subscriptions: subscriptions,
        }
    }

    /// The view of the active tab's terminal, once it exists.
    pub fn active_view(&self, cx: &App) -> Option<Entity<TerminalView>> {
        let session = self.quick.read(cx).active_session()?;
        self.views
            .iter()
            .find(|terminal| terminal.session == session)
            .map(|terminal| terminal.view.clone())
    }

    /// Whether the program in the active terminal has the alternate screen (vim, less, a TUI).
    fn active_on_alt_screen(&self, cx: &App) -> bool {
        let quick = self.quick.read(cx);
        quick.tabs.get(quick.active).is_some_and(|tab| {
            let shared = tab.attached.read(cx).shared().clone();
            shared.read(cx).model().modes().alt_screen
        })
    }

    /// Builds the views the tabs lack and drops the ones whose tab went.
    fn sync_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted: Vec<(SessionId, Entity<TerminalSession>)> = self
            .quick
            .read(cx)
            .tabs
            .iter()
            .map(|tab| (tab.session, tab.attached.clone()))
            .collect();
        self.views.retain(|terminal| {
            wanted
                .iter()
                .any(|(session, _)| *session == terminal.session)
        });
        for (session, attached) in wanted {
            if self
                .views
                .iter()
                .any(|terminal| terminal.session == session)
            {
                continue;
            }
            let (shared, backend) = {
                let attached = attached.read(cx);
                (attached.shared().clone(), attached.backend())
            };
            let style = TerminalStyle::get(cx);
            let view = cx.new(|cx| {
                TerminalView::with_shared(shared, backend, style, ViewMode::Interactive, window, cx)
            });
            let quick = self.quick.clone();
            let events = cx.subscribe(&view, move |_, _, event: &TerminalViewEvent, cx| {
                let request = match event {
                    TerminalViewEvent::OpenLink(target) => QuickEvent::OpenLink(target.clone()),
                    TerminalViewEvent::Notice(text) => QuickEvent::Notice(text.clone()),
                    // The panel has no toast stack; the path is already typed in the terminal.
                    TerminalViewEvent::ImageSaved(path) => {
                        QuickEvent::Notice(format!("Image saved to {}", path.display()))
                    }
                    // The panel has no chat to send a block to; the title is the session's.
                    TerminalViewEvent::SendBlockToAgent { .. }
                    | TerminalViewEvent::TitleChanged(_) => return,
                };
                quick.update(cx, |quick, cx| quick.request(request, cx));
            });
            self.views.push(PanelTerminal {
                session,
                view,
                _events: events,
            });
        }
    }

    /// Gives the keyboard to the active tab's terminal when the model says it is due.
    fn focus_due(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let epoch = self.quick.read(cx).focus_epoch;
        if epoch == self.focused_epoch {
            return;
        }
        let Some(view) = self.active_view(cx) else {
            return;
        };
        self.focused_epoch = epoch;
        let handle = view.focus_handle(cx);
        window.defer(cx, move |window, cx| window.focus(&handle, cx));
    }

    fn tab_strip(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let quick = self.quick.read(cx);
        let key = |action: &dyn gpui::Action, fallback: &str| {
            super::render::key_hint(window, action)
                .map_or_else(|| fallback.to_owned(), |keys| keys.to_string())
        };
        let new_tab = key(&actions::NewTerminal, "⌘T");
        let to_main = key(&MoveToMain, "⌘⇧↩");
        let tabs = quick.tabs.iter().enumerate().map(|(index, tab)| {
            let session = tab.session;
            let title = tab
                .attached
                .read(cx)
                .info()
                .map(|info| info.title.clone())
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| "Terminal".to_owned());
            let active = index == quick.active;
            div()
                .flex()
                .items_center()
                .gap(px(tokens.spacing / 2.0))
                .pl(px(tokens.spacing))
                .child(
                    Button::new(("quick-tab", index), title, &theme, &tokens)
                        .ghost()
                        .compact()
                        .selected(active)
                        .max_width(200.0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.quick.update(cx, |quick, cx| quick.select(index, cx));
                        })),
                )
                .child(
                    IconButton::new(
                        ("quick-close", index),
                        Icon::ActionClose,
                        "Close tab ⌘W",
                        &theme,
                        &tokens,
                    )
                    .compact()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.quick.update(cx, |quick, cx| {
                            quick.request(QuickEvent::CloseTab(session), cx)
                        });
                    })),
                )
        });
        let active_session = quick.active_session();
        div()
            .flex()
            .flex_none()
            .items_center()
            .w_full()
            .h(px(tokens.tab_height))
            .bg(theme.sidebar)
            .border_b_1()
            .border_color(theme.border)
            .children(tabs)
            .child(
                IconButton::new(
                    "quick-new",
                    Icon::ActionAddContext,
                    format!("New tab {new_tab}"),
                    &theme,
                    &tokens,
                )
                .compact()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.quick
                        .update(cx, |quick, cx| quick.request(QuickEvent::NewTab, cx));
                })),
            )
            .child(div().flex_1())
            .when_some(active_session, |strip, session| {
                strip.child(
                    IconButton::new(
                        "quick-move",
                        Icon::ActionOpenSession,
                        format!("Move to the main window {to_main}"),
                        &theme,
                        &tokens,
                    )
                    .compact()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.quick.update(cx, |quick, cx| {
                            quick.request(QuickEvent::MoveToMain(session), cx);
                        });
                    })),
                )
            })
            .into_any_element()
    }
}

impl Render for QuickPanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_views(window, cx);
        self.focus_due(window, cx);
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let height = window.viewport_size().height;
        let (wanted, status) = {
            let quick = self.quick.read(cx);
            let status = if let Some(error) = quick.error() {
                Some(error.to_owned())
            } else if quick.tabs.is_empty() {
                Some("Starting a terminal…".to_owned())
            } else {
                None
            };
            (quick.wanted, status)
        };
        let body = match (self.active_view(cx), status) {
            (Some(view), _) => div().flex_1().min_h_0().child(view).into_any_element(),
            (None, text) => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(tokens.font_size))
                .text_color(theme.muted)
                .child(SharedString::from(text.unwrap_or_default()))
                .into_any_element(),
        };
        let panel = div()
            .absolute()
            .top_0()
            .left_0()
            .w_full()
            .h(height)
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(theme.bg)
            .border_b_1()
            .border_color(theme.border)
            .rounded_b(px(tokens.card_radius))
            .child(self.tab_strip(window, cx))
            .child(body);
        let panel = motion::transition(
            "quick-panel",
            // The shortcut is a key press, but it is not "a key that changes the layout": the
            // drop happens every time (Reduce Motion: a fade).
            Kind::Toast,
            if wanted { 1.0 } else { 0.0 },
            panel,
            move |panel, frame| {
                let panel = if frame.reduced {
                    panel.opacity(frame.value.clamp(0.0, 1.0))
                } else {
                    panel.mt(-height * (1.0 - frame.value.clamp(0.0, 1.0)))
                };
                if frame.visible() {
                    panel
                } else {
                    panel.invisible()
                }
            },
        )
        .from(0.0);
        div()
            .id("quick-terminal")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .overflow_hidden()
            .on_action(cx.listener(|this, _: &HideOnEscape, _, cx| {
                if this.escape_hides && !this.active_on_alt_screen(cx) {
                    this.quick
                        .update(cx, |quick, cx| quick.request(QuickEvent::Hide, cx));
                } else {
                    // The terminal's program gets its Escape.
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &MoveToMain, _, cx| {
                this.quick.update(cx, |quick, cx| {
                    if let Some(session) = quick.active_session() {
                        quick.request(QuickEvent::MoveToMain(session), cx);
                    }
                });
            }))
            .on_action(cx.listener(|this, _: &actions::NewTerminal, _, cx| {
                this.quick
                    .update(cx, |quick, cx| quick.request(QuickEvent::NewTab, cx));
            }))
            .on_action(cx.listener(|this, _: &actions::ClosePaneItem, _, cx| {
                this.close_active(cx);
            }))
            .on_action(cx.listener(|this, _: &actions::CloseTerminalTab, _, cx| {
                this.close_active(cx);
            }))
            .on_action(cx.listener(|this, _: &actions::ActivateNextTab, _, cx| {
                this.quick
                    .update(cx, |quick, cx| quick.select_relative(true, cx));
            }))
            .on_action(
                cx.listener(|this, _: &actions::ActivatePreviousTab, _, cx| {
                    this.quick
                        .update(cx, |quick, cx| quick.select_relative(false, cx));
                }),
            )
            .child(panel)
    }
}

impl QuickPanelView {
    fn close_active(&mut self, cx: &mut Context<Self>) {
        self.quick.update(cx, |quick, cx| {
            if let Some(session) = quick.active_session() {
                quick.request(QuickEvent::CloseTab(session), cx);
            }
        });
    }
}

/// How long after the panel left an owner keeps its window (the exit animation, and a frame).
fn exit_time(reduce: bool) -> Duration {
    Kind::Toast.exit_duration(reduce) + Duration::from_millis(50)
}

/// The quick terminal's state inside the workbench.
pub(crate) struct QuickControl {
    pub(crate) model: Entity<QuickTerminal>,
    /// The shortcut setting the system was last asked about, whatever the outcome (a refused
    /// shortcut is reported once, not on every settings reload).
    shortcut_tried: Option<String>,
    shortcut_active: bool,
    /// The window closes when this ends, unless the panel was shown again.
    exit: Option<Task<()>>,
    /// The main window, which a tab moves to.
    pub(crate) _events: Option<Subscription>,
}

impl QuickControl {
    pub(crate) fn new(cx: &mut App) -> Self {
        Self {
            model: cx.new(|_| QuickTerminal::default()),
            shortcut_tried: None,
            shortcut_active: false,
            exit: None,
            _events: None,
        }
    }
}

impl Workbench {
    // ---- the shortcut -----------------------------------------------------------------------

    /// The quick terminal's model (tests and the status of the panel).
    pub fn quick_terminal(&self) -> &Entity<QuickTerminal> {
        &self.quick.model
    }

    /// Registers `terminal.quick_hotkey` when it (or whether it is on) changed since the system
    /// was last asked. A shortcut that cannot be registered (taken by another app, not a
    /// shortcut, or the Mawas shortcut) is reported once in the window; the setting is left
    /// alone.
    pub(crate) fn sync_quick_hotkey(&mut self, cx: &mut Context<Self>) {
        let settings = self.settings.get::<TerminalSettings>();
        let wanted = if settings.quick_terminal {
            settings.quick_hotkey.trim().to_owned()
        } else {
            String::new()
        };
        let assistant = self.settings.get::<crate::settings::AssistantSettings>();
        let mawas = if assistant.enabled {
            assistant.global_shortcut.trim().to_owned()
        } else {
            String::new()
        };
        // Both settings decide the registration: the Mawas shortcut may free or take the key.
        let key = format!("{wanted}\u{0}{mawas}");
        if self.quick.shortcut_tried.as_deref() == Some(key.as_str()) {
            return;
        }
        self.quick.shortcut_tried = Some(key);
        if self.quick.shortcut_active {
            self.mac.quick_hotkey.unregister();
            self.quick.shortcut_active = false;
        }
        if wanted.is_empty() {
            return;
        }
        let notice = match register_quick(&*self.mac.quick_hotkey, &wanted, &mawas) {
            Ok(Some(warning)) => {
                self.quick.shortcut_active = true;
                Some(warning)
            }
            Ok(None) => {
                self.quick.shortcut_active = true;
                None
            }
            Err(text) => Some(text),
        };
        if let Some(text) = notice {
            tracing::warn!("{text}");
            self.set_notice(text, cx);
        }
    }

    // ---- showing and hiding -----------------------------------------------------------------

    /// The shortcut (or *Terminal: Toggle Quick Terminal*): shows the panel, or hides it.
    pub fn toggle_quick_terminal(&mut self, cx: &mut Context<Self>) {
        if self.quick.model.read(cx).wanted {
            self.hide_quick_terminal(true, cx);
        } else {
            self.show_quick_terminal(cx);
        }
    }

    /// Drops the panel down on the screen the pointer is on and gives it the keyboard. Does
    /// nothing while another app is reading a password (secure keyboard input): taking the
    /// keyboard then would steal it from that app.
    pub fn show_quick_terminal(&mut self, cx: &mut Context<Self>) {
        if self.quick.model.read(cx).wanted {
            self.quick
                .model
                .update(cx, |quick, cx| quick.select(quick.active, cx));
            return;
        }
        if self.mac.quick_panel.secure_input() {
            tracing::info!("the quick terminal stays hidden: another app has secure input");
            return;
        }
        self.mac.quick_panel.remember_front_app();
        self.quick.exit = None;
        let settings = self.settings.get::<TerminalSettings>();
        self.quick.model.update(cx, |quick, cx| {
            quick.wanted = true;
            quick.focus_epoch += 1;
            cx.notify();
        });
        if self.quick.model.read(cx).window.is_none() {
            self.open_quick_window(&settings, cx);
        }
        self.sync_quick_tabs(cx);
        if self.quick.model.read(cx).tabs.is_empty() {
            self.start_quick_session(true, cx);
        }
        cx.notify();
    }

    /// Slides the panel away; the sessions stay. `restore`: give the keyboard back to the app
    /// that had it (not when Hutan itself is about to come forward).
    pub fn hide_quick_terminal(&mut self, restore: bool, cx: &mut Context<Self>) {
        if !self.quick.model.read(cx).wanted {
            return;
        }
        self.quick.model.update(cx, |quick, cx| {
            quick.wanted = false;
            cx.notify();
        });
        if restore {
            self.mac.quick_panel.restore_front_app();
        }
        let model = self.quick.model.clone();
        let wait = exit_time(cx.reduce_motion());
        self.quick.exit = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |this, cx| {
                // Shown again meanwhile: the window stays.
                if model.read(cx).wanted {
                    return;
                }
                this.close_quick_window(cx);
            })
            .ok();
        }));
        cx.notify();
    }

    fn close_quick_window(&mut self, cx: &mut Context<Self>) {
        self.quick.exit = None;
        let Some(window) = self.quick.model.update(cx, |quick, _| quick.window.take()) else {
            return;
        };
        window
            .update(cx, |_, window, _| window.remove_window())
            .ok();
    }

    fn open_quick_window(&mut self, settings: &TerminalSettings, cx: &mut Context<Self>) {
        let pointer = self.mac.quick_panel.pointer();
        let displays = cx.displays();
        let display = pointer
            .and_then(|p| {
                displays
                    .iter()
                    .find(|display| display.bounds().contains(&point(px(p.x), px(p.y))))
                    .cloned()
            })
            .or_else(|| cx.primary_display());
        let (bounds, display_id): (Bounds<gpui::Pixels>, Option<DisplayId>) = match &display {
            Some(display) => {
                let screen = display.bounds();
                // Below the menu bar of that screen, not over it.
                let top = pointer
                    .filter(|p| screen.contains(&point(px(p.x), px(p.y))))
                    .map_or(0.0, |p| p.menu_bar);
                let usable = (f32::from(screen.size.height) - top).max(1.0);
                let height = (usable * settings.quick_height_fraction()).round();
                (
                    Bounds::new(point(px(0.0), px(top)), size(screen.size.width, px(height))),
                    Some(display.id()),
                )
            }
            None => (
                Bounds::new(point(px(0.0), px(0.0)), size(px(1200.0), px(400.0))),
                None,
            ),
        };
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            focus: true,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            display_id,
            window_background: WindowBackgroundAppearance::Transparent,
            ..WindowOptions::default()
        };
        let model = self.quick.model.clone();
        let escape_hides = settings.quick_escape_hides;
        match cx.open_window(options, move |window, cx| {
            cx.new(|cx| QuickPanelView::new(model, escape_hides, window, cx))
        }) {
            Ok(handle) => {
                self.quick
                    .model
                    .update(cx, |quick, _| quick.window = Some(handle));
            }
            Err(error) => {
                tracing::warn!("cannot open the quick terminal's window: {error}");
                self.quick.model.update(cx, |quick, cx| {
                    quick.wanted = false;
                    cx.notify();
                });
                self.set_notice(format!("Cannot open the quick terminal: {error}"), cx);
            }
        }
    }

    // ---- sessions ---------------------------------------------------------------------------

    /// Starts a session for a new tab: the first one where `quick_start_in` says, later ones
    /// where the active tab is.
    fn start_quick_session(&mut self, first: bool, cx: &mut Context<Self>) {
        let settings = self.settings.get::<TerminalSettings>();
        let (workspace, cwd) = {
            let store = self.sessions.read(cx);
            let next_to = self.quick.model.read(cx).active_session();
            match (first, settings.quick_start_in, next_to) {
                (false, _, Some(session)) => self.session_split_target(Some(session), cx),
                (_, QuickStart::Project, _) => {
                    let workspace = self.target_workspace(store);
                    let cwd = workspace
                        .and_then(|w| w.roots.first().cloned())
                        .or_else(std::env::home_dir);
                    (workspace.map(|w| w.id), cwd)
                }
                _ => (store.home_workspace().map(|w| w.id), std::env::home_dir()),
            }
        };
        let spec = self.new_terminal_spec(workspace, cwd, cx);
        let task = self
            .sessions
            .update(cx, |store, cx| store.create_session(workspace, spec, cx));
        self.quick
            .model
            .update(cx, |quick, cx| quick.set_starting(true, cx));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(info) => this.attach_quick_session(info.id, cx),
                Err(error) => {
                    let text = format!("Cannot start a terminal: {error}");
                    this.quick
                        .model
                        .update(cx, |quick, cx| quick.fail(text.clone(), cx));
                    this.set_notice(text, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn attach_quick_session(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let attached = self
            .sessions
            .update(cx, |store, cx| store.terminal_session(session, cx));
        self.quick.model.update(cx, |quick, cx| match attached {
            Some(attached) => quick.push_tab(session, attached, cx),
            None => quick.fail("The new terminal has no screen to show", cx),
        });
    }

    /// Drops the tabs whose session ended or went, and hides the panel when none is left (the
    /// shell exited). Runs on every change of the sessions.
    pub(crate) fn sync_quick_tabs(&mut self, cx: &mut Context<Self>) {
        let gone: Vec<SessionId> = {
            let store = self.sessions.read(cx);
            if !store.synced() {
                return;
            }
            self.quick
                .model
                .read(cx)
                .tabs
                .iter()
                .map(|tab| tab.session)
                .filter(|session| {
                    store.session(*session).is_none_or(|info| {
                        matches!(
                            info.state,
                            SessionState::Exited { .. }
                                | SessionState::Failed { .. }
                                | SessionState::Stopped
                        )
                    })
                })
                .collect()
        };
        if gone.is_empty() {
            return;
        }
        self.quick.model.update(cx, |quick, cx| {
            for session in gone {
                quick.remove_tab(session, cx);
            }
        });
        let model = self.quick.model.read(cx);
        if model.wanted && model.tabs.is_empty() && !model.starting {
            self.hide_quick_terminal(true, cx);
        }
    }

    // ---- what the panel asks ----------------------------------------------------------------

    pub(crate) fn on_quick_event(
        &mut self,
        event: &QuickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            QuickEvent::Hide => self.hide_quick_terminal(true, cx),
            QuickEvent::NewTab => self.start_quick_session(false, cx),
            QuickEvent::CloseTab(session) => self.close_quick_tab(*session, cx),
            QuickEvent::MoveToMain(session) => self.move_quick_tab_to_main(*session, window, cx),
            QuickEvent::OpenLink(target) => {
                self.open_terminal_link(target, window, cx);
            }
            QuickEvent::Notice(text) => self.set_notice(text.clone(), cx),
        }
    }

    /// ⌘W: the tab goes. `tabs.close_session` decides what happens to its session, as for a tab
    /// of the main window: closed with it (`always` stops what runs in it; `when_idle` keeps a
    /// session that runs something, with a notice), or kept running and listed in the sidebar
    /// (`never`). The panel hides when it was the last tab.
    pub fn close_quick_tab(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let removed = self
            .quick
            .model
            .update(cx, |quick, cx| quick.remove_tab(session, cx));
        if !removed {
            return;
        }
        let mode = self.close_session_mode();
        if mode != crate::settings::CloseSessionMode::Never {
            let stop = mode == crate::settings::CloseSessionMode::Always;
            let task = self
                .sessions
                .update(cx, |store, cx| store.close(session, stop, cx));
            cx.spawn(async move |this, cx| {
                let notice = match task.await {
                    Ok(()) => return,
                    Err(crate::sessions::CloseFailure::Busy(busy)) => {
                        format!("The session was kept: {}", super::closing::busy_text(&busy))
                    }
                    Err(crate::sessions::CloseFailure::Failed(error)) => {
                        format!("The session was kept: it could not be closed ({error})")
                    }
                };
                this.update(cx, |this, cx| this.set_notice(notice, cx)).ok();
            })
            .detach();
        }
        if self.quick.model.read(cx).tabs.is_empty() {
            self.hide_quick_terminal(true, cx);
        }
    }

    /// ⌘⇧↩: the session is a tab of the main window now, which comes forward; the panel hides
    /// without handing the keyboard back to the app that had it.
    pub fn move_quick_tab_to_main(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self
            .quick
            .model
            .read(cx)
            .tabs
            .iter()
            .any(|tab| tab.session == session)
        {
            return;
        }
        // The main window holds the session before the panel lets go of it, so it is never
        // detached in between.
        self.bring_forward(window, cx);
        self.open_session(session, window, cx);
        self.quick
            .model
            .update(cx, |quick, cx| quick.remove_tab(session, cx));
        self.hide_quick_terminal(false, cx);
    }

    /// Subscribes the workbench to what its panel asks (built once, with the workbench).
    pub(crate) fn watch_quick_events(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model = self.quick.model.clone();
        self.quick._events = Some(cx.subscribe_in(
            &model,
            window,
            |this, _, event: &QuickEvent, window, cx| this.on_quick_event(event, window, cx),
        ));
    }
}

/// Registers `wanted` on `hotkey` unless it is the Mawas shortcut (`mawas`, also written like a
/// keystroke). `Ok(Some(text))`: registered, but macOS or a launcher gives it a meaning of its
/// own. `Err(text)`: not registered, and why.
fn register_quick(
    hotkey: &dyn crate::macos::GlobalHotkey,
    wanted: &str,
    mawas: &str,
) -> Result<Option<String>, String> {
    let shortcut = Shortcut::parse(wanted).map_err(|error| {
        format!(
            "The quick terminal shortcut \"{wanted}\" is not valid: {error} (terminal.quick_hotkey)"
        )
    })?;
    if !mawas.is_empty()
        && Shortcut::parse(mawas).is_ok_and(|other| {
            other.key_code == shortcut.key_code && other.modifiers == shortcut.modifiers
        })
    {
        return Err(format!(
            "The quick terminal shortcut {} is also Mawas's system-wide shortcut \
             (assistant.global_shortcut); it was left unregistered. Change one of them.",
            shortcut.display
        ));
    }
    match hotkey.register(&shortcut) {
        Ok(()) => Ok(shortcut.known_conflict().map(|owner| {
            format!(
                "The quick terminal shortcut {} is also used by {owner}; both may react to it. \
                 Change terminal.quick_hotkey if that is not what you want.",
                shortcut.display
            )
        })),
        Err(HotkeyError::Taken) => Err(format!(
            "The quick terminal shortcut {} is not available: another app already uses it. The \
             setting terminal.quick_hotkey is unchanged; the quick terminal is also in the \
             command palette.",
            shortcut.display
        )),
        Err(error) => Err(format!(
            "The quick terminal shortcut {} is not available: {error}. The setting \
             terminal.quick_hotkey is unchanged.",
            shortcut.display
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macos::FakeHotkey;

    #[test]
    fn a_shortcut_that_is_not_one_is_refused_with_the_setting_named() {
        let hotkey = FakeHotkey::new();
        let text = register_quick(&hotkey, "space", "").unwrap_err();
        assert!(text.contains("terminal.quick_hotkey"), "{text}");
        assert!(hotkey.attempts().is_empty());
    }

    #[test]
    fn the_mawas_shortcut_wins_a_clash() {
        let hotkey = FakeHotkey::new();
        let text = register_quick(&hotkey, "ctrl-alt-k", "alt-ctrl-k").unwrap_err();
        assert!(text.contains("Mawas"), "{text}");
        assert!(hotkey.attempts().is_empty());
        // Another combination is fine.
        assert_eq!(register_quick(&hotkey, "ctrl-alt-k", "alt-space"), Ok(None));
        assert_eq!(hotkey.active().as_deref(), Some("⌃⌥K"));
    }

    #[test]
    fn a_taken_shortcut_is_reported_and_a_known_one_warns() {
        let hotkey = FakeHotkey::new();
        hotkey.take("⌃⇧`");
        let text = register_quick(&hotkey, "ctrl-shift-grave", "").unwrap_err();
        assert!(text.contains("another app"), "{text}");
        let warning = register_quick(&hotkey, "alt-space", "").unwrap().unwrap();
        assert!(warning.contains("Raycast"), "{warning}");
        assert_eq!(hotkey.active().as_deref(), Some("⌥Space"));
    }

    /// The default must not be a key Hutan binds inside the app: a system-wide hot key wins over
    /// the app's own keymap, so the quick terminal would eat that key in every Hutan window.
    /// (⌃` toggles the bottom dock in the Zed base, which is why the default is ⌃⇧`.)
    #[test]
    fn the_default_shortcut_is_not_a_key_hutan_binds() {
        let default = Shortcut::parse(&TerminalSettings::default().quick_hotkey).unwrap();
        assert_eq!(default.known_conflict(), None);
        for base in crate::settings::KeymapBase::ALL {
            for binding in super::actions::default_bindings(base) {
                let keys = hutan_settings::keymap::format_keys(&binding.keys);
                if let Ok(bound) = Shortcut::parse(&keys) {
                    assert!(
                        bound.key_code != default.key_code || bound.modifiers != default.modifiers,
                        "{base:?}: {keys} is bound to {} in the app",
                        binding.action
                    );
                }
            }
        }
    }
}
