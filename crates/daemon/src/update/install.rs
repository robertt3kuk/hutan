//! How this Hutan was installed (ADR 0024 decision 3), read from the running executable and
//! Homebrew's Caskroom and Cellar. Read-only: `brew` is never run to find out, and nothing is
//! written.
//!
//! - **Cask**: the executable is `Contents/MacOS/<exe>` of an `.app` that a `Caskroom/hutan/<version>/`
//!   entry points at (Homebrew moves the app out and leaves a link behind), or one that lies
//!   inside the Caskroom itself.
//! - **Formula**: the executable is in `Cellar/hutan/<version>/`.
//! - **Bundle**: an `.app` that no Caskroom entry knows, in a folder the user can write (the
//!   update replaces the bundle, which needs its folder).
//! - **Unknown**: anything else: a development build, an `.app` in a folder that is not
//!   writable.

use std::path::{Path, PathBuf};

use hutan_protocol::update::InstallMethod;
use rustix::fs::{Access, access};

/// The cask and formula are both named `hutan` in the tap `robertt3kuk/hutan`.
const TOKEN: &str = "hutan";

/// Where to look.
#[derive(Debug, Clone)]
pub struct Layout {
    /// The running executable (resolved here; pass the path as it is).
    pub exe: PathBuf,
    /// Homebrew prefixes: where `Caskroom` and `Cellar` are.
    pub prefixes: Vec<PathBuf>,
}

/// What the layout says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub method: InstallMethod,
    /// The app bundle (cask, bundle) or the executable (formula).
    pub path: Option<PathBuf>,
    /// The Homebrew prefix of a cask or formula install: its `bin` holds `brew`.
    pub prefix: Option<PathBuf>,
}

impl Detected {
    fn unknown() -> Self {
        Self {
            method: InstallMethod::Unknown,
            path: None,
            prefix: None,
        }
    }
}

/// The prefixes to look in: Homebrew's two defaults, and `HOMEBREW_PREFIX` when the environment
/// says so.
pub fn default_prefixes(env_prefix: Option<&str>) -> Vec<PathBuf> {
    let mut prefixes = vec![PathBuf::from("/opt/homebrew"), PathBuf::from("/usr/local")];
    if let Some(prefix) = env_prefix.filter(|p| Path::new(p).is_absolute()) {
        let prefix = PathBuf::from(prefix);
        if !prefixes.contains(&prefix) {
            prefixes.insert(0, prefix);
        }
    }
    prefixes
}

pub fn detect(layout: &Layout) -> Detected {
    let exe = std::fs::canonicalize(&layout.exe).unwrap_or_else(|_| layout.exe.clone());
    if let Some(bundle) = bundle_of(&exe) {
        for prefix in &layout.prefixes {
            if cask_owns(prefix, &bundle) {
                return Detected {
                    method: InstallMethod::Cask,
                    path: Some(bundle),
                    prefix: Some(prefix.clone()),
                };
            }
        }
        let writable = bundle.parent().is_some_and(writable);
        return if writable {
            Detected {
                method: InstallMethod::Bundle,
                path: Some(bundle),
                prefix: None,
            }
        } else {
            Detected {
                path: Some(bundle),
                ..Detected::unknown()
            }
        };
    }
    for prefix in &layout.prefixes {
        let cellar = prefix.join("Cellar").join(TOKEN);
        let cellar = std::fs::canonicalize(&cellar).unwrap_or(cellar);
        if exe.starts_with(&cellar) {
            return Detected {
                method: InstallMethod::Formula,
                path: Some(exe),
                prefix: Some(prefix.clone()),
            };
        }
    }
    Detected::unknown()
}

/// The `.app` that `exe` is `Contents/MacOS/<exe>` of.
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then(|| bundle.to_owned())
}

/// Whether `<prefix>/Caskroom/hutan` holds this bundle, or a link to it.
fn cask_owns(prefix: &Path, bundle: &Path) -> bool {
    let room = prefix.join("Caskroom").join(TOKEN);
    let Ok(versions) = std::fs::read_dir(&room) else {
        return false;
    };
    if std::fs::canonicalize(&room).is_ok_and(|room| bundle.starts_with(room)) {
        return true;
    }
    versions
        .flatten()
        .filter_map(|version| std::fs::read_dir(version.path()).ok())
        .flat_map(|entries| entries.flatten())
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "app"))
        .any(|entry| std::fs::canonicalize(entry.path()).is_ok_and(|real| real == bundle))
}

