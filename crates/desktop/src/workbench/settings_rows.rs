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
use super::view::{Region, Workbench};
use super::vim_ui::OverlayField;
use crate::macos::LoginItemStatus;
use crate::theme::{DensityExt as _, Theme, Tokens};

/// The color of the launch agent's status next to the Start at login toggle.
fn status_tone(status: LoginItemStatus, theme: Theme) -> gpui::Hsla {
    match status {
        LoginItemStatus::Enabled => theme.success,
        LoginItemStatus::RequiresApproval => theme.warning,
        LoginItemStatus::NotFound => theme.danger,
        LoginItemStatus::NotRegistered => theme.muted,
    }
}

/// A compact button of the sheet's rows; `active` marks the selected one of a group.
pub(super) fn small_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    theme: &Theme,
    tokens: &Tokens,
) -> gpui::Stateful<gpui::Div> {
    let hover = theme.surface_hover;
    let (accent, border, soft) = (theme.accent, theme.border, theme.accent_soft);
    div()
        .id(id.into())
        .px(px(tokens.spacing * 2.0))
        .h(px(tokens.control_height - 2.0))
        .flex()
        .flex_none()
        .items_center()
        .rounded(px(tokens.radius))
        .border_1()
        .border_color(if active { accent } else { border })
        .when(active, move |s| s.bg(soft))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(label.into())
}

/// A row of the sheet: `title` and `help` on the left, the `control` on the right, and
/// whatever `under` adds below the help.
pub(super) fn sheet_row(
    id: impl Into<gpui::ElementId>,
    title: impl Into<SharedString>,
    help: impl Into<SharedString>,
    under: Vec<AnyElement>,
    control: AnyElement,
    theme: &Theme,
    tokens: &Tokens,
) -> AnyElement {
    div()
        .id(id.into())
        .flex()
        .items_start()
        .justify_between()
        .gap(px(tokens.spacing * 3.0))
        .py(px(tokens.spacing * 2.0))
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(title.into()),
                )
                .child(
                    div()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child(help.into()),
                )
                .children(under),
        )
        .child(div().flex().items_start().child(control))
        .into_any_element()
}

/// A line of small muted text.
pub(super) fn note(text: impl Into<SharedString>, theme: &Theme, tokens: &Tokens) -> gpui::Div {
    div()
        .text_size(px(tokens.secondary_font_size))
        .text_color(theme.muted)
        .child(text.into())
}

