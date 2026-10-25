//! `[[host]]` profiles of `settings.toml`: the machines `hutan --host NAME` reaches over SSH
//! (ADR 0022, slice R-2).
//!
//! ```toml
//! [[host]]
//! name = "studio"                 # what `--host` takes
//! ssh = "me@studio.local"         # what `ssh` is given: user@host, a ~/.ssh/config alias, ssh://…
//! hutan = "/opt/homebrew/bin/hutan" # optional: `hutan` on that machine (default: from its PATH)
//! ```
//!
//! A profile is a name and an address, never a credential: authentication, host keys and jump
//! hosts are the user's `~/.ssh/config`. Profiles live in the user's file only; a workspace or
//! project `.hutan/settings.toml` that carries `[[host]]` is refused (a repository must not
//! choose which machine a command runs on). They are not a settings namespace (an array of
//! tables is not a table), so this module reads and edits them, keeping comments.

use std::path::Path;

use serde::{Deserialize, Serialize};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

use crate::SettingsPaths;
use crate::error::SettingsError;

/// The top-level key of the profiles.
pub const KEY: &str = "host";

/// The longest profile name.
pub const NAME_MAX: usize = 64;

/// One `[[host]]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostProfile {
    /// What `hutan --host` takes: letters, digits, `.`, `_`, `-`.
    pub name: String,
    /// The destination handed to `ssh`: `user@host`, an `ssh_config` alias or an `ssh://` URI.
    pub ssh: String,
    /// `hutan` on the host: a path, or a name found on the non-interactive `PATH` there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hutan: Option<String>,
}

impl HostProfile {
    /// Why this profile cannot be used, if it cannot.
    pub fn check(&self) -> Result<(), String> {
        let name_ok = !self.name.is_empty()
            && self.name.len() <= NAME_MAX
            && self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            && !self.name.starts_with(['-', '.']);
        if !name_ok {
            return Err(format!(
                "`{}` is not a host name: use up to {NAME_MAX} letters, digits, `.`, `_` or `-`, \
                 not starting with `-` or `.`",
                self.name
            ));
        }
        if self.ssh.is_empty()
            || self.ssh.starts_with('-')
            || self
                .ssh
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(format!(
                "`ssh = {:?}` is not an ssh destination: one word (user@host, a ~/.ssh/config \
                 alias or an ssh:// URI) not starting with `-`; options belong in ~/.ssh/config",
                self.ssh
            ));
        }
        if let Some(hutan) = &self.hutan
            && (hutan.is_empty() || hutan.chars().any(char::is_control))
        {
            return Err("`hutan` is empty or has control characters".to_owned());
        }
        Ok(())
    }
}

/// The profiles of the TOML value of the `host` key; every problem found, not only the first.
pub fn parse_value(value: &toml::Value) -> Result<Vec<HostProfile>, Vec<String>> {
    let Some(entries) = value.as_array() else {
        return Err(vec![
            "`host` must be an array of tables: write `[[host]]`".to_owned(),
        ]);
    };
    let mut profiles: Vec<HostProfile> = Vec::new();
    let mut problems = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let at = format!("[[host]] #{}", index + 1);
        match entry.clone().try_into::<HostProfile>() {
            Err(e) => problems.push(format!("{at}: {}", e.message())),
            Ok(profile) => match profile.check() {
                Err(e) => problems.push(format!("{at}: {e}")),
                Ok(()) if profiles.iter().any(|p| p.name == profile.name) => {
                    problems.push(format!("{at}: the name `{}` is used twice", profile.name));
                }
                Ok(()) => profiles.push(profile),
            },
        }
    }
    if problems.is_empty() {
        Ok(profiles)
    } else {
        Err(problems)
    }
}

/// The profiles in `text` (the contents of a `settings.toml`); none when it has no `[[host]]`.
pub fn parse(text: &str) -> Result<Vec<HostProfile>, Vec<String>> {
    let table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| vec![e.to_string()])?;
    match table.get(KEY) {
        None => Ok(Vec::new()),
        Some(value) => parse_value(value),
    }
}

