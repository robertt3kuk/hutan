//! The opt-in status-line tee of Claude Code (`hutan integration install claude --statusline`).
//!
//! Claude Code runs one `statusLine` command per user and writes the session's JSON (context
//! window, cost, durations) to its standard input. Hutan puts its own script in that place: it
//! gives the JSON to `hutan agent statusline` and then runs the command the user had, with the
//! same input, passing its output through (`integrations/hooks/hutan-statusline.sh`).
//!
//! What is kept so nothing of the user's is lost, in the hooks folder beside the script:
//!
//! - `hutan-statusline.original.json`: `{"statusLine": <what `settings.json` had, or null>}`, the
//!   whole value, so that uninstall puts it back as it was;
//! - `hutan-statusline.command`: the original command as text, which the script runs.
//!
//! The install never replaces a status line it cannot chain to (one that is not a command), and
//! never overwrites the user's own command: it is moved, not removed. An uninstall restores the
//! original only while the entry is still Hutan's; a status line the user set since is theirs and
//! is left alone.

use std::path::Path;

use serde_json::{Map, Value};

use super::hooks_json::{EditError, MARKER};

pub const SCRIPT_NAME: &str = "hutan-statusline.sh";
pub const COMMAND_FILE: &str = "hutan-statusline.command";
pub const ORIGINAL_FILE: &str = "hutan-statusline.original.json";
/// The marker value of the tee's command (after [`MARKER`]).
const TARGET: &str = "claude-statusline";

/// The `statusLine.command` that runs the tee: a no-op builtin carrying the marker, then the
/// script.
pub fn command(script: &Path) -> String {
    format!(
        ": {MARKER}{TARGET}; sh {}",
        super::shell_quote(&script.to_string_lossy())
    )
}

/// Whether `value`, a `statusLine` entry, is the tee.
pub fn is_ours(value: &Value) -> bool {
    value
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(&format!("{MARKER}{TARGET}")))
}

/// What an install found, so that the caller can keep it.
#[derive(Debug, PartialEq)]
pub struct Found {
    /// The user's own entry, now moved aside: `Some(None)` when there was none. `None`: the entry
    /// was the tee already, and what was kept stays.
    pub original: Option<Option<Value>>,
    pub changed: bool,
}

/// Makes the document's `statusLine` run `tee`, keeping the other keys of the entry (`padding`,
/// ...). An entry that is not a command is refused: there would be nothing to chain to.
pub fn install(doc: &mut Value, tee: &str) -> Result<Found, EditError> {
    let root = doc.as_object_mut().ok_or(EditError::NotAnObject)?;
    match root.get("statusLine") {
        Some(value) if is_ours(value) => {
            let current = value.get("command").and_then(Value::as_str);
            if current == Some(tee) {
                return Ok(Found {
                    original: None,
                    changed: false,
                });
            }
            // The script moved (a new --dir): point at the new one, keep the rest.
            let mut entry = value.clone();
            set_command(&mut entry, tee);
            root.insert("statusLine".to_owned(), entry);
            Ok(Found {
                original: None,
                changed: true,
            })
        }
        Some(value) => {
            let command = value.get("command").and_then(Value::as_str);
            let is_command = value
                .get("type")
                .and_then(Value::as_str)
                .is_none_or(|kind| kind == "command");
            if !value.is_object() || command.is_none() || !is_command {
                return Err(EditError::Refused(
                    "`statusLine` is not a command, so Hutan cannot run it after its own; \
                     leaving it as it is"
                        .to_owned(),
                ));
            }
            let original = value.clone();
            let mut entry = value.clone();
            set_command(&mut entry, tee);
            root.insert("statusLine".to_owned(), entry);
            Ok(Found {
                original: Some(Some(original)),
                changed: true,
            })
        }
        None => {
            let mut entry = Value::Object(Map::new());
            entry["type"] = Value::String("command".to_owned());
            set_command(&mut entry, tee);
            root.insert("statusLine".to_owned(), entry);
            Ok(Found {
                original: Some(None),
                changed: true,
            })
        }
    }
}

fn set_command(entry: &mut Value, tee: &str) {
    if let Some(object) = entry.as_object_mut() {
        object.insert("command".to_owned(), Value::String(tee.to_owned()));
    }
}

/// Puts the user's status line back where the tee is: `original` is what the install kept
/// (`Some(None)`: there was none; `None`: nothing was kept, the tee is removed and nothing put
/// in its place). A `statusLine` that is not the tee is not touched. Returns whether the
/// document changed.
pub fn uninstall(doc: &mut Value, original: Option<Option<Value>>) -> Result<bool, EditError> {
    let root = doc.as_object_mut().ok_or(EditError::NotAnObject)?;
    if !root.get("statusLine").is_some_and(is_ours) {
        return Ok(false);
    }
    match original {
        Some(Some(value)) => {
            root.insert("statusLine".to_owned(), value);
        }
        Some(None) | None => {
            root.remove("statusLine");
        }
    }
    Ok(true)
}

