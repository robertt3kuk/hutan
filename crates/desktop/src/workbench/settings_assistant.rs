//! The Assistant page's part that belongs to the daemon: which backend and model answer the
//! assistants of each place, and what they may read.
//!
//! A place is Global (the Home workspace) or the workspace or project the window shows.
//! Each has Inherit, OMP, Claude Code, Codex and Provider, and a model (Automatic, or one of the
//! backend's catalog). Choosing stores it with `assistant.configure`; Inherit calls
//! `assistant.reset`, so the place follows the one above it again: its parent workspace, then
//! Global, then `daemon.toml`, then Automatic (the first backend that is ready).
//!
//! The daemon cannot say what is stored for a place. The window therefore shows what *it*
//! stored or reset since it started, and, from `assistant.list`, what the default assistant of
//! the place runs on now. Permissions (`assistant.context.*`, `assistant.global_context.*`,
//! and the overrides of the shown workspace) are plain daemon settings.

use std::collections::HashMap;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::*, px,
};
use hutan_core::{ModelInfo, WorkspaceId};
use hutan_protocol::assistant::{
    AssistantBackend, AssistantConfig, AssistantConfigure, AssistantListRequest, AssistantScope,
};
use hutan_protocol::grants::ControlGrant;
use hutan_protocol::messages::{WorkspaceInfo, WorkspaceKind};

use super::agents::Models;
use super::panels::ScopeKey;
use super::settings_daemon::sections;
use super::settings_labels::{daemon_field_help, daemon_field_title, words};
use super::settings_rows::{note, sheet_row, small_button};
use super::view::Workbench;
use crate::theme::DensityExt as _;

/// What the default assistant of a place runs on (`assistant.list`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Running {
    pub backend: AssistantBackend,
    pub provider: Option<String>,
    pub model: String,
}

/// What Mawas may do without asking: the grants its approvals stored (`control.grants.list`).
#[derive(Debug, Default)]
pub struct MawasGrants {
    pub grants: Vec<ControlGrant>,
    /// The daemon answered at least once.
    pub loaded: bool,
    /// The daemon has no grants (older than protocol 0.31).
    pub unsupported: bool,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct AssistantDefaults {
    running: HashMap<ScopeKey, Running>,
    /// What this window stored (`Some`) or reset (`None`) since it started.
    chosen: HashMap<ScopeKey, Option<AssistantConfig>>,
    errors: HashMap<ScopeKey, String>,
    /// The place whose providers are listed (after "Provider").
    providers_open: Option<ScopeKey>,
}

impl AssistantDefaults {
    /// What the default assistant of `scope` runs on, when it exists.
    pub fn running(&self, scope: ScopeKey) -> Option<&Running> {
        self.running.get(&scope)
    }

    /// `Some(Some(config))`: this window stored it; `Some(None)`: it reset the place; `None`:
    /// not known.
    pub fn chosen(&self, scope: ScopeKey) -> Option<Option<&AssistantConfig>> {
        self.chosen.get(&scope).map(Option::as_ref)
    }

    pub fn error(&self, scope: ScopeKey) -> Option<&str> {
        self.errors.get(&scope).map(String::as_str)
    }
}

pub(super) fn tag(scope: ScopeKey) -> String {
    match scope {
        ScopeKey::Global => "global".to_owned(),
        ScopeKey::Workspace(id) => id.to_string(),
    }
}

fn wire(scope: ScopeKey) -> (AssistantScope, Option<WorkspaceId>) {
    match scope {
        ScopeKey::Global => (AssistantScope::Global, None),
        ScopeKey::Workspace(id) => (AssistantScope::Workspace, Some(id)),
    }
}

fn backend_name(backend: AssistantBackend) -> &'static str {
    match backend {
        AssistantBackend::Omp => words::BACKEND_OMP,
        AssistantBackend::Claude => words::BACKEND_CLAUDE,
        AssistantBackend::Codex => words::BACKEND_CODEX,
        AssistantBackend::Provider => words::BACKEND_PROVIDER,
    }
}

/// "OMP", "Provider · mock".
fn backend_text(backend: AssistantBackend, provider: Option<&str>) -> String {
    match provider {
        Some(provider) => format!("{} · {provider}", backend_name(backend)),
        None => backend_name(backend).to_owned(),
    }
}

