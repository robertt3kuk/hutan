//! Which command runs a folder's tests, found by the files that are there (`agents.verify`,
//! `docs/assistant.md` § Checks › Verification): nothing is run to find out, and nothing is
//! guessed beyond what the files say.
//!
//! In this order, the first match wins and the others found are named in the reason:
//!
//! | File | Command |
//! |---|---|
//! | `Cargo.toml` | `cargo test` |
//! | `package.json` with a `test` script (npm's placeholder does not count) | `pnpm test` with `pnpm-lock.yaml`, `yarn test` with `yarn.lock`, `bun test` with `bun.lock`/`bun.lockb`, else `npm test` |
//! | `go.mod` | `go test ./...` |
//! | `pytest.ini`, a `pyproject.toml` that has `[tool.pytest`, a `setup.cfg` that has `[tool:pytest]`, or either of the last two beside a `tests/` or `test/` folder | `pytest` |
//! | `Makefile`, `makefile` or `GNUmakefile` with a `test:` target | `make test` |

use std::fs;
use std::path::{Path, PathBuf};

/// A file is read for its keywords only up to this many bytes.
const READ_MAX: u64 = 1024 * 1024;

/// A command found, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub command: String,
    /// The folder whose files said so; the command runs there.
    pub dir: PathBuf,
    /// In words, for the card and the CLI.
    pub why: String,
}

/// What a message says was looked for when nothing was found.
pub const LOOKED_FOR: &str = "Cargo.toml, a package.json with a test script, go.mod, pytest \
configuration, or a Makefile with a `test:` target";

/// The command of the first of `dirs` (the session's folder, then its parents up to the checkout)
/// that has one.
pub fn detect(dirs: &[PathBuf]) -> Option<Detected> {
    dirs.iter().find_map(|dir| detect_in(dir))
}

fn detect_in(dir: &Path) -> Option<Detected> {
    let mut found: Vec<(String, String)> = Vec::new();
    if dir.join("Cargo.toml").is_file() {
        found.push(("cargo test".to_owned(), "Cargo.toml".to_owned()));
    }
    if let Some(found_node) = node(dir) {
        found.push(found_node);
    }
    if dir.join("go.mod").is_file() {
        found.push(("go test ./...".to_owned(), "go.mod".to_owned()));
    }
    if let Some(why) = pytest(dir) {
        found.push(("pytest".to_owned(), why));
    }
    if let Some(name) = makefile(dir) {
        found.push((
            "make test".to_owned(),
            format!("{name} with a `test:` target"),
        ));
    }
    let mut found = found.into_iter();
    let (command, reason) = found.next()?;
    let others: Vec<String> = found.map(|(command, _)| format!("`{command}`")).collect();
    let mut why = format!("{reason} in {}", dir.display());
    if !others.is_empty() {
        why.push_str(&format!(
            " (the folder also suggests {})",
            others.join(", ")
        ));
    }
    Some(Detected {
        command,
        dir: dir.to_path_buf(),
        why,
    })
}

