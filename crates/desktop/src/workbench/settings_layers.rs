//! Layers in the settings sheet: where each field's value comes from (default, you, workspace,
//! project), which file a control edits, and the Reset that removes a key from that file.
//!
//! The store layers `settings.toml` of the user, the workspace's and the project's
//! `.hutan/settings.toml` (the last two only for namespaces that may be overridden). A control
//! edits the layer its value comes from when that is the workspace or the project: otherwise
//! the edit would write a value that the layer above hides. The header's switch picks a layer
//! for the whole page instead.

use std::path::PathBuf;

use gpui::{App, Context, Window};
use hutan_protocol::messages::WorkspaceKind;
use hutan_settings::{LayerInfo, SettingsLayer};

use super::settings_labels::{field_title, words};
use super::settings_schema::{Field, FieldKind};
use super::view::Workbench;

/// Where the effective value of a field comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSource {
    /// `None`: the built-in default.
    pub layer: Option<SettingsLayer>,
    /// "Default", "You", "Workspace <name>" or "Project <name>".
    pub label: String,
    /// The file of the layer (what the label shows on hover).
    pub path: Option<PathBuf>,
}

/// Fields that mirror the state of something else (the daemon's lifetime, the login item):
/// "Reset page" leaves them alone.
fn mirrors_system(f: &Field) -> bool {
    f.namespace == "app" && matches!(f.dotted().as_str(), "keep_running" | "start_at_login")
}

impl Workbench {
    /// The layers a value can be written to now, lowest first: the user's file, and the
    /// workspace's and the project's while the window shows a container that has them.
    pub fn sheet_layers(&self) -> Vec<LayerInfo> {
        self.settings.layers()
    }

    /// "You", "Workspace <name>" or "Project <name>": the layer as the sheet names it.
    pub fn layer_label(&self, layer: SettingsLayer, cx: &App) -> String {
        let (container, project) = self.layer_containers(cx);
        match layer {
            SettingsLayer::User => words::SOURCE_YOU.to_owned(),
            SettingsLayer::Workspace => {
                words::layer_named(words::LAYER_WORKSPACE, container.as_ref().map(|c| &c.name))
            }
            SettingsLayer::Project => {
                let named = match &container {
                    Some(c) if c.kind == WorkspaceKind::Project => Some(&c.name),
                    _ => project.as_ref().map(|p| &p.name),
                };
                words::layer_named(words::LAYER_PROJECT, named)
            }
        }
    }

    /// The layers the header's switch offers for the selected page: empty unless a namespace of
    /// the page may be overridden and a workspace or project layer exists.
    pub fn sheet_layer_choices(&self, cx: &App) -> Vec<(SettingsLayer, String)> {
        let Some(sheet) = &self.sheet else {
            return Vec::new();
        };
        let overridable = sheet
            .fields()
            .iter()
            .filter_map(|f| sheet.entry_of(f.namespace))
            .any(|e| e.workspace_overridable);
        let layers = self.settings.layers();
        if !overridable || layers.len() < 2 {
            return Vec::new();
        }
        layers
            .into_iter()
            .map(|l| (l.layer, self.layer_label(l.layer, cx)))
            .collect()
    }

    /// Picks the layer every field of the page edits (`None`: each edits the layer its value
    /// comes from).
    pub fn sheet_set_layer(&mut self, layer: Option<SettingsLayer>, cx: &mut Context<Self>) {
        if let Some(sheet) = &mut self.sheet {
            sheet.layer = layer;
            sheet.edit = None;
            sheet.dropdown = None;
            cx.notify();
        }
    }

    /// The layer a write to `f` goes to: the one the user picked, else the one the value comes
    /// from when that is the workspace or the project, else the user's file. A namespace that
    /// may not be overridden is the user's alone.
    pub(super) fn edit_layer_of(&self, f: &Field) -> SettingsLayer {
        let Some(sheet) = &self.sheet else {
            return SettingsLayer::User;
        };
        if !sheet
            .entry_of(f.namespace)
            .is_some_and(|e| e.workspace_overridable)
        {
            return SettingsLayer::User;
        }
        let layers = self.settings.layers();
        let exists = |layer: SettingsLayer| layers.iter().any(|l| l.layer == layer);
        if let Some(chosen) = sheet.layer
            && exists(chosen)
        {
            return chosen;
        }
        match self.settings.source_of(&f.key_path()) {
            Some(layer) if exists(layer) => layer,
            _ => SettingsLayer::User,
        }
    }

