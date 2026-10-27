//! The rows of the settings sheet: one per field, with the control its type needs (toggle,
//! dropdown, number or text box, list of choices or strings, rows of variables), the layer its
//! value comes from, and Reset.

use gpui::{
    AnyElement, App, AppContext as _, Context, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, div, prelude::*, px,
};
use serde_json::Value as Json;

use super::render::probe;
use super::settings_labels::{field_help, field_title, value_label, words};
use super::settings_schema::{Field, FieldKind, number_range, value_at};
use super::settings_sheet::display_value;
use super::style::TextTooltip;
use super::ui::motion::{self, PresentExt as _};
use super::ui::{Button, FocusRingExt as _, IconButton, StyledExt as _};
use super::view::{Region, Workbench};
use super::vim_ui::OverlayField;
use crate::macos::LoginItemStatus;
use crate::theme::{DensityExt as _, Theme, Tokens};

/// The color of the launch agent's status next to the Start at login toggle.
fn status_tone(status: LoginItemStatus, theme: Theme) -> gpui::Hsla {
    match status {
        LoginItemStatus::Enabled => theme.success,
        LoginItemStatus::RequiresApproval => theme.warning,
        LoginItemStatus::Missing => theme.danger,
        LoginItemStatus::NotRegistered => theme.muted,
    }
}

/// The narrowest the control column of a row is, in px: a dropdown, a field and a switch's cell
/// all start at the same x, whatever their value says.
pub(super) const CONTROL_MIN_WIDTH: f32 = 120.0;

/// The height of a row of the sheet: 56 px comfortable, 52 px compact.
pub(super) fn row_height(tokens: &Tokens) -> f32 {
    tokens.sidebar_row + 26.0
}

/// One row of a page of the sheet: the label (13 medium) with its chip, one line of help (12
/// `muted`, wrapped whole: a row grows to its help instead of cutting it with "…"),
/// and at the right the ghost ↺ and the control. A hairline under it.
pub(super) struct SheetRow {
    id: gpui::ElementId,
    title: SharedString,
    help: SharedString,
    under: Vec<AnyElement>,
    control: AnyElement,
    chip: Option<AnyElement>,
    reset: Option<AnyElement>,
    top: bool,
    mark: Option<AnyElement>,
}

impl SheetRow {
    pub(super) fn new(
        id: impl Into<gpui::ElementId>,
        title: impl Into<SharedString>,
        help: impl Into<SharedString>,
        control: AnyElement,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            help: help.into(),
            under: Vec::new(),
            control,
            chip: None,
            reset: None,
            top: false,
            mark: None,
        }
    }

    /// Lines under the help that the row cannot do without (a warning, an error).
    #[must_use]
    pub(super) fn under(mut self, under: Vec<AnyElement>) -> Self {
        self.under = under;
        self
    }

    /// The chip after the label (`You`).
    #[must_use]
    pub(super) fn chip(mut self, chip: Option<AnyElement>) -> Self {
        self.chip = chip;
        self
    }

    /// The ghost ↺ before the control.
    #[must_use]
    pub(super) fn reset(mut self, reset: Option<AnyElement>) -> Self {
        self.reset = reset;
        self
    }

    /// The control is taller than a line (a list, a table): the label stays at the top.
    #[must_use]
    pub(super) fn top(mut self) -> Self {
        self.top = true;
        self
    }

    /// An invisible probe child, to find the row in tests.
    #[must_use]
    pub(super) fn marked(mut self, probe: impl IntoElement) -> Self {
        self.mark = Some(probe.into_any_element());
        self
    }

    pub(super) fn build(self, theme: &Theme, tokens: &Tokens) -> AnyElement {
        let help_id = gpui::ElementId::from((self.id.clone(), "help"));
        let has_help = !self.help.is_empty();
        let row = div()
            .id(self.id)
            .flex()
            .relative()
            .when(self.top, |s| s.items_start())
            .when(!self.top, |s| s.items_center())
            .justify_between()
            .gap(px(tokens.spacing * 3.0))
            .min_h(px(row_height(tokens)))
            .py(px(tokens.spacing))
            .border_b_1()
            .border_color(theme.border)
            .children(self.mark)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(tokens.spacing * 2.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(self.title),
                            )
                            .children(self.chip),
                    )
                    .when(has_help, |s| {
                        s.child(
                            div()
                                .id(help_id)
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.muted)
                                .child(self.help),
                        )
                    })
                    .children(self.under),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_end()
                    // One column: the controls of a page start at the same x.
                    .min_w(px(CONTROL_MIN_WIDTH))
                    .gap(px(tokens.spacing))
                    .children(self.reset)
                    .child(self.control),
            );
        row.into_any_element()
    }
}