/// The user's profiles. A missing file has none; an unreadable or invalid one is an error that
/// names the file and what is wrong.
pub fn list(paths: &SettingsPaths) -> Result<Vec<HostProfile>, SettingsError> {
    let text = read(&paths.user)?;
    parse(&text).map_err(|problems| fail(&paths.user, problems.join("; ")))
}

/// The profile called `name`, or an error that lists the ones there are.
pub fn find(paths: &SettingsPaths, name: &str) -> Result<HostProfile, SettingsError> {
    let profiles = list(paths)?;
    match profiles.iter().find(|p| p.name == name) {
        Some(profile) => Ok(profile.clone()),
        None => {
            let known = if profiles.is_empty() {
                "none is defined; add one with `hutan host add NAME USER@HOST`".to_owned()
            } else {
                format!(
                    "known: {}",
                    profiles
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            Err(SettingsError::error(
                Some(&paths.user),
                KEY,
                format!("no host named `{name}` ({known})"),
            ))
        }
    }
}

/// Appends `profile` to the user's `settings.toml` (created when missing), keeping everything
/// else in the file as it is. Refuses a name that is taken and a profile that cannot be used.
pub fn add(paths: &SettingsPaths, profile: &HostProfile) -> Result<(), SettingsError> {
    profile
        .check()
        .map_err(|message| fail(&paths.user, message))?;
    let text = read(&paths.user)?;
    let existing = parse(&text).map_err(|problems| fail(&paths.user, problems.join("; ")))?;
    if existing.iter().any(|p| p.name == profile.name) {
        return Err(fail(
            &paths.user,
            format!(
                "a host named `{}` exists; remove it first (`hutan host remove {}`)",
                profile.name, profile.name
            ),
        ));
    }
    let mut doc = document(&paths.user, &text)?;
    let hosts = array(&paths.user, &mut doc)?;
    let mut table = Table::new();
    table.insert("name", toml_edit::value(profile.name.as_str()));
    table.insert("ssh", toml_edit::value(profile.ssh.as_str()));
    if let Some(hutan) = &profile.hutan {
        table.insert("hutan", toml_edit::value(hutan.as_str()));
    }
    hosts.push(table);
    write(&paths.user, &doc.to_string())
}

/// Removes the profile called `name`; `false` when there is none.
pub fn remove(paths: &SettingsPaths, name: &str) -> Result<bool, SettingsError> {
    let text = read(&paths.user)?;
    let mut doc = document(&paths.user, &text)?;
    if !doc.contains_key(KEY) {
        return Ok(false);
    }
    let hosts = array(&paths.user, &mut doc)?;
    let Some(index) = hosts
        .iter()
        .position(|t| t.get("name").and_then(Item::as_str) == Some(name))
    else {
        return Ok(false);
    };
    hosts.remove(index);
    if hosts.is_empty() {
        doc.remove(KEY);
    }
    write(&paths.user, &doc.to_string())?;
    Ok(true)
}

/// Diagnostics for the `host` key of a settings file, for the store's report.
pub(crate) fn diagnose(file: Option<&Path>, value: &toml::Value) -> Vec<SettingsError> {
    match parse_value(value) {
        Ok(_) => Vec::new(),
        Err(problems) => problems
            .into_iter()
            .map(|message| SettingsError::error(file, KEY, message))
            .collect(),
    }
}

fn document(path: &Path, text: &str) -> Result<DocumentMut, SettingsError> {
    text.parse()
        .map_err(|e: toml_edit::TomlError| fail(path, format!("invalid TOML: {e}")))
}

fn array<'a>(
    path: &Path,
    doc: &'a mut DocumentMut,
) -> Result<&'a mut ArrayOfTables, SettingsError> {
    if !doc.contains_key(KEY) {
        doc.insert(KEY, Item::ArrayOfTables(ArrayOfTables::new()));
    }
    doc.get_mut(KEY)
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| fail(path, "`host` is not an array of tables; write `[[host]]`"))
}

fn read(path: &Path) -> Result<String, SettingsError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(fail(path, format!("cannot read file: {e}"))),
    }
}