pub(super) fn writable(dir: &Path) -> bool {
    access(dir, Access::WRITE_OK).is_ok()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    fn bundle_in(dir: &Path, name: &str) -> PathBuf {
        let bundle = dir.join(format!("{name}.app"));
        fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        fs::write(bundle.join("Contents/MacOS/hutand"), b"").unwrap();
        bundle
    }

    fn layout(dir: &tempfile::TempDir, exe: PathBuf) -> Layout {
        Layout {
            exe,
            prefixes: vec![dir.path().join("prefix")],
        }
    }

    #[test]
    fn a_bundle_the_caskroom_links_to_is_a_cask() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("Applications");
        let bundle = bundle_in(&apps, "Hutan");
        let staged = dir.path().join("prefix/Caskroom/hutan/0.3.1");
        fs::create_dir_all(&staged).unwrap();
        symlink(&bundle, staged.join("Hutan.app")).unwrap();
        let exe = bundle.join("Contents/MacOS/hutand");
        let found = detect(&layout(&dir, exe));
        assert_eq!(found.method, InstallMethod::Cask);
        assert_eq!(found.path, Some(fs::canonicalize(&bundle).unwrap()));
        assert_eq!(found.prefix, Some(dir.path().join("prefix")));
    }

    #[test]
    fn a_bundle_inside_the_caskroom_is_a_cask() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("prefix/Caskroom/hutan/0.3.1");
        let bundle = bundle_in(&staged, "Hutan");
        let exe = bundle.join("Contents/MacOS/hutand");
        assert_eq!(detect(&layout(&dir, exe)).method, InstallMethod::Cask);
    }

    #[test]
    fn the_cli_inside_a_cask_bundle_through_the_brew_bin_link_is_a_cask() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("Applications");
        let bundle = bundle_in(&apps, "Hutan");
        let staged = dir.path().join("prefix/Caskroom/hutan/0.3.1");
        fs::create_dir_all(&staged).unwrap();
        symlink(&bundle, staged.join("Hutan.app")).unwrap();
        fs::create_dir_all(dir.path().join("prefix/bin")).unwrap();
        let link = dir.path().join("prefix/bin/hutand");
        symlink(bundle.join("Contents/MacOS/hutand"), &link).unwrap();
        assert_eq!(detect(&layout(&dir, link)).method, InstallMethod::Cask);
    }

    #[test]
    fn a_copied_bundle_is_a_bundle_and_an_unwritable_folder_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let home_apps = dir.path().join("Apps");
        let bundle = bundle_in(&home_apps, "Hutan");
        let exe = bundle.join("Contents/MacOS/hutand");
        // A Caskroom that holds another version's link to another bundle is not this one.
        let other = bundle_in(&dir.path().join("Elsewhere"), "Hutan");
        let staged = dir.path().join("prefix/Caskroom/hutan/0.3.0");
        fs::create_dir_all(&staged).unwrap();
        symlink(&other, staged.join("Hutan.app")).unwrap();
        let found = detect(&layout(&dir, exe.clone()));
        assert_eq!(found.method, InstallMethod::Bundle);
        assert_eq!(found.prefix, None);

        fs::set_permissions(&home_apps, fs::Permissions::from_mode(0o555)).unwrap();
        let found = detect(&layout(&dir, exe));
        let still_writable = writable(&home_apps);
        fs::set_permissions(&home_apps, fs::Permissions::from_mode(0o755)).unwrap();
        // Root can write anywhere; a gate run as root cannot tell the two apart.
        if still_writable {
            return;
        }
        assert_eq!(found.method, InstallMethod::Unknown);
        assert_eq!(found.path, Some(fs::canonicalize(&bundle).unwrap()));
    }

    #[test]
    fn an_executable_in_the_cellar_is_a_formula() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("prefix/Cellar/hutan/0.3.1/bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("hutand"), b"").unwrap();
        fs::create_dir_all(dir.path().join("prefix/bin")).unwrap();
        let link = dir.path().join("prefix/bin/hutand");
        symlink(bin.join("hutand"), &link).unwrap();
        let found = detect(&layout(&dir, link));
        assert_eq!(found.method, InstallMethod::Formula);
        assert_eq!(found.prefix, Some(dir.path().join("prefix")));
    }

    #[test]
    fn a_development_build_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("target/debug/hutand");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"").unwrap();
        let found = detect(&layout(&dir, exe));
        assert_eq!(found, Detected::unknown());
        // A missing executable is no reason to fail either.
        assert_eq!(
            detect(&layout(&dir, dir.path().join("nowhere"))).method,
            InstallMethod::Unknown
        );
    }

    #[test]
    fn homebrew_prefix_from_the_environment_is_tried_first() {
        assert_eq!(
            default_prefixes(Some("/custom/brew")),
            vec![
                PathBuf::from("/custom/brew"),
                PathBuf::from("/opt/homebrew"),
                PathBuf::from("/usr/local")
            ]
        );
        assert_eq!(default_prefixes(Some("relative")).len(), 2);
        assert_eq!(default_prefixes(None).len(), 2);
    }
}
