//! Settings types used by the integration tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use hutan_settings::Setting;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Cursor {
    pub style: String,
    pub blink: bool,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            style: "block".into(),
            blink: true,
        }
    }
}

/// Not workspace-overridable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Terminal {
    pub font_family: String,
    pub font_size: u16,
    pub cursor: Cursor,
    pub tags: Vec<String>,
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            font_family: "Menlo".into(),
            font_size: 13,
            cursor: Cursor::default(),
            tags: vec!["a".into()],
        }
    }
}

impl Setting for Terminal {
    const NAMESPACE: &'static str = "terminal";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct Dock {
    pub size: u32,
    pub panels: Vec<String>,
}

/// Workspace-overridable, with a map to exercise deep merging.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Layout {
    pub preset: String,
    pub presets: BTreeMap<String, Dock>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            preset: "default".into(),
            presets: BTreeMap::new(),
        }
    }
}

impl Setting for Layout {
    const NAMESPACE: &'static str = "layout";
    const WORKSPACE_OVERRIDABLE: bool = true;
}

/// Nested dotted namespace, workspace-overridable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Sessions {
    pub group_by: String,
    pub needs_you: bool,
}

impl Default for Sessions {
    fn default() -> Self {
        Self {
            group_by: "workspace".into(),
            needs_you: true,
        }
    }
}

impl Setting for Sessions {
    const NAMESPACE: &'static str = "panels.sessions";
    const WORKSPACE_OVERRIDABLE: bool = true;
}

/// Same namespace as [`Terminal`], different type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct Impostor {
    pub x: u8,
}

impl Setting for Impostor {
    const NAMESPACE: &'static str = "terminal";
}

/// Atomic replace, the way most editors save.
pub fn write_file(path: &std::path::Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

/// A namespace that renamed `old_size` (a number) to `size` (a string).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Renamed {
    pub size: String,
}

impl Default for Renamed {
    fn default() -> Self {
        Self {
            size: "medium".into(),
        }
    }
}

impl Setting for Renamed {
    const NAMESPACE: &'static str = "renamed";

    fn upgrade(layer: &mut toml::Table) {
        if let Some(old) = layer.remove("old_size") {
            let size = if old.as_integer().is_some_and(|n| n > 10) {
                "large"
            } else {
                "small"
            };
            layer
                .entry("size")
                .or_insert_with(|| toml::Value::String(size.to_owned()));
        }
    }
}
