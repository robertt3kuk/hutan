//! Drawing the History panel (`history.rs` holds its state and requests): the header, the
//! Workspace / Project / All control, the search field, the scan strip and the index controls
//! behind it, the list bar, the virtualized grouped list with its expandable rows, the menus, and
//! the off-state card.

use std::rc::Rc;

use gpui::{
    AnyElement, AnyView, App, ClickEvent, Context, HighlightStyle, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, StyledText, Window, anchored, deferred, div, point,
    prelude::*, px,
};
use hutan_client::ConnectionState;
use hutan_protocol::history::{HistoryScope, HistorySessionSort, HistorySource};

use super::common::time_tooltip;
use super::history::{HistoryPanel, Index, Menu, Mode, Results, shared};
use super::history_catalog::{
    Entry, GroupBy, RowAction, SEARCH_LIMIT, SessionItem, blocker, list_bar, results_bar,
    scan_line, sort_label, turn_label,
};
use super::history_model::{SOURCES, format_bytes, source_icon, source_label};
use crate::icons::{Icon, icon};
use crate::theme::{DensityExt as _, Theme, Tokens};
use crate::workbench::Region;
use crate::workbench::actions::OpenSettingsFor;
use crate::workbench::render::probe;
use crate::workbench::style::UiStyle;
use crate::workbench::ui::{self, ButtonKind, FocusRingExt as _, Mascot, StyledExt as _};
use crate::workbench::vim_ui::{self, VimConfig};

/// A chip: a compact filter toggle; the chosen one (`active`) is an `accent_soft` fill without
/// the outline (the Tasks, Changes and Agents panels use it too).
pub(crate) fn chip(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    theme: &Theme,
    tokens: &Tokens,
) -> ui::Button {
    ui::Button::new(id, label, theme, tokens)
        .compact()
        .selected(active)
}

/// Right (and left) padding of a row: the preview and the title never touch the panel's edge.
const ROW_PAD: f32 = 12.0;

/// The latest-turn preview of a row: two lines with "…" when it does not fit, all of it while
/// the row is expanded. It fills the row's width and may shrink below its text.
fn preview_style<E: Styled>(element: E, expanded: bool) -> E {
    let element = element.w_full().min_w_0();
    if expanded {
        element
    } else {
        element.clamp_lines(2)
    }
}

/// What a menu entry does when it is picked.
#[derive(Clone, Copy)]
enum Pick {
    Group(GroupBy),
    Source(HistorySource),
    Sort(HistorySessionSort),
    Row(RowAction, usize),
}

/// The tooltip of a time shown relative: the absolute local time.
type Tooltip = Rc<dyn Fn(&mut Window, &mut App) -> AnyView>;

/// What a history row shows that takes work to make (see [`HistoryPanel::row_text`]).
pub(crate) struct RowText {
    minute: u64,
    title: SharedString,
    meta: SharedString,
    tooltip: Option<Tooltip>,
}

impl Render for HistoryPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.env.regions.rendered(cx.entity_id());
        self.mark_drawn();
        self.show_snippets = self
            .env
            .settings
            .get::<crate::settings::HistorySettings>()
            .show_snippets;
        self.render_ms = super::history::now_ms();
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let narrow = self.is_narrow();
        let enabled = matches!(&self.index, Index::Known(info) if info.enabled);
        let body = match &self.index {
            Index::Unsupported => self.render_note(
                "This hutand does not offer history search (protocol 0.8). Restart it from the \
                 Daemon menu after updating Hutan.",
                &theme,
                &tokens,
            ),
            Index::Unknown => {
                let connected =
                    self.env.sessions.read(cx).connection_state() == ConnectionState::Connected;
                let text = if connected {
                    "Reading the index state…"
                } else {
                    "Waiting for the daemon…"
                };
                // The field is there before the state is known, so what is typed right after
                // *Search history…* lands in it instead of in whatever had the keyboard.
                div()
                    .flex()
                    .flex_col()
                    .child(self.render_search(window, &theme, &tokens, cx))
                    .child(self.render_note(text, &theme, &tokens))
                    .into_any_element()
            }
            Index::Known(info) if !info.enabled => self.render_disabled(&theme, &tokens, cx),
            Index::Known(_) => self.render_enabled(window, &theme, &tokens, cx),
        };
        let menu = self.render_menu(&theme, &tokens, cx);
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .child(self.render_header(narrow, enabled, &theme, &tokens, cx))
            .child(body)
            .child(probe(&self.env.regions, Region::History("panel")))
            .children(menu)
    }
}

impl HistoryPanel {
    // ---- header ----------------------------------------------------------------------------

