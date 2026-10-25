//! Several accounts per harness on the Accounts page (protocol 0.50, ADR 0015 § Amendment).
//!
//! Under each harness's own logins the card lists the accounts Hutan created for it: a name, a
//! label, what the harness reports for the account's home, and *Log in*, *Set as default*,
//! *Remove…* (which asks first). *Add account…* is a small form (name, label) whose last box
//! adds the account and then runs the harness's own login for it in a terminal session, like
//! *Log in* does. The form's two boxes share the sheet's single text editor, as the MCP form
//! does (`settings_mcp.rs`): Return keeps what was typed and moves on, the last box saves.
//!
//! The window holds names, labels and statuses only: what is in an account's folder is the
//! harness's and never reaches Hutan.

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use hutan_protocol::account::{AccountStatus, ManagedAccount, RemoveLogout, SYSTEM_ACCOUNT};

use super::render::probe;
use super::settings_daemon::EDIT_PREFIX;
use super::ui::{self, Button, FocusRingExt as _};
use super::view::{Region, Workbench};
use super::vim_ui::OverlayField;
use crate::theme::DensityExt as _;

/// What `Edit.field` of a box of the form starts with (after [`EDIT_PREFIX`]).
pub(super) const FORM_PREFIX: &str = "acct-form:";

/// A box of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountField {
    Name,
    Label,
}

impl AccountField {
    fn id(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Label => "label",
        }
    }

    pub(super) fn parse(id: &str) -> Option<Self> {
        match id {
            "name" => Some(Self::Name),
            "label" => Some(Self::Label),
            _ => None,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Label => "Label",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Name => "work (lowercase letters, digits, - and _)",
            Self::Label => "Work (optional; shown in the chat header)",
        }
    }
}

/// The *Add account…* form while it is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountForm {
    pub harness: &'static str,
    pub name: String,
    pub label: String,
    /// What is wrong, from the box or from the daemon.
    pub error: Option<String>,
    /// The daemon is adding the account.
    pub saving: bool,
}

impl AccountForm {
    fn text(&self, field: AccountField) -> &str {
        match field {
            AccountField::Name => &self.name,
            AccountField::Label => &self.label,
        }
    }

    fn set_text(&mut self, field: AccountField, text: &str) {
        match field {
            AccountField::Name => text.clone_into(&mut self.name),
            AccountField::Label => text.clone_into(&mut self.label),
        }
    }
}

impl Workbench {
    /// The form that is open, if any (tests, inspection).
    pub fn account_form(&self) -> Option<&AccountForm> {
        self.daemon_settings.account_form.as_ref()
    }

    // ---- the form ------------------------------------------------------------------------

    /// "Add account…" of the card of `harness`: an empty form with the name box ready.
    pub fn account_begin_add(&mut self, harness: &'static str, cx: &mut Context<Self>) {
        if self.sheet.is_none() {
            return;
        }
        self.daemon_settings.account_form = Some(AccountForm {
            harness,
            name: String::new(),
            label: String::new(),
            error: None,
            saving: false,
        });
        self.account_begin_field(AccountField::Name, cx);
    }

    /// Keeps what is typed in the box being typed in, when it is one of the form's.
    fn account_keep_typed(&mut self) {
        let Some(edit) = self.sheet.as_ref().and_then(|s| s.edit.clone()) else {
            return;
        };
        let Some(field) = edit
            .field
            .strip_prefix(EDIT_PREFIX)
            .and_then(|f| f.strip_prefix(FORM_PREFIX))
            .and_then(AccountField::parse)
        else {
            return;
        };
        if let Some(form) = &mut self.daemon_settings.account_form {
            form.set_text(field, &edit.text);
        }
        if let Some(sheet) = &mut self.sheet {
            sheet.edit = None;
        }
    }

    /// Starts typing in `field` (what was typed in the other box is kept).
    pub fn account_begin_field(&mut self, field: AccountField, cx: &mut Context<Self>) {
        self.account_keep_typed();
        let Some(form) = &self.daemon_settings.account_form else {
            return;
        };
        let text = form.text(field).to_owned();
        self.daemon_start_edit(
            &format!("{FORM_PREFIX}{}", field.id()),
            text,
            false,
            None,
            cx,
        );
    }

