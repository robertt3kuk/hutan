//! Which install of Hutan is running (FX-REINSTALL-SETUP, `docs/design.md` § First-run setup →
//! Reinstall and update).
//!
//! `app.setup_done` lives in the data folder, which `brew uninstall --cask` keeps, so a reinstall
//! opens straight into the old state. The desktop therefore remembers *which bundle* it last ran
//! from — [`InstallRecord`] in `desktop-state.json` — and compares it with the bundle it runs
//! from now ([`Install`]):
//!
//! - the **install id** is the bundle's creation time and the inode of `Contents/Info.plist`: it
//!   changes on every reinstall (also of the same version) and stays across launches;
//! - a **higher version** is an update ([`Change::Updated`]): a *What's new* notice, no offer;
//! - the same or a lower version under another install id, after the setup was done, is a
//!   reinstall ([`Change::Reinstalled`]): the setup is offered again, once.
//!
//! A development build outside an app bundle has no [`Install`] and never offers anything.

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::macos::login_item::inside_app_bundle;

/// The bundle file whose creation time and inode make the install id.
const IDENTITY_FILE: &str = "Contents/Info.plist";

/// Where the bundled changelog is, relative to the bundle (`xtask bundle` copies it).
pub const CHANGELOG_FILE: &str = "Contents/Resources/CHANGELOG.md";

/// The install this desktop runs from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    /// `CARGO_PKG_VERSION` of the running desktop.
    pub version: String,
    /// The `.app` folder.
    pub bundle: PathBuf,
    /// Creation time and inode of the bundle's `Info.plist` ([`install_id`]).
    pub id: String,
}

/// What `desktop-state.json` keeps (`install`): the last install the desktop acknowledged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRecord {
    pub version: String,
    pub bundle_path: String,
    /// The install id the user has settled with: written when the desktop saw it for the first
    /// time and nothing was to be offered, or when the reinstall card was answered.
    pub install_id: String,
}

/// What the running install is, against the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Nothing to say: the same install, a first record, or a bundle that never ran setup.
    None,
    /// The same or a lower version in a new bundle, after the setup was done: offer it again.
    Reinstalled,
    /// A higher version than the record's: say what is new.
    Updated { from: String },
}

impl Install {
    /// The install of the running executable: `None` outside an app bundle (a development build)
    /// and when the bundle's `Info.plist` cannot be read.
    pub fn current() -> Option<Self> {
        Self::of_executable(&std::env::current_exe().ok()?, env!("CARGO_PKG_VERSION"))
    }

    /// The install `exe` belongs to (`<Name>.app/Contents/MacOS/<exe>`), running `version`.
    pub fn of_executable(exe: &Path, version: &str) -> Option<Self> {
        if !inside_app_bundle(exe) {
            return None;
        }
        let bundle = exe.parent()?.parent()?.parent()?.to_path_buf();
        let id = install_id(&bundle)?;
        Some(Self {
            version: version.to_owned(),
            bundle,
            id,
        })
    }

    /// The record of this install.
    pub fn record(&self) -> InstallRecord {
        InstallRecord {
            version: self.version.clone(),
            bundle_path: self.bundle.display().to_string(),
            install_id: self.id.clone(),
        }
    }

    /// Where this bundle keeps its changelog.
    pub fn changelog_path(&self) -> PathBuf {
        self.bundle.join(CHANGELOG_FILE)
    }

    /// What changed since `previous`, given whether the setup was done before.
    pub fn change(&self, previous: Option<&InstallRecord>, setup_done: bool) -> Change {
        let Some(previous) = previous else {
            return Change::None;
        };
        match (
            parse_version(&self.version),
            parse_version(&previous.version),
        ) {
            (Some(now), Some(before)) if now > before => {
                return Change::Updated {
                    from: previous.version.clone(),
                };
            }
            _ => {}
        }
        if previous.install_id != self.id && setup_done {
            Change::Reinstalled
        } else {
            Change::None
        }
    }
}

/// The id of the install in `bundle`: the creation time of `Contents/Info.plist` (seconds and
/// nanoseconds since the epoch) and its inode. `None` when the file cannot be read.
pub fn install_id(bundle: &Path) -> Option<String> {
    let meta = std::fs::metadata(bundle.join(IDENTITY_FILE)).ok()?;
    let created = meta
        .created()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or_else(
            || "0.0".to_owned(),
            |d| format!("{}.{:09}", d.as_secs(), d.subsec_nanos()),
        );
    Some(format!("{created}:{}", meta.ino()))
}

/// `1.2.3` or `1.2.3-rc.1` as `(1, 2, 3)`; `None` for anything else. A pre-release counts as its
/// release (an update is told by the numbers).
fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

