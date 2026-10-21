//! Turns the settings store's JSON schemas into editable field descriptions for the settings UI.
//! Pure data: no GPUI.

use std::collections::BTreeMap;
use std::rc::Rc;

use hutan_settings::themes;
use hutan_settings::{AppearanceSettings, Setting, SettingsStore};
use serde_json::Value as Json;

use crate::settings::{
    AgentsSettings, AppSettings, AssistantSettings, ChatSettings, EditorSettings,
    FilesPanelSettings, HistorySettings, InspectorPanelSettings, IntegrationsSettings,
    KeymapSettings, LauncherSettings, LayoutSettings, ResourcesSettings, SessionsPanelSettings,
    SidebarSettings, TasksSettings, TerminalSettings, WorkspacesSettings,
};
use crate::workbench::settings_labels::{field_search_text, namespace_title, page_keywords};

/// How a field is edited.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    Bool,
    /// One of these strings (a dropdown).
    Enum(Vec<String>),
    Integer,
    Number,
    String,
    StringList,
    /// A list whose items come from this set (add from a menu, reorder, remove).
    ChoiceList(Vec<String>),
    /// A map of text to text (`terminal.env`): rows of name and value.
    StringMap,
    /// Shown read-only with this hint (maps and other shapes the UI cannot edit).
    Unsupported(String),
}

/// One editable key of a namespace.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// The settings namespace the key belongs to (`terminal`); a page can show several. Empty
    /// until [`Field::in_namespace`] says where the field lives.
    pub namespace: &'static str,
    /// Key path inside the namespace (`["cursor", "style"]`).
    pub path: Vec<String>,
    pub doc: String,
    pub kind: FieldKind,
}

impl Field {
    /// `cursor.style`
    pub fn dotted(&self) -> String {
        self.path.join(".")
    }

    /// The same field, known to live in `namespace`.
    #[must_use]
    pub fn in_namespace(mut self, namespace: &'static str) -> Self {
        self.namespace = namespace;
        self
    }

    /// `terminal.cursor.style`: the key in the settings files.
    pub fn key_path(&self) -> String {
        format!("{}.{}", self.namespace, self.dotted())
    }
}

fn resolve<'a>(root: &'a Json, schema: &'a Json) -> &'a Json {
    let mut current = schema;
    // `$ref` chains (and the `allOf: [{ $ref }]` wrapper schemars uses to keep a doc comment).
    for _ in 0..8 {
        if let Some(target) = current
            .get("$ref")
            .and_then(Json::as_str)
            .and_then(|r| r.strip_prefix("#/"))
        {
            let mut node = root;
            for part in target.split('/') {
                node = match node.get(part) {
                    Some(next) => next,
                    None => return current,
                };
            }
            current = node;
        } else if let Some([only]) = current
            .get("allOf")
            .and_then(Json::as_array)
            .map(Vec::as_slice)
        {
            current = only;
        } else {
            break;
        }
    }
    current
}

