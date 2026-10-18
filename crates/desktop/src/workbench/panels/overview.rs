//! The `overview` panel: a board of session cards with live previews.
//!
//! Every preview draws from the same [`SharedTerminal`] as the session's pane tab (one model per
//! terminal), at a reduced font size, in [`ViewMode::Preview`]: it never resizes the terminal,
//! never takes input, and shows the newest rows that fit the card.

use crate::theme::DensityExt as _;
use std::collections::HashMap;

use gpui::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Window, div, prelude::*, px,
};
use hutan_core::SessionId;
use hutan_protocol::messages::SessionInfo;
use hutan_terminal_view::{TerminalView, TerminalViewSettings, ViewMode};

use super::common::{
    age_label, agent_badge, kind_icon, panel_header, placement, state_dot, time_tooltip, tone_color,
};
use crate::sessions::TerminalSession;
use crate::settings::{OverviewPanelSettings, PreviewMode, SortOrder};
use crate::workbench::BadgeSite;
use crate::workbench::actions::{OpenSessionFromOverview, SessionMenuAt, ShowSessionMenu};
use crate::workbench::registry::PanelEnv;
use crate::workbench::style::{TerminalStyle, UiStyle};
use crate::workbench::ui::{self, Mascot, StyledExt as _};

/// Preview text is this fraction of the terminal font size.
const PREVIEW_SCALE: f32 = 0.5;

struct Preview {
    /// Held in `live` mode so the attachment (and the model's updates) lasts as long as the card;
    /// `None` in `static` mode, where the model freezes once the terminal is detached elsewhere.
    _session: Option<Entity<TerminalSession>>,
    view: Entity<TerminalView>,
    mode: PreviewMode,
}

pub struct OverviewPanel {
    env: PanelEnv,
    previews: HashMap<SessionId, Preview>,
}

impl std::fmt::Debug for OverviewPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverviewPanel")
            .field("previews", &self.previews.len())
            .finish_non_exhaustive()
    }
}

fn preview_settings(base: TerminalViewSettings) -> TerminalViewSettings {
    TerminalViewSettings {
        font_size: (base.font_size * PREVIEW_SCALE).max(4.0),
        padding: base.padding * PREVIEW_SCALE,
        cursor: hutan_terminal_view::CursorSettings {
            blink: false,
            ..base.cursor
        },
        blocks: hutan_terminal_view::BlockSettings {
            gutter_width: base.blocks.gutter_width * PREVIEW_SCALE,
            ..base.blocks
        },
        ..base
    }
}

impl OverviewPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        cx.observe(&env.sessions, |_, _, cx| cx.notify()).detach();
        Self {
            env,
            previews: HashMap::new(),
        }
    }

    /// Sessions with a live preview right now (tests).
    pub fn preview_sessions(&self) -> Vec<SessionId> {
        self.previews.keys().copied().collect()
    }

    /// The preview view of `session`, if it has one (tests).
    pub fn preview_view(&self, session: SessionId) -> Option<&Entity<TerminalView>> {
        self.previews.get(&session).map(|p| &p.view)
    }

    fn sorted(&self, settings: &OverviewPanelSettings, cx: &gpui::App) -> Vec<SessionInfo> {
        let store = self.env.sessions.read(cx);
        let mut sessions: Vec<SessionInfo> = store.sessions().to_vec();
        match settings.sort {
            SortOrder::Attention => {
                // Stable: sessions that need the user first, the rest in creation order.
                sessions.sort_by_key(|s| !store.presentation(s).needs_you);
            }
            SortOrder::Recent => sessions.sort_by_key(|s| std::cmp::Reverse(s.created_at_ms)),
            SortOrder::Name => sessions.sort_by_key(|s| s.title.to_lowercase()),
        }
        sessions
    }

    /// Creates, keeps and drops preview views so exactly the cards that show one have one.
    fn sync_previews(
        &mut self,
        sessions: &[SessionInfo],
        mode: PreviewMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let wanted: Vec<SessionId> = if mode == PreviewMode::Off {
            Vec::new()
        } else {
            sessions
                .iter()
                .filter(|s| s.terminal_id.is_some())
                .map(|s| s.id)
                .collect()
        };
        self.previews
            .retain(|id, preview| wanted.contains(id) && preview.mode == mode);
        let settings = preview_settings(TerminalStyle::get(cx));
        for id in wanted {
            if let Some(existing) = self.previews.get(&id) {
                existing
                    .view
                    .update(cx, |view, cx| view.update_settings(settings.clone(), cx));
                continue;
            }
            let sessions_store = self.env.sessions.clone();
            let session = match mode {
                PreviewMode::Live => sessions_store.update(cx, |s, cx| s.terminal_session(id, cx)),
                PreviewMode::Static => sessions_store.read(cx).attached(id),
                PreviewMode::Off => None,
            };
            let Some(session) = session else { continue };
            let (shared, backend) = {
                let session = session.read(cx);
                (session.shared().clone(), session.backend())
            };
            let settings = settings.clone();
            let view = cx.new(|cx| {
                TerminalView::with_shared(shared, backend, settings, ViewMode::Preview, window, cx)
            });
            self.previews.insert(
                id,
                Preview {
                    _session: (mode == PreviewMode::Live).then_some(session),
                    view,
                    mode,
                },
            );
        }
    }
}