    fn render_header(
        &self,
        narrow: bool,
        enabled: bool,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let catalog = enabled && self.catalog_supported(cx);
        let title = if narrow {
            "History"
        } else {
            "Agent Session History"
        };
        let subtitle = if enabled {
            "Indexed history"
        } else {
            "Resume past sessions"
        };
        let mut controls = div().flex().flex_none().items_center().gap(px(2.0));
        if catalog && self.mode() == Mode::Catalog {
            controls = controls.child(
                ui::IconButton::new(
                    "history-group",
                    Icon::ActionGroup,
                    "Group by",
                    theme,
                    tokens,
                )
                .compact()
                .on_click(cx.listener(|this, e: &ClickEvent, _, cx| {
                    this.open_menu(Menu::GroupBy(e.position()), cx);
                }))
                .child(probe(&self.env.regions, Region::History("group"))),
            );
        }
        if enabled {
            controls = controls
                .child(
                    ui::IconButton::new(
                        "history-sources",
                        Icon::ActionFilter,
                        "Sources",
                        theme,
                        tokens,
                    )
                    .compact()
                    .on_click(cx.listener(|this, e: &ClickEvent, _, cx| {
                        this.open_menu(Menu::Sources(e.position()), cx);
                    }))
                    .child(probe(&self.env.regions, Region::History("sources"))),
                )
                .child(
                    ui::IconButton::new(
                        "history-refresh",
                        Icon::ActionRefresh,
                        "Refresh",
                        theme,
                        tokens,
                    )
                    .compact()
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_now(cx)))
                    .child(probe(&self.env.regions, Region::History("refresh"))),
                );
        }
        controls = controls.child(
            ui::IconButton::new(
                "history-gear",
                Icon::ActionSettings,
                "History settings",
                theme,
                tokens,
            )
            .compact()
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(OpenSettingsFor("history".to_owned())), cx);
            }),
        );
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap(px(tokens.spacing))
            .px(px(tokens.spacing * 3.0))
            .py(px(tokens.spacing * 1.5))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(tokens.font_size))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(title),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child(subtitle),
                    ),
            )
            .child(controls)
            .into_any_element()
    }

    fn render_note(&self, text: &str, theme: &Theme, tokens: &Tokens) -> AnyElement {
        let mascot = if text.starts_with("Searching") {
            Mascot::Thinking
        } else {
            Mascot::Empty
        };
        ui::empty_state(mascot, text.to_owned(), None, theme, tokens)
            .h(px(260.0))
            .into_any_element()
    }

    // ---- the off-state card ----------------------------------------------------------------

    /// While indexing is off: one quiet line and a button; the sources (unticked), what stays
    /// local and **Enable** only appear after *Set up history…*.
    fn render_disabled(
        &mut self,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.load_card(cx);
        if !self.card_open {
            return self.render_short_card(theme, tokens, cx);
        }
        let busy = self.busy.is_some();
        let none = self.card_sources.iter().all(|on| !on);
        let mut sources = div().flex().flex_col().gap(px(tokens.spacing));
        for (index, (source, place)) in [
            (HistorySource::Hutan, "Hutan terminals and chats"),
            (HistorySource::Claude, "~/.claude/projects"),
            (HistorySource::Codex, "~/.codex/sessions"),
            (HistorySource::Omp, "~/.omp/agent/sessions"),
        ]
        .into_iter()
        .enumerate()
        {
            let on = self.card_sources[index];
            let name = match source {
                HistorySource::Claude => "Claude Code",
                other => source_label(other),
            };
            sources = sources.child(
                div()
                    .id(("history-card-source", index))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .py(px(2.0))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_card_source(index, cx)))
                    .child(
                        div()
                            .flex_none()
                            .size(px(14.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.0))
                            .border_1()
                            // Checked: the fill shows through a transparent edge.
                            .border_color(if on {
                                gpui::transparent_black()
                            } else {
                                theme.faint
                            })
                            .when(on, |s| s.bg(theme.accent).text_color(theme.on_accent()))
                            .text_size(px(10.0))
                            .children(
                                (on).then(|| {
                                    crate::icons::glyph(crate::icons::Icon::StateCompleted)
                                }),
                            ),
                    )
                    .child(
                        div()
                            .w(px(14.0))
                            .flex_none()
                            .text_color(theme.muted)
                            .child(crate::icons::glyph(source_icon(source))),
                    )
                    .child(
                        div().flex_1().min_w_0().child(name).child(
                            div()
                                .truncate()
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.muted)
                                .child(place),
                        ),
                    )
                    .child(probe(
                        &self.env.regions,
                        Region::HistoryItem("source", index),
                    )),
            );
        }
        div()
            .id("history-disabled")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 3.0))
            .px(px(tokens.spacing * 3.0))
            .py(px(tokens.spacing * 2.0))
            .child(
                div()
                    .text_size(px(tokens.font_size + 2.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child("Search your agent sessions"),
            )
            .child(div().text_color(theme.muted).child(
                "Browse, search and resume the sessions of Hutan, Claude Code, Codex and OMP \
                     in one list. Tick what Hutan may read; nothing is read until you press \
                     Enable:",
            ))
            .child(sources)
            .child(
                div()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(
                        "Nothing leaves this machine. Hutan reads the transcripts into one local \
                         index file, with secrets masked as far as possible; Clear deletes it.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        ui::Button::new(
                            "history-not-now",
                            "Not now",
                            theme,
                            tokens,
                        )
                        .ghost()
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| this.close_card(cx)))
                        .child(probe(&self.env.regions, Region::History("not-now"))),
                    )
                    .child(
                        ui::Button::new(
                            "history-enable",
                            self.busy.unwrap_or("Enable"),
                            theme,
                            tokens,
                        )
                        .primary()
                        .loading(busy)
                        .disabled(none)
                        .on_click(cx.listener(|this, _, _, cx| this.enable_indexing(cx)))
                        .child(probe(&self.env.regions, Region::History("enable"))),
                    )
                    .child(
                        ui::Button::new("history-settings-link", "Settings ›", theme, tokens)
                            .link()
                            .on_click(|_, window, cx| {
                                window.dispatch_action(
                                    Box::new(OpenSettingsFor("history".to_owned())),
                                    cx,
                                );
                            })
                            .child(probe(&self.env.regions, Region::History("settings"))),
                    ),
            )
            .when_some(self.error.clone(), |s, error| {
                s.child(div().text_color(theme.danger).child(shared(error)))
            })
            .into_any_element()
    }

    /// The calm default of the off state: what this is, that it is off, one quiet action.
    fn render_short_card(
        &mut self,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("history-disabled")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 2.0))
            .px(px(tokens.spacing * 3.0))
            .py(px(tokens.spacing * 3.0))
            .child(
                div()
                    .text_size(px(tokens.font_size + 1.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child("Search your agent sessions"),
            )
            .child(
                div()
                    .text_color(theme.muted)
                    .child("Off until you choose sources."),
            )
            .child(
                div().flex().child(
                    ui::Button::new("history-setup", "Set up history…", theme, tokens)
                        .on_click(cx.listener(|this, _, _, cx| this.open_card(cx)))
                        .child(probe(&self.env.regions, Region::History("setup"))),
                ),
            )
            .into_any_element()
    }

    // ---- indexing on -----------------------------------------------------------------------

    fn render_enabled(
        &mut self,
        window: &mut Window,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.take_focus_wanted() {
            cx.defer_in(window, |this, window, cx| {
                window.focus(&this.search_focus, cx);
            });
        }
        let catalog = self.catalog_supported(cx);
        let mut column = div().flex_1().min_h_0().flex().flex_col();
        if !catalog {
            column = column.child(
                div()
                    .px(px(tokens.spacing * 3.0))
                    .pb(px(tokens.spacing))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.warning)
                    .child(
                        "This hutand predates the session list (protocol 0.29): showing search \
                         results only. Restart it from the Daemon menu after updating Hutan.",
                    ),
            );
        }
        column = column
            .child(self.render_scope_bar(theme, tokens, cx))
            .child(self.render_search(window, theme, tokens, cx))
            .child(self.render_strip(catalog, theme, tokens, cx));
        if self.scan_open() {
            column = column.child(self.render_index_details(theme, tokens, cx));
        }
        column
            .child(self.render_list_bar(catalog, theme, tokens, cx))
            .child(self.render_list(catalog, theme, tokens, cx))
            .into_any_element()
    }

    fn render_scope_bar(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let project_known = self.project_scope_available(cx);
        let mut bar = div()
            .flex()
            .flex_none()
            .p(px(2.0))
            .mx(px(tokens.spacing * 3.0))
            .mb(px(tokens.spacing * 1.5))
            .rounded(px(tokens.radius))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border);
        for (scope, label, name, enabled) in [
            (
                HistoryScope::Workspace,
                "Workspace",
                "scope-workspace",
                true,
            ),
            (
                HistoryScope::Project,
                "Project",
                "scope-project",
                project_known,
            ),
            (HistoryScope::All, "All", "scope-all", true),
        ] {
            let active = self.scope() == scope;
            let (hover, fg, muted, faint) =
                (theme.surface_hover, theme.fg, theme.muted, theme.faint);
            bar = bar.child(
                div()
                    .id(name)
                    .relative()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(tokens.font_size + 8.0))
                    .rounded(px(tokens.radius - 2.0))
                    .text_size(px(tokens.secondary_font_size))
                    .when(active, |s| {
                        s.bg(theme.right_sidebar_accent)
                            .text_color(fg)
                            .font_weight(gpui::FontWeight::MEDIUM)
                    })
                    .when(!active && enabled, move |s| {
                        s.text_color(muted)
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                    })
                    .when(!enabled, move |s| s.text_color(faint))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .when(enabled, |s| {
                        s.on_click(cx.listener(move |this, _, _, cx| this.set_scope(scope, cx)))
                    })
                    .child(label)
                    .child(probe(&self.env.regions, Region::History(name))),
            );
        }
        bar.into_any_element()
    }

    fn render_search(
        &mut self,
        window: &mut Window,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focused = self.search_focus.is_focused(window);
        div()
            .id("history-search")
            .key_context("HistorySearch")
            .track_focus(&self.search_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_key(event, window, cx);
            }))
            .relative()
            .mx(px(tokens.spacing * 3.0))
            .mb(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .h(px(32.0))
            .flex_none()
            .flex()
            .items_center()
            .outlined(theme, tokens)
            .focus_ring(focused, theme, tokens)
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, window, cx| {
                    // The dock re-focuses the workbench on this same mouse down; take focus
                    // after the event has been handled.
                    cx.defer_in(window, |this, window, cx| {
                        window.focus(&this.search_focus, cx);
                    });
                }),
            )
            .child(crate::icons::icon(Icon::ActionSearch, 14.0, theme.muted))
            .children(vim_ui::string_field(
                "history-search-text",
                &self.query_edit,
                &self.query,
                VimConfig::of(&self.env.settings).enabled,
                "Search sessions…",
                focused,
                false,
                self.search_field(cx),
                theme,
                tokens,
            ))
            .child(probe(&self.env.regions, Region::History("search")))
            .into_any_element()
    }

    /// `✓ Scanned 986 files · Just now ⌄`: click for the per-source breakdown and the index
    /// controls.
    fn render_strip(
        &self,
        catalog: bool,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = match (&self.catalog, self.status()) {
            (Some(answer), _) if catalog => scan_line(&answer.scan, super::history::now_ms()),
            (_, Some(info)) => format!(
                "{} rows indexed",
                info.sources.iter().map(|s| s.rows).sum::<u64>()
            ),
            _ => String::new(),
        };
        let hover = theme.surface_hover;
        div()
            .id("history-scan")
            .relative()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .h(px(28.0))
            .px(px(tokens.spacing * 3.0))
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.muted)
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| this.toggle_scan(cx)))
            .child(div().truncate().child(shared(text)))
            .child(icon(
                if self.scan_open() {
                    Icon::ActionCollapse
                } else {
                    Icon::ActionExpand
                },
                14.0,
                theme.muted,
            ))
            .child(probe(&self.env.regions, Region::History("scan")))
            .into_any_element()
    }

    /// Per-source files and rows, the index size, and Clear / Rebuild / Disable.
    fn render_index_details(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let small = px(tokens.secondary_font_size);
        let mut lines = div().flex().flex_col().gap(px(2.0));
        let (mut rows, mut errors, mut bytes) = (0u64, 0u64, 0u64);
        if let Some(info) = self.status() {
            bytes = info.db_bytes;
            for source in &info.sources {
                rows += source.rows;
                errors += source.errors;
                let files = self
                    .catalog
                    .as_ref()
                    .and_then(|c| c.scan.per_source.iter().find(|s| s.name == source.name))
                    .map_or(source.files, |s| s.files);
                let indexed = source
                    .last_indexed_ms
                    .map_or_else(|| "—".to_owned(), hutan_client::time::relative_to_now);
                let text = if source.name == "hutan" {
                    format!("hutan · {} rows · {indexed}", source.rows)
                } else {
                    format!(
                        "{} · {files} files · {} rows · {indexed}",
                        source.name, source.rows
                    )
                };
                lines = lines.child(div().truncate().child(shared(text)));
            }
        }
        let summary = format!(
            "{rows} rows · {} · {}",
            format_bytes(bytes),
            match errors {
                0 => "no errors".to_owned(),
                1 => "1 unreadable line".to_owned(),
                n => format!("{n} unreadable lines"),
            }
        );
        let busy = self.busy.is_some();
        let controls: AnyElement = match self.pending_confirmation() {
            Some(what) => {
                let question = match what {
                    super::history::Confirm::Clear => {
                        "Delete the whole index? Hutan's own records cannot be rebuilt."
                    }
                    super::history::Confirm::Rebuild => "Read every transcript again?",
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing))
                    .child(div().text_color(theme.warning).child(question))
                    .child(
                        div()
                            .flex()
                            .gap(px(tokens.spacing))
                            .child(
                                ui::Button::new("history-cancel", "Cancel", theme, tokens)
                                    .compact()
                                    .ghost()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.cancel_confirmation(cx);
                                    })),
                            )
                            .child(
                                ui::Button::new(
                                    "history-confirm",
                                    match what {
                                        super::history::Confirm::Clear => "Clear index",
                                        super::history::Confirm::Rebuild => "Rebuild",
                                    },
                                    theme,
                                    tokens,
                                )
                                .compact()
                                .kind(match what {
                                    super::history::Confirm::Clear => ButtonKind::Destructive,
                                    super::history::Confirm::Rebuild => ButtonKind::Primary,
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.confirmed(cx)))
                                .child(probe(&self.env.regions, Region::History("confirm"))),
                            ),
                    )
                    .into_any_element()
            }
            None => div()
                .flex()
                .items_center()
                .gap(px(tokens.spacing))
                .child(
                    ui::Button::new("history-clear", "Clear…", theme, tokens)
                        .compact()
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ask(super::history::Confirm::Clear, cx);
                        }))
                        .child(probe(&self.env.regions, Region::History("clear"))),
                )
                .child(
                    ui::Button::new("history-rebuild", "Rebuild…", theme, tokens)
                        .compact()
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ask(super::history::Confirm::Rebuild, cx);
                        }))
                        .child(probe(&self.env.regions, Region::History("rebuild"))),
                )
                .child(
                    ui::Button::new("history-disable", "Disable", theme, tokens)
                        .compact()
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| this.disable_indexing(cx)))
                        .child(probe(&self.env.regions, Region::History("disable"))),
                )
                .when_some(self.busy, |s, busy| {
                    s.child(div().text_color(theme.muted).child(busy))
                })
                .into_any_element(),
        };
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .px(px(tokens.spacing * 3.0))
            .py(px(tokens.spacing * 1.5))
            .border_t_1()
            .border_b_1()
            .border_color(theme.border)
            .text_size(small)
            .text_color(theme.muted)
            .child(lines)
            .child(div().child(shared(summary)))
            .child(controls)
            .when_some(self.error().map(str::to_owned), |s, error| {
                s.child(div().text_color(theme.danger).child(shared(error)))
            })
            .into_any_element()
    }

    /// `2 of 250 sessions` and the sort dropdown; a search says `N results`.
    fn render_list_bar(
        &self,
        catalog: bool,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (left, right) = match (self.mode(), &self.catalog) {
            (Mode::Search, _) => (
                match &self.results {
                    Results::Hits(hits) => results_bar(hits.len(), SEARCH_LIMIT),
                    _ => "Search failed".to_owned(),
                },
                None,
            ),
            (Mode::Catalog, Some(answer)) if catalog => {
                let mut text = list_bar(answer.sessions.len(), answer.matched, answer.loaded);
                if self.searching() {
                    text.push_str(" · searching transcripts…");
                }
                (text, Some(sort_label(self.sort())))
            }
            (Mode::Catalog, _) if self.searching() => ("Searching…".to_owned(), None),
            (Mode::Catalog, _) if catalog => ("Loading sessions…".to_owned(), None),
            _ => (String::new(), None),
        };
        div()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .h(px(32.0))
            .px(px(tokens.spacing * 3.0))
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.muted)
            .child(
                div()
                    .id("history-count")
                    .relative()
                    .truncate()
                    .child(shared(left))
                    .child(probe(&self.env.regions, Region::History("count"))),
            )
            .when_some(right, |s, label| {
                s.child(
                    ui::Button::new("history-sort", label, theme, tokens)
                        .compact()
                        .ghost()
                        .child(icon(Icon::ActionExpand, 14.0, theme.muted))
                        .on_click(cx.listener(|this, e: &ClickEvent, _, cx| {
                            this.open_menu(Menu::Sort(e.position()), cx);
                        }))
                        .child(probe(&self.env.regions, Region::History("sort"))),
                )
            })
            .into_any_element()
    }

    // ---- the list --------------------------------------------------------------------------

    fn render_list(
        &mut self,
        catalog: bool,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let list = div().flex_1().min_h_0().flex().flex_col();
        let note = match (&self.results, &self.catalog, &self.catalog_error) {
            (Results::Failed(error), _, _) => Some(format!("Search failed: {error}")),
            (Results::Hits(hits), _, _) if hits.is_empty() => Some(
                if self.filters().narrows() {
                    "No session matches. A filter may be hiding some."
                } else {
                    "No session matches."
                }
                .to_owned(),
            ),
            (Results::Idle, _, Some(error)) if self.entries.is_empty() => {
                Some(format!("Cannot list sessions: {error}"))
            }
            (Results::Idle, None, _) if !catalog && self.query.trim().is_empty() => Some(
                "Type a phrase, a command or a file name. Results are one row per session."
                    .to_owned(),
            ),
            (Results::Idle, None, _) if !catalog => Some(if self.searching() {
                "Searching…".to_owned()
            } else {
                "No session matches.".to_owned()
            }),
            (Results::Idle, None, _) => Some("Loading sessions…".to_owned()),
            (Results::Idle, Some(_), _) if self.entries.is_empty() => Some(
                if self.query.trim().is_empty() && self.filters().sources.is_empty() {
                    if self.scope() == HistoryScope::All {
                        "No sessions yet. The agents' transcripts appear here once they exist."
                    } else {
                        "No sessions in this scope. Try All."
                    }
                } else {
                    "No session matches."
                }
                .to_owned(),
            ),
            _ => None,
        };
        match note {
            Some(text) => {
                let error = text.starts_with("Search failed") || text.starts_with("Cannot list");
                if error {
                    list.child(
                        div()
                            .px(px(tokens.spacing * 3.0))
                            .py(px(tokens.spacing * 2.0))
                            .text_color(theme.danger)
                            .child(shared(text)),
                    )
                    .into_any_element()
                } else {
                    list.child(self.render_note(&text, theme, tokens))
                        .into_any_element()
                }
            }
            // Only the rows in view are built: a frame costs what the window shows, not what the
            // daemon has.
            None => list
                .child(gpui::list(self.list.clone(), cx.processor(Self::render_entry)).size_full())
                .into_any_element(),
        }
    }

    /// A list lays an item out as a root with the list's width only available, so the wrapper
    /// takes that width and stretches the entry in it (see the sessions panel).
    fn render_entry(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .w_full()
            .child(self.render_list_entry(ix, window, cx))
            .into_any_element()
    }

    fn render_list_entry(
        &mut self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        match self.entries.get(ix).cloned() {
            Some(Entry::Header {
                key,
                label,
                count,
                collapsed,
            }) => self.render_group(ix, key, label, count, collapsed, &theme, &tokens, cx),
            Some(Entry::Row(item)) => self.render_row(item, &theme, &tokens, cx),
            Some(Entry::More { remaining }) => div()
                .flex()
                .justify_center()
                .py(px(tokens.spacing * 2.0))
                .child(
                    ui::Button::new(
                        "history-show-more",
                        format!("Show {remaining} more"),
                        &theme,
                        &tokens,
                    )
                    .compact()
                    .on_click(cx.listener(|this, _, _, cx| this.show_more(cx)))
                    .child(probe(
                        &self.env.regions,
                        Region::HistoryItem("show-more", 0),
                    )),
                )
                .into_any_element(),
            None => div().into_any_element(),
        }
    }

    #[allow(clippy::too_many_arguments)] // one call site; a struct would only rename them
    fn render_group(
        &self,
        ix: usize,
        key: String,
        label: String,
        count: usize,
        collapsed: bool,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = theme.surface_hover;
        div()
            .id(("history-group-row", ix))
            .relative()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 1.5))
            .h(px(28.0))
            .px(px(tokens.spacing * 3.0))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_group(&key, cx)))
            .child(
                div()
                    .w(px(10.0))
                    .flex_none()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(crate::icons::glyph(if collapsed {
                        crate::icons::Icon::ActionDisclose
                    } else {
                        crate::icons::Icon::ActionExpand
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(tokens.secondary_font_size + 1.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.muted)
                    .child(shared(label)),
            )
            .child(ui::counter(count.to_string(), theme, tokens))
            .child(probe(&self.env.regions, Region::HistoryItem("group", ix)))
            .into_any_element()
    }

    /// A row's title, meta line and the tooltip of its age, made once per item and minute: the
    /// list asks for them again for every row it lays out, and building them formats times in
    /// the local zone and shortens paths.
    fn row_text(
        &self,
        index: usize,
        item: &SessionItem,
    ) -> (SharedString, SharedString, Option<Tooltip>) {
        let minute = self.render_ms / 60_000;
        let mut cache = self.row_cache.borrow_mut();
        let text = match cache.get(&index) {
            Some(text) if text.minute == minute => text,
            _ => cache
                .entry(index)
                .insert_entry(RowText {
                    minute,
                    title: shared(item.display_title()),
                    meta: shared(item.meta(self.home.as_deref(), self.render_ms)),
                    tooltip: item.age_ms().map(|ms| Rc::new(time_tooltip(ms)) as Tooltip),
                })
                .into_mut(),
        };
        (text.title.clone(), text.meta.clone(), text.tooltip.clone())
    }

    fn render_row(
        &mut self,
        index: usize,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(item) = self.items.get(index) else {
            return div().into_any_element();
        };
        let key = item.item_key();
        let selected = self.selected.as_ref() == Some(&key);
        let expanded = self.expanded.contains(&key);
        // Orca's sidebar-accent: a step off the panel, never the brand tint.
        let (hover, subtle) = (
            theme.right_sidebar_accent.opacity(0.6),
            theme.right_sidebar_accent,
        );
        let show_snippets = self.show_snippets;
        let (title, meta, tooltip) = self.row_text(index, item);

        // The latest turn ("Agent: …"), or what matched in a full-text search.
        let preview: Option<AnyElement> = match &item.evidence {
            Some(evidence) => (show_snippets && !evidence.snippet.text.is_empty()).then(|| {
                let prefix = format!("{}: ", turn_label(evidence.role));
                let marks: Vec<_> = evidence
                    .snippet
                    .marks
                    .iter()
                    .map(|range| {
                        (
                            range.start + prefix.len()..range.end + prefix.len(),
                            HighlightStyle {
                                color: Some(theme.accent),
                                font_weight: Some(gpui::FontWeight::MEDIUM),
                                ..HighlightStyle::default()
                            },
                        )
                    })
                    .collect();
                StyledText::new(shared(format!("{prefix}{}", evidence.snippet.text)))
                    .with_highlights(marks)
                    .into_any_element()
            }),
            None => Some(match item.latest_turn() {
                Some((label, text)) => div()
                    .child(shared(format!("{label}: {text}")))
                    .into_any_element(),
                None => match item
                    .first_prompt
                    .as_deref()
                    .and_then(|p| p.lines().map(str::trim).find(|l| !l.is_empty()))
                {
                    Some(line) => div()
                        .child(shared(format!("You: {line}")))
                        .into_any_element(),
                    None => div()
                        .text_color(theme.faint)
                        .child("No conversation preview available")
                        .into_any_element(),
                },
            }),
        };

        let mut row = div()
            .id(("history-row", index))
            .relative()
            .flex_none()
            // The list gives a row its width; long text wraps or truncates inside it.
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .min_h(px(98.0))
            .pl(px(ROW_PAD))
            .pr(px(ROW_PAD))
            .py(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .when(selected || expanded, move |s| s.bg(subtle))
            .when(!(selected || expanded), move |s| {
                s.hover(move |s| s.bg(hover))
            })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_expanded(index, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let key = key.clone();
                    move |this, event: &MouseDownEvent, _, cx| {
                        this.selected = Some(key.clone());
                        this.open_menu(Menu::Row(key.clone(), event.position), cx);
                    }
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(tokens.font_size))
                            .line_height(px(20.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(title),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_none()
                            .w(px(16.0))
                            .text_color(theme.muted)
                            .child(icon(
                                if expanded {
                                    Icon::ActionCollapse
                                } else {
                                    Icon::ActionExpand
                                },
                                14.0,
                                theme.muted,
                            ))
                            .child(probe(
                                &self.env.regions,
                                Region::HistoryItem("chevron", index),
                            )),
                    )
                    .child(
                        ui::IconButton::new(
                            ("history-more", index),
                            Icon::ActionMore,
                            "Actions",
                            theme,
                            tokens,
                        )
                        .compact()
                        .on_click({
                            let key = key.clone();
                            cx.listener(move |this, e: &ClickEvent, _, cx| {
                                this.selected = Some(key.clone());
                                this.open_menu(Menu::Row(key.clone(), e.position()), cx);
                            })
                        })
                        .child(probe(&self.env.regions, Region::HistoryItem("more", index))),
                    ),
            )
            .when_some(preview, |s, preview| {
                s.child(
                    preview_style(div(), expanded)
                        .text_size(px(tokens.secondary_font_size + 1.0))
                        .line_height(px(16.0))
                        .text_color(theme.muted)
                        .child(preview),
                )
            })
            .child(
                div()
                    .id(("history-meta", index))
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing))
                    .text_size(px(tokens.secondary_font_size))
                    .line_height(px(16.0))
                    .text_color(theme.muted)
                    .when_some(tooltip, |s, tip| {
                        s.tooltip(move |window, cx| tip(window, cx))
                    })
                    .child(
                        div()
                            .flex_none()
                            .w(px(14.0))
                            .child(crate::icons::glyph(source_icon(item.source))),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(meta)),
            );
        if expanded {
            row = row.child(self.render_details(index, item, theme, tokens, cx));
        }
        row.child(probe(&self.env.regions, Region::HistoryItem("row", index)))
            .into_any_element()
    }

    /// Resume and the other actions, the first prompt, the latest turns and where it ran.
    fn render_details(
        &self,
        index: usize,
        item: &SessionItem,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (entries, exists) = self.row_actions(item, cx);
        let small = px(tokens.secondary_font_size);
        let mut buttons = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(tokens.spacing))
            .mt(px(tokens.spacing));
        let mut n = 0usize;
        for (position, entry) in entries.iter().enumerate() {
            if entry.action == RowAction::CopyLogPath {
                continue;
            }
            let action = entry.action;
            let kind = if position == 0 && entry.enabled {
                ButtonKind::Primary
            } else {
                ButtonKind::Secondary
            };
            let mut b = ui::Button::new(
                SharedString::from(format!("history-act-{index}-{n}")),
                entry.label,
                theme,
                tokens,
            )
            .compact()
            .kind(kind)
            .disabled(!entry.enabled)
            .child(probe(&self.env.regions, Region::HistoryItem("action", n)));
            if entry.enabled {
                b = b.on_click(cx.listener(move |this, _, window, cx| {
                    this.run_action(action, index, window, cx);
                }));
            }
            buttons = buttons.child(b);
            n += 1;
        }
        let card = |label: &str, text: String| {
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .p(px(8.0))
                .rounded(px(tokens.radius))
                .bg(theme.surface)
                .border_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_size(px(tokens.section_font_size))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.muted)
                        .child(shared(label.to_owned())),
                )
                .child(
                    div()
                        .text_size(small)
                        .line_height(px(16.0))
                        .child(shared(text)),
                )
        };
        let mut details = div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 1.5))
            .pt(px(tokens.spacing))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(buttons);
        if let Some(note) = blocker(item, exists) {
            details = details.child(div().text_size(small).text_color(theme.muted).child(note));
        }
        if let Some(prompt) = item
            .first_prompt
            .as_deref()
            .filter(|p| !p.trim().is_empty())
        {
            details = details.child(card("First prompt", prompt.trim().to_owned()));
        }
        if !item.latest.is_empty() {
            let turns = item
                .latest
                .iter()
                .map(|turn| format!("{}: {}", turn_label(turn.role), turn.text))
                .collect::<Vec<_>>()
                .join("\n");
            details = details.child(card("Latest", turns));
        }
        let mut place = Vec::new();
        if let Some(cwd) = item.cwd.as_deref().filter(|c| !c.is_empty()) {
            place.push(super::history_model::short_path(
                cwd,
                std::env::home_dir().as_deref(),
            ));
        }
        if let Some(branch) = item.branch.as_deref().filter(|b| !b.is_empty()) {
            place.push(branch.to_owned());
        }
        if !place.is_empty() {
            details = details.child(
                div()
                    .truncate()
                    .text_size(small)
                    .text_color(theme.muted)
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .when(item.branch.as_deref().is_some_and(|b| !b.is_empty()), |s| {
                        s.child(crate::icons::glyph(Icon::VcsBranch))
                    })
                    .child(shared(place.join(" · "))),
            );
        }
        details.into_any_element()
    }

    // ---- menus -----------------------------------------------------------------------------

    fn render_menu(
        &mut self,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let menu = self.menu.clone()?;
        // (label, checked, enabled, what it does)
        let (position, entries): (_, Vec<(String, bool, bool, Pick)>) = match menu {
            Menu::GroupBy(at) => (
                at,
                GroupBy::ALL
                    .into_iter()
                    .map(|g| {
                        (
                            g.label().to_owned(),
                            self.group_by() == g,
                            true,
                            Pick::Group(g),
                        )
                    })
                    .collect(),
            ),
            Menu::Sources(at) => (
                at,
                SOURCES
                    .into_iter()
                    .map(|s| {
                        (
                            source_label(s).to_owned(),
                            self.source_shown(s),
                            true,
                            Pick::Source(s),
                        )
                    })
                    .collect(),
            ),
            Menu::Sort(at) => (
                at,
                [HistorySessionSort::Updated, HistorySessionSort::Created]
                    .into_iter()
                    .map(|s| {
                        (
                            sort_label(s).to_owned(),
                            self.sort() == s,
                            true,
                            Pick::Sort(s),
                        )
                    })
                    .collect(),
            ),
            Menu::Row(key, at) => {
                let (index, item) = self
                    .items
                    .iter()
                    .enumerate()
                    .find(|(_, i)| i.item_key() == key)
                    .map(|(i, item)| (i, item.clone()))?;
                let (actions, _) = self.row_actions(&item, cx);
                (
                    at,
                    actions
                        .into_iter()
                        .map(|a| {
                            (
                                a.label.to_owned(),
                                false,
                                a.enabled,
                                Pick::Row(a.action, index),
                            )
                        })
                        .collect(),
                )
            }
        };
        let hover = theme.surface_hover;
        let faint = theme.faint;
        let rows: Vec<AnyElement> = entries
            .into_iter()
            .enumerate()
            .map(|(i, (label, checked, enabled, pick))| {
                div()
                    .id(("history-menu-row", i))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing))
                    .h(px(tokens.sidebar_row))
                    .px(px(tokens.spacing * 2.0))
                    .rounded(px(tokens.radius))
                    .when(!enabled, move |s| s.text_color(faint))
                    .when(enabled, move |s| {
                        s.cursor_pointer().hover(move |s| s.bg(hover))
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .when(enabled, |s| {
                        s.on_click(cx.listener(move |this, _, window, cx| {
                            this.pick_menu(pick, window, cx);
                        }))
                    })
                    .child(div().w(px(12.0)).children(
                        (checked).then(|| crate::icons::glyph(crate::icons::Icon::StateCompleted)),
                    ))
                    .child(shared(label))
                    .child(probe(&self.env.regions, Region::HistoryItem("menu", i)))
                    .into_any_element()
            })
            .collect();
        Some(
            deferred(
                anchored()
                    .position(point(position.x, position.y))
                    .snap_to_window_with_margin(px(8.0))
                    .child(
                        div()
                            .id("history-menu")
                            .occlude()
                            .min_w(px(190.0))
                            .flex()
                            .flex_col()
                            .popover(theme, tokens)
                            .text_size(px(tokens.font_size))
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)))
                            .children(rows),
                    ),
            )
            .priority(10)
            .into_any_element(),
        )
    }

    fn pick_menu(&mut self, pick: Pick, window: &mut Window, cx: &mut Context<Self>) {
        match pick {
            Pick::Group(by) => self.set_group_by(by, cx),
            // The sources menu stays open: several can be switched in a row.
            Pick::Source(source) => self.toggle_source(source, cx),
            Pick::Sort(sort) => self.set_sort(sort, cx),
            Pick::Row(action, item) => self.run_action(action, item, window, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_collapsed_preview_is_two_lines_with_an_ellipsis_and_an_expanded_one_is_whole() {
        let mut clamped = preview_style(div(), false);
        let text = &clamped.style().text;
        assert_eq!(text.line_clamp, Some(2));
        assert!(
            matches!(text.text_overflow, Some(gpui::TextOverflow::Truncate(ref s)) if s.as_ref() == "…"),
            "the cut line ends in an ellipsis"
        );
        let mut whole = preview_style(div(), true);
        assert_eq!(whole.style().text.line_clamp, None);
        assert!(whole.style().text.text_overflow.is_none());
    }
}