    /// Return in a box: keeps the text; after the name the label is next, after the label the
    /// account is added.
    pub(super) fn account_commit_field(
        &mut self,
        field: AccountField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.account_keep_typed();
        match field {
            AccountField::Name => self.account_begin_field(AccountField::Label, cx),
            AccountField::Label => self.account_save(window, cx),
        }
    }

    /// Adds the account, then runs the harness's login for it in a terminal the window shows.
    /// A refusal of the daemon shows in the form.
    pub fn account_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.account_keep_typed();
        let Some(form) = self.daemon_settings.account_form.clone() else {
            return;
        };
        if form.saving {
            return;
        }
        let name = form.name.trim().to_owned();
        if name.is_empty() {
            self.account_form_error("Type a name for the account.".to_owned(), cx);
            return;
        }
        let label = form.label.trim().to_owned();
        if let Some(form) = &mut self.daemon_settings.account_form {
            form.saving = true;
            form.error = None;
        }
        let harness = form.harness;
        let task = self.sessions.update(cx, |store, cx| {
            store.account_add(harness, &name, (!label.is_empty()).then_some(label), cx)
        });
        cx.notify();
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(added) => {
                this.update_in(cx, |this, window, cx| {
                    this.daemon_settings.account_form = None;
                    this.refresh_accounts(cx);
                    this.account_flow_as(harness, None, Some(added.name), true, window, cx);
                })
                .ok();
            }
            Err(error) => {
                this.update(cx, |this, cx| {
                    if let Some(form) = &mut this.daemon_settings.account_form {
                        form.saving = false;
                    }
                    this.account_form_error(error, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn account_form_error(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.daemon_settings.account_form {
            form.error = Some(message);
        }
        cx.notify();
    }

    /// Cancel: the form closes and nothing is sent.
    pub fn account_cancel(&mut self, cx: &mut Context<Self>) {
        self.daemon_settings.account_form = None;
        if let Some(sheet) = &mut self.sheet
            && sheet
                .edit
                .as_ref()
                .is_some_and(|e| e.field.starts_with(&format!("{EDIT_PREFIX}{FORM_PREFIX}")))
        {
            sheet.edit = None;
        }
        cx.notify();
    }

    // ---- rows ----------------------------------------------------------------------------

    /// "Set as default": new sessions of `harness` take `account` (`None` clears the default).
    pub fn account_set_default(
        &mut self,
        harness: &str,
        account: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let directory = self.env.harnesses.clone();
        directory.update(cx, |directory, cx| directory.set_problem(None, cx));
        let task = self
            .sessions
            .update(cx, |store, cx| store.account_default(harness, account, cx));
        cx.spawn(async move |_, cx| {
            if let Err(error) = task.await {
                directory.update(cx, |directory, cx| directory.set_problem(Some(error), cx));
            }
        })
        .detach();
    }

    /// "Remove…": asks, then logs the account out (the harness's own logout), deletes its folder
    /// and forgets it. What the harness could not log out says so in a notice.
    pub fn account_remove(
        &mut self,
        harness: &'static str,
        name: &str,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = name.to_owned();
        self.open_destructive(
            format!("Remove the account {label}?"),
            "Hutan logs the harness out of this account, then deletes the account's folder. \
             A chat that runs as it must be stopped first. Your other accounts are not touched.",
            "Remove account",
            move |this, _, cx| {
                let directory = this.env.harnesses.clone();
                directory.update(cx, |directory, cx| directory.set_problem(None, cx));
                let task = this
                    .sessions
                    .update(cx, |store, cx| store.account_remove(harness, &name, cx));
                cx.spawn(async move |this, cx| match task.await {
                    Ok(removed) => {
                        if removed.logout == RemoveLogout::Failed
                            && let Some(detail) = removed.detail
                        {
                            this.update(cx, |this, cx| this.set_notice(detail, cx)).ok();
                        }
                    }
                    Err(error) => {
                        directory
                            .update(cx, |directory, cx| directory.set_problem(Some(error), cx));
                    }
                })
                .detach();
            },
            window,
            cx,
        );
    }

    // ---- rendering -----------------------------------------------------------------------

    /// What the card of `harness` shows under its own logins: a line saying what new sessions
    /// use, a row per managed account, and the form or *Add account…*.
    pub(super) fn render_managed_accounts(
        &self,
        harness: &'static str,
        found: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = self.theme;
        let tokens = self.density.ui();
        let directory = self.env.harnesses.read(cx);
        let accounts: Vec<ManagedAccount> = directory.managed_of(harness).cloned().collect();
        let default = directory.default_account(harness).map(str::to_owned);
        let mut out: Vec<AnyElement> = Vec::new();
        if !accounts.is_empty() {
            let label_of = |name: &str| {
                accounts
                    .iter()
                    .find(|a| a.name == name)
                    .map_or_else(|| name.to_owned(), |a| a.label.clone())
            };
            let uses = match default.as_deref() {
                None | Some(SYSTEM_ACCOUNT) => "the harness's own login".to_owned(),
                Some(name) => label_of(name),
            };
            out.push(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(format!("New chats use {uses}.")))
                    .into_any_element(),
            );
        }
        for (index, account) in accounts.iter().enumerate() {
            out.push(self.render_managed_row(harness, index, account, default.as_deref(), cx));
        }
        let form = self
            .daemon_settings
            .account_form
            .as_ref()
            .filter(|f| f.harness == harness);
        match form {
            Some(form) => out.push(self.render_account_form(form, cx)),
            None => out.push(
                div()
                    .flex()
                    .child(
                        Button::new(
                            format!("account-add-{harness}"),
                            "Add account…",
                            &theme,
                            &tokens,
                        )
                        .compact()
                        .disabled(!found)
                        .when(found, |b| {
                            b.on_click(cx.listener(move |this, _, _, cx| {
                                this.account_begin_add(harness, cx);
                            }))
                        })
                        .child(probe(&self.regions, Region::Account(harness, "add"))),
                    )
                    .into_any_element(),
            ),
        }
        out
    }

    fn render_managed_row(
        &self,
        harness: &'static str,
        index: usize,
        account: &ManagedAccount,
        default: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let is_default = default == Some(account.name.as_str());
        let login = account.logins.first();
        let (status, color) = match login.map(|l| l.status) {
            Some(AccountStatus::LoggedIn) => ("logged in", theme.success),
            Some(AccountStatus::LoggedOut) => ("logged out", theme.warning),
            _ => ("unknown", theme.muted),
        };
        let name = account.name.clone();
        let label = account.label.clone();
        let mut line = div()
            .id(SharedString::from(format!(
                "managed-account-{harness}-{index}"
            )))
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(account.label.clone())),
            );
        if account.label != account.name {
            line = line.child(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(account.name.clone())),
            );
        }
        if let Some(plan) = login.and_then(|l| l.plan.clone()) {
            line = line.child(ui::pill(plan, &theme, &tokens));
        }
        if is_default {
            line = line.child(ui::pill("default", &theme, &tokens));
        }
        line = line.child(ui::state_label(
            ui::state_glyph(color, &theme),
            status,
            color,
            false,
            &theme,
            &tokens,
        ));
        let login_name = name.clone();
        line = line.child(
            Button::new(
                format!("managed-login-{harness}-{index}"),
                "Log in",
                &theme,
                &tokens,
            )
            .compact()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.account_flow_as(harness, None, Some(login_name.clone()), true, window, cx);
            }))
            .child(probe(
                &self.regions,
                Region::SettingsItem("account-login", index),
            )),
        );
        let default_name = name.clone();
        line = line.child(if is_default {
            Button::new(
                format!("managed-default-{harness}-{index}"),
                "Clear default",
                &theme,
                &tokens,
            )
            .compact()
            .ghost()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.account_set_default(harness, None, cx);
            }))
            .child(probe(
                &self.regions,
                Region::SettingsItem("account-default", index),
            ))
        } else {
            Button::new(
                format!("managed-default-{harness}-{index}"),
                "Set as default",
                &theme,
                &tokens,
            )
            .compact()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.account_set_default(harness, Some(default_name.clone()), cx);
            }))
            .child(probe(
                &self.regions,
                Region::SettingsItem("account-default", index),
            ))
        });
        line = line.child(
            Button::new(
                format!("managed-remove-{harness}-{index}"),
                "Remove…",
                &theme,
                &tokens,
            )
            .compact()
            .ghost()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.account_remove(harness, &name, &label, window, cx);
            }))
            .child(probe(
                &self.regions,
                Region::SettingsItem("account-remove", index),
            )),
        );
        div()
            .pl(px(tokens.spacing * 3.0))
            .child(line)
            .into_any_element()
    }

    fn render_account_form(&self, form: &AccountForm, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let active = self
            .sheet
            .as_ref()
            .and_then(|s| s.edit.as_ref())
            .and_then(|e| e.field.strip_prefix(EDIT_PREFIX))
            .and_then(|f| f.strip_prefix(FORM_PREFIX))
            .and_then(AccountField::parse);
        let text_box = |this: &Self, field: AccountField, cx: &mut Context<Self>| {
            let base = div()
                .flex()
                .items_center()
                .px(px(tokens.spacing * 2.0))
                .h(px(tokens.control_height - 2.0))
                .rounded(px(tokens.radius))
                .border_1()
                .relative();
            let index = usize::from(field == AccountField::Label);
            let probed = probe(&this.regions, Region::SettingsItem("account-field", index));
            if active == Some(field) {
                base.border_color(theme.border)
                    .focus_ring(true, &theme, &tokens)
                    .children(this.overlay_parts(OverlayField::SheetValue, field.hint(), cx))
                    .child(probed)
                    .into_any_element()
            } else {
                let text = form.text(field);
                base.id(SharedString::from(format!("account-field-{}", field.id())))
                    .border_color(theme.border)
                    .cursor_text()
                    .text_color(if text.is_empty() {
                        theme.faint
                    } else {
                        theme.fg
                    })
                    .child(SharedString::from(if text.is_empty() {
                        field.hint().to_owned()
                    } else {
                        text.to_owned()
                    }))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.account_begin_field(field, cx)),
                    )
                    .child(probed)
                    .into_any_element()
            }
        };
        let line = |field: AccountField, control: AnyElement| {
            div()
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 3.0))
                .child(
                    div()
                        .w(px(80.0))
                        .flex_none()
                        .text_color(theme.muted)
                        .child(SharedString::from(field.title())),
                )
                .child(div().flex_1().min_w_0().child(control))
        };
        let name_box = text_box(self, AccountField::Name, cx);
        let label_box = text_box(self, AccountField::Label, cx);
        div()
            .id("account-form")
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 2.0))
            .my(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 3.0))
            .rounded(px(tokens.radius))
            .border_1()
            .border_color(theme.border)
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(SharedString::from(format!(
                        "Add an account to {}",
                        super::harnesses::harness_name(form.harness)
                    ))),
            )
            .child(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(
                        "Hutan creates an empty folder for it and opens the harness's own login in a terminal. \
                         It never reads what the harness keeps there.",
                    ),
            )
            .child(line(AccountField::Name, name_box))
            .child(line(AccountField::Label, label_box))
            .when_some(form.error.clone(), |s, message| {
                s.child(
                    div()
                        .id("account-form-error")
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.danger)
                        .child(SharedString::from(message)),
                )
            })
            .child(
                ui::dialog_footer()
                    .child(
                        Button::new("account-cancel", "Cancel", &theme, &tokens)
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| this.account_cancel(cx)))
                            .child(probe(&self.regions, Region::Settings("account-cancel"))),
                    )
                    .child(
                        Button::new(
                            "account-save",
                            if form.saving {
                                "Adding…"
                            } else {
                                "Add and log in"
                            },
                            &theme,
                            &tokens,
                        )
                        .compact()
                        .primary()
                        .disabled(form.saving)
                        .on_click(cx.listener(|this, _, window, cx| this.account_save(window, cx)))
                        .child(probe(&self.regions, Region::Settings("account-save"))),
                    ),
            )
            .into_any_element()
    }
}
