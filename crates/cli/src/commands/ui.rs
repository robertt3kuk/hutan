//! `hutan ui actions | layout | run | set`: the desktop window through the daemon (ADR 0016).

use hutan_protocol::ui::{
    AppearanceSummary, LayoutSummary, PaneSummary, UiAction, UiActionList, UiRan, UiRun,
    UiSetSetting, UiSetting,
};
use serde_json::{Map, Value};

use super::Ctx;
use crate::args::{UiActionsArgs, UiCommand, UiRunArgs, UiSetArgs, UiWindowArgs};
use crate::error::CliError;
use hutan_protocol::messages::Request;

/// The catalog id of the desktop's *Run Setup Again…* (`workbench::RunSetup`).
const SETUP_ACTION: &str = "workbench::RunSetup";

impl Ctx {
    pub(super) async fn ui(&self, command: UiCommand) -> Result<(), CliError> {
        match command {
            UiCommand::Actions(args) => self.ui_actions(args).await,
            UiCommand::Layout(args) => self.ui_layout(args).await,
            UiCommand::Run(args) => self.ui_run(args).await,
            UiCommand::Set(args) => self.ui_set(args).await,
        }
    }

    /// `hutan setup`: the setup is the desktop's. With a window connected it opens there
    /// (`ui.run workbench::RunSetup`, the same action as *Run Setup Again…*); without one the
    /// command says where it lives.
    pub(super) async fn setup(&self) -> Result<(), CliError> {
        let client = self.connect().await?;
        let mut opened = false;
        if client.supports_control() {
            match client
                .ui_run(UiRun {
                    window_id: None,
                    action: SETUP_ACTION.to_owned(),
                    args: None,
                })
                .await
            {
                Ok(_) => opened = true,
                // No window is open.
                Err(hutan_client::ClientError::Remote(body))
                    if body.code == hutan_protocol::ErrorCode::InvalidState => {}
                Err(error) => return Err(self.fail(error)),
            }
        }
        self.print(&serde_json::json!({ "opened": opened }), || {
            if opened {
                "the setup is open in Hutan".to_owned()
            } else {
                "Hutan's setup opens in the desktop window and none is open: start Hutan, then \
                 choose Hutan ▸ Run Setup Again… (or run `hutan setup` again)"
                    .to_owned()
            }
        })
    }

    async fn catalog(
        &self,
        client: &hutan_client::Client,
        window: Option<String>,
    ) -> Result<UiActionList, CliError> {
        self.call(
            client,
            Request::UiActions(hutan_protocol::ui::UiTarget { window_id: window }),
        )
        .await
    }

    async fn ui_actions(&self, args: UiActionsArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let mut list = self.catalog(&client, args.window.window).await?;
        if let Some(filter) = args.filter.as_deref().map(str::to_lowercase) {
            list.actions.retain(|a| {
                a.id.to_lowercase().contains(&filter) || a.title.to_lowercase().contains(&filter)
            });
            list.settings.retain(|s| {
                format!("{}.{}", s.namespace, s.key)
                    .to_lowercase()
                    .contains(&filter)
                    || s.doc.to_lowercase().contains(&filter)
            });
        }
        self.print(&list, || actions_text(&list))
    }

    async fn ui_layout(&self, args: UiWindowArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let result: hutan_protocol::ui::UiLayoutResult = self
            .call(
                &client,
                Request::UiLayout(hutan_protocol::ui::UiTarget {
                    window_id: args.window,
                }),
            )
            .await?;
        self.print(&result.layout, || layout_text(&result.layout))
    }

    async fn ui_run(&self, args: UiRunArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let catalog = self.catalog(&client, args.window.window.clone()).await.ok();
        let window_id = catalog
            .as_ref()
            .map(|list| list.window_id.clone())
            .or_else(|| args.window.window.clone())
            .unwrap_or_default();
        let schema = catalog
            .and_then(|list| list.actions.into_iter().find(|a| a.id == args.action))
            .and_then(|a| a.args_schema);
        let arguments = parse_args(&args.args, schema.as_ref())?;
        let ran = match client
            .call::<UiRan>(Request::UiRun(UiRun {
                window_id: args.window.window,
                action: args.action.clone(),
                args: (!arguments.is_empty()).then_some(Value::Object(arguments)),
            }))
            .await
        {
            Ok(ran) => ran,
            // An action that closes the window (Quit) ends the window's connection before it
            // can answer: the daemon says so, and that is the action's success.
            Err(hutan_client::ClientError::Remote(body)) if window_closed(&body) => UiRan {
                window_id,
                result: serde_json::json!({ "ran": args.action, "window_closed": true }),
            },
            Err(error) => return Err(self.fail(error)),
        };
        self.print(&ran, || ran_text(&args.action, &ran))
    }