/// A row of the sheet without a chip, ↺ or focus: `title` and `help` on the left, the
/// `control` on the right, and whatever `under` adds below the help.
pub(super) fn sheet_row(
    id: impl Into<gpui::ElementId>,
    title: impl Into<SharedString>,
    help: impl Into<SharedString>,
    under: Vec<AnyElement>,
    control: AnyElement,
    theme: &Theme,
    tokens: &Tokens,
) -> AnyElement {
    SheetRow::new(id, title, help, control)
        .under(under)
        .build(theme, tokens)
}

/// The chip after a row's label: `You`, `Workspace acme`, `Project api` (`counter` style, a
/// neutral fill: no color says who set a value). `tip` is its hover.
pub(super) fn source_chip(
    id: impl Into<gpui::ElementId>,
    text: impl Into<SharedString>,
    tip: impl Into<SharedString>,
    theme: &Theme,
    tokens: &Tokens,
) -> AnyElement {
    let tip: SharedString = tip.into();
    super::ui::counter(text, theme, tokens)
        .id(id.into())
        .max_w(px(160.0))
        .truncate()
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
        .into_any_element()
}

/// The ghost ↺ that takes a value back to what it inherits.
pub(super) fn reset_button(
    id: impl Into<gpui::ElementId>,
    tip: impl Into<SharedString>,
    theme: &Theme,
    tokens: &Tokens,
) -> Button {
    Button::new(id, "↺", theme, tokens)
        .ghost()
        .compact()
        .tooltip(tip)
}

/// A line of small muted text.
pub(super) fn note(text: impl Into<SharedString>, theme: &Theme, tokens: &Tokens) -> gpui::Div {
    div()
        .text_size(px(tokens.secondary_font_size))
        .text_color(theme.muted)
        .child(text.into())
}

/// Tells the heads of different menus apart (FNV-1a of the dropdown's tag).
pub(super) fn menu_key(tag: &str) -> u64 {
    tag.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The panel of a menu or picker: a popover (elevation 2) that takes the mouse and scrolls
/// past `max_height`.
pub(super) fn menu_panel(
    id: impl Into<gpui::ElementId>,
    max_height: f32,
    theme: &Theme,
    tokens: &Tokens,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id.into())
        .occlude()
        .flex()
        .flex_col()
        .max_h(px(max_height))
        .overflow_y_scroll()
        .popover(theme, tokens)
        .text_size(px(tokens.font_size))
        .text_color(theme.fg)
}

/// One option of a menu: a check where it is the current one, its label, and the fill of the
/// cursor (arrow keys) or the hover. The caller adds the click.
pub(super) fn menu_option(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    current: bool,
    cursor: bool,
    theme: &Theme,
    tokens: &Tokens,
) -> gpui::Stateful<gpui::Div> {
    let hover = theme.surface_hover;
    div()
        .id(id.into())
        .row(tokens.control_height, tokens)
        .flex_none()
        .gap(px(tokens.spacing))
        .cursor_pointer()
        .when(cursor, |s| s.bg(hover))
        .hover(move |s| s.bg(hover))
        .child(
            div()
                .w(px(12.0))
                .flex_none()
                .children(current.then(|| crate::icons::glyph(crate::icons::Icon::StateCompleted))),
        )
        .child(div().min_w_0().truncate().child(label.into()))
}