fn write(path: &Path, text: &str) -> Result<(), SettingsError> {
    crate::store::write_atomic(path, text)
        .map_err(|e| fail(path, format!("cannot write file: {e}")))
}

fn fail(path: &Path, message: impl Into<String>) -> SettingsError {
    SettingsError::error(Some(path), KEY, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &tempfile::TempDir) -> SettingsPaths {
        SettingsPaths::in_dir(dir.path())
    }

    fn profile(name: &str, ssh: &str) -> HostProfile {
        HostProfile {
            name: name.into(),
            ssh: ssh.into(),
            hutan: None,
        }
    }

    #[test]
    fn profiles_parse_and_every_problem_is_named() {
        let text = "[[host]]\nname = \"a\"\nssh = \"me@a\"\nhutan = \"~/bin/hutan\"\n\n[[host]]\nname = \"b\"\nssh = \"b-alias\"\n";
        let profiles = parse(text).unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].hutan.as_deref(), Some("~/bin/hutan"));

        let bad = "[[host]]\nname = \"a b\"\nssh = \"x\"\n[[host]]\nname = \"c\"\nssh = \"-oProxyCommand=x\"\n[[host]]\nname = \"d\"\nshh = \"typo\"\n[[host]]\nname = \"a\"\nssh = \"y\"\n[[host]]\nname = \"a\"\nssh = \"z\"\n";
        let problems = parse(bad).unwrap_err();
        assert_eq!(problems.len(), 4, "{problems:?}");
        assert!(problems[0].contains("#1") && problems[0].contains("not a host name"));
        assert!(problems[1].contains("#2") && problems[1].contains("ssh destination"));
        assert!(problems[2].contains("#3"));
        assert!(
            problems[3].contains("#5") && problems[3].contains("twice"),
            "{problems:?}"
        );
        assert!(parse("host = 3\n").is_err());
        assert_eq!(parse("[x]\na = 1\n").unwrap(), Vec::new());
    }

    #[test]
    fn add_and_remove_keep_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(&dir);
        std::fs::write(
            &paths.user,
            "# my settings\n[terminal]\nfont_size = 14 # big\n",
        )
        .unwrap();

        add(&paths, &profile("a", "me@a")).unwrap();
        add(
            &paths,
            &HostProfile {
                hutan: Some("/opt/hutan".into()),
                ..profile("b", "ssh://me@b:2222")
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(&paths.user).unwrap();
        assert!(
            text.starts_with("# my settings\n[terminal]\nfont_size = 14 # big\n"),
            "{text}"
        );
        assert_eq!(list(&paths).unwrap().len(), 2);
        assert_eq!(
            find(&paths, "b").unwrap().hutan.as_deref(),
            Some("/opt/hutan")
        );

        assert!(
            add(&paths, &profile("a", "other"))
                .unwrap_err()
                .message
                .contains("exists")
        );
        assert!(add(&paths, &profile("c", "-x")).is_err());
        assert!(
            find(&paths, "zzz")
                .unwrap_err()
                .message
                .contains("known: a, b")
        );

        assert!(remove(&paths, "a").unwrap());
        assert!(!remove(&paths, "a").unwrap());
        assert!(remove(&paths, "b").unwrap());
        let text = std::fs::read_to_string(&paths.user).unwrap();
        assert_eq!(text, "# my settings\n[terminal]\nfont_size = 14 # big\n");
    }

    #[test]
    fn a_missing_file_has_no_hosts_and_a_broken_one_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(&dir);
        assert_eq!(list(&paths).unwrap(), Vec::new());
        assert!(!remove(&paths, "a").unwrap());
        assert!(
            find(&paths, "a")
                .unwrap_err()
                .message
                .contains("none is defined")
        );
        std::fs::write(&paths.user, "host = \"nope\"\n").unwrap();
        let error = list(&paths).unwrap_err();
        assert_eq!(error.file.as_deref(), Some(paths.user.as_path()));
        assert!(error.message.contains("[[host]]"), "{error:?}");
    }
}