    async fn ui_set(&self, args: UiSetArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let catalog = self.catalog(&client, args.window.window.clone()).await?;
        let setting = find_setting(&catalog.settings, &args.key)?;
        let value = parse_setting_value(setting, &args.value)?;
        let ran: UiRan = self
            .call(
                &client,
                Request::UiSetSetting(UiSetSetting {
                    window_id: args.window.window,
                    namespace: setting.namespace.clone(),
                    key: setting.key.clone(),
                    value,
                    layer: args.layer.name().to_owned(),
                }),
            )
            .await?;
        self.print(&ran, || {
            format!(
                "set {}.{} in the {} layer",
                setting.namespace,
                setting.key,
                args.layer.name()
            )
        })
    }
}

/// What the daemon answers a `ui.run` whose window went away while it ran the action
/// (`crates/daemon/src/uihub.rs`).
const WINDOW_CLOSED: &str = "the window closed before it answered";

/// Whether `body` is the daemon's "the window closed before it answered".
fn window_closed(body: &hutan_protocol::ErrorBody) -> bool {
    body.code == hutan_protocol::ErrorCode::InvalidState && body.message == WINDOW_CLOSED
}

/// `--arg key=value` pairs as a JSON object. A value is read as JSON when the argument's schema
/// does not say it is a string and the text parses as JSON; otherwise it is the text itself.
fn parse_args(pairs: &[String], schema: Option<&Value>) -> Result<Map<String, Value>, CliError> {
    let mut out = Map::new();
    for pair in pairs {
        let (key, text) = pair
            .split_once('=')
            .ok_or_else(|| CliError::Usage(format!("`--arg {pair}`: expected KEY=VALUE")))?;
        if key.is_empty() {
            return Err(CliError::Usage(format!("`--arg {pair}`: the key is empty")));
        }
        let declared = schema
            .and_then(|s| s.pointer(&format!("/properties/{key}/type")))
            .and_then(Value::as_str);
        let value = match declared {
            Some("string") => Value::String(text.to_owned()),
            _ => serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned())),
        };
        out.insert(key.to_owned(), value);
    }
    Ok(out)
}

/// The setting named `namespace.key`: namespaces contain dots too (`panels.sessions`), so the
/// published catalog decides where the key starts.
fn find_setting<'a>(settings: &'a [UiSetting], dotted: &str) -> Result<&'a UiSetting, CliError> {
    settings
        .iter()
        .find(|s| format!("{}.{}", s.namespace, s.key) == dotted)
        .ok_or_else(|| {
            CliError::Failed(format!(
                "unknown setting `{dotted}`; `hutan ui actions` lists the settings"
            ))
        })
}

fn parse_setting_value(setting: &UiSetting, text: &str) -> Result<Value, CliError> {
    let bad = |what: &str| {
        CliError::Usage(format!(
            "{}.{} takes {what}, not `{text}`",
            setting.namespace, setting.key
        ))
    };
    match setting.kind.as_str() {
        "bool" => match text {
            "true" | "on" | "yes" => Ok(Value::Bool(true)),
            "false" | "off" | "no" => Ok(Value::Bool(false)),
            _ => Err(bad("true or false")),
        },
        "integer" => text
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| bad("a whole number")),
        "number" => text
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| bad("a number")),
        "string_list" => match serde_json::from_str::<Value>(text) {
            Ok(list @ Value::Array(_)) => Ok(list),
            _ => Ok(Value::Array(
                text.split(',')
                    .map(|item| Value::String(item.trim().to_owned()))
                    .collect(),
            )),
        },
        "unsupported" => Err(CliError::Failed(format!(
            "{}.{} cannot be changed here; edit settings.toml",
            setting.namespace, setting.key
        ))),
        // `enum` and `string`.
        _ => Ok(Value::String(text.to_owned())),
    }
}

fn actions_text(list: &UiActionList) -> String {
    let mut out = format!(
        "window {}\n\nactions ({})\n",
        list.window_id,
        list.actions.len()
    );
    for action in &list.actions {
        out.push_str(&format!("  {}  {}", action.id, action.title));
        if let Some(args) = action_args(action) {
            out.push_str(&format!("   [{args}]"));
        }
        out.push('\n');
    }
    out.push_str(&format!("\nsettings ({})\n", list.settings.len()));
    for setting in &list.settings {
        let current = setting
            .current
            .as_ref()
            .map(|v| format!(" = {v}"))
            .unwrap_or_default();
        let values = if setting.values.is_empty() {
            String::new()
        } else {
            format!(" ({})", setting.values.join("|"))
        };
        out.push_str(&format!(
            "  {}.{} [{}{values}]{current}  layers: {}\n",
            setting.namespace,
            setting.key,
            setting.kind,
            setting.layers.join(",")
        ));
    }
    out.trim_end().to_owned()
}

