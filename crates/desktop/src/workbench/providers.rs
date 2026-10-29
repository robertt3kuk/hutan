//! Model-provider settings. Configuration belongs to the daemon; keys are write-only inputs.

use super::chat::Look;
use super::modal::PromptSpec;
use super::ui::StyledExt as _;
use super::view::Workbench;
use crate::theme::DensityExt as _;
use crate::workbench::ui::Button;
use gpui::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*,
    px,
};
use hutan_client::ClientError;
use hutan_core::{ModelInfo, SessionId};
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::{Empty, Request, SessionResult};
use hutan_protocol::provider::{
    ChatSetModel, ProviderAdd, ProviderDiscovery, ProviderInfo, ProviderList, ProviderModels,
    ProviderRef, ProviderResult, ProviderSetCredential, ProviderUpdate,
};
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct ProvidersState {
    pub providers: Vec<ProviderInfo>,
    pub models: HashMap<String, Vec<ModelInfo>>,
    pub error: Option<ProviderFailure>,
    /// The "Details" of the failure on screen are open.
    pub details_open: bool,
    pub loading: bool,
    /// Counts the local changes of `providers` (a provider added, updated or removed). A list
    /// requested at generation `n` is only applied while it is still `n`: an answer older than
    /// the latest local change would undo it.
    generation: u64,
}

/// What went wrong with a provider, for a person: a plain sentence that says what to check,
/// and the technical text the daemon sent, behind "Details".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure {
    pub summary: String,
    pub details: String,
}

impl ProviderFailure {
    /// A message that needs no details.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            summary: text.into(),
            details: String::new(),
        }
    }
}

/// What kind of failure a provider's error is, by what the gateway says (its messages are the
/// contract: `hutan_gateway::GatewayError`'s `Display`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// Nothing answered at the address.
    Connect,
    /// Something answered too slowly.
    Timeout,
    /// 401 / 403, or no key at all.
    Auth,
    /// 404: the model or the address is wrong.
    NotFound,
    Other,
}

/// The kind of a provider error from its message.
pub fn provider_error_kind(message: &str) -> ProviderErrorKind {
    let message = message.to_ascii_lowercase();
    if message.contains("could not connect to the provider") {
        ProviderErrorKind::Connect
    } else if message.contains("did not answer in time") || message.contains("timed out") {
        ProviderErrorKind::Timeout
    } else if message.contains("refused the credentials")
        || message.contains("needs an api key")
        || message.contains("(http 401)")
        || message.contains("(http 403)")
    {
        ProviderErrorKind::Auth
    } else if message.starts_with("not found:") {
        ProviderErrorKind::NotFound
    } else {
        ProviderErrorKind::Other
    }
}

/// The failure of a call about `provider` (its name and base URL, when the call was about one),
/// in words a person can act on.
pub fn provider_failure(provider: Option<(&str, &str)>, error: &ClientError) -> ProviderFailure {
    let details = error.to_string();
    let message = match error {
        ClientError::Remote(body) if body.code == ErrorCode::ProviderError => body.message.as_str(),
        ClientError::Remote(body) => {
            // The daemon's own refusals (a missing name, an unknown provider) are plain already.
            return ProviderFailure {
                summary: body.message.clone(),
                details,
            };
        }
        ClientError::Disconnected => {
            return ProviderFailure {
                summary: "Hutan lost the connection to the daemon. It reconnects by itself; try again in a moment.".to_owned(),
                details,
            };
        }
        _ => "",
    };
    let named = provider.map(|(name, _)| name);
    let subject = named.unwrap_or("the provider");
    let capital = named.map_or_else(|| "The provider".to_owned(), str::to_owned);
    let summary = match (provider_error_kind(message), provider) {
        (ProviderErrorKind::Connect, Some((name, url))) if !url.is_empty() => format!(
            "Could not connect to {name} at {url}. Check that the server is running and the address is right."
        ),
        (ProviderErrorKind::Connect, _) => format!(
            "Could not connect to {subject}. Check that the server is running and the address is right."
        ),
        (ProviderErrorKind::Timeout, _) => format!(
            "{capital} did not answer in time. Check that the server is running, then try again."
        ),
        (ProviderErrorKind::Auth, _) => format!(
            "{capital} did not accept the API key, or has none. Set a valid key for it, and check that the key may use this model."
        ),
        (ProviderErrorKind::NotFound, _) => format!(
            "{capital} does not know that model or address. Check the model name and the address of the provider."
        ),
        (ProviderErrorKind::Other, _) => format!("{capital} reported a problem."),
    };
    ProviderFailure { summary, details }
}

