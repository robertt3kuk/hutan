//! `hutan config ls|get|set|reset`: the daemon's own settings (`daemon.toml`), through the
//! daemon (`docs/protocol.md` § Daemon settings).

use hutan_client::Client;
use hutan_protocol::daemon_settings::{Applies, DaemonSetting, DaemonSettings};
use serde_json::Value;

use super::Ctx;
use crate::args::{ConfigCommand, ConfigKeyArgs, ConfigSetArgs};
use crate::error::CliError;

/// What a change that needs a restart is followed by.
const RESTART_HINT: &str = "restart the daemon to apply (`hutan daemon restart`)";

impl Ctx {
    pub(super) async fn config(&self, command: ConfigCommand) -> Result<(), CliError> {
        let client = self.connect().await?;
        if !client.supports_daemon_settings() {
            return Err(CliError::Failed(
                "this hutand does not offer daemon settings (it is older than protocol 0.28); \
                 restart it with `hutan daemon restart`"
                    .to_owned(),
            ));
        }
        match command {
            ConfigCommand::Ls => self.config_ls(&client).await,
            ConfigCommand::Get(args) => self.config_get(&client, args).await,
            ConfigCommand::Set(args) => self.config_set(&client, args).await,
            ConfigCommand::Reset(args) => self.config_reset(&client, args).await,
        }
    }

    async fn config_ls(&self, client: &Client) -> Result<(), CliError> {
        let settings = client.daemon_settings().await.map_err(|e| self.fail(e))?;
        self.print(&settings, || table(&settings))
    }

    async fn config_get(&self, client: &Client, args: ConfigKeyArgs) -> Result<(), CliError> {
        let settings = client.daemon_settings().await.map_err(|e| self.fail(e))?;
        let entry = entry(&settings, &args.key)?;
        self.print(entry, || {
            let mut text = render(&entry.value);
            if entry.pending_restart {
                text.push_str(&format!("\n{RESTART_HINT}"));
            }
            text
        })
    }

    async fn config_set(&self, client: &Client, args: ConfigSetArgs) -> Result<(), CliError> {
        let value = parse_value(&args.value);
        let settings = client
            .daemon_setting_set(&args.key, Some(value))
            .await
            .map_err(|e| self.fail(e))?;
        self.changed(&settings, &args.key, "set")
    }

    async fn config_reset(&self, client: &Client, args: ConfigKeyArgs) -> Result<(), CliError> {
        let settings = client
            .daemon_setting_set(&args.key, None)
            .await
            .map_err(|e| self.fail(e))?;
        self.changed(&settings, &args.key, "reset")
    }

    /// What `set` and `reset` print: the key as it is now, and when the running daemon will
    /// not use it before a restart.
    fn changed(&self, settings: &DaemonSettings, key: &str, verb: &str) -> Result<(), CliError> {
        let entry = entry(settings, key)?;
        self.print(entry, || {
            let mut text = format!("{verb}: {key} = {}", render(&entry.value));
            if verb == "reset" {
                text.push_str(" (the default)");
            }
            if entry.pending_restart {
                text.push_str(&format!("\n{RESTART_HINT}"));
            }
            text
        })
    }
}

fn entry<'a>(settings: &'a DaemonSettings, key: &str) -> Result<&'a DaemonSetting, CliError> {
    settings
        .entries
        .iter()
        .find(|entry| entry.key == key)
        .ok_or_else(|| {
            CliError::Failed(format!(
                "`{key}` is not a setting of {} (`hutan config ls` lists them)",
                settings.file
            ))
        })
}

/// `true`, `30`, `"x"`, `["a", "b"]` as TOML; text that is no TOML value (`read_only`, a path
/// without quotes) is a plain string.
fn parse_value(raw: &str) -> Value {
    let parsed = format!("value = {raw}")
        .parse::<toml_edit::DocumentMut>()
        .ok()
        .and_then(|doc| {
            doc.get("value")
                .and_then(toml_edit::Item::as_value)
                .map(json)
        });
    parsed.unwrap_or_else(|| Value::String(raw.to_owned()))
}

fn json(value: &toml_edit::Value) -> Value {
    match value {
        toml_edit::Value::String(s) => Value::String(s.value().clone()),
        toml_edit::Value::Integer(n) => Value::from(*n.value()),
        toml_edit::Value::Float(f) => {
            serde_json::Number::from_f64(*f.value()).map_or(Value::Null, Value::Number)
        }
        toml_edit::Value::Boolean(b) => Value::Bool(*b.value()),
        toml_edit::Value::Datetime(d) => Value::String(d.value().to_string()),
        toml_edit::Value::Array(items) => Value::Array(items.iter().map(json).collect()),
        toml_edit::Value::InlineTable(table) => Value::Object(
            table
                .iter()
                .map(|(key, value)| (key.to_owned(), json(value)))
                .collect(),
        ),
    }
}

/// A value for a table cell: text as it is (`-` when empty), the rest as JSON.
fn render(value: &Value) -> String {
    match value {
        Value::String(s) if s.is_empty() => "-".to_owned(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn table(settings: &DaemonSettings) -> String {
    let mut rows = vec![[
        "KEY".to_owned(),
        "VALUE".to_owned(),
        "DEFAULT".to_owned(),
        "SET".to_owned(),
        "APPLIES".to_owned(),
        "PENDING".to_owned(),
    ]];
    for entry in &settings.entries {
        rows.push([
            entry.key.clone(),
            render(&entry.value),
            render(&entry.default),
            if entry.set_in_file { "yes" } else { "-" }.to_owned(),
            match entry.applies {
                Applies::Live => "live",
                Applies::Restart => "restart",
            }
            .to_owned(),
            if entry.pending_restart {
                "restart"
            } else {
                "-"
            }
            .to_owned(),
        ]);
    }
    let mut widths = [0usize; 6];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut lines: Vec<String> = rows
        .iter()
        .map(|row| {
            let mut line = String::new();
            for (i, cell) in row.iter().enumerate() {
                line.push_str(cell);
                if i + 1 < row.len() {
                    line.push_str(&" ".repeat(widths[i] - cell.chars().count() + 2));
                }
            }
            line.trim_end().to_owned()
        })
        .collect();
    lines.push(String::new());
    lines.push(format!("file: {}", settings.file));
    if settings.restart_pending {
        lines.push(RESTART_HINT.to_owned());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn values_are_toml_and_bare_text_is_a_string() {
        assert_eq!(parse_value("true"), json!(true));
        assert_eq!(parse_value("30"), json!(30));
        assert_eq!(parse_value("-1"), json!(-1));
        assert_eq!(parse_value("\"x y\""), json!("x y"));
        assert_eq!(parse_value("[\"a\", \"b\"]"), json!(["a", "b"]));
        assert_eq!(parse_value("{ A = \"1\" }"), json!({"A": "1"}));
        assert_eq!(parse_value("read_only"), json!("read_only"));
        assert_eq!(parse_value("/usr/bin/omp"), json!("/usr/bin/omp"));
        assert_eq!(parse_value(""), json!(""));
    }

    #[test]
    fn cells_show_empty_text_as_a_dash() {
        assert_eq!(render(&json!("")), "-");
        assert_eq!(render(&json!("x")), "x");
        assert_eq!(render(&json!(false)), "false");
        assert_eq!(render(&json!(["a"])), "[\"a\"]");
    }
}