    /// The layer a write to `field` of the selected page goes to.
    pub fn sheet_edit_layer(&self, field: &str) -> Option<SettingsLayer> {
        let f = self.sheet.as_ref()?.field(field)?;
        Some(self.edit_layer_of(f))
    }

    /// Where the effective value of `field` comes from.
    pub fn sheet_source(&self, field: &str, cx: &App) -> Option<FieldSource> {
        let f = self.sheet.as_ref()?.field(field)?;
        Some(self.source_of_field(f, cx))
    }

    pub(super) fn source_of_field(&self, f: &Field, cx: &App) -> FieldSource {
        let layer = self.settings.source_of(&f.key_path());
        match layer {
            None => FieldSource {
                layer: None,
                label: words::SOURCE_DEFAULT.to_owned(),
                path: None,
            },
            Some(layer) => FieldSource {
                layer: Some(layer),
                label: self.layer_label(layer, cx),
                path: self
                    .settings
                    .layers()
                    .into_iter()
                    .find(|l| l.layer == layer)
                    .and_then(|l| l.path),
            },
        }
    }

    /// Whether the file Reset of `field` would edit sets the key.
    pub fn sheet_has_key(&self, field: &str) -> bool {
        self.sheet
            .as_ref()
            .and_then(|s| s.field(field))
            .is_some_and(|f| self.field_has_key(f))
    }

    pub(super) fn field_has_key(&self, f: &Field) -> bool {
        let layer = self.edit_layer_of(f);
        self.settings.layer_value(layer, &f.key_path()).is_some()
    }

    /// The fields of the page that "Reset page" removes: the ones whose key is set in the file
    /// they edit, each with the layer. Saved layout presets stay (they have their own
    /// Delete), and so do the fields that mirror the daemon's lifetime and the login item.
    pub fn sheet_page_keys(&self) -> Vec<(Field, SettingsLayer)> {
        let Some(sheet) = &self.sheet else {
            return Vec::new();
        };
        sheet
            .fields()
            .iter()
            .filter(|f| !matches!(f.kind, FieldKind::Unsupported(_)) && !mirrors_system(f))
            .filter(|f| self.field_has_key(f))
            .map(|f| (f.clone(), self.edit_layer_of(f)))
            .collect()
    }

    /// "Reset page…": asks first, listing what goes, then removes every key of the page from the
    /// file it is edited in.
    pub fn sheet_reset_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keys = self.sheet_page_keys();
        if keys.is_empty() {
            self.set_notice(words::RESET_PAGE_NOTHING, cx);
            return;
        }
        let listing: Vec<String> = keys
            .iter()
            .map(|(f, layer)| {
                format!(
                    "{} ({})",
                    field_title(f.namespace, &f.dotted()),
                    self.layer_label(*layer, cx)
                )
            })
            .collect();
        self.open_confirm(
            words::RESET_PAGE_TITLE,
            words::reset_page_message(&listing),
            words::RESET_PAGE_CONFIRM,
            |this, _window, cx| this.sheet_reset_page_now(cx),
            window,
            cx,
        );
    }

    /// Removes every key "Reset page" lists.
    pub fn sheet_reset_page_now(&mut self, cx: &mut Context<Self>) {
        let mut failure = None;
        for (f, layer) in self.sheet_page_keys() {
            if let Err(err) = self.settings.remove(layer, f.namespace, &f.dotted()) {
                failure = Some(err.to_string());
                break;
            }
        }
        if let Some(sheet) = &mut self.sheet {
            sheet.message = failure;
            sheet.edit = None;
            sheet.dropdown = None;
        }
        cx.notify();
    }
}