impl Workbench {
    /// The chip after a field's label when its value is set in a file (`You`, `Workspace
    /// acme`, `Project api`): its hover is the file, and where a change goes when that is not
    /// the layer the value comes from. A built-in default has no chip.
    pub(super) fn field_chip(&self, field: &Field, cx: &App) -> Option<AnyElement> {
        let source = self.source_of_field(field, cx);
        source.layer?;
        let edit = self.edit_layer_of(field);
        let mut tip = source.path.as_ref().map_or_else(
            || words::SOURCE_DEFAULT_TIP.to_owned(),
            |path| path.display().to_string(),
        );
        if source.layer != Some(edit) {
            let target = self.layer_label(edit, cx);
            if target != source.label {
                tip = format!("{tip} · {}", words::editing(&target));
            }
        }
        Some(
            div()
                .relative()
                .flex_none()
                .child(source_chip(
                    SharedString::from(format!("source-{}-{}", field.namespace, field.dotted())),
                    source.label,
                    tip,
                    &self.theme,
                    &self.density.ui(),
                ))
                .child(probe(
                    &self.regions,
                    Region::SettingsItem("chip", self.field_index(field)),
                ))
                .into_any_element(),
        )
    }

    /// The place of `field` on the selected page: what tells the probes of its parts apart.
    pub(super) fn field_index(&self, field: &Field) -> usize {
        self.sheet
            .as_ref()
            .and_then(|s| s.fields().iter().position(|f| f == field))
            .unwrap_or(0)
    }

    /// Opens the dropdown `tag`, or closes it when it is the one open. Either way the keyboard
    /// is on `row` of the controls afterwards, so Escape (or a pick) leaves it where the
    /// dropdown was opened from.
    pub(super) fn toggle_dropdown(&mut self, tag: &str, row: &str, cx: &mut Context<Self>) {
        if let Some(sheet) = &mut self.sheet {
            let open = sheet.dropdown.as_deref() == Some(tag);
            sheet.dropdown = (!open).then(|| tag.to_owned());
            sheet.cursor = None;
            sheet.pack_filter.clear();
            sheet.pack_preview = None;
            sheet.pack_menu = None;
            sheet.edit = None;
            sheet.zone = super::settings_keys::SheetFocus::Controls;
            sheet.focus_row = Some(row.to_owned());
            cx.notify();
        }
    }

    /// Closes the open dropdown without choosing anything (Escape, a click outside it).
    pub(super) fn close_dropdown(&mut self, cx: &mut Context<Self>) {
        if let Some(sheet) = &mut self.sheet {
            sheet.dropdown = None;
            sheet.cursor = None;
            sheet.pack_filter.clear();
            sheet.pack_preview = None;
            sheet.pack_menu = None;
        }
        cx.notify();
    }