/// The file `ORIGINAL_FILE` holds, from its text: the kept entry, `Some(None)` for "there was
/// none"; `None` for a file that is not ours to read.
pub fn read_original(text: &str) -> Option<Option<Value>> {
    let doc: Value = serde_json::from_str(text).ok()?;
    let value = doc.as_object()?.get("statusLine")?;
    Some((!value.is_null()).then(|| value.clone()))
}

/// The text of `ORIGINAL_FILE` for what the install kept.
pub fn original_text(original: &Option<Value>) -> String {
    let doc = serde_json::json!({
        "statusLine": original.clone().unwrap_or(Value::Null),
    });
    let mut text = serde_json::to_string_pretty(&doc).unwrap_or_default();
    text.push('\n');
    text
}

/// The original command as the script runs it: the text of the entry's `command`, or nothing.
pub fn command_text(original: &Option<Value>) -> String {
    original
        .as_ref()
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const TEE: &str = ": hutan-integration=claude-statusline; sh '/c/hooks/hutan-statusline.sh'";

    #[test]
    fn a_users_own_command_is_kept_whole_and_the_tee_takes_its_place() {
        let own = json!({"type": "command", "command": "~/bin/line.sh --short", "padding": 2});
        let mut doc = json!({"model": "opus", "statusLine": own});
        let found = install(&mut doc, TEE).unwrap();
        assert_eq!(
            found,
            Found {
                original: Some(Some(own.clone())),
                changed: true
            }
        );
        assert_eq!(doc["statusLine"]["command"], TEE);
        assert_eq!(doc["statusLine"]["padding"], 2);
        assert_eq!(doc["model"], "opus");
        assert_eq!(command_text(&Some(own)), "~/bin/line.sh --short");
    }

    #[test]
    fn installing_twice_changes_nothing_and_remembers_the_first() {
        let mut doc = json!({"statusLine": {"type": "command", "command": "echo hi"}});
        install(&mut doc, TEE).unwrap();
        let again = install(&mut doc, TEE).unwrap();
        assert_eq!(
            again,
            Found {
                original: None,
                changed: false
            }
        );
        // The script moved: only the command changes.
        let moved = install(
            &mut doc,
            ": hutan-integration=claude-statusline; sh '/d/x.sh'",
        )
        .unwrap();
        assert_eq!(moved.original, None);
        assert!(moved.changed);
    }

    #[test]
    fn no_status_line_before_means_none_after_the_uninstall() {
        let mut doc = json!({"model": "opus"});
        let found = install(&mut doc, TEE).unwrap();
        assert_eq!(found.original, Some(None));
        assert_eq!(doc["statusLine"]["type"], "command");
        assert!(uninstall(&mut doc, found.original).unwrap());
        assert_eq!(doc, json!({"model": "opus"}));
    }

    #[test]
    fn the_uninstall_restores_the_original_only_while_the_tee_is_in_place() {
        let own = json!({"type": "command", "command": "echo mine", "padding": 1});
        let mut doc = json!({"statusLine": own});
        let found = install(&mut doc, TEE).unwrap();
        let mut restored = doc.clone();
        assert!(uninstall(&mut restored, found.original.clone()).unwrap());
        assert_eq!(restored, json!({"statusLine": own}));
        // The user set another one since: it is theirs.
        let mut changed = json!({"statusLine": {"type": "command", "command": "echo newer"}});
        assert!(!uninstall(&mut changed, found.original).unwrap());
        assert_eq!(changed["statusLine"]["command"], "echo newer");
    }

    #[test]
    fn a_status_line_that_is_not_a_command_is_refused() {
        let mut doc = json!({"statusLine": {"type": "text", "text": "hi"}});
        assert!(install(&mut doc, TEE).is_err());
        assert_eq!(doc, json!({"statusLine": {"type": "text", "text": "hi"}}));
        let mut odd = json!({"statusLine": "echo"});
        assert!(install(&mut odd, TEE).is_err());
    }

    #[test]
    fn what_is_kept_reads_back() {
        let own = Some(json!({"type": "command", "command": "echo x"}));
        assert_eq!(read_original(&original_text(&own)), Some(own));
        assert_eq!(read_original(&original_text(&None)), Some(None));
        assert_eq!(read_original("not json"), None);
    }
}