fn doc_of(schema: &Json, resolved: &Json) -> String {
    schema
        .get("description")
        .or_else(|| resolved.get("description"))
        .and_then(Json::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// String constants of an enum schema (`enum: [...]`, or `oneOf: [{ const }]`).
fn enum_values(schema: &Json) -> Option<Vec<String>> {
    if let Some(values) = schema.get("enum").and_then(Json::as_array) {
        let out: Vec<String> = values
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        return (out.len() == values.len() && !out.is_empty()).then_some(out);
    }
    let variants = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Json::as_array)?;
    let mut out = Vec::new();
    for variant in variants {
        if let Some(c) = variant.get("const").and_then(Json::as_str) {
            out.push(c.to_owned());
        } else {
            let values = variant.get("enum").and_then(Json::as_array)?;
            out.extend(values.iter().filter_map(|v| v.as_str().map(str::to_owned)));
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The primary JSON type of a schema (`["string", "null"]` counts as `string`).
fn primary_type(schema: &Json) -> Option<&str> {
    match schema.get("type")? {
        Json::String(t) => Some(t.as_str()),
        Json::Array(types) => types.iter().filter_map(Json::as_str).find(|t| *t != "null"),
        _ => None,
    }
}

fn classify(root: &Json, schema: &Json) -> FieldKind {
    if let Some(values) = enum_values(schema) {
        return FieldKind::Enum(values);
    }
    match primary_type(schema) {
        Some("boolean") => FieldKind::Bool,
        Some("integer") => FieldKind::Integer,
        Some("number") => FieldKind::Number,
        Some("string") => FieldKind::String,
        Some("array") => {
            let items = schema.get("items").map(|i| resolve(root, i));
            match items.and_then(primary_type) {
                Some("string") => FieldKind::StringList,
                _ => FieldKind::Unsupported("edit settings.toml".to_owned()),
            }
        }
        _ => FieldKind::Unsupported("edit settings.toml".to_owned()),
    }
}

fn collect(root: &Json, schema: &Json, prefix: &mut Vec<String>, out: &mut Vec<Field>) {
    let resolved = resolve(root, schema);
    let Some(properties) = resolved.get("properties").and_then(Json::as_object) else {
        return;
    };
    for (key, property) in properties {
        let target = resolve(root, property);
        prefix.push(key.clone());
        let nested = target
            .get("properties")
            .and_then(Json::as_object)
            .is_some_and(|p| !p.is_empty());
        if nested {
            collect(root, property, prefix, out);
        } else {
            let kind = match target.get("additionalProperties") {
                Some(extra) if extra.is_object() => {
                    if primary_type(resolve(root, extra)) == Some("string") {
                        FieldKind::StringMap
                    } else {
                        FieldKind::Unsupported("edit settings.toml".to_owned())
                    }
                }
                _ => classify(root, target),
            };
            out.push(Field {
                namespace: "",
                path: prefix.clone(),
                doc: doc_of(property, target),
                kind,
            });
        }
        prefix.pop();
    }
}

/// Every editable field of a namespace's JSON schema, in declaration order.
pub fn fields_of(schema: &Json) -> Vec<Field> {
    let mut out = Vec::new();
    collect(schema, schema, &mut Vec::new(), &mut out);
    out
}

/// The row part ids of `panels.sessions.row`, with the line each shows on.
pub const SESSION_ROW_PARTS: [&str; 6] = ["state", "kind", "title", "folder", "placement", "age"];

/// The tab ids of `panels.inspector.tabs`.
pub const INSPECTOR_TABS: [&str; 3] = ["approvals", "changes", "info"];

/// `field` knowing what the app accepts: a theme, a row part or an inspector tab is picked
/// from the known ones. A layout preset depends on the presets saved, which the workbench
/// knows (`Workbench::sheet_kind`).
pub fn refine_kind(namespace: &str, key: &str, kind: &FieldKind) -> FieldKind {
    let own = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
    match (namespace, key, kind) {
        ("terminal", "theme", FieldKind::String) => {
            let mut values = own(&[themes::AUTO]);
            values.extend(themes::builtin_ids().iter().map(|id| (*id).to_owned()));
            FieldKind::Enum(values)
        }
        ("panels.sessions", "row", FieldKind::StringList) => {
            FieldKind::ChoiceList(own(&SESSION_ROW_PARTS))
        }
        ("panels.inspector", "tabs", FieldKind::StringList) => {
            FieldKind::ChoiceList(own(&INSPECTOR_TABS))
        }
        _ => kind.clone(),
    }
}

/// The range the app works within for a number: `(smallest, largest)`. A value outside it is
/// clamped when it is typed, so the file never holds one the app would silently ignore.
pub fn number_range(namespace: &str, key: &str) -> Option<(f64, f64)> {
    match (namespace, key) {
        ("terminal", "font_size") => Some((
            TerminalSettings::MIN_FONT_SIZE,
            TerminalSettings::MAX_FONT_SIZE,
        )),
        ("terminal", "line_height") => Some((
            TerminalSettings::MIN_LINE_HEIGHT,
            TerminalSettings::MAX_LINE_HEIGHT,
        )),
        ("resources", "interval_ms") => Some((
            f64::from(hutan_protocol::resources::MIN_INTERVAL_MS),
            f64::from(crate::workbench::panels::MAX_INTERVAL_MS),
        )),
        _ => None,
    }
}

/// The value at `path` inside `value`.
pub fn value_at<'a>(value: &'a Json, path: &[String]) -> Option<&'a Json> {
    path.iter().try_fold(value, |node, key| node.get(key))
}

/// The TOML value to write for `json` (strings, numbers, booleans and string lists).
pub fn json_to_toml(json: &Json) -> Option<toml::Value> {
    Some(match json {
        Json::Bool(b) => toml::Value::Boolean(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) => toml::Value::Integer(i),
            None => toml::Value::Float(n.as_f64()?),
        },
        Json::String(s) => toml::Value::String(s.clone()),
        Json::Array(items) => {
            toml::Value::Array(items.iter().map(json_to_toml).collect::<Option<Vec<_>>>()?)
        }
        _ => return None,
    })
}

/// A namespace the settings UI can show: its title and how to read its current and default
/// values as JSON. A namespace is a page of its own unless it names another `page` to share
/// (`sidebar` shows on the Projects Sidebar page).
#[derive(Clone)]
pub struct CatalogEntry {
    pub namespace: &'static str,
    /// The page that shows this namespace's fields (the namespace of the page's first entry).
    pub page: &'static str,
    pub workspace_overridable: bool,
    current: Rc<dyn Fn(&SettingsStore) -> Json>,
    pub default: Json,
}

impl std::fmt::Debug for CatalogEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogEntry")
            .field("namespace", &self.namespace)
            .field("page", &self.page)
            .finish_non_exhaustive()
    }
}