    /// `head` with the `panel` of its menu hanging below it, right edges aligned, while
    /// there is one (the panel is drawn in front of the page, not inside its scroll, so it
    /// never pushes a row). A press outside the panel calls `close`, except on the head, which
    /// toggles the menu itself.
    pub(super) fn menu_anchor(
        &self,
        tag: &str,
        head: impl ParentElement + IntoElement,
        panel: Option<gpui::Stateful<gpui::Div>>,
        close: fn(&mut Self, &mut Context<Self>),
        cx: &Context<Self>,
    ) -> AnyElement {
        let key = menu_key(tag);
        let head = head.child(probe(&self.regions, Region::SettingsMenu(key)));
        let regions = self.regions.clone();
        let menu_probe = probe(&self.regions, Region::Settings("menu"));
        let panel = panel.map(|panel| {
            hutan_ui::scroll_y(
                gpui::SharedString::from(format!("menu-scroll-{key:?}")),
                panel
                    .relative()
                    .child(menu_probe)
                    .on_mouse_down_out(cx.listener(
                        move |this, event: &gpui::MouseDownEvent, _, cx| {
                            let on_head = regions
                                .get(Region::SettingsMenu(key))
                                .is_some_and(|bounds| bounds.contains(&event.position));
                            if !on_head {
                                close(this, cx);
                            }
                        },
                    )),
            )
        });
        div()
            .relative()
            .flex_none()
            .child(head)
            .children(panel.map(|panel| {
                // A dropdown that closed leaves; any other menu this draws (the ⋯ of a picker) is
                // there because it is open.
                let open = self
                    .sheet
                    .as_ref()
                    .is_some_and(|s| s.dropdown.as_deref() == Some(tag));
                let leaving = !open
                    && self
                        .exits
                        .dropdown
                        .held(cx.background_executor().now())
                        .is_some_and(|held| held == tag);
                let wanted = !leaving;
                div().absolute().top_full().right_0().size_0().child(
                    gpui::deferred(
                        gpui::anchored()
                            .anchor(gpui::Anchor::TopRight)
                            .offset(gpui::point(px(0.0), px(4.0)))
                            .snap_to_window_with_margin(px(8.0))
                            .child(
                                panel
                                    .presented(
                                        gpui::SharedString::from(format!("menu-motion-{key:?}")),
                                        motion::Kind::Popover,
                                        wanted,
                                        motion::Slide::Drop,
                                    )
                                    .inert(!wanted),
                            ),
                    )
                    .priority(10),
                )
            }))
            .into_any_element()
    }