/// `name: string, count: integer` for an action that takes arguments.
fn action_args(action: &UiAction) -> Option<String> {
    let properties = action
        .args_schema
        .as_ref()?
        .get("properties")?
        .as_object()?;
    let required: Vec<&str> = action
        .args_schema
        .as_ref()?
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let parts: Vec<String> = properties
        .iter()
        .map(|(name, property)| {
            let kind = property
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("any");
            let star = if required.contains(&name.as_str()) {
                ""
            } else {
                "?"
            };
            format!("--arg {name}{star}=<{kind}>")
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn ran_text(action: &str, ran: &UiRan) -> String {
    match &ran.result {
        Value::Object(map) if map.is_empty() => format!("ran {action} in window {}", ran.window_id),
        Value::Null => format!("ran {action} in window {}", ran.window_id),
        Value::Object(map)
            if map.len() == 2 && map.get("window_closed") == Some(&Value::Bool(true)) =>
        {
            format!("ran {action}; the window closed")
        }
        result => format!("ran {action} in window {}: {result}", ran.window_id),
    }
}

fn layout_text(layout: &LayoutSummary) -> String {
    let AppearanceSummary { chrome, density } = &layout.appearance;
    let mut out = format!(
        "window {}{}\npreset {}   chrome {chrome}   density {density}\nfocus {} {}{}\n",
        layout.window_id,
        layout
            .workspace_id
            .map(|w| format!("   workspace {w}"))
            .unwrap_or_default(),
        layout.preset,
        layout.focused.kind,
        layout.focused.target,
        layout
            .zoomed
            .as_ref()
            .map(|z| format!("   zoomed {z}"))
            .unwrap_or_default(),
    );
    out.push_str("\ndocks\n");
    for dock in &layout.docks {
        let panels: Vec<String> = dock
            .panels
            .iter()
            .map(|p| {
                if dock.active.as_deref() == Some(p.as_str()) {
                    format!("{p}*")
                } else {
                    p.clone()
                }
            })
            .collect();
        out.push_str(&format!(
            "  {:<7} {}  {}\n",
            dock.position,
            if dock.visible { "visible" } else { "hidden " },
            panels.join(" ")
        ));
    }
    out.push_str("\ncenter\n");
    pane_text(&layout.panes, 1, &mut out);
    out.trim_end().to_owned()
}

fn pane_text(pane: &PaneSummary, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    match pane {
        PaneSummary::Split { axis, children } => {
            out.push_str(&format!("{indent}split {axis}\n"));
            for child in children {
                pane_text(child, depth + 1, out);
            }
        }
        PaneSummary::Pane {
            pane_id,
            items,
            active,
        } => {
            out.push_str(&format!("{indent}{pane_id}\n"));
            for (i, item) in items.iter().enumerate() {
                let mark = if *active == u32::try_from(i).ok() {
                    "*"
                } else {
                    " "
                };
                let what = match (&item.session_id, &item.panel) {
                    (Some(session), _) => format!("{} {session}", item.kind),
                    (None, Some(panel)) => format!("{} {panel}", item.kind),
                    (None, None) => item.kind.clone(),
                };
                out.push_str(&format!("{indent}  {mark} {}  ({what})\n", item.title));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setting(kind: &str) -> UiSetting {
        UiSetting {
            namespace: "panels.sessions".into(),
            key: "group_by".into(),
            kind: kind.into(),
            doc: String::new(),
            layers: vec!["user".into()],
            values: vec![],
            current: None,
        }
    }

    #[test]
    fn a_dotted_name_resolves_through_the_catalog() {
        let settings = vec![setting("enum")];
        let found = find_setting(&settings, "panels.sessions.group_by").unwrap();
        assert_eq!(
            (found.namespace.as_str(), found.key.as_str()),
            ("panels.sessions", "group_by")
        );
        assert!(find_setting(&settings, "panels.sessions").is_err());
    }

    #[test]
    fn setting_values_follow_the_declared_type() {
        assert_eq!(
            parse_setting_value(&setting("bool"), "on").unwrap(),
            json!(true)
        );
        assert_eq!(
            parse_setting_value(&setting("integer"), "14").unwrap(),
            json!(14)
        );
        assert_eq!(
            parse_setting_value(&setting("number"), "13.5").unwrap(),
            json!(13.5)
        );
        assert_eq!(
            parse_setting_value(&setting("string"), "14").unwrap(),
            json!("14")
        );
        assert_eq!(
            parse_setting_value(&setting("string_list"), "a, b").unwrap(),
            json!(["a", "b"])
        );
        assert!(parse_setting_value(&setting("bool"), "maybe").is_err());
        assert!(parse_setting_value(&setting("unsupported"), "x").is_err());
    }

    #[test]
    fn args_are_json_unless_the_schema_says_string() {
        let schema = json!({"properties": {"name": {"type": "string"}, "n": {"type": "integer"}}});
        let args = parse_args(
            &["name=123".into(), "n=4".into(), "flag=true".into()],
            Some(&schema),
        )
        .unwrap();
        assert_eq!(args["name"], json!("123"));
        assert_eq!(args["n"], json!(4));
        assert_eq!(args["flag"], json!(true));
        assert!(parse_args(&["novalue".into()], None).is_err());
    }
}