fn read(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > READ_MAX {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// `package.json`'s `test` script, and the package manager the lockfile names.
fn node(dir: &Path) -> Option<(String, String)> {
    let manifest: serde_json::Value =
        serde_json::from_str(&read(&dir.join("package.json"))?).ok()?;
    let script = manifest.get("scripts")?.get("test")?.as_str()?.trim();
    // What `npm init` writes: no test at all.
    if script.is_empty() || script.contains("no test specified") {
        return None;
    }
    let lock = |name: &str| dir.join(name).is_file();
    let (manager, lockfile) = if lock("pnpm-lock.yaml") {
        ("pnpm", Some("pnpm-lock.yaml"))
    } else if lock("yarn.lock") {
        ("yarn", Some("yarn.lock"))
    } else if lock("bun.lock") {
        ("bun", Some("bun.lock"))
    } else if lock("bun.lockb") {
        ("bun", Some("bun.lockb"))
    } else if lock("package-lock.json") {
        ("npm", Some("package-lock.json"))
    } else {
        ("npm", None)
    };
    let why = match lockfile {
        Some(lockfile) => format!("package.json has a test script and {lockfile} is there"),
        None => "package.json has a test script (no lockfile: npm)".to_owned(),
    };
    Some((format!("{manager} test"), why))
}

fn pytest(dir: &Path) -> Option<String> {
    if dir.join("pytest.ini").is_file() {
        return Some("pytest.ini".to_owned());
    }
    let tests_folder = dir.join("tests").is_dir() || dir.join("test").is_dir();
    for (file, section) in [
        ("pyproject.toml", "[tool.pytest"),
        ("setup.cfg", "[tool:pytest]"),
    ] {
        let Some(text) = read(&dir.join(file)) else {
            continue;
        };
        if text.contains(section) {
            return Some(format!("{file} configures pytest"));
        }
        if tests_folder {
            return Some(format!("{file} and a tests folder"));
        }
    }
    None
}

fn makefile(dir: &Path) -> Option<&'static str> {
    ["Makefile", "makefile", "GNUmakefile"]
        .into_iter()
        .find(|name| {
            read(&dir.join(name)).is_some_and(|text| {
                text.lines().any(|line| {
                    line.strip_prefix("test").is_some_and(|rest| {
                        rest.trim_start().starts_with(':') && !rest.trim_start().starts_with(":=")
                    })
                })
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        dir
    }

    fn found(files: &[(&str, &str)]) -> Option<Detected> {
        let dir = dir_with(files);
        detect(&[dir.path().to_path_buf()]).map(|mut d| {
            // The folder is a temporary one; the tests compare the rest.
            d.why = d.why.replace(&dir.path().display().to_string(), "<dir>");
            d.dir = PathBuf::from("<dir>");
            d
        })
    }

    #[test]
    fn a_cargo_project_runs_cargo_test() {
        let d = found(&[("Cargo.toml", "[package]\nname = \"x\"\n")]).unwrap();
        assert_eq!(d.command, "cargo test");
        assert_eq!(d.why, "Cargo.toml in <dir>");
    }

    #[test]
    fn the_lockfile_picks_the_package_manager() {
        let manifest = r#"{"scripts": {"test": "vitest run"}}"#;
        for (lock, command) in [
            ("pnpm-lock.yaml", "pnpm test"),
            ("yarn.lock", "yarn test"),
            ("bun.lock", "bun test"),
            ("bun.lockb", "bun test"),
            ("package-lock.json", "npm test"),
        ] {
            let d = found(&[("package.json", manifest), (lock, "")]).unwrap();
            assert_eq!(d.command, command, "{lock}");
            assert!(d.why.contains(lock), "{}", d.why);
        }
        let d = found(&[("package.json", manifest)]).unwrap();
        assert_eq!(d.command, "npm test");
        assert!(d.why.contains("no lockfile"), "{}", d.why);
    }

    #[test]
    fn npms_placeholder_test_script_is_not_a_test_command() {
        let placeholder = r#"{"scripts": {"test": "echo \"Error: no test specified\" && exit 1"}}"#;
        assert!(found(&[("package.json", placeholder)]).is_none());
        assert!(found(&[("package.json", r#"{"name": "x"}"#)]).is_none());
        assert!(found(&[("package.json", "not json")]).is_none());
    }

    #[test]
    fn go_python_and_make_are_found_by_their_files() {
        assert_eq!(
            found(&[("go.mod", "module x\n")]).unwrap().command,
            "go test ./..."
        );
        assert_eq!(found(&[("pytest.ini", "")]).unwrap().command, "pytest");
        assert_eq!(
            found(&[("pyproject.toml", "[tool.pytest.ini_options]\n")])
                .unwrap()
                .command,
            "pytest"
        );
        assert_eq!(
            found(&[("setup.cfg", "[tool:pytest]\n")]).unwrap().command,
            "pytest"
        );
        let d = found(&[("pyproject.toml", "[project]\n"), ("tests/test_a.py", "")]).unwrap();
        assert_eq!(d.command, "pytest");
        assert_eq!(d.why, "pyproject.toml and a tests folder in <dir>");
        assert!(found(&[("pyproject.toml", "[project]\n")]).is_none());
        let d = found(&[("Makefile", "build:\n\tcc x.c\ntest: build\n\t./a.out\n")]).unwrap();
        assert_eq!(d.command, "make test");
        assert!(found(&[("Makefile", "build:\n\tcc x.c\ntestdata:\n")]).is_none());
        assert!(found(&[("Makefile", "TEST := 1\n")]).is_none());
    }

    #[test]
    fn the_first_ecosystem_wins_and_the_others_are_named() {
        let d = found(&[
            ("Cargo.toml", ""),
            ("package.json", r#"{"scripts": {"test": "jest"}}"#),
            ("Makefile", "test:\n"),
        ])
        .unwrap();
        assert_eq!(d.command, "cargo test");
        assert_eq!(
            d.why,
            "Cargo.toml in <dir> (the folder also suggests `npm test`, `make test`)"
        );
    }

    #[test]
    fn a_parent_folder_is_looked_at_when_the_folder_has_nothing() {
        let dir = dir_with(&[("Cargo.toml", ""), ("crates/x/src/lib.rs", "")]);
        let inner = dir.path().join("crates/x");
        let d = detect(&[inner, dir.path().to_path_buf()]).unwrap();
        assert_eq!(d.dir, dir.path());
        assert!(detect(&[]).is_none());
        let empty = tempfile::tempdir().unwrap();
        assert!(detect(&[empty.path().to_path_buf()]).is_none());
    }
}