impl Workbench {
    /// The line under a field's help that says which layer the value comes from; the file is
    /// the hover.
    fn source_line(&self, field: &Field, cx: &App) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let source = self.source_of_field(field, cx);
        let edit = self.edit_layer_of(field);
        let mut text = source.label.clone();
        if source.layer != Some(edit) {
            // The value is hidden by another layer, or not set anywhere: say where it goes.
            let target = self.layer_label(edit, cx);
            if target != source.label {
                text = format!("{text} · {}", words::editing(&target));
            }
        }
        let tip: SharedString = source
            .path
            .as_ref()
            .map_or_else(
                || words::SOURCE_DEFAULT_TIP.to_owned(),
                |path| path.display().to_string(),
            )
            .into();
        div()
            .id(SharedString::from(format!(
                "source-{}-{}",
                field.namespace,
                field.dotted()
            )))
            .w_full()
            .text_size(px(tokens.secondary_font_size))
            .text_color(match source.layer {
                None => theme.faint,
                Some(hutan_settings::SettingsLayer::User) => theme.muted,
                Some(_) => theme.accent,
            })
            .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
            .child(SharedString::from(text))
            .into_any_element()
    }

    /// One row of the page: the field's title and help, where its value comes from, its
    /// control, and Reset when the file it edits sets it.
    pub(super) fn render_field_row(&self, field: &Field, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let Some(sheet) = &self.sheet else {
            return div().into_any_element();
        };
        let key = field.dotted();
        let ns = field.namespace;
        let current = sheet
            .entry_of(ns)
            .map_or(Json::Null, |e| e.current(&self.settings));
        let value = value_at(&current, &field.path)
            .cloned()
            .unwrap_or(Json::Null);
        let kind = self.sheet_kind_of(field);
        let editing = sheet.edit.clone();
        let dropdown = sheet.dropdown.clone();
        let mut under = vec![self.source_line(field, cx)];

        let control: AnyElement = match (&kind, (ns, key.as_str())) {
            (FieldKind::Bool, _) => self.render_toggle(field, &value, cx),
            (FieldKind::Enum(options), _) => {
                let open = dropdown.as_deref() == Some(key.as_str());
                let k = key.clone();
                let known = matches!(&value, Json::String(s) if options.contains(s));
                let label = match &value {
                    Json::String(s) if known => value_label(ns, &key, s),
                    Json::String(s) => words::not_available(s),
                    other => display_value(other),
                };
                let head = small_button(
                    SharedString::from(format!("dropdown-{key}")),
                    SharedString::from(format!("{label} ▾")),
                    open,
                    &theme,
                    &tokens,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(sheet) = &mut this.sheet {
                        sheet.dropdown = if sheet.dropdown.as_deref() == Some(k.as_str()) {
                            None
                        } else {
                            Some(k.clone())
                        };
                        cx.notify();
                    }
                }));
                div()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(tokens.spacing))
                    .child(head)
                    .when(open, |s| {
                        s.children(options.iter().map(|option| {
                            let (k, o) = (key.clone(), option.clone());
                            small_button(
                                SharedString::from(format!("option-{key}-{option}")),
                                SharedString::from(value_label(ns, &key, option)),
                                Json::String(option.clone()) == value,
                                &theme,
                                &tokens,
                            )
                            .when(self.dropdown_cursor_at(&key, option), |b| {
                                b.bg(theme.surface_hover)
                            })
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.sheet_choose(&k, &o, cx);
                                },
                            ))
                        }))
                    })
                    .into_any_element()
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
                            .rounded(px(tokens.radius))
                            .bg(theme.bg)
                            .border_1()
                            .border_color(if error.is_some() {
                                theme.danger
                            } else if active.is_some() {
                                theme.accent
                            } else {
                                theme.border
                            })
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
                self.render_list_control(field, &kind, &value, cx)
            }
            (FieldKind::StringMap, _) => {
                under.push(
                    note(words::ENV_WARNING, &theme, &tokens)
                        .text_color(theme.warning)
                        .into_any_element(),
                );
                self.render_map_control(field, &value, cx)
            }
            (FieldKind::Unsupported(_), ("layout", "presets")) => self.render_presets_control(cx),
            (FieldKind::Unsupported(hint), _) => div()
                .text_color(theme.muted)
                .max_w(px(260.0))
                .child(SharedString::from(format!("{value} — {hint}")))
                .into_any_element(),
        };

        let reset =
            (!matches!(kind, FieldKind::Unsupported(_)) && self.field_has_key(field)).then(|| {
                let k = key.clone();
                small_button(
                    SharedString::from(format!("reset-{key}")),
                    words::RESET,
                    false,
                    &theme,
                    &tokens,
                )
                .text_color(theme.muted)
                .on_click(cx.listener(move |this, _, _, cx| this.sheet_reset(&k, cx)))
            });
        let id = SharedString::from(format!("field-{key}"));
        let help = field_help(ns, &key, &field.doc);
        let row = sheet_row(
            id,
            field_title(ns, &key),
            help,
            under,
            div()
                .flex()
                .items_start()
                .gap(px(tokens.spacing * 2.0))
                .children(reset)
                .child(control)
                .into_any_element(),
            &theme,
            &tokens,
        );
        self.focus_ring(&format!("f:{key}"), row)
    }

    /// The On/Off toggle of a boolean field; Start at login also shows what the system says
    /// about the launch agent and how to approve it.
    fn render_toggle(&self, field: &Field, value: &Json, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let key = field.dotted();
        let on = value.as_bool().unwrap_or(false);
        let k = key.clone();
        let toggle = small_button(
            SharedString::from(format!("toggle-{key}")),
            if on { "On" } else { "Off" },
            on,
            &theme,
            &tokens,
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
                        small_button(
                            SharedString::from("open-login-items"),
                            "Open Login Items",
                            false,
                            &theme,
                            &tokens,
                        )
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
                div()
                    .px(px(tokens.spacing * 2.0))
                    .h(px(tokens.control_height - 2.0))
                    .flex()
                    .items_center()
                    .text_color(theme.muted)
                    .child("Off")
                    .into_any_element()
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
        let menu_open = sheet.dropdown.as_deref() == Some(menu.as_str());
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
                div()
                    .id(SharedString::from(id))
                    .text_color(if enabled { theme.fg } else { theme.border })
                    .when(enabled, |s| s.cursor_pointer())
                    .child(label)
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
                    div()
                        .id(SharedString::from(format!("remove-{key}-{index}")))
                        .text_color(theme.muted)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.sheet_remove_list_item(&k, index, cx);
                        }))
                        .child("×"),
                )
        });
        let add: AnyElement = if choices.is_some() {
            if remaining.is_empty() {
                div().into_any_element()
            } else {
                let k = key.clone();
                let toggle_key = menu.clone();
                div()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(tokens.spacing))
                    .child(
                        small_button(
                            SharedString::from(format!("add-{key}")),
                            words::ADD_CHOICE,
                            menu_open,
                            &theme,
                            &tokens,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(sheet) = &mut this.sheet {
                                sheet.dropdown =
                                    if sheet.dropdown.as_deref() == Some(toggle_key.as_str()) {
                                        None
                                    } else {
                                        Some(toggle_key.clone())
                                    };
                                cx.notify();
                            }
                        })),
                    )
                    .when(menu_open, |s| {
                        s.children(remaining.iter().map(|choice| {
                            let (k, c) = (k.clone(), choice.clone());
                            small_button(
                                SharedString::from(format!("choose-{key}-{choice}")),
                                SharedString::from(value_label(ns, &key, choice)),
                                false,
                                &theme,
                                &tokens,
                            )
                            .when(self.dropdown_cursor_at(&menu, choice), |b| {
                                b.bg(theme.surface_hover)
                            })
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.sheet_add_list_item(&k, &c, cx);
                                },
                            ))
                        }))
                    })
                    .into_any_element()
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
                        theme.accent
                    })
                    .min_w(px(80.0))
                    .children(self.overlay_parts(OverlayField::SheetValue, "", cx))
                    .into_any_element(),
                None => {
                    let k = key.clone();
                    small_button(
                        SharedString::from(format!("add-{key}")),
                        "+ add",
                        false,
                        &theme,
                        &tokens,
                    )
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

    /// Reset page… and the layer switch of the page's header.
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
                s.child(note(words::EDIT_IN, &theme, &tokens)).children(
                    choices
                        .into_iter()
                        .enumerate()
                        .map(|(index, (layer, label))| {
                            small_button(
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
                )
            })
            .when(resettable, |s| {
                s.child(
                    small_button("reset-page", words::RESET_PAGE, false, &theme, &tokens)
                        .relative()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.sheet_reset_page(window, cx);
                        }))
                        .child(probe(&self.regions, Region::Settings("reset-page"))),
                )
            })
            .into_any_element()
    }
}
