//! Hutan settings: layered TOML store with per-namespace schemas, hot reload, keymap parsing,
//! appearance, color packs (`themes`) and their registry. No GPUI (ADR 0010).
//!
//! Layers, lowest to highest: built-in defaults (each [`Setting`]'s `Default`), user
//! `settings.toml`, workspace `<root>/.hutan/settings.toml` and project
//! `<root>/.hutan/settings.toml` (both only for namespaces with
//! [`Setting::WORKSPACE_OVERRIDABLE`]; the project layer wins), runtime overrides. Tables merge
//! deeply, arrays and scalars replace. Each namespace is then deserialized into its owner's
//! type; an invalid result keeps the last good value and records a [`SettingsError`].

use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub mod appearance;
mod edit;
mod error;
pub mod keymap;
mod store;
pub mod themes;
mod value;
mod watch;

pub use appearance::{
    AppearanceMode, AppearanceSettings, Density, ReduceMotionSetting, ThemeOverrides,
};
pub use error::{SettingsError, Severity};
pub use keymap::{BindingSpec, Keystroke};
pub use store::{
    ChangeSet, KEYMAP_NAMESPACE, LayerInfo, SettingsLayer, SettingsPaths, SettingsStore,
};
pub use themes::{Rgb, Rgba, SystemAppearance, TerminalTheme, ThemeRegistry};
pub use watch::WatchGuard;

/// A typed settings namespace owned by one component.
///
/// Implementors should be `#[serde(default)]` structs so partial tables deserialize, and must
/// serialize to a TOML table.
pub trait Setting:
    Serialize + DeserializeOwned + JsonSchema + Default + Clone + Send + Sync + 'static
{
    /// Dotted path of the namespace in the settings files, e.g. `terminal`, `panels.sessions`.
    const NAMESPACE: &'static str;
    /// Whether `<repo>/.hutan/settings.toml` may override this namespace.
    const WORKSPACE_OVERRIDABLE: bool = false;
    /// Rewrites one layer's table of this namespace before the layers are merged and read, so a
    /// key an older Hutan wrote still counts (`chat.send_on_enter` is `chat.send_key`). Runs on
    /// the user, workspace, project and runtime layers alone, never on the defaults: a key the
    /// layer sets itself must win over the one it replaces, and a layer that sets neither must
    /// leave the default alone.
    fn upgrade(_layer: &mut toml::Table) {}
}