fn model_label(models: &[ModelInfo], id: &str) -> String {
    models
        .iter()
        .find(|m| m.id == id)
        .map_or_else(|| words::model_unlisted(id), |m| m.display_name.clone())
}

const BACKENDS: [AssistantBackend; 4] = [
    AssistantBackend::Omp,
    AssistantBackend::Claude,
    AssistantBackend::Codex,
    AssistantBackend::Provider,
];

const PERMISSIONS: [&str; 4] = ["shell", "agents", "files", "search"];

impl Workbench {
    // ---- reading -------------------------------------------------------------------------

    pub fn assistant_defaults(&self) -> &AssistantDefaults {
        &self.assistant_defaults
    }

    /// The workspace or project the window shows, when it is not Home.
    pub fn assistant_workspace(&self, cx: &gpui::App) -> Option<WorkspaceInfo> {
        self.sessions
            .read(cx)
            .active_workspace()
            .filter(|w| w.kind != WorkspaceKind::Home)
            .cloned()
    }

    /// The places the page sets a backend for: Global, and the shown workspace.
    pub fn assistant_scopes(&self, cx: &gpui::App) -> Vec<ScopeKey> {
        let mut scopes = vec![ScopeKey::Global];
        if let Some(workspace) = self.assistant_workspace(cx) {
            scopes.push(ScopeKey::Workspace(workspace.id));
        }
        scopes
    }

    // ---- grants --------------------------------------------------------------------------

    /// What Mawas may do without asking, as the daemon last said.
    pub fn mawas_grants(&self) -> &MawasGrants {
        &self.mawas_grants
    }