impl ProvidersState {
    /// The generation a list request is made at.
    fn request_generation(&self) -> u64 {
        self.generation
    }

    /// Applies a list answered for a request made at `requested`; `false` (nothing applied)
    /// when a local change happened since.
    fn apply_list(&mut self, requested: u64, list: Vec<ProviderInfo>) -> bool {
        if requested != self.generation {
            return false;
        }
        self.providers = list;
        true
    }

    /// Records a provider the daemon just answered for.
    fn remember(&mut self, info: ProviderInfo) {
        self.generation += 1;
        if let Some(existing) = self.providers.iter_mut().find(|p| p.id == info.id) {
            *existing = info;
        } else {
            self.providers.push(info);
        }
    }

    /// Forgets a removed provider and its models.
    fn forget(&mut self, id: &str) {
        self.generation += 1;
        self.providers.retain(|p| p.id != id);
        self.models.remove(id);
    }
}

impl Workbench {
    pub fn providers(&self) -> &[ProviderInfo] {
        &self.providers.providers
    }

    /// The failure of the last provider call, in the words the Models page shows.
    pub fn provider_failure(&self) -> Option<&ProviderFailure> {
        self.providers.error.as_ref()
    }

    /// Whether the provider list requested when the Models sheet opened is still on its way.
    pub fn providers_loading(&self) -> bool {
        self.providers.loading
    }
    fn provider_call<T>(
        &mut self,
        request: Request,
        done: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) where
        T: serde::de::DeserializeOwned + Send + 'static,
    {
        let connection = self.sessions.read(cx).connection().clone();
        // The name and address of the provider the call is about, for the words of a failure.
        let about = match &request {
            Request::ProviderUpdate(ProviderUpdate { provider_id, .. })
            | Request::ProviderRemove(ProviderRef { provider_id })
            | Request::ProviderSetCredential(ProviderSetCredential { provider_id, .. })
            | Request::ProviderClearCredential(ProviderRef { provider_id })
            | Request::ProviderModels(ProviderRef { provider_id }) => self
                .providers
                .providers
                .iter()
                .find(|p| p.id == *provider_id)
                .map(|p| (p.name.clone(), p.base_url.clone())),
            Request::ProviderAdd(add) => add.base_url.clone().map(|url| (add.name.clone(), url)),
            _ => None,
        };
        let task = cx.background_spawn(async move { connection.call::<T>(request).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(answer) => done(this, answer, cx),
                    Err(error) => {
                        let failure = provider_failure(
                            about.as_ref().map(|(n, u)| (n.as_str(), u.as_str())),
                            &error,
                        );
                        this.providers.loading = false;
                        if let Some(picker) = &mut this.agent_picker
                            && picker.direct
                        {
                            picker.creating = false;
                            picker.error = Some(failure.clone());
                        }
                        this.providers.details_open = false;
                        this.providers.error = Some(failure);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn open_models_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(Some("models"), window, cx);
    }

    pub fn refresh_providers(&mut self, cx: &mut Context<Self>) {
        self.providers.loading = true;
        self.providers.error = None;
        let requested = self.providers.request_generation();
        self.provider_call(
            Request::ProviderList(Empty {}),
            move |this, list: ProviderList, cx| {
                if !this.providers.apply_list(requested, list.providers) {
                    // Older than a local change: ask again rather than undo it.
                    this.refresh_providers(cx);
                    return;
                }
                this.providers.loading = false;
                this.launcher_on_providers_changed(cx);
                if this
                    .agent_picker
                    .as_ref()
                    .is_some_and(|p| p.direct && p.provider_id.is_none())
                {
                    if let Some(id) = this.providers.providers.first().map(|p| p.id.clone()) {
                        this.choose_direct_provider(&id, cx);
                    } else if let Some(picker) = &mut this.agent_picker {
                        picker.models = super::agents::Models::Ready(Vec::new());
                        picker.error = Some(ProviderFailure::plain(
                            "Configure a provider in Models settings first",
                        ));
                    }
                }
            },
            cx,
        );
    }

    fn remember_provider(&mut self, info: ProviderInfo) {
        self.providers.remember(info);
    }

    pub fn set_provider_key(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let provider_id = id.to_owned();
        self.open_prompt(
            PromptSpec {
                title: "Set API key",
                message: "Stored only in macOS Keychain by hutand. Paste the key here.",
                initial: "",
                confirm_label: "Store key",
            },
            move |this, secret, _, cx| {
                // The modal has been taken from Workbench before this callback runs. Move the key
                // into the one request; it never enters settings, state files or provider metadata.
                this.provider_call(
                    Request::ProviderSetCredential(ProviderSetCredential {
                        provider_id: provider_id.clone(),
                        secret,
                    }),
                    |this, result: ProviderResult, _| this.remember_provider(result.provider),
                    cx,
                );
            },
            window,
            cx,
        );
        if let Some(modal) = &mut self.modal {
            modal.secure_input = true;
        }
    }

    /// "Clear key": asks first. The key leaves the Keychain and Hutan cannot show it again.
    pub fn clear_provider_key(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let id = id.to_owned();
        self.open_destructive(
            super::settings_labels::words::CLEAR_KEY_TITLE,
            super::settings_labels::words::CLEAR_KEY_MESSAGE,
            super::settings_labels::words::CLEAR_KEY_CONFIRM,
            move |this, _, cx| this.clear_provider_key_now(&id, cx),
            window,
            cx,
        );
    }

    /// Removes the key of provider `id` from the Keychain (what Clear key does once confirmed).
    pub fn clear_provider_key_now(&mut self, id: &str, cx: &mut Context<Self>) {
        self.provider_call(
            Request::ProviderClearCredential(ProviderRef {
                provider_id: id.to_owned(),
            }),
            |this, result: ProviderResult, _| this.remember_provider(result.provider),
            cx,
        );
    }

    pub fn remove_provider(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let id = id.to_owned();
        self.open_destructive(
            "Remove provider?",
            "Existing chats keep their history; this provider and its stored key will be removed.",
            "Remove",
            move |this, _, cx| {
                let removed = id.clone();
                this.provider_call(
                    Request::ProviderRemove(ProviderRef {
                        provider_id: id.clone(),
                    }),
                    move |this, _: Empty, _| {
                        this.providers.forget(&removed);
                    },
                    cx,
                );
            },
            window,
            cx,
        );
    }

    pub fn provider_models(&mut self, id: &str, cx: &mut Context<Self>) {
        self.provider_call(
            Request::ProviderModels(ProviderRef {
                provider_id: id.to_owned(),
            }),
            |this, answer: ProviderModels, _| {
                this.providers
                    .models
                    .insert(answer.provider_id, answer.models);
            },
            cx,
        );
    }

    /// Add/edit uses the existing prompt dialogs for non-secret fields. The kind stays fixed
    /// on edits: changing a wire format is a new provider, not a silent conversion.
    pub fn edit_provider(
        &mut self,
        existing: Option<ProviderInfo>,
        kind: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let kind = kind.to_owned();
        let name = existing
            .as_ref()
            .map_or(kind.as_str(), |p| p.name.as_str())
            .to_owned();
        let spec = PromptSpec {
            title: "Provider name",
            message: "Display name",
            initial: &name,
            confirm_label: "Next",
        };
        let old = existing.clone();
        self.open_prompt(
            spec,
            move |this, name, window, cx| {
                let url = old.as_ref().map(|p| p.base_url.clone()).unwrap_or_else(|| {
                    match kind.as_str() {
                        "anthropic" => "https://api.anthropic.com".into(),
                        "ollama" => "http://127.0.0.1:11434".into(),
                        "openai-compatible" => "http://127.0.0.1:1234/v1".into(),
                        _ => "https://api.openai.com/v1".into(),
                    }
                });
                let old = old.clone();
                let kind = kind.clone();
                this.open_prompt(
                    PromptSpec {
                        title: "Provider endpoint",
                        message: "Base URL (HTTP or HTTPS, without credentials)",
                        initial: &url,
                        confirm_label: "Next",
                    },
                    move |this, base_url, window, cx| {
                        let old = old.clone();
                        let kind = kind.clone();
                        let name = name.clone();
                        let initial = old
                            .as_ref()
                            .and_then(|p| p.default_model.as_deref())
                            .unwrap_or("auto")
                            .to_owned();
                        this.open_prompt(
                            PromptSpec {
                                title: "Default model",
                                message: "Model ID, or auto to use the provider's default",
                                initial: &initial,
                                confirm_label: "Save",
                            },
                            move |this, model, _, cx| {
                                let default_model = (model != "auto").then_some(model);
                                let req = if let Some(old) = &old {
                                    Request::ProviderUpdate(ProviderUpdate {
                                        provider_id: old.id.clone(),
                                        name: Some(name.clone()),
                                        base_url: Some(base_url.clone()),
                                        default_model: Some(default_model.unwrap_or_default()),
                                    })
                                } else {
                                    Request::ProviderAdd(ProviderAdd {
                                        kind: kind.clone(),
                                        name: name.clone(),
                                        base_url: Some(base_url.clone()),
                                        default_model,
                                    })
                                };
                                this.provider_call(
                                    req,
                                    |this, result: ProviderResult, _| {
                                        this.remember_provider(result.provider)
                                    },
                                    cx,
                                );
                            },
                            window,
                            cx,
                        );
                    },
                    window,
                    cx,
                );
            },
            window,
            cx,
        );
    }

    pub fn discover_providers(&mut self, cx: &mut Context<Self>) {
        self.provider_call(
            Request::ProviderDiscover(Empty {}),
            |this, result: ProviderDiscovery, _| {
                let entries = result
                    .found
                    .into_iter()
                    .filter(|p| p.provider_id.is_none())
                    .map(|p| {
                        let name = p.name.clone();
                        let request = ProviderAdd {
                            kind: p.kind,
                            name: p.name,
                            base_url: Some(p.base_url),
                            default_model: p.models.first().cloned(),
                        };
                        super::view::MenuEntry {
                            label: format!("Add {name}").into(),
                            checked: false,
                            disabled: false,
                            submenu: false,
                            hint: None,
                            separator: false,
                            tip: None,
                            run: std::rc::Rc::new(move |this, _, cx| {
                                this.provider_call(
                                    Request::ProviderAdd(request.clone()),
                                    |this, result: ProviderResult, _| {
                                        this.remember_provider(result.provider)
                                    },
                                    cx,
                                );
                            }),
                        }
                    })
                    .collect::<Vec<_>>();
                if entries.is_empty() {
                    this.providers.error = Some(ProviderFailure::plain(
                        "No unconfigured local model servers found on ports 11434 or 1234",
                    ));
                } else {
                    this.context_menu = Some(super::view::ContextMenu {
                        position: gpui::point(px(400.0), px(180.0)),
                        entries,
                    });
                }
            },
            cx,
        );
    }

    pub(crate) fn render_providers(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let look = Look::of(cx);
        let mut body = div().flex().flex_col().gap(px(12.0)).py(px(12.0))
            .child(div().child("Models and providers"))
            .child(div().text_color(look.theme.muted).child("Hutan keeps provider settings and keys for you. Keys are stored in the macOS Keychain and are never shown again."));
        let mut add = div().flex().flex_wrap().gap(px(6.0));
        for (kind, label) in [
            ("openai", "Add OpenAI"),
            ("anthropic", "Add Anthropic"),
            ("ollama", "Add Ollama"),
            ("openai-compatible", "Add compatible"),
        ] {
            add = add.child(
                Button::new(
                    format!("provider-add-{kind}"),
                    label,
                    &look.theme,
                    &look.tokens,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.edit_provider(None, kind, window, cx)
                })),
            );
        }
        body = body.child(add).child(
            div()
                .flex()
                .gap(px(6.0))
                .child(
                    Button::new("provider-refresh", "Refresh", &look.theme, &look.tokens)
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_providers(cx))),
                )
                .child(
                    Button::new(
                        "provider-discover",
                        "Discover local",
                        &look.theme,
                        &look.tokens,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.discover_providers(cx))),
                ),
        );
        if self.providers.loading {
            body = body.child("Loading providers…");
        }
        if let Some(error) = self.providers.error.clone() {
            body = body.child(self.render_provider_failure(&error, cx));
        }
        for provider in &self.providers.providers {
            let id = provider.id.clone();
            let edit = provider.clone();
            let key = id.clone();
            let clear = id.clone();
            // Nothing to clear without a key: the button is off and does nothing.
            let has_key = provider.has_credential;
            let remove = id.clone();
            let models = id.clone();
            let mut row = div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .p(px(12.0))
                .mb(px(8.0))
                .card(&look.theme, &look.tokens)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .child(SharedString::from(format!(
                            "{} · {} · key",
                            provider.name, provider.kind
                        )))
                        .child(crate::icons::glyph(if provider.has_credential {
                            crate::icons::Icon::StateCompleted
                        } else {
                            crate::icons::Icon::StateUnknown
                        }))
                        .child(if provider.has_credential {
                            "saved"
                        } else {
                            "missing"
                        }),
                )
                .child(
                    div()
                        .text_color(look.theme.muted)
                        .child(SharedString::from(format!(
                            "{} · default {}",
                            provider.base_url,
                            provider.default_model.as_deref().unwrap_or("auto")
                        ))),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.0))
                        .child(
                            Button::new(
                                format!("provider-edit-{id}"),
                                "Edit",
                                &look.theme,
                                &look.tokens,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.edit_provider(Some(edit.clone()), &edit.kind, window, cx)
                                },
                            )),
                        )
                        .child(
                            Button::new(
                                format!("provider-key-{id}"),
                                "Set key…",
                                &look.theme,
                                &look.tokens,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| this.set_provider_key(&key, window, cx),
                            )),
                        )
                        .child(
                            Button::new(
                                format!("provider-clear-{id}"),
                                "Clear key",
                                &look.theme,
                                &look.tokens,
                            )
                            .disabled(!has_key)
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    if has_key {
                                        this.clear_provider_key(&clear, window, cx)
                                    }
                                },
                            )),
                        )
                        .child(
                            Button::new(
                                format!("provider-models-{id}"),
                                "Models / refresh",
                                &look.theme,
                                &look.tokens,
                            )
                            .on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.provider_models(&models, cx)
                                }),
                            ),
                        )
                        .child(
                            Button::new(
                                format!("provider-remove-{id}"),
                                "Remove…",
                                &look.theme,
                                &look.tokens,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.remove_provider(&remove, window, cx)
                                },
                            )),
                        ),
                );
            if let Some(models) = self.providers.models.get(&id) {
                for model in models {
                    row = row.child(
                        div()
                            .text_color(look.theme.muted)
                            .child(SharedString::from(model.id.clone())),
                    );
                }
            }
            body = body.child(row);
        }
        body.into_any_element()
    }

    pub fn choose_direct_provider(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(picker) = &mut self.agent_picker else {
            return;
        };
        if picker.creating {
            return;
        }
        picker.direct = true;
        picker.provider_id = Some(id.to_owned());
        picker.models = super::agents::Models::Loading;
        picker.error = None;
        picker.selected = 0;
        picker.model_query.clear();
        let chosen = id.to_owned();
        let default_model = self
            .providers
            .providers
            .iter()
            .find(|p| p.id == id)
            .and_then(|p| p.default_model.clone());
        self.provider_call(
            Request::ProviderModels(ProviderRef {
                provider_id: id.to_owned(),
            }),
            move |this, mut result: ProviderModels, _| {
                let Some(picker) = &mut this.agent_picker else {
                    return;
                };
                if picker.provider_id.as_deref() != Some(chosen.as_str()) {
                    return;
                }
                if let Some(default) = &default_model {
                    for model in &mut result.models {
                        model.is_default = &model.id == default;
                    }
                }
                picker.models = super::agents::Models::Ready(result.models);
                picker.selected = picker.initial_selection();
                picker.reveal_selected();
            },
            cx,
        );
        cx.notify();
    }

    pub fn open_model_switch(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chat = self
            .sessions
            .read(cx)
            .session(session)
            .and_then(|s| s.chat.clone());
        let Some(chat) = chat else {
            return;
        };
        if chat.turn_active {
            self.set_notice("The model can be switched between turns", cx);
            return;
        }
        if super::chat::is_harness_chat(&chat.agent) {
            self.open_harness_model_switch(session, chat.agent, window, cx);
            return;
        }
        if !super::chat::is_gateway_chat(&chat.agent) {
            self.set_notice(
                "A Codex chat keeps the model it started with: start another chat to change it",
                cx,
            );
            return;
        }
        self.open_direct_model_switch(session, window, cx);
    }

    pub(crate) fn confirm_model_switch(
        &mut self,
        session: SessionId,
        provider_id: Option<String>,
        model: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let connection = self.sessions.read(cx).connection().clone();
        let about = provider_id.as_deref().and_then(|id| {
            self.providers
                .providers
                .iter()
                .find(|p| p.id == id)
                .map(|p| (p.name.clone(), p.base_url.clone()))
        });
        let task = cx.background_spawn(async move {
            connection
                .call::<SessionResult>(Request::ChatSetModel(ChatSetModel {
                    session_id: session,
                    provider_id,
                    model,
                }))
                .await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(_) => this.close_agent_picker(window, cx),
                Err(error) => {
                    if let Some(picker) = &mut this.agent_picker {
                        picker.creating = false;
                        picker.error = Some(provider_failure(
                            about.as_ref().map(|(n, u)| (n.as_str(), u.as_str())),
                            &error,
                        ));
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The failure to list models, as the Start sheet and the model picker show it: with its
    /// "Details" when it is the provider failure on record, else as plain text.
    pub(crate) fn render_models_failure(
        &self,
        summary: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match &self.providers.error {
            Some(failure) if failure.summary == summary => {
                self.render_provider_failure(failure, cx)
            }
            _ => div()
                .text_color(self.theme.danger)
                .child(SharedString::from(summary.to_owned()))
                .into_any_element(),
        }
    }

    /// A failure as the Models page, the Start sheet and the model picker show it: the plain
    /// sentence, and "Details" with what the daemon said.
    pub(crate) fn render_provider_failure(
        &self,
        failure: &ProviderFailure,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let open = self.providers.details_open;
        let has_details = !failure.details.is_empty();
        div()
            .id("provider-failure")
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .child(
                div()
                    .text_color(theme.danger)
                    .child(SharedString::from(failure.summary.clone())),
            )
            .when(has_details, |s| {
                s.child(
                    div()
                        .id("provider-failure-details")
                        .cursor_pointer()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.providers.details_open = !this.providers.details_open;
                            cx.notify();
                        }))
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .child(super::settings_labels::words::PROVIDER_DETAILS)
                        .child(crate::icons::glyph(if open {
                            crate::icons::Icon::ActionCollapse
                        } else {
                            crate::icons::Icon::ActionExpand
                        })),
                )
            })
            .when(has_details && open, |s| {
                s.child(
                    div()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child(SharedString::from(failure.details.clone())),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str) -> ProviderInfo {
        ProviderInfo {
            id: id.to_owned(),
            name: id.to_owned(),
            kind: "openai".to_owned(),
            base_url: String::new(),
            default_model: None,
            has_credential: false,
            created_at_ms: 0,
        }
    }

    fn remote(message: &str) -> ClientError {
        ClientError::Remote(hutan_protocol::ErrorBody::new(
            ErrorCode::ProviderError,
            message,
        ))
    }

    const LOCAL: Option<(&str, &str)> = Some(("r4-local", "http://127.0.0.1:9"));

    #[test]
    fn a_refused_connection_names_the_provider_and_the_address() {
        let error = remote(
            "could not connect to the provider after 3 attempt(s): client error (Connect): tcp connect error: Connection refused (os error 61)",
        );
        assert_eq!(
            provider_error_kind(&error.to_string()),
            ProviderErrorKind::Connect
        );
        let failure = provider_failure(LOCAL, &error);
        assert_eq!(
            failure.summary,
            "Could not connect to r4-local at http://127.0.0.1:9. Check that the server is running and the address is right."
        );
        // The technical text is kept, for Details.
        assert!(failure.details.contains("os error 61"), "{failure:?}");
        assert!(failure.details.contains("3 attempt(s)"), "{failure:?}");
        assert!(!failure.summary.contains("os error"));
        // Without a provider at hand the sentence still reads.
        let failure = provider_failure(None, &error);
        assert!(
            failure
                .summary
                .starts_with("Could not connect to the provider."),
            "{failure:?}"
        );
    }

    #[test]
    fn each_kind_of_failure_gets_its_own_sentence() {
        let cases = [
            (
                "the provider did not answer in time",
                ProviderErrorKind::Timeout,
                "r4-local did not answer in time. Check that the server is running, then try again.",
            ),
            (
                "the provider refused the credentials (HTTP 401): bad key",
                ProviderErrorKind::Auth,
                "r4-local did not accept the API key, or has none. Set a valid key for it, and check that the key may use this model.",
            ),
            (
                "the provider refused the credentials (HTTP 403): no access",
                ProviderErrorKind::Auth,
                "r4-local did not accept the API key, or has none. Set a valid key for it, and check that the key may use this model.",
            ),
            (
                "the provider needs an API key and none is stored",
                ProviderErrorKind::Auth,
                "r4-local did not accept the API key, or has none. Set a valid key for it, and check that the key may use this model.",
            ),
            (
                "not found: model gpt-x does not exist; check the model name and the provider's base URL",
                ProviderErrorKind::NotFound,
                "r4-local does not know that model or address. Check the model name and the address of the provider.",
            ),
            (
                "the provider failed (HTTP 500): boom",
                ProviderErrorKind::Other,
                "r4-local reported a problem.",
            ),
        ];
        for (message, kind, summary) in cases {
            let error = remote(message);
            assert_eq!(provider_error_kind(message), kind, "{message}");
            let failure = provider_failure(LOCAL, &error);
            assert_eq!(failure.summary, summary, "{message}");
            assert!(failure.details.contains(message), "{failure:?}");
        }
        // No provider: the subject is "The provider".
        let failure = provider_failure(None, &remote("the provider did not answer in time"));
        assert!(
            failure
                .summary
                .starts_with("The provider did not answer in time.")
        );
    }

    #[test]
    fn the_daemons_own_refusals_and_a_lost_connection_stay_plain() {
        let refusal = ClientError::Remote(hutan_protocol::ErrorBody::new(
            ErrorCode::BadRequest,
            "a provider needs a name",
        ));
        let failure = provider_failure(None, &refusal);
        assert_eq!(failure.summary, "a provider needs a name");
        let failure = provider_failure(LOCAL, &ClientError::Disconnected);
        assert!(
            failure.summary.contains("lost the connection"),
            "{failure:?}"
        );
        assert!(failure.details.contains("disconnected"), "{failure:?}");
    }

    #[test]
    fn a_list_older_than_a_local_change_is_dropped() {
        let mut state = ProvidersState::default();
        let requested = state.request_generation();
        // The user adds a provider while the list is on its way.
        state.remember(info("new"));
        assert!(!state.apply_list(requested, Vec::new()));
        assert_eq!(state.providers.len(), 1, "the new provider survives");
        // A list requested after the change applies.
        let requested = state.request_generation();
        assert!(state.apply_list(requested, vec![info("new"), info("other")]));
        assert_eq!(state.providers.len(), 2);
    }

    #[test]
    fn a_removal_also_outdates_lists_in_flight() {
        let mut state = ProvidersState::default();
        state.remember(info("gone"));
        let requested = state.request_generation();
        state.forget("gone");
        assert!(!state.apply_list(requested, vec![info("gone")]));
        assert!(state.providers.is_empty());
    }
}