    /// One row of the page: the field's title and one line of help, its `You` chip when the
    /// value is set in a file, its control, and the ghost ↺ when the file it edits sets it.
    pub(super) fn render_field_row(&self, field: &Field, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let Some(sheet) = &self.sheet else {
            return div().into_any_element();
        };
        let key = field.dotted();
        let ns = field.namespace;
        if let Some(pack) = super::settings_themes::PackField::of(ns, &key) {
            return self.render_pack_row(pack, field, cx);
        }
        let current = sheet
            .entry_of(ns)
            .map_or(Json::Null, |e| e.current(&self.settings));
        let value = value_at(&current, &field.path)
            .cloned()
            .unwrap_or(Json::Null);
        let kind = self.sheet_kind_of(field);
        let editing = sheet.edit.clone();
        let row_tag = format!("f:{key}");
        let mut under: Vec<AnyElement> = Vec::new();
        let mut tall = false;

        let control: AnyElement = match (&kind, (ns, key.as_str())) {
            (FieldKind::Bool, _) => self.render_toggle(field, &value, cx),
            (FieldKind::Enum(options), _) => {
                let open = self.dropdown_drawn(&key, cx);
                let (k, row) = (key.clone(), row_tag.clone());
                let known = matches!(&value, Json::String(s) if options.contains(s));
                let label = match &value {
                    Json::String(s) if known => value_label(ns, &key, s),
                    Json::String(s) => words::not_available(s),
                    other => display_value(other),
                };
                let head = Button::new(
                    SharedString::from(format!("dropdown-{key}")),
                    SharedString::from(label),
                    &theme,
                    &tokens,
                )
                .compact()
                .min_width(CONTROL_MIN_WIDTH)
                .selected(open)
                .child(crate::icons::glyph(crate::icons::Icon::ActionMenuDown))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_dropdown(&k, &row, cx);
                }))
                .child(probe(&self.regions, Region::Settings("dropdown")));
                let panel = open.then(|| {
                    menu_panel(
                        SharedString::from(format!("menu-{key}")),
                        320.0,
                        &theme,
                        &tokens,
                    )
                    .min_w(px(160.0))
                    .children(options.iter().map(|option| {
                        let (k, o) = (key.clone(), option.clone());
                        menu_option(
                            SharedString::from(format!("option-{key}-{option}")),
                            SharedString::from(value_label(ns, &key, option)),
                            Json::String(option.clone()) == value,
                            self.dropdown_cursor_at(&key, option),
                            &theme,
                            &tokens,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.sheet_choose(&k, &o, cx);
                        }))
                    }))
                });
                self.menu_anchor(&key, head, panel, Self::close_dropdown, cx)
            }
            (FieldKind::Integer | FieldKind::Number | FieldKind::String, _) => {
                let k = key.clone();
                let active = editing.as_ref().filter(|e| e.field == key);
                let text = active.map_or_else(|| display_value(&value), |e| e.text.clone());
                let error = active.and_then(|e| e.error.clone());
                let range = number_range(ns, &key);
                div()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(2.0))
                    .child(
                        div()
                            .id(SharedString::from(format!("input-{key}")))
                            .flex()
                            .items_center()
                            .min_w(px(150.0))
                            .h(px(tokens.control_height))
                            .px(px(tokens.spacing * 2.0))
                            .outlined(&theme, &tokens)
                            .when(error.is_some(), |s| s.border_color(theme.danger))
                            .focus_ring(active.is_some(), &theme, &tokens)
                            .cursor_text()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.sheet_begin_edit(&k, cx);
                            }))
                            .children(if active.is_some() {
                                self.overlay_parts(OverlayField::SheetValue, "", cx)
                            } else {
                                vec![SharedString::from(text).into_any_element()]
                            }),
                    )
                    .when_some(range, |s, (low, high)| {
                        s.child(note(words::range_hint(low, high), &theme, &tokens))
                    })
                    .when_some(error, |s, error| {
                        s.child(
                            div()
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.danger)
                                .child(SharedString::from(error)),
                        )
                    })
                    .into_any_element()
            }
            (FieldKind::StringList | FieldKind::ChoiceList(_), _) => {
                tall = true;
                self.render_list_control(field, &kind, &value, cx)
            }
            (FieldKind::StringMap, _) => {
                tall = true;
                under.push(
                    note(words::ENV_WARNING, &theme, &tokens)
                        .text_color(theme.warning)
                        .into_any_element(),
                );
                self.render_map_control(field, &value, cx)
            }
            (FieldKind::Unsupported(_), ("layout", "presets")) => {
                tall = true;
                self.render_presets_control(cx)
            }
            (FieldKind::Unsupported(hint), _) => div()
                .text_color(theme.muted)
                .max_w(px(260.0))
                .child(SharedString::from(format!("{value} — {hint}")))
                .into_any_element(),
        };

        let reset = self.field_reset(field, &kind, cx);
        let help = field_help(ns, &key, &field.doc);
        let mut row = SheetRow::new(
            SharedString::from(format!("field-{key}")),
            field_title(ns, &key),
            help,
            control,
        )
        .under(under)
        .chip(self.field_chip(field, cx))
        .reset(reset)
        .marked(probe(
            &self.regions,
            Region::SettingsItem("row", self.field_index(field)),
        ));
        if tall {
            row = row.top();
        }
        self.sheet_row_focus(&row_tag, row.build(&theme, &tokens))
    }

    /// The ghost ↺ of a field whose key the file it edits sets.
    pub(super) fn field_reset(
        &self,
        field: &Field,
        kind: &FieldKind,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if matches!(kind, FieldKind::Unsupported(_)) || !self.field_has_key(field) {
            return None;
        }
        let key = field.dotted();
        let k = key.clone();
        Some(
            reset_button(
                SharedString::from(format!("reset-{key}")),
                words::RESET_TIP,
                &self.theme,
                &self.density.ui(),
            )
            .child(probe(
                &self.regions,
                Region::SettingsItem("reset", self.field_index(field)),
            ))
            .on_click(cx.listener(move |this, _, _, cx| this.sheet_reset(&k, cx)))
            .into_any_element(),
        )
    }

    /// The switch of a boolean field; Start at login also shows what the system says about the
    /// launch agent and how to approve it.
    fn render_toggle(&self, field: &Field, value: &Json, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let key = field.dotted();
        let on = value.as_bool().unwrap_or(false);
        let k = key.clone();
        let toggle = super::ui::switch(
            SharedString::from(format!("toggle-{key}")),
            on,
            true,
            &theme,
        )
        .on_click(cx.listener(move |this, _, window, cx| this.sheet_toggle_in(&k, window, cx)));
        if !(field.namespace == "app" && key == "start_at_login") {
            return toggle.into_any_element();
        }
        // The toggle, what the system says about the launch agent, and the way to approve it.
        let available = self.start_at_login_available();
        let status = self.start_at_login_status();
        let (text, tone) = if available {
            (status.label().to_owned(), status_tone(status, theme))
        } else {
            ("available in Hutan.app".to_owned(), theme.muted)
        };
        div()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .when(
                available && status == LoginItemStatus::RequiresApproval,
                |s| {
                    s.child(
                        Button::new(
                            SharedString::from("open-login-items"),
                            "Open login items",
                            &theme,
                            &tokens,
                        )
                        .compact()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.open_login_items(cx);
                        })),
                    )
                },
            )
            .child(
                div()
                    .id("start-at-login-status")
                    .text_color(tone)
                    .child(SharedString::from(text)),
            )
            .child(if available {
                toggle.into_any_element()
            } else {
                super::ui::switch("start-at-login-off", false, false, &theme).into_any_element()
            })
            .into_any_element()
    }

    /// The items of a list as chips (move up, move down, remove), and a way to add: a menu of
    /// the choices that are not in the list yet, or a text box for a list without a known set.
    fn render_list_control(
        &self,
        field: &Field,
        kind: &FieldKind,
        value: &Json,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let key = field.dotted();
        let ns = field.namespace;
        let items: Vec<String> = value
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let Some(sheet) = &self.sheet else {
            return div().into_any_element();
        };
        let adding = sheet.edit.as_ref().filter(|e| e.field == key);
        let menu = format!("add:{key}");
        let menu_open = self.dropdown_drawn(&menu, cx);
        let choices: Option<&Vec<String>> = match kind {
            FieldKind::ChoiceList(choices) => Some(choices),
            _ => None,
        };
        let remaining: Vec<String> = choices
            .map(|c| c.iter().filter(|c| !items.contains(c)).cloned().collect())
            .unwrap_or_default();
        let last = items.len().saturating_sub(1);
        let chips = items.iter().enumerate().map(|(index, item)| {
            let k = key.clone();
            let (k_up, k_down) = (key.clone(), key.clone());
            let step = |id: String, label: &'static str, enabled: bool| {
                Button::new(SharedString::from(id), label, &theme, &tokens)
                    .ghost()
                    .compact()
                    .disabled(!enabled)
            };
            let unknown = choices.is_some_and(|c| !c.contains(item));
            let shown = if unknown {
                words::not_available(item)
            } else {
                value_label(ns, &key, item)
            };
            div()
                .id(SharedString::from(format!("item-{key}-{index}")))
                .flex()
                .items_center()
                .gap(px(tokens.spacing))
                .px(px(tokens.spacing * 2.0))
                .h(px(tokens.control_height - 2.0))
                .rounded(px(tokens.radius))
                .border_1()
                .border_color(if unknown { theme.warning } else { theme.border })
                .child(SharedString::from(shown))
                .child(
                    step(format!("up-{key}-{index}"), "↑", index > 0).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.sheet_move_list_item(&k_up, index, -1, cx);
                        },
                    )),
                )
                .child(
                    step(format!("down-{key}-{index}"), "↓", index < last).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.sheet_move_list_item(&k_down, index, 1, cx);
                        },
                    )),
                )
                .child(
                    IconButton::new(
                        SharedString::from(format!("remove-{key}-{index}")),
                        crate::icons::Icon::ActionClose,
                        "Remove",
                        &theme,
                        &tokens,
                    )
                    .compact()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sheet_remove_list_item(&k, index, cx);
                    })),
                )
        });
        let add: AnyElement = if choices.is_some() {
            if remaining.is_empty() {
                div().into_any_element()
            } else {
                let (k, row) = (key.clone(), format!("f:{key}"));
                let tag = menu.clone();
                let head = Button::new(
                    SharedString::from(format!("add-{key}")),
                    words::ADD_CHOICE,
                    &theme,
                    &tokens,
                )
                .compact()
                .selected(menu_open)
                .child(crate::icons::glyph(crate::icons::Icon::ActionMenuDown))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_dropdown(&tag, &row, cx);
                }));
                let panel = menu_open.then(|| {
                    menu_panel(
                        SharedString::from(format!("menu-{key}")),
                        320.0,
                        &theme,
                        &tokens,
                    )
                    .min_w(px(160.0))
                    .children(remaining.iter().map(|choice| {
                        let (k, c) = (k.clone(), choice.clone());
                        menu_option(
                            SharedString::from(format!("choose-{key}-{choice}")),
                            SharedString::from(value_label(ns, &key, choice)),
                            false,
                            self.dropdown_cursor_at(&menu, choice),
                            &theme,
                            &tokens,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.sheet_add_list_item(&k, &c, cx);
                        }))
                    }))
                });
                self.menu_anchor(&menu, head, panel, Self::close_dropdown, cx)
            }
        } else {
            match adding {
                Some(edit) => div()
                    .flex()
                    .items_center()
                    .px(px(tokens.spacing * 2.0))
                    .h(px(tokens.control_height - 2.0))
                    .rounded(px(tokens.radius))
                    .border_1()
                    .border_color(if edit.error.is_some() {
                        theme.danger
                    } else {
                        theme.border
                    })
                    .focus_ring(true, &theme, &tokens)
                    .min_w(px(80.0))
                    .children(self.overlay_parts(OverlayField::SheetValue, "", cx))
                    .into_any_element(),
                None => {
                    let k = key.clone();
                    Button::new(
                        SharedString::from(format!("add-{key}")),
                        "+ add",
                        &theme,
                        &tokens,
                    )
                    .compact()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sheet_begin_edit(&k, cx);
                    }))
                    .into_any_element()
                }
            }
        };
        div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(tokens.spacing))
            .max_w(px(320.0))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(px(tokens.spacing))
                    .children(chips),
            )
            .child(add)
            .when_some(adding.and_then(|e| e.error.clone()), |s, error| {
                s.child(
                    div()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.danger)
                        .child(SharedString::from(error)),
                )
            })
            .into_any_element()
    }

    /// The page's header actions: the layer switch (while a workspace or project layer exists)
    /// and the ghost Reset page….
    pub(super) fn render_sheet_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let choices = self.sheet_layer_choices(cx);
        let chosen = self.sheet.as_ref().and_then(|s| s.layer);
        let resettable = self.sheet.as_ref().is_some_and(|s| !s.fields().is_empty());
        div()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .when(!choices.is_empty(), |s| {
                s.child(note(words::EDIT_IN, &theme, &tokens)).child(
                    super::ui::segmented(&theme, &tokens).children(
                        choices
                            .into_iter()
                            .enumerate()
                            .map(|(index, (layer, label))| {
                                super::ui::segment(
                                    SharedString::from(format!("layer-{index}")),
                                    SharedString::from(label),
                                    chosen == Some(layer),
                                    &theme,
                                    &tokens,
                                )
                                .relative()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let next = (this.sheet.as_ref().and_then(|s| s.layer)
                                        != Some(layer))
                                    .then_some(layer);
                                    this.sheet_set_layer(next, cx);
                                }))
                                .child(probe(&self.regions, Region::SettingsItem("layer", index)))
                            }),
                    ),
                )
            })
            .when(resettable, |s| {
                s.child(
                    Button::new("reset-page", words::RESET_PAGE, &theme, &tokens)
                        .destructive()
                        .ghost()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.sheet_reset_page(window, cx);
                        }))
                        .child(probe(&self.regions, Region::Settings("reset-page"))),
                )
            })
            .into_any_element()
    }
}