impl Render for OverviewPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.env.regions.rendered(cx.entity_id());
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let settings = self.env.settings.get::<OverviewPanelSettings>();
        let sessions = self.sorted(&settings, cx);
        self.sync_previews(&sessions, settings.preview, window, cx);
        let (card_width, preview_height) = settings.card_size.dimensions();
        let hover = theme.surface_hover;

        let cards: Vec<AnyElement> = sessions
            .iter()
            .map(|info| {
                let presentation = self.env.sessions.read(cx).presentation(info);
                let id = info.id.to_string();
                let open_id = id.clone();
                let menu_id = id.clone();
                let preview = self.previews.get(&info.id).map(|p| p.view.clone());
                let badge = {
                    let badge = self.env.sessions.read(cx).agent_badge(info.id);
                    agent_badge(
                        &theme,
                        &badge,
                        BadgeSite::Card,
                        info.id,
                        &self.env.regions,
                        cx,
                    )
                };
                let subtitle = if presentation.detail.is_empty() {
                    presentation.label.clone()
                } else {
                    format!("{} · {}", presentation.label, presentation.detail)
                };
                let border = if presentation.needs_you {
                    tone_color(&theme, presentation.tone)
                } else {
                    theme.border
                };
                div()
                    .id(SharedString::from(format!("card-{id}")))
                    .flex()
                    .flex_col()
                    .w(px(card_width))
                    .card(&theme, &tokens)
                    .border_color(border)
                    .overflow_hidden()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |_, _, window, cx| {
                        window.dispatch_action(
                            Box::new(OpenSessionFromOverview(open_id.clone())),
                            cx,
                        );
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |_, event: &MouseDownEvent, window, cx| {
                            window.dispatch_action(
                                Box::new(ShowSessionMenu(SessionMenuAt {
                                    session: menu_id.clone(),
                                    position: event.position,
                                })),
                                cx,
                            );
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(tokens.spacing * 1.5))
                            .h(px(tokens.sidebar_row))
                            .px(px(tokens.spacing * 2.0))
                            .child(state_dot(&theme, &presentation))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme.muted)
                                    .child(kind_icon(info.kind)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(SharedString::from(info.title.clone())),
                            )
                            .children(badge)
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(tokens.secondary_font_size))
                                    .text_color(tone_color(&theme, presentation.tone))
                                    .child(SharedString::from(subtitle)),
                            ),
                    )
                    .when_some(preview, |card, view| {
                        card.child(
                            div()
                                .h(px(preview_height))
                                .w_full()
                                .overflow_hidden()
                                .child(view),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .h(px(tokens.sidebar_row - 4.0))
                            .px(px(tokens.spacing * 2.0))
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child(SharedString::from(placement(info)))
                            .child(
                                div()
                                    .id(SharedString::from(format!("card-age-{}", info.id)))
                                    .tooltip(time_tooltip(info.created_at_ms))
                                    .child(SharedString::from(age_label(info.created_at_ms))),
                            ),
                    )
                    .into_any_element()
            })
            .collect();

        let body: AnyElement = if cards.is_empty() {
            ui::empty_state(
                Mascot::Empty,
                "No sessions yet. ⌘T starts a terminal.",
                None,
                &theme,
                &tokens,
            )
            .h(px(360.0))
            .into_any_element()
        } else {
            div()
                .id("overview-board")
                .flex()
                .flex_wrap()
                .content_start()
                .gap(px(tokens.spacing * 3.0))
                .p(px(tokens.spacing * 3.0))
                .children(cards)
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .child(panel_header("Overview", "panels.overview", cx))
            .child(
                div()
                    .id("overview-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
    }
}