impl CatalogEntry {
    fn of<T: Setting>() -> Self {
        Self {
            namespace: T::NAMESPACE,
            page: T::NAMESPACE,
            workspace_overridable: T::WORKSPACE_OVERRIDABLE,
            current: Rc::new(|store| {
                serde_json::to_value(&*store.get::<T>()).unwrap_or(Json::Null)
            }),
            default: serde_json::to_value(T::default()).unwrap_or(Json::Null),
        }
    }

    /// A page the app draws itself: no settings namespace behind it.
    fn custom(namespace: &'static str) -> Self {
        Self {
            namespace,
            page: namespace,
            workspace_overridable: false,
            current: Rc::new(|_| Json::Null),
            default: Json::Null,
        }
    }

    /// The same entry, shown on the page of `page`.
    #[must_use]
    fn on_page(mut self, page: &'static str) -> Self {
        self.page = page;
        self
    }

    /// Whether this entry is a page of the sheet's list (not a section of another page).
    pub fn is_page(&self) -> bool {
        self.page == self.namespace
    }

    pub fn current(&self, store: &SettingsStore) -> Json {
        (self.current)(store)
    }
}

/// All namespaces of the app, in display order. A page shows its own namespace and every
/// later entry that names it as its `page`.
pub fn catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry::of::<AppearanceSettings>(),
        CatalogEntry::of::<AppSettings>(),
        CatalogEntry::custom("data"),
        CatalogEntry::of::<KeymapSettings>(),
        CatalogEntry::of::<TerminalSettings>(),
        CatalogEntry::of::<LayoutSettings>(),
        CatalogEntry::of::<SessionsPanelSettings>(),
        CatalogEntry::of::<SidebarSettings>().on_page(SessionsPanelSettings::NAMESPACE),
        CatalogEntry::of::<InspectorPanelSettings>(),
        CatalogEntry::of::<FilesPanelSettings>(),
        CatalogEntry::of::<WorkspacesSettings>(),
        CatalogEntry::custom("sessions_restore"),
        CatalogEntry::of::<HistorySettings>(),
        CatalogEntry::of::<ChatSettings>(),
        CatalogEntry::of::<EditorSettings>(),
        CatalogEntry::of::<AgentsSettings>(),
        CatalogEntry::custom("agents_tools"),
        CatalogEntry::of::<ResourcesSettings>(),
        CatalogEntry::of::<TasksSettings>(),
        CatalogEntry::custom("notifications"),
        CatalogEntry::of::<AssistantSettings>(),
        CatalogEntry::custom("control"),
        CatalogEntry::of::<IntegrationsSettings>(),
        CatalogEntry::of::<LauncherSettings>(),
        CatalogEntry::custom("accounts"),
        CatalogEntry::custom("models"),
    ]
}

/// The schema of `namespace` from `store.schema()`.
pub fn namespace_schema(schema: &Json, namespace: &str) -> Option<Json> {
    schema
        .get("namespaces")?
        .get(namespace)?
        .get("schema")
        .cloned()
}

/// The fields of every page of `entries`, by page: its own namespace's fields, then those of
/// the entries that share it, each knowing its namespace.
pub fn page_fields(entries: &[CatalogEntry], schema: &Json) -> BTreeMap<&'static str, Vec<Field>> {
    let mut pages: BTreeMap<&'static str, Vec<Field>> = BTreeMap::new();
    for entry in entries {
        let fields = namespace_schema(schema, entry.namespace)
            .map(|s| fields_of(&s))
            .unwrap_or_default()
            .into_iter()
            .map(|f| f.in_namespace(entry.namespace));
        pages.entry(entry.page).or_default().extend(fields);
    }
    pages
}