/// The 1-based line of the `## ` heading of `changelog` that names `version` (`## 0.2.0 —
/// 2026-10-23`), else of `## Unreleased` (a build of a version that is not released yet). `None`
/// when neither is there.
pub fn whats_new_line(changelog: &str, version: &str) -> Option<u32> {
    let heading_line = |name: &str| {
        changelog
            .lines()
            .position(|line| {
                line.strip_prefix("## ")
                    .and_then(|heading| heading.split_whitespace().next())
                    .is_some_and(|word| word.trim_start_matches('v') == name)
            })
            .and_then(|index| u32::try_from(index + 1).ok())
    };
    heading_line(version).or_else(|| heading_line("Unreleased"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(dir: &Path) -> PathBuf {
        let app = dir.join("Hutan.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join(IDENTITY_FILE), "plist").unwrap();
        std::fs::write(app.join("Contents/MacOS/hutan-desktop"), "").unwrap();
        app
    }

    fn install(version: &str, id: &str) -> Install {
        Install {
            version: version.to_owned(),
            bundle: PathBuf::from("/Applications/Hutan.app"),
            id: id.to_owned(),
        }
    }

    fn record(version: &str, id: &str) -> InstallRecord {
        install(version, id).record()
    }

    #[test]
    fn the_identity_of_a_bundle_is_stable_and_changes_when_it_is_installed_again() {
        let dir = tempfile::tempdir().unwrap();
        let app = bundle(dir.path());
        let exe = app.join("Contents/MacOS/hutan-desktop");
        let first = Install::of_executable(&exe, "0.2.0").unwrap();
        assert_eq!(first.bundle, app);
        assert_eq!(first.version, "0.2.0");
        assert_eq!(Install::of_executable(&exe, "0.2.0").unwrap(), first);

        // A reinstall copies the bundle again: a new Info.plist file, a new inode.
        std::fs::remove_file(app.join(IDENTITY_FILE)).unwrap();
        std::fs::write(app.join(IDENTITY_FILE), "plist").unwrap();
        let second = Install::of_executable(&exe, "0.2.0").unwrap();
        assert_ne!(second.id, first.id);
        assert_eq!(second.bundle, first.bundle);
    }

    #[test]
    fn a_development_build_has_no_install() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("target/debug/hutan-desktop");
        assert_eq!(Install::of_executable(&exe, "0.2.0"), None);
        // A bundle without an Info.plist is not one either.
        let app = dir.path().join("Hutan.app/Contents/MacOS");
        std::fs::create_dir_all(&app).unwrap();
        assert_eq!(
            Install::of_executable(&app.join("hutan-desktop"), "0.2.0"),
            None
        );
    }

    #[test]
    fn what_changed_is_told_from_the_version_and_the_install_id() {
        let now = install("0.2.0", "b");
        // Nothing recorded: the first launch of a bundle; nothing to offer.
        assert_eq!(now.change(None, true), Change::None);
        assert_eq!(now.change(None, false), Change::None);
        // The same install.
        assert_eq!(now.change(Some(&record("0.2.0", "b")), true), Change::None);
        // Another install of the same or a lower version, after the setup was done.
        assert_eq!(
            now.change(Some(&record("0.2.0", "a")), true),
            Change::Reinstalled
        );
        assert_eq!(
            now.change(Some(&record("0.3.0", "a")), true),
            Change::Reinstalled
        );
        // The first screen still shows for a setup that never ran.
        assert_eq!(now.change(Some(&record("0.2.0", "a")), false), Change::None);
        // A higher version is an update, whatever the install id, and no offer.
        assert_eq!(
            now.change(Some(&record("0.1.9", "a")), true),
            Change::Updated {
                from: "0.1.9".to_owned()
            }
        );
        assert_eq!(
            install("0.10.0", "a").change(Some(&record("0.9.0", "a")), true),
            Change::Updated {
                from: "0.9.0".to_owned()
            }
        );
    }

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
        assert_eq!(parse_version("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("dev"), None);
    }

    #[test]
    fn whats_new_opens_the_versions_heading_else_unreleased() {
        let log = "# Changelog\n\n## Unreleased\n\n- next\n\n## 0.2.0 — 2026-10-23\n\n- one\n\n## 0.1.0 — 2026-09-01\n\n- old\n";
        assert_eq!(whats_new_line(log, "0.2.0"), Some(7));
        assert_eq!(whats_new_line(log, "0.1.0"), Some(11));
        assert_eq!(whats_new_line(log, "0.3.0"), Some(3), "not released yet");
        assert_eq!(whats_new_line("# Changelog\n", "0.3.0"), None);
    }
}