    /// Reads the grants (`control.grants.list`).
    pub fn refresh_mawas_grants(&mut self, cx: &mut Context<Self>) {
        if !self.daemon_connected(cx) {
            return;
        }
        let connection = self.connection(cx);
        if !connection.supports_control_grants() {
            self.mawas_grants.unsupported = true;
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = connection.control_grants(None).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(grants) => {
                        this.mawas_grants.grants = grants;
                        this.mawas_grants.loaded = true;
                        this.mawas_grants.error = None;
                    }
                    Err(error) => this.mawas_grants.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Revokes one grant: the next request it covered asks again.
    pub fn revoke_mawas_grant(&mut self, id: u64, cx: &mut Context<Self>) {
        self.revoke_mawas_grants(vec![id], cx);
    }

    /// Revokes every grant.
    pub fn revoke_all_mawas_grants(&mut self, cx: &mut Context<Self>) {
        let ids = self.mawas_grants.grants.iter().map(|g| g.id).collect();
        self.revoke_mawas_grants(ids, cx);
    }

    fn revoke_mawas_grants(&mut self, ids: Vec<u64>, cx: &mut Context<Self>) {
        if ids.is_empty() || !self.daemon_connected(cx) {
            return;
        }
        let connection = self.connection(cx);
        cx.spawn(async move |this, cx| {
            let mut failure = None;
            for id in ids {
                if let Err(error) = connection.control_grant_revoke(id).await {
                    failure = Some(error.to_string());
                }
            }
            let grants = connection.control_grants(None).await;
            this.update(cx, |this, cx| {
                if let Ok(grants) = grants {
                    this.mawas_grants.grants = grants;
                    this.mawas_grants.loaded = true;
                }
                this.mawas_grants.error = failure;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- asking the daemon ---------------------------------------------------------------

    /// Reads what the default assistants run on (`assistant.list`) and asks for the models of
    /// the backends chosen so far.
    pub fn refresh_assistant_defaults(&mut self, cx: &mut Context<Self>) {
        if !self.daemon_connected(cx) {
            return;
        }
        let connection = self.connection(cx);
        if !connection.supports_assistant() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    connection
                        .assistant_list(AssistantListRequest::default())
                        .await
                })
                .await;
            this.update(cx, |this, cx| {
                if let Ok(entries) = result {
                    this.assistant_defaults.running.clear();
                    for entry in entries.into_iter().filter(|e| e.default) {
                        let key = match entry.scope {
                            AssistantScope::Global => ScopeKey::Global,
                            AssistantScope::Workspace => ScopeKey::Workspace(entry.workspace_id),
                        };
                        let chat = entry.session.chat.as_ref();
                        this.assistant_defaults.running.insert(
                            key,
                            Running {
                                backend: entry.backend,
                                provider: chat.and_then(|c| c.provider_id.clone()),
                                model: chat.map(|c| c.model.clone()).unwrap_or_default(),
                            },
                        );
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        // The models of what is chosen, for the model menus.
        let chosen: Vec<AssistantConfig> = self
            .assistant_defaults
            .chosen
            .values()
            .flatten()
            .cloned()
            .collect();
        for config in chosen {
            self.load_assistant_models(config.backend, config.provider.as_deref(), cx);
        }
    }

    /// Asks for the models of a backend: the harness's catalog, or the provider's.
    fn load_assistant_models(
        &mut self,
        backend: AssistantBackend,
        provider: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        match (backend.harness(), provider) {
            (Some(harness), _) => {
                self.env
                    .harnesses
                    .update(cx, |directory, cx| directory.load_models(harness, cx));
            }
            (None, Some(provider)) => self.provider_models(provider, cx),
            (None, None) => {}
        }
    }

    /// The models `config`'s backend offers.
    fn assistant_models(
        &self,
        backend: AssistantBackend,
        provider: Option<&str>,
        cx: &gpui::App,
    ) -> Models {
        match (backend.harness(), provider) {
            (Some(harness), _) => self
                .env
                .harnesses
                .read(cx)
                .models(harness)
                .cloned()
                .unwrap_or(Models::Loading),
            (None, Some(provider)) => self
                .providers
                .models
                .get(provider)
                .cloned()
                .map_or(Models::Loading, Models::Ready),
            (None, None) => Models::Ready(Vec::new()),
        }
    }

    // ---- choosing ------------------------------------------------------------------------

    /// Stores `backend` (with `provider` for a provider, and `model`, none for Automatic) as
    /// what the assistants of `scope` are created with from now on.
    pub fn assistant_configure(
        &mut self,
        scope: ScopeKey,
        backend: AssistantBackend,
        provider: Option<String>,
        model: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if !self.daemon_connected(cx) {
            return;
        }
        let connection = self.connection(cx);
        let (kind, workspace_id) = wire(scope);
        self.assistant_defaults.errors.remove(&scope);
        let request = AssistantConfigure {
            scope: kind,
            workspace_id,
            backend,
            model,
            provider,
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { connection.assistant_configure(request).await })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(config) => {
                        this.sessions.update(cx, |store, cx| {
                            store.note_assistant_pinned(wire(scope).1, true, cx);
                        });
                        this.load_assistant_models(config.backend, config.provider.as_deref(), cx);
                        this.assistant_defaults.chosen.insert(scope, Some(config));
                        this.assistant_defaults.providers_open = None;
                        this.refresh_assistant_defaults(cx);
                    }
                    Err(error) => {
                        this.assistant_defaults
                            .errors
                            .insert(scope, error.to_string());
                    }
                }
                if let Some(sheet) = &mut this.sheet {
                    sheet.dropdown = None;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A backend button: a harness is stored at once (Automatic model); Provider lists the
    /// providers to pick from.
    pub fn assistant_choose_backend(
        &mut self,
        scope: ScopeKey,
        backend: AssistantBackend,
        cx: &mut Context<Self>,
    ) {
        if backend == AssistantBackend::Provider {
            let open = self.assistant_defaults.providers_open != Some(scope);
            self.assistant_defaults.providers_open = open.then_some(scope);
            if open {
                self.refresh_providers(cx);
            }
            cx.notify();
            return;
        }
        self.assistant_configure(scope, backend, None, None, cx);
    }

    /// Picks the model of the backend stored for `scope`; `None` is Automatic: no model is
    /// stored and the daemon picks one when it creates the assistant.
    pub fn assistant_choose_model(
        &mut self,
        scope: ScopeKey,
        model: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(Some(config)) = self.assistant_defaults.chosen.get(&scope).cloned() else {
            return;
        };
        self.assistant_configure(scope, config.backend, config.provider, model, cx);
    }

    /// Inherit: `assistant.reset`, the place follows the one above it again.
    pub fn assistant_inherit(&mut self, scope: ScopeKey, cx: &mut Context<Self>) {
        if !self.daemon_connected(cx) {
            return;
        }
        let connection = self.connection(cx);
        if !connection.supports_daemon_settings() {
            self.assistant_defaults
                .errors
                .insert(scope, words::DAEMON_UNSUPPORTED.to_owned());
            cx.notify();
            return;
        }
        let (kind, workspace_id) = wire(scope);
        self.assistant_defaults.errors.remove(&scope);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { connection.assistant_reset(kind, workspace_id).await })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.sessions.update(cx, |store, cx| {
                            store.note_assistant_pinned(wire(scope).1, false, cx);
                        });
                        this.assistant_defaults.chosen.insert(scope, None);
                        this.assistant_defaults.providers_open = None;
                        this.refresh_assistant_defaults(cx);
                    }
                    Err(error) => {
                        this.assistant_defaults
                            .errors
                            .insert(scope, error.to_string());
                    }
                }
                if let Some(sheet) = &mut this.sheet {
                    sheet.dropdown = None;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Allows, denies (`Some`) or inherits (`None`) one permission for the shown workspace.
    pub fn assistant_permission_set(
        &mut self,
        permission: &str,
        allowed: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.assistant_workspace(cx) else {
            return;
        };
        let keys = self.workspace_permission_keys(&workspace, permission);
        match allowed {
            Some(allowed) => {
                // The key the file already uses (the workspace's name or id), else its id.
                let key = keys
                    .iter()
                    .find(|k| self.daemon_settings.entry(k).is_some_and(|e| e.set_in_file))
                    .cloned()
                    .unwrap_or_else(|| keys[0].clone());
                self.daemon_setting_set(&key, Some(serde_json::Value::Bool(allowed)), cx);
            }
            None => {
                let set: Vec<String> = keys
                    .into_iter()
                    .filter(|k| self.daemon_settings.entry(k).is_some_and(|e| e.set_in_file))
                    .collect();
                for key in set {
                    self.daemon_setting_reset(&key, cx);
                }
            }
        }
    }

    /// `assistant.workspaces.<id>.<permission>` and the same by the workspace's name: the
    /// file may use either.
    fn workspace_permission_keys(
        &self,
        workspace: &WorkspaceInfo,
        permission: &str,
    ) -> Vec<String> {
        vec![
            format!("assistant.workspaces.{}.{permission}", workspace.id),
            format!("assistant.workspaces.{}.{permission}", workspace.name),
        ]
    }

    /// What the shown workspace's assistant may do with `permission`: `Some(allowed)` when
    /// the file overrides it, `None` when it follows the setting for projects.
    pub fn assistant_permission(&self, permission: &str, cx: &gpui::App) -> Option<bool> {
        let workspace = self.assistant_workspace(cx)?;
        self.workspace_permission_keys(&workspace, permission)
            .iter()
            .filter_map(|k| self.daemon_settings.entry(k))
            .find(|e| e.set_in_file)
            .and_then(|e| e.value.as_bool())
    }

    /// The options of the model menu of the place whose tag is `scope`: Automatic (the empty
    /// value) and the backend's models, with the model that is stored as current.
    pub(super) fn assistant_model_options(
        &self,
        scope: &str,
        cx: &gpui::App,
    ) -> Option<super::settings_keys::DropdownOptions> {
        let scope = self
            .assistant_scopes(cx)
            .into_iter()
            .find(|s| tag(*s) == scope)?;
        let config = self.assistant_defaults.chosen.get(&scope)?.clone()?;
        let mut options = vec![(String::new(), words::AUTOMATIC.to_owned())];
        if let Models::Ready(models) =
            self.assistant_models(config.backend, config.provider.as_deref(), cx)
        {
            options.extend(
                models
                    .iter()
                    .filter(|m| !m.hidden)
                    .map(|m| (m.id.clone(), m.display_name.clone())),
            );
        }
        Some((options, Some(config.model.unwrap_or_default())))
    }

    /// Picks `model` (empty: Automatic) in the model menu of the place whose tag is `scope`.
    pub(super) fn assistant_choose_model_in(
        &mut self,
        scope: &str,
        model: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(scope) = self
            .assistant_scopes(cx)
            .into_iter()
            .find(|s| tag(*s) == scope)
        {
            self.assistant_choose_model(scope, (!model.is_empty()).then(|| model.to_owned()), cx);
        }
    }

    // ---- what Inherit follows ------------------------------------------------------------

    /// Where `scope` gets its backend when it inherits, as far as this window knows: the first
    /// place above it with a choice, else `daemon.toml`, else Automatic. The backend it gives
    /// is `Some` when one is known.
    fn assistant_inherited(
        &self,
        scope: ScopeKey,
        cx: &gpui::App,
    ) -> (String, Option<String>, Option<AssistantBackend>) {
        let store = self.sessions.read(cx);
        let mut above: Vec<(String, ScopeKey)> = Vec::new();
        if let ScopeKey::Workspace(id) = scope {
            if let Some(parent) = store
                .workspace(id)
                .and_then(|w| w.parent_id)
                .and_then(|p| store.workspace(p))
            {
                above.push((
                    format!("the workspace “{}”", parent.name),
                    ScopeKey::Workspace(parent.id),
                ));
            }
            above.push((words::SCOPE_GLOBAL.to_owned(), ScopeKey::Global));
        }
        for (from, place) in above {
            if let Some(Some(config)) = self.assistant_defaults.chosen.get(&place) {
                return (
                    from,
                    Some(backend_text(config.backend, config.provider.as_deref())),
                    Some(config.backend),
                );
            }
        }
        let backend = self
            .daemon_settings
            .value("assistant.backend")
            .and_then(|v| v.as_str())
            .and_then(|name| BACKENDS.into_iter().find(|b| b.as_str() == name));
        let provider = self
            .daemon_settings
            .entry("assistant.provider")
            .filter(|e| e.set_in_file)
            .and_then(|e| e.value.as_str().map(str::to_owned));
        let in_file = |key: &str| {
            self.daemon_settings
                .entry(key)
                .is_some_and(|e| e.set_in_file)
        };
        match backend {
            Some(backend) if in_file("assistant.backend") => (
                "daemon.toml".to_owned(),
                Some(backend_text(backend, provider.as_deref())),
                Some(backend),
            ),
            _ if provider.is_some() => (
                "daemon.toml".to_owned(),
                Some(backend_text(
                    AssistantBackend::Provider,
                    provider.as_deref(),
                )),
                Some(AssistantBackend::Provider),
            ),
            _ => (
                words::AUTOMATIC.to_owned(),
                Some("the first backend that is ready".to_owned()),
                None,
            ),
        }
    }

    /// The button of `scope` that is selected: the stored choice, else Inherit — unless the
    /// assistant that runs now contradicts what inheriting gives, which means something was
    /// stored before this window started.
    fn assistant_selected(&self, scope: ScopeKey, cx: &gpui::App) -> Option<AssistantConfig> {
        match self.assistant_defaults.chosen.get(&scope) {
            Some(choice) => choice.clone(),
            None => None,
        }
        .or_else(|| {
            let expected = self.assistant_inherited(scope, cx).2;
            let running = self.assistant_defaults.running.get(&scope)?;
            expected
                .filter(|expected| *expected != running.backend)
                .map(|_| AssistantConfig {
                    scope: wire(scope).0,
                    workspace_id: wire(scope).1,
                    backend: running.backend,
                    model: None,
                    provider: running.provider.clone(),
                })
        })
    }

    // ---- rendering -----------------------------------------------------------------------

    /// The daemon's part of the Assistant page, under the app's own fields: backend and model
    /// of each place, then what assistants may read.
    pub(super) fn render_assistant_daemon(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = self.theme;
        let tokens = self.density.ui();
        let mut out = match self.daemon_gate(cx) {
            Err(instead) => return instead,
            Ok(banner) => banner,
        };
        out.push(self.section_heading(
            words::SEC_ASSISTANT_BACKEND,
            words::SEC_ASSISTANT_BACKEND_HELP,
        ));
        for scope in self.assistant_scopes(cx) {
            out.push(self.render_assistant_scope(scope, cx));
        }
        if self.assistant_workspace(cx).is_none() {
            out.push(
                note(words::NO_PROJECT_NOTE, &theme, &tokens)
                    .py(px(tokens.spacing * 2.0))
                    .into_any_element(),
            );
        }
        let all = sections("assistant");
        // What Mawas may do, and what it may do without asking.
        out.extend(self.render_daemon_section(&all[0], cx));
        out.extend(self.render_mawas_grants(cx));
        // What Mawas remembers: on or off, and how much.
        out.extend(self.render_daemon_section(&all[5], cx));
        // When nothing is chosen: the file's own backend, provider and model.
        out.extend(self.render_daemon_section(&all[1], cx));
        out.extend(self.render_daemon_section(&all[2], cx));
        out.extend(self.render_daemon_section(&all[3], cx));
        if let Some(workspace) = self.assistant_workspace(cx) {
            out.push(
                self.section_heading(words::SEC_ASSISTANT_THIS, words::SEC_ASSISTANT_THIS_HELP),
            );
            for permission in PERMISSIONS {
                out.push(self.render_workspace_permission(&workspace, permission, cx));
            }
        }
        out.extend(self.render_daemon_section(&all[4], cx));
        out
    }

    /// The grants Mawas holds, each with Revoke, and Revoke all.
    fn render_mawas_grants(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = self.theme;
        let tokens = self.density.ui();
        let mut out = vec![self.section_heading(
            words::SEC_ASSISTANT_GRANTS,
            words::SEC_ASSISTANT_GRANTS_HELP,
        )];
        if self.mawas_grants.unsupported {
            out.push(
                note(words::GRANTS_UNSUPPORTED, &theme, &tokens)
                    .py(px(tokens.spacing * 2.0))
                    .into_any_element(),
            );
            return out;
        }
        if let Some(error) = self.mawas_grants.error.clone() {
            out.push(
                div()
                    .py(px(tokens.spacing))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.danger)
                    .child(SharedString::from(error))
                    .into_any_element(),
            );
        }
        if self.mawas_grants.grants.is_empty() {
            out.push(
                note(words::GRANTS_NONE, &theme, &tokens)
                    .id("mawas-grants-none")
                    .py(px(tokens.spacing * 2.0))
                    .into_any_element(),
            );
            return out;
        }
        let grants = self.mawas_grants.grants.clone();
        for grant in &grants {
            let id = grant.id;
            let place = match (grant.workspace_id, grant.chat_session_id) {
                (Some(workspace), _) => {
                    let name = self
                        .sessions
                        .read(cx)
                        .workspace(workspace)
                        .map_or_else(|| workspace.to_string(), |w| w.name.clone());
                    format!("Always, in {name}")
                }
                (None, Some(chat)) => {
                    let title = self
                        .sessions
                        .read(cx)
                        .session(chat)
                        .map_or_else(|| chat.to_string(), |s| s.title.clone());
                    format!("For the chat “{title}”")
                }
                (None, None) => String::new(),
            };
            let revoke = small_button(
                SharedString::from(format!("grant-revoke-{id}")),
                words::GRANT_REVOKE,
                false,
                &theme,
                &tokens,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.revoke_mawas_grant(id, cx)));
            out.push(sheet_row(
                SharedString::from(format!("grant-{id}")),
                grant.tool.clone(),
                place,
                Vec::new(),
                revoke.into_any_element(),
                &theme,
                &tokens,
            ));
        }
        out.push(
            div()
                .flex()
                .justify_end()
                .py(px(tokens.spacing * 2.0))
                .child(
                    small_button(
                        "grant-revoke-all",
                        words::GRANT_REVOKE_ALL,
                        false,
                        &theme,
                        &tokens,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.revoke_all_mawas_grants(cx))),
                )
                .into_any_element(),
        );
        out
    }

    /// The row of one place: what it follows or runs on, and the buttons.
    fn render_assistant_scope(&self, scope: ScopeKey, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let id = tag(scope);
        let workspace = match scope {
            ScopeKey::Workspace(id) => self.sessions.read(cx).workspace(id).cloned(),
            ScopeKey::Global => None,
        };
        let title = match (&scope, &workspace) {
            (ScopeKey::Global, _) => words::SCOPE_GLOBAL.to_owned(),
            (_, Some(w)) if w.kind == WorkspaceKind::Workspace => format!("Workspace · {}", w.name),
            (_, Some(w)) => format!("Project · {}", w.name),
            _ => "Workspace".to_owned(),
        };
        let help = match scope {
            ScopeKey::Global => {
                "The assistant of the Home workspace. It sees the Home folder only."
            }
            ScopeKey::Workspace(_) => {
                "The assistant of the workspace you are in. It sees the workspace's folders."
            }
        };
        let selected = self.assistant_selected(scope, cx);
        let inheriting = selected.is_none();
        let mut under: Vec<AnyElement> = Vec::new();
        if inheriting {
            let (from, backend, _) = self.assistant_inherited(scope, cx);
            under.push(
                div()
                    .id(SharedString::from(format!("ainherits-{id}")))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.accent)
                    .child(SharedString::from(words::inherits(
                        &from,
                        backend.as_deref(),
                    )))
                    .into_any_element(),
            );
        }
        let running = self.assistant_defaults.running.get(&scope);
        let automatic = self.sessions.read(cx).assistant_is_automatic(wire(scope).1);
        under.push(
            div()
                .id(SharedString::from(format!("aruns-{id}")))
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.faint)
                .child(SharedString::from(match running {
                    Some(r) => {
                        let backend = backend_text(r.backend, r.provider.as_deref());
                        let model = Some(r.model.as_str()).filter(|m| !m.is_empty());
                        if automatic {
                            words::automatic_runs_on(&backend, model)
                        } else {
                            words::runs_on(&backend, model)
                        }
                    }
                    None => words::NOT_OPENED.to_owned(),
                }))
                .into_any_element(),
        );
        if let Some(error) = self.assistant_defaults.error(scope) {
            under.push(
                div()
                    .id(SharedString::from(format!("aerror-{id}")))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.danger)
                    .child(SharedString::from(error.to_owned()))
                    .into_any_element(),
            );
        }

        let mut buttons: Vec<AnyElement> = vec![
            small_button(
                SharedString::from(format!("ainherit-{id}")),
                words::INHERIT,
                inheriting,
                &theme,
                &tokens,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.assistant_inherit(scope, cx)))
            .into_any_element(),
        ];
        let providers_open = self.assistant_defaults.providers_open == Some(scope);
        for backend in BACKENDS {
            let active = match (&selected, backend) {
                (Some(c), b) => c.backend == b,
                _ => false,
            } || (backend == AssistantBackend::Provider && providers_open);
            buttons.push(
                small_button(
                    SharedString::from(format!("abackend-{id}-{}", backend.as_str())),
                    backend_name(backend),
                    active,
                    &theme,
                    &tokens,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.assistant_choose_backend(scope, backend, cx);
                }))
                .into_any_element(),
            );
        }
        let mut control = div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(tokens.spacing))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(px(tokens.spacing))
                    .children(buttons),
            );
        if providers_open {
            let providers = self.providers().to_vec();
            if providers.is_empty() {
                control = control.child(note(words::PROVIDERS_NONE, &theme, &tokens));
            }
            control = control.child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(px(tokens.spacing))
                    .children(providers.into_iter().map(|provider| {
                        let active = selected.as_ref().is_some_and(|c| {
                            c.backend == AssistantBackend::Provider
                                && c.provider.as_deref() == Some(provider.id.as_str())
                        });
                        let pid = provider.id.clone();
                        small_button(
                            SharedString::from(format!("aprovider-{id}-{}", provider.id)),
                            SharedString::from(provider.name.clone()),
                            active,
                            &theme,
                            &tokens,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.assistant_configure(
                                scope,
                                AssistantBackend::Provider,
                                Some(pid.clone()),
                                None,
                                cx,
                            );
                        }))
                    })),
            );
        }
        if let Some(config) = &selected {
            control = control.child(self.render_assistant_model(scope, config, cx));
        }
        sheet_row(
            SharedString::from(format!("assistant-scope-{id}")),
            title,
            help,
            under,
            control.into_any_element(),
            &theme,
            &tokens,
        )
    }

    /// "Model: Automatic ▾" of the backend stored for a place, and its menu.
    fn render_assistant_model(
        &self,
        scope: ScopeKey,
        config: &AssistantConfig,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let id = tag(scope);
        let menu = format!("assistant-model:{id}");
        let open = self
            .sheet
            .as_ref()
            .is_some_and(|s| s.dropdown.as_deref() == Some(menu.as_str()));
        let models = self.assistant_models(config.backend, config.provider.as_deref(), cx);
        let list: Vec<ModelInfo> = match &models {
            Models::Ready(models) => models.iter().filter(|m| !m.hidden).cloned().collect(),
            _ => Vec::new(),
        };
        let running = self
            .assistant_defaults
            .running
            .get(&scope)
            .filter(|r| r.backend == config.backend && !r.model.is_empty());
        let label = match (&config.model, running) {
            (Some(model), _) => match &models {
                Models::Ready(all) => model_label(all, model),
                _ => model.clone(),
            },
            (None, Some(r)) => words::automatic_runs(&r.model),
            (None, None) => words::AUTOMATIC.to_owned(),
        };
        let toggle = menu.clone();
        let (backend, provider) = (config.backend, config.provider.clone());
        let head = small_button(
            SharedString::from(format!("amodel-{id}")),
            SharedString::from(format!("{}: {label} ▾", words::MODEL_TITLE)),
            open,
            &theme,
            &tokens,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            let opening = this
                .sheet
                .as_ref()
                .is_some_and(|s| s.dropdown.as_deref() != Some(toggle.as_str()));
            if opening
                && !matches!(
                    this.assistant_models(backend, provider.as_deref(), cx),
                    Models::Ready(_)
                )
            {
                this.load_assistant_models(backend, provider.as_deref(), cx);
            }
            if let Some(sheet) = &mut this.sheet {
                sheet.dropdown = opening.then(|| toggle.clone());
                cx.notify();
            }
        }));
        let note_text = match &models {
            Models::Loading => Some(words::MODEL_LOADING.to_owned()),
            Models::Failed(error) => Some(error.clone()),
            Models::Ready(_) if list.is_empty() => Some(words::MODEL_NONE.to_owned()),
            Models::Ready(_) => None,
        };
        div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(tokens.spacing))
            .child(head)
            .when(open, |s| {
                s.child(
                    small_button(
                        SharedString::from(format!("amodel-option-{id}-automatic")),
                        words::AUTOMATIC,
                        config.model.is_none(),
                        &theme,
                        &tokens,
                    )
                    .when(self.dropdown_cursor_at(&menu, ""), |b| {
                        b.bg(theme.surface_hover)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.assistant_choose_model(scope, None, cx);
                    })),
                )
                .children(list.iter().map(|model| {
                    let chosen = model.id.clone();
                    small_button(
                        SharedString::from(format!("amodel-option-{id}-{}", model.id)),
                        SharedString::from(model.display_name.clone()),
                        config.model.as_deref() == Some(model.id.as_str()),
                        &theme,
                        &tokens,
                    )
                    .when(self.dropdown_cursor_at(&menu, &model.id), |b| {
                        b.bg(theme.surface_hover)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.assistant_choose_model(scope, Some(chosen.clone()), cx);
                    }))
                }))
                .when_some(note_text.clone(), |s, text| {
                    s.child(note(text, &theme, &tokens))
                })
            })
            .into_any_element()
    }

    /// One permission of the shown workspace: Inherit, Allow or Deny.
    fn render_workspace_permission(
        &self,
        workspace: &WorkspaceInfo,
        permission: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let override_value = self.assistant_permission(permission, cx);
        let inherited = self
            .daemon_settings
            .value(&format!("assistant.context.{permission}"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let key = self.workspace_permission_keys(workspace, permission)[0].clone();
        let under = vec![
            note(
                match override_value {
                    Some(allowed) => {
                        if allowed {
                            "Allowed for this workspace".to_owned()
                        } else {
                            "Not allowed for this workspace".to_owned()
                        }
                    }
                    None => words::inheriting_permission(inherited),
                },
                &theme,
                &tokens,
            )
            .id(SharedString::from(format!("aperm-note-{permission}")))
            .into_any_element(),
        ];
        let option = |name: &'static str,
                      label: &'static str,
                      value: Option<bool>,
                      active: bool,
                      cx: &mut Context<Self>| {
            small_button(
                SharedString::from(format!("aperm-{permission}-{name}")),
                label,
                active,
                &theme,
                &tokens,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.assistant_permission_set(permission, value, cx);
            }))
        };
        let control = div()
            .flex()
            .gap(px(tokens.spacing))
            .child(option(
                "inherit",
                words::INHERIT,
                None,
                override_value.is_none(),
                cx,
            ))
            .child(option(
                "allow",
                words::ALLOW,
                Some(true),
                override_value == Some(true),
                cx,
            ))
            .child(option(
                "deny",
                words::DENY,
                Some(false),
                override_value == Some(false),
                cx,
            ))
            .into_any_element();
        sheet_row(
            SharedString::from(format!("aperm-{permission}")),
            daemon_field_title(&key),
            daemon_field_help(&key),
            under,
            control,
            &theme,
            &tokens,
        )
    }
}