/// Pages matching `query` (case-insensitive substring of the namespace key or title, or of
/// any field's key, title, help or choice labels, or of the page's `keywords`: what its
/// custom controls are called), in catalog order. `keywords` gives the extra text of a page.
pub fn search<'a>(
    entries: &'a [CatalogEntry],
    pages: &BTreeMap<&'static str, Vec<Field>>,
    query: &str,
    keywords: &dyn Fn(&str) -> String,
) -> Vec<&'a CatalogEntry> {
    let query = query.trim().to_lowercase();
    entries
        .iter()
        .filter(|entry| entry.is_page())
        .filter(|entry| {
            query.is_empty()
                || entry.namespace.to_lowercase().contains(&query)
                || namespace_title(entry.namespace)
                    .to_lowercase()
                    .contains(&query)
                || page_keywords(entry.namespace)
                    .to_lowercase()
                    .contains(&query)
                || keywords(entry.namespace).to_lowercase().contains(&query)
                || pages.get(entry.namespace).is_some_and(|fields| {
                    fields.iter().any(|f| {
                        f.dotted().to_lowercase().contains(&query)
                            || field_search_text(f.namespace, &f.dotted(), &f.doc)
                                .to_lowercase()
                                .contains(&query)
                    })
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_settings::SettingsPaths;

    fn store() -> (tempfile::TempDir, SettingsStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(SettingsPaths::in_dir(dir.path()));
        crate::settings::register_all(&store);
        (dir, store)
    }

    fn fields(store: &SettingsStore, namespace: &str) -> Vec<Field> {
        namespace_schema(&store.schema(), namespace)
            .map(|s| fields_of(&s))
            .unwrap_or_default()
    }

    fn kind(fields: &[Field], path: &str) -> FieldKind {
        fields
            .iter()
            .find(|f| f.dotted() == path)
            .unwrap_or_else(|| panic!("no field {path}: {fields:?}"))
            .kind
            .clone()
    }

    #[test]
    fn terminal_fields_cover_every_widget_kind() {
        let (_dir, store) = store();
        let f = fields(&store, "terminal");
        assert_eq!(kind(&f, "font_family"), FieldKind::String);
        assert_eq!(kind(&f, "font_size"), FieldKind::Number);
        assert_eq!(kind(&f, "copy_on_select"), FieldKind::Bool);
        assert_eq!(
            kind(&f, "option_as_meta"),
            FieldKind::Enum(
                ["none", "left", "right", "both"]
                    .map(str::to_owned)
                    .to_vec()
            )
        );
        assert_eq!(
            kind(&f, "cursor.style"),
            FieldKind::Enum(["block", "bar", "underline"].map(str::to_owned).to_vec())
        );
        assert_eq!(kind(&f, "cursor.blink"), FieldKind::Bool);
        assert!(
            f.iter()
                .find(|f| f.dotted() == "font_size")
                .unwrap()
                .doc
                .contains("points"),
            "docs come from the field comments"
        );
    }

    #[test]
    fn lists_enums_and_maps_are_recognised() {
        let (_dir, store) = store();
        assert_eq!(
            kind(&fields(&store, "panels.sessions"), "row"),
            FieldKind::StringList
        );
        assert_eq!(
            kind(&fields(&store, "panels.sessions"), "group_by"),
            FieldKind::Enum(["workspace", "state", "none"].map(str::to_owned).to_vec())
        );
        assert!(matches!(
            kind(&fields(&store, "layout"), "presets"),
            FieldKind::Unsupported(_)
        ));
        assert_eq!(
            kind(&fields(&store, "terminal"), "env"),
            FieldKind::StringMap
        );
        assert_eq!(
            kind(&fields(&store, "app"), "on_last_window_close"),
            FieldKind::Enum(["quit", "menu_bar"].map(str::to_owned).to_vec())
        );
        assert_eq!(
            kind(&fields(&store, "app"), "keep_running"),
            FieldKind::Bool
        );
        assert_eq!(
            kind(&fields(&store, "appearance"), "density"),
            FieldKind::Enum(vec!["compact".into(), "comfortable".into()])
        );
        assert_eq!(
            kind(&fields(&store, "keymap"), "base"),
            FieldKind::Enum(vec!["zed".into(), "hutan".into()])
        );
    }

    #[test]
    fn catalog_values_and_toml_conversion() {
        let (_dir, store) = store();
        let entries = catalog();
        let terminal = entries.iter().find(|e| e.namespace == "terminal").unwrap();
        let current = terminal.current(&store);
        let size = value_at(&current, &["font_size".to_owned()]).unwrap();
        assert_eq!(json_to_toml(size), Some(toml::Value::Float(13.0)));
        assert_eq!(
            json_to_toml(&serde_json::json!(["a", "b"])),
            Some(toml::Value::Array(vec!["a".into(), "b".into()]))
        );
        assert!(terminal.workspace_overridable);
        assert_eq!(terminal.default, current);
    }

    #[test]
    fn search_matches_namespaces_and_field_docs() {
        let (_dir, store) = store();
        let entries = catalog();
        let pages = page_fields(&entries, &store.schema());
        let names = |q: &str| -> Vec<&'static str> {
            search(&entries, &pages, q, &|_| String::new())
                .into_iter()
                .map(|e| e.namespace)
                .collect()
        };
        assert_eq!(names("panels.").len(), 3);
        let mut font = names("font");
        font.sort_unstable();
        assert_eq!(font, ["editor", "terminal"], "code and terminal fonts");
        assert_eq!(names("density"), ["appearance"]);
        assert_eq!(names("on_last_window_close"), ["app"]);
        // What the sheet shows is what it finds: titles, help and choice labels.
        assert_eq!(names("Projects Sidebar").len(), 1);
        assert_eq!(names("Keep in the menu bar"), ["app"]);
        assert_eq!(names("Login Items"), ["app"]);
        assert!(names("zzzz").is_empty());
    }

    #[test]
    fn the_sidebar_namespace_is_a_section_of_the_projects_sidebar_page() {
        let (_dir, store) = store();
        let entries = catalog();
        let pages = page_fields(&entries, &store.schema());
        let sidebar = entries.iter().find(|e| e.namespace == "sidebar").unwrap();
        assert_eq!(sidebar.page, "panels.sessions");
        assert!(!sidebar.is_page());
        assert!(!pages.contains_key("sidebar"));
        let fields = &pages["panels.sessions"];
        let organize = fields.iter().find(|f| f.dotted() == "organize").unwrap();
        assert_eq!(organize.namespace, "sidebar");
        assert_eq!(organize.key_path(), "sidebar.organize");
        assert_eq!(
            organize.kind,
            FieldKind::Enum(vec!["workspaces".into(), "projects".into()])
        );
        let group_by = fields.iter().find(|f| f.dotted() == "group_by").unwrap();
        assert_eq!(group_by.namespace, "panels.sessions");
        // The page is found by what the section says.
        let found = |q: &str| {
            search(&entries, &pages, q, &|_| String::new())
                .into_iter()
                .map(|e| e.namespace)
                .collect::<Vec<_>>()
        };
        assert_eq!(found("organize"), ["panels.sessions"]);
    }

    #[test]
    fn known_sets_become_choices_and_numbers_have_a_range() {
        let theme = refine_kind("terminal", "theme", &FieldKind::String);
        assert_eq!(
            theme,
            FieldKind::Enum(vec![
                "auto".into(),
                "hutan-dark".into(),
                "hutan-light".into()
            ])
        );
        let row = refine_kind("panels.sessions", "row", &FieldKind::StringList);
        assert!(matches!(&row, FieldKind::ChoiceList(v) if v.len() == 6 && v[0] == "state"));
        let tabs = refine_kind("panels.inspector", "tabs", &FieldKind::StringList);
        assert_eq!(
            tabs,
            FieldKind::ChoiceList(vec!["approvals".into(), "changes".into(), "info".into()])
        );
        // Everything else stays as it is.
        assert_eq!(
            refine_kind("terminal", "font_family", &FieldKind::String),
            FieldKind::String
        );
        assert_eq!(number_range("terminal", "font_size"), Some((4.0, 72.0)));
        assert_eq!(number_range("terminal", "line_height"), Some((0.8, 3.0)));
        assert_eq!(
            number_range("resources", "interval_ms"),
            Some((1000.0, 60000.0))
        );
        assert_eq!(number_range("chat", "attach_max_bytes"), None);
    }
}
