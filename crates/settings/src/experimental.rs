//! The `experimental` namespace: features that ship switched off, at the user's own risk.
//!
//! Each is a plain boolean of the user's `settings.toml` (never a workspace's or project's: a
//! repository must not switch on what changes who may control this Mac). The CLI reads it with
//! [`ExperimentalSettings::load`]; the desktop registers it like any other namespace and shows
//! it on the Experimental page of Settings.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Setting, SettingsError, SettingsPaths, SettingsStore};

/// Experimental features. All off by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct ExperimentalSettings {
    /// Reach the daemon of another Mac over SSH (`hutan --host`, `hutan host …`). Experimental:
    /// it may change or break between releases, and whoever can SSH into that host's account
    /// has the same control as the user of this Mac. `docs/remote.md`.
    pub remote_hosts: bool,
}

/// What every place that offers remote hosts says next to it (Settings, `hutan host add|test`,
/// the refusal while the switch is off).
pub const REMOTE_HOSTS_DISCLAIMER: &str = "Remote hosts are experimental: they may change or \
break between releases, and anyone who can SSH into that host's account has the same control \
over its Hutan as the user of this Mac. Use at your own risk.";

/// Where the docs of remote hosts are, for a link.
pub const REMOTE_HOSTS_DOCS_URL: &str =
    "https://github.com/robertt3kuk/hutan/blob/master/docs/remote.md";

impl Setting for ExperimentalSettings {
    const NAMESPACE: &'static str = "experimental";
}

impl ExperimentalSettings {
    /// The settings of the user's `settings.toml` at `paths`. A file that cannot be read or has
    /// an invalid `experimental` table leaves everything off, so a broken file never switches
    /// anything on.
    pub fn load(paths: &SettingsPaths) -> Result<Self, SettingsError> {
        let store = SettingsStore::new(paths.clone());
        store.register::<Self>();
        if let Some(error) = store
            .errors()
            .into_iter()
            .find(|e| e.key_path == Self::NAMESPACE || e.key_path.starts_with("experimental."))
            .filter(|e| e.severity == crate::Severity::Error)
        {
            return Err(error);
        }
        Ok(*store.get::<Self>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(text: Option<&str>) -> (tempfile::TempDir, SettingsPaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = SettingsPaths::in_dir(dir.path());
        if let Some(text) = text {
            std::fs::write(&paths.user, text).unwrap();
        }
        (dir, paths)
    }

    #[test]
    fn everything_is_off_until_the_user_turns_it_on() {
        let (_dir, none) = paths(None);
        assert!(!ExperimentalSettings::load(&none).unwrap().remote_hosts);
        let (_dir, on) = paths(Some("[experimental]\nremote_hosts = true\n"));
        assert!(ExperimentalSettings::load(&on).unwrap().remote_hosts);
        let (_dir, wrong) = paths(Some("[experimental]\nremote_hosts = \"yes\"\n"));
        assert!(ExperimentalSettings::load(&wrong).is_err());
    }
}
