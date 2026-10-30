//! Turns the settings store's JSON schemas into editable field descriptions for the settings UI.
//! Pure data: no GPUI.

use std::collections::BTreeMap;
use std::rc::Rc;

use hutan_settings::themes;
use hutan_settings::{AppearanceSettings, ExperimentalSettings, Setting, SettingsStore};
use serde_json::Value as Json;

use crate::settings::{
    AgentsSettings, AppSettings, AssistantSettings, ChatSettings, EditorSettings,
    FilesPanelSettings, HistorySettings, InspectorPanelSettings, IntegrationsSettings,
    KeymapSettings, LauncherSettings, LayoutSettings, NotificationsSettings, ResourcesSettings,
    SessionsPanelSettings, SidebarSettings, TasksSettings, TerminalSettings, WorkspacesSettings,
};
use crate::workbench::settings_labels::{
    field_search_text, namespace_title, page_keywords, page_title,
};

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
        ("sidebar", "nav_rows", FieldKind::StringList) => {
            FieldKind::ChoiceList(own(&crate::settings::SidebarSettings::NAV_ROWS))
        }
        // Mappings are keys written as in Vim (`<C-w>`, `<Esc>`): settings.toml holds them, the
        // Keyboard section lists them.
        ("editor", "vim_mappings.insert" | "vim_mappings.normal" | "vim_mappings.visual", _) => {
            FieldKind::Unsupported("edit settings.toml; listed in Keyboard".to_owned())
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
        ("assistant", "chat_width_rem") => Some((
            f64::from(crate::settings::AssistantSettings::MIN_CHAT_WIDTH_REM),
            f64::from(crate::settings::AssistantSettings::MAX_CHAT_WIDTH_REM),
        )),
        ("terminal", "quick_height") => Some((
            TerminalSettings::MIN_QUICK_HEIGHT,
            TerminalSettings::MAX_QUICK_HEIGHT,
        )),
        ("editor", "wrap_column") => Some((
            crate::settings::EditorSettings::MIN_WRAP_COLUMN as f64,
            crate::settings::EditorSettings::MAX_WRAP_COLUMN as f64,
        )),
        ("editor", "scroll_beyond_last_line") => Some((0.0, 1.0)),
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
/// values as JSON. A namespace is a section of its own unless it names another `section` to
/// share (`sidebar` shows in the Projects Sidebar section); [`PAGES`] says which page a
/// section is on.
#[derive(Clone)]
pub struct CatalogEntry {
    pub namespace: &'static str,
    /// The section that shows this namespace's fields: the namespace of the section's first
    /// entry. A section is what used to be a page of the sheet, and still the id that deep
    /// links, ⌘K and the panels' gears use.
    pub section: &'static str,
    /// The page the section is on ([`PAGES`]); empty for a section no page lists (a test
    /// fails for it).
    pub page: &'static str,
    pub workspace_overridable: bool,
    current: Rc<dyn Fn(&SettingsStore) -> Json>,
    pub default: Json,
}

impl std::fmt::Debug for CatalogEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogEntry")
            .field("namespace", &self.namespace)
            .field("section", &self.section)
            .field("page", &self.page)
            .finish_non_exhaustive()
    }
}

impl CatalogEntry {
    fn of<T: Setting>() -> Self {
        Self {
            namespace: T::NAMESPACE,
            section: T::NAMESPACE,
            page: "",
            workspace_overridable: T::WORKSPACE_OVERRIDABLE,
            current: Rc::new(|store| {
                serde_json::to_value(&*store.get::<T>()).unwrap_or(Json::Null)
            }),
            default: serde_json::to_value(T::default()).unwrap_or(Json::Null),
        }
    }

    /// A section the app draws itself: no settings namespace behind it.
    fn custom(namespace: &'static str) -> Self {
        Self {
            namespace,
            section: namespace,
            page: "",
            workspace_overridable: false,
            current: Rc::new(|_| Json::Null),
            default: Json::Null,
        }
    }

    /// The same entry, shown in the section of `section`.
    #[must_use]
    fn in_section(mut self, section: &'static str) -> Self {
        self.section = section;
        self
    }

    /// Whether this entry is a section of its own (not part of another entry's section).
    pub fn is_section(&self) -> bool {
        self.section == self.namespace
    }

    pub fn current(&self, store: &SettingsStore) -> Json {
        (self.current)(store)
    }
}

/// The groups of the sheet's page list, in order: the pages of a group are together in
/// [`PAGES`], so the groups are runs of the list and the arrow keys walk through them in the
/// order they are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavGroup {
    General,
    Agents,
    /// Projects, sessions, files and changes: the places the work happens in.
    Workspace,
    Data,
    /// Collapsed in the list until it holds the selected page or a search is on; it also
    /// holds the features that ship switched off.
    Advanced,
}

impl NavGroup {
    pub const ALL: [NavGroup; 5] = [
        Self::General,
        Self::Agents,
        Self::Workspace,
        Self::Data,
        Self::Advanced,
    ];

    /// The label over the group's pages.
    pub const fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Agents => "Agents",
            Self::Workspace => "Workspace",
            Self::Data => "Data",
            Self::Advanced => "Advanced",
        }
    }

    /// The group of `id`: a page, or a section or namespace of one.
    pub fn of(id: &str) -> Self {
        page_of(id).map_or(Self::General, |page| page.group)
    }
}

/// A page of the sheet's list: its sections are what used to be pages of their own (and are
/// still addressed by their old id: a deep link, ⌘K, a panel's gear, `hutan ui` open the
/// page scrolled to the section). A page's id is the id of its first section.
#[derive(Debug, Clone, Copy)]
pub struct NavPage {
    pub id: &'static str,
    pub group: NavGroup,
    /// The sections, in the order they are drawn.
    pub sections: &'static [&'static str],
}

/// The pages of the sheet in the order they are listed: the pages of a group are together
/// ([`NavGroup`]). Every section of the catalog is on exactly one of them.
pub const PAGES: &[NavPage] = &[
    NavPage {
        id: "appearance",
        group: NavGroup::General,
        sections: &["appearance", "layout", "tabs", "status"],
    },
    NavPage {
        id: "app",
        group: NavGroup::General,
        sections: &["app", "notifications", "sessions_restore", "updates"],
    },
    NavPage {
        id: "terminal",
        group: NavGroup::General,
        sections: &["terminal"],
    },
    NavPage {
        id: "editor",
        group: NavGroup::General,
        sections: &["editor", "keymap"],
    },
    NavPage {
        id: "assistant",
        group: NavGroup::Agents,
        sections: &["assistant"],
    },
    NavPage {
        id: "agents",
        group: NavGroup::Agents,
        sections: &[
            "agents",
            "chat",
            "launcher",
            "panels.inspector",
            "panels.context",
        ],
    },
    NavPage {
        id: "accounts",
        group: NavGroup::Agents,
        sections: &["accounts", "models"],
    },
    NavPage {
        id: "integrations",
        group: NavGroup::Agents,
        sections: &["integrations", "automation"],
    },
    NavPage {
        id: "panels.sessions",
        group: NavGroup::Workspace,
        sections: &["panels.sessions", "workspaces", "tasks"],
    },
    NavPage {
        id: "panels.files",
        group: NavGroup::Workspace,
        sections: &[
            "panels.files",
            "changes",
            "panels.search",
            "panels.problems",
            "panels.instructions",
        ],
    },
    NavPage {
        id: "history",
        group: NavGroup::Data,
        sections: &["history"],
    },
    NavPage {
        id: "data",
        group: NavGroup::Data,
        sections: &["data"],
    },
    NavPage {
        id: "agents_tools",
        group: NavGroup::Advanced,
        sections: &["agents_tools", "control", "resources"],
    },
    NavPage {
        id: "experimental",
        group: NavGroup::Advanced,
        sections: &["experimental"],
    },
];

/// The page that has `section` among its sections.
pub fn page_of_section(section: &str) -> Option<&'static NavPage> {
    PAGES.iter().find(|page| page.sections.contains(&section))
}

/// The page `id` names: a page id, or the id of a section on it (an old page id).
pub fn page_of(id: &str) -> Option<&'static NavPage> {
    PAGES
        .iter()
        .find(|page| page.id == id)
        .or_else(|| page_of_section(id))
}

/// All namespaces of the app: the order of the sections within a page is the order of
/// [`PAGES`]; this list only has to put an entry after the one whose section it joins. A
/// section shows its own namespace and every later entry that names it as its `section`.
pub fn catalog() -> Vec<CatalogEntry> {
    let mut entries = vec![
        CatalogEntry::of::<AppearanceSettings>(),
        CatalogEntry::of::<AppSettings>(),
        CatalogEntry::of::<KeymapSettings>(),
        CatalogEntry::of::<TerminalSettings>(),
        CatalogEntry::of::<EditorSettings>(),
        CatalogEntry::of::<ChatSettings>(),
        CatalogEntry::of::<LayoutSettings>(),
        CatalogEntry::of::<crate::settings::TabsSettings>(),
        CatalogEntry::of::<SessionsPanelSettings>(),
        CatalogEntry::of::<SidebarSettings>().in_section(SessionsPanelSettings::NAMESPACE),
        CatalogEntry::of::<InspectorPanelSettings>(),
        CatalogEntry::of::<FilesPanelSettings>(),
        CatalogEntry::of::<crate::settings::ProblemsPanelSettings>(),
        CatalogEntry::of::<crate::settings::InstructionsPanelSettings>(),
        CatalogEntry::of::<crate::settings::ContextPanelSettings>(),
        CatalogEntry::of::<crate::settings::SearchPanelSettings>(),
        CatalogEntry::of::<crate::settings::StatusSettings>(),
        CatalogEntry::of::<WorkspacesSettings>(),
        CatalogEntry::of::<TasksSettings>(),
        CatalogEntry::of::<crate::settings::ChangesSettings>(),
        CatalogEntry::of::<crate::settings::ChangesPanelSettings>()
            .in_section(crate::settings::ChangesSettings::NAMESPACE),
        CatalogEntry::of::<NotificationsSettings>(),
        CatalogEntry::of::<AssistantSettings>(),
        CatalogEntry::of::<AgentsSettings>(),
        CatalogEntry::of::<IntegrationsSettings>(),
        CatalogEntry::custom("automation"),
        CatalogEntry::of::<LauncherSettings>(),
        CatalogEntry::custom("accounts"),
        CatalogEntry::custom("models"),
        CatalogEntry::of::<HistorySettings>(),
        CatalogEntry::custom("data"),
        CatalogEntry::custom("sessions_restore"),
        CatalogEntry::custom("updates"),
        CatalogEntry::of::<ExperimentalSettings>(),
        CatalogEntry::custom("agents_tools"),
        CatalogEntry::custom("control"),
        CatalogEntry::of::<ResourcesSettings>(),
    ];
    for entry in &mut entries {
        entry.page = page_of_section(entry.section).map_or("", |page| page.id);
    }
    entries
}

/// The schema of `namespace` from `store.schema()`.
pub fn namespace_schema(schema: &Json, namespace: &str) -> Option<Json> {
    schema
        .get("namespaces")?
        .get(namespace)?
        .get("schema")
        .cloned()
}

/// The fields of every section of `entries`, by section: its own namespace's fields, then
/// those of the entries that share it, each knowing its namespace.
pub fn section_fields(
    entries: &[CatalogEntry],
    schema: &Json,
) -> BTreeMap<&'static str, Vec<Field>> {
    let mut sections: BTreeMap<&'static str, Vec<Field>> = BTreeMap::new();
    for entry in entries {
        let fields = namespace_schema(schema, entry.namespace)
            .map(|s| fields_of(&s))
            .unwrap_or_default()
            .into_iter()
            .map(|f| f.in_namespace(entry.namespace));
        sections.entry(entry.section).or_default().extend(fields);
    }
    sections
}

/// The fields of every page: those of its sections, in the order the page draws them.
pub fn page_fields(
    sections: &BTreeMap<&'static str, Vec<Field>>,
) -> BTreeMap<&'static str, Vec<Field>> {
    PAGES
        .iter()
        .map(|page| {
            let fields = page
                .sections
                .iter()
                .filter_map(|section| sections.get(section))
                .flatten()
                .cloned()
                .collect();
            (page.id, fields)
        })
        .collect()
}

/// What a search is asked: the words to find, and the two filters of the rows (`@modified`
/// and `@restart`, typed in the box or switched on by the chips under it).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Filters {
    /// Only the settings a file sets: what was changed from the default (`@modified`).
    pub modified: bool,
    /// Only the settings that apply after the daemon restarts (`@restart`).
    pub restart: bool,
}

impl Filters {
    pub const MODIFIED: &'static str = "@modified";
    pub const RESTART: &'static str = "@restart";

    /// Whether a filter is on.
    pub fn any(self) -> bool {
        self.modified || self.restart
    }

    /// Whether a row passes: with both filters on it is either (FX-UI-B; they used to be
    /// ANDed, which left almost nothing), with one it is that one, with none it is any.
    /// `modified`: a settings file sets it; `restart`: it applies after the daemon restarts.
    pub fn passes(self, modified: bool, restart: bool) -> bool {
        (!self.any()) || (self.modified && modified) || (self.restart && restart)
    }
}

/// The words and filters of a search box: `@modified` and `@restart` (any case, anywhere in
/// the text) are filters, the rest is what to find.
pub fn parse_query(query: &str) -> (String, Filters) {
    let mut filters = Filters::default();
    let mut words = Vec::new();
    for word in query.split_whitespace() {
        match word.to_lowercase().as_str() {
            Filters::MODIFIED => filters.modified = true,
            Filters::RESTART => filters.restart = true,
            _ => words.push(word),
        }
    }
    (words.join(" "), filters)
}

/// `query` with `token` (a filter) switched on or off, the other words kept.
pub fn toggle_filter(query: &str, token: &str) -> String {
    let mut words: Vec<&str> = query.split_whitespace().collect();
    let before = words.len();
    words.retain(|w| !w.eq_ignore_ascii_case(token));
    if words.len() == before {
        words.push(token);
    }
    words.join(" ")
}

/// Sections matching `text` (case-insensitive substring of the namespace key or title, or of
/// any field's key, title, help or choice labels, or of the section's `keywords`: what its
/// custom controls are called), in catalog order. `keywords` gives the extra text of a
/// section; `shown` says whether a section has anything to show under the filters.
pub fn search_sections(
    entries: &[CatalogEntry],
    sections: &BTreeMap<&'static str, Vec<Field>>,
    text: &str,
    keywords: &dyn Fn(&str) -> String,
    shown: &dyn Fn(&str) -> bool,
) -> Vec<&'static str> {
    let text = text.trim().to_lowercase();
    entries
        .iter()
        .filter(|entry| entry.is_section())
        .filter(|entry| shown(entry.namespace))
        .filter(|entry| {
            text.is_empty()
                || entry.namespace.to_lowercase().contains(&text)
                || namespace_title(entry.namespace)
                    .to_lowercase()
                    .contains(&text)
                || page_keywords(entry.namespace)
                    .to_lowercase()
                    .contains(&text)
                || keywords(entry.namespace).to_lowercase().contains(&text)
                || sections.get(entry.namespace).is_some_and(|fields| {
                    fields.iter().any(|f| {
                        f.dotted().to_lowercase().contains(&text)
                            || field_search_text(f.namespace, &f.dotted(), &f.doc)
                                .to_lowercase()
                                .contains(&text)
                    })
                })
        })
        .map(|entry| entry.namespace)
        .collect()
}

/// The pages with a section [`search_sections`] finds, or whose own title the text is in, in
/// the order of the list, and for each the first section that matched (what the page opens
/// at; the first one that has something to show when only the title matched).
pub fn search(
    entries: &[CatalogEntry],
    sections: &BTreeMap<&'static str, Vec<Field>>,
    text: &str,
    keywords: &dyn Fn(&str) -> String,
    shown: &dyn Fn(&str) -> bool,
) -> Vec<(&'static NavPage, &'static str)> {
    let found = search_sections(entries, sections, text, keywords, shown);
    let text = text.trim().to_lowercase();
    PAGES
        .iter()
        .filter_map(|page| {
            let first = page
                .sections
                .iter()
                .find(|section| found.contains(section))
                .or_else(|| {
                    (!text.is_empty() && page_title(page.id).to_lowercase().contains(&text))
                        .then(|| page.sections.iter().find(|section| shown(section)))
                        .flatten()
                })?;
            Some((page, *first))
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
    fn the_pages_of_a_group_are_together_in_the_list_and_the_groups_are_in_order() {
        // The groups of the page list are runs of PAGES, in the groups' order, and every group
        // has a page: the arrow keys and the drawn list agree.
        let mut seen: Vec<NavGroup> = Vec::new();
        for page in PAGES {
            if seen.last() != Some(&page.group) {
                assert!(
                    !seen.contains(&page.group),
                    "{} comes after another group's page",
                    page.id
                );
                seen.push(page.group);
            }
        }
        assert_eq!(seen, NavGroup::ALL);
        assert_eq!(
            seen.iter().map(|g| g.title()).collect::<Vec<_>>(),
            ["General", "Agents", "Workspace", "Data", "Advanced"]
        );
        assert_eq!(PAGES[0].id, "appearance", "the sheet opens on it");
        assert_eq!(PAGES.len(), 14);
    }

    #[test]
    fn every_section_is_on_exactly_one_page_and_a_page_is_named_like_its_first_section() {
        let entries = catalog();
        let mut listed: Vec<&str> = PAGES
            .iter()
            .flat_map(|p| p.sections.iter().copied())
            .collect();
        for page in PAGES {
            assert_eq!(
                page.sections[0], page.id,
                "{} opens at its first section",
                page.id
            );
        }
        // Each section is a section of the catalog, listed once.
        for section in &listed {
            let entry = entries.iter().find(|e| e.namespace == *section);
            assert!(
                entry.is_some_and(CatalogEntry::is_section),
                "{section} is not a section of the catalog"
            );
        }
        let count = listed.len();
        listed.sort_unstable();
        listed.dedup();
        assert_eq!(listed.len(), count, "a section is on two pages");
        // Every entry of the catalog is on a page, through its section: a new namespace is
        // added to the `sections` of one of PAGES.
        for entry in &entries {
            assert!(
                !entry.page.is_empty(),
                "`{}` (section `{}`) is on no page: add its section to a page of PAGES",
                entry.namespace,
                entry.section
            );
            assert!(listed.contains(&entry.section), "{}", entry.namespace);
        }
    }

    /// Every old page of the sheet and where it is now: the table of `docs/design.md`
    /// § Settings sheet. An old id is still a deep link.
    const OLD_PAGES: &[(&str, &str)] = &[
        ("appearance", "appearance"),
        ("app", "app"),
        ("keymap", "editor"),
        ("terminal", "terminal"),
        ("editor", "editor"),
        ("chat", "agents"),
        ("layout", "appearance"),
        ("panels.sessions", "panels.sessions"),
        ("sidebar", "panels.sessions"),
        ("panels.inspector", "agents"),
        ("panels.files", "panels.files"),
        ("panels.problems", "panels.files"),
        ("panels.instructions", "panels.files"),
        ("panels.search", "panels.files"),
        ("status", "appearance"),
        ("workspaces", "panels.sessions"),
        ("tasks", "panels.sessions"),
        ("changes", "panels.files"),
        ("panels.changes", "panels.files"),
        ("notifications", "app"),
        ("assistant", "assistant"),
        ("agents", "agents"),
        ("integrations", "integrations"),
        ("automation", "integrations"),
        ("launcher", "agents"),
        ("accounts", "accounts"),
        ("models", "accounts"),
        ("history", "history"),
        ("data", "data"),
        ("sessions_restore", "app"),
        ("updates", "app"),
        ("agents_tools", "agents_tools"),
        ("control", "agents_tools"),
        ("resources", "agents_tools"),
        ("experimental", "experimental"),
    ];

    #[test]
    fn every_old_page_id_names_a_section_of_a_page() {
        let entries = catalog();
        for (old, page) in OLD_PAGES {
            let entry = entries
                .iter()
                .find(|e| e.namespace == *old)
                .unwrap_or_else(|| panic!("{old} is not in the catalog"));
            assert_eq!(entry.page, *page, "{old}");
            assert_eq!(page_of(entry.section).map(|p| p.id), Some(*page), "{old}");
        }
        assert_eq!(NavGroup::of("models"), NavGroup::Agents);
        assert_eq!(NavGroup::of("control"), NavGroup::Advanced);
    }

    #[test]
    fn two_sections_of_a_page_never_hold_a_field_of_the_same_key() {
        let (_dir, store) = store();
        let entries = catalog();
        let sections = section_fields(&entries, &store.schema());
        for page in PAGES {
            let mut seen: BTreeMap<String, &str> = BTreeMap::new();
            for section in page.sections {
                for f in sections.get(section).into_iter().flatten() {
                    if let Some(other) = seen.insert(f.dotted(), section) {
                        panic!(
                            "`{}` is a field of `{other}` and of `{section}` on page {}: the \
                             sheet addresses a row by its key inside the namespace, so a page \
                             cannot show both; put one of them on another page",
                            f.dotted(),
                            page.id
                        );
                    }
                }
            }
        }
    }

    fn found(
        entries: &[CatalogEntry],
        sections: &BTreeMap<&'static str, Vec<Field>>,
        query: &str,
    ) -> Vec<&'static str> {
        let (text, _) = parse_query(query);
        search(entries, sections, &text, &|_| String::new(), &|_| true)
            .into_iter()
            .map(|(page, _)| page.id)
            .collect()
    }

    #[test]
    fn search_matches_sections_and_field_docs_and_lists_their_pages() {
        let (_dir, store) = store();
        let entries = catalog();
        let sections = section_fields(&entries, &store.schema());
        let names = |q: &str| found(&entries, &sections, q);
        // sessions, inspector, files, problems, instructions, search, changes (panel): four
        // sections on the Files page, one on Agents, one on Projects.
        assert_eq!(
            names("panels."),
            ["agents", "panels.sessions", "panels.files"]
        );
        // Code and terminal fonts: two sections on two pages.
        assert_eq!(names("font"), ["terminal", "editor"]);
        assert_eq!(names("density"), ["appearance"]);
        assert_eq!(names("on_last_window_close"), ["app"]);
        // What the sheet shows is what it finds: titles, help and choice labels.
        assert_eq!(names("Projects Sidebar"), ["panels.sessions"]);
        assert_eq!(names("Keep in the menu bar"), ["app"]);
        assert_eq!(names("Login Items"), ["app"]);
        // The title of a page finds it though no section is called so.
        assert_eq!(names("Projects & sessions"), ["panels.sessions"]);
        assert!(names("zzzz").is_empty());
        // Every key is found, on the page of its section.
        for entry in &entries {
            let Some(namespace) = namespace_schema(&store.schema(), entry.namespace) else {
                continue;
            };
            for f in fields_of(&namespace) {
                let key = f.dotted();
                assert!(
                    names(&key).contains(&entry.page),
                    "`{}.{key}` is not found on {}",
                    entry.namespace,
                    entry.page
                );
            }
        }
    }

    #[test]
    fn a_search_opens_a_page_at_the_section_that_matched() {
        let (_dir, store) = store();
        let entries = catalog();
        let sections = section_fields(&entries, &store.schema());
        let hits = |q: &str| {
            search(&entries, &sections, q, &|_| String::new(), &|_| true)
                .into_iter()
                .map(|(page, section)| (page.id, section))
                .collect::<Vec<_>>()
        };
        // Notifications is the section of General that says "Dock badge".
        assert_eq!(hits("badge")[0], ("app", "notifications"));
        assert_eq!(hits("on_last_window_close"), [("app", "app")]);
        assert_eq!(hits("Zed keys"), [("editor", "keymap")]);
        // The page that no section but its title matches opens at its first section.
        assert_eq!(hits("Editing & keys"), [("editor", "editor")]);
    }

    #[test]
    fn the_filters_are_words_of_the_search_box_and_the_chips_write_them() {
        let (text, filters) = parse_query("font @Modified  size @restart");
        assert_eq!(text, "font size");
        assert_eq!(
            filters,
            Filters {
                modified: true,
                restart: true
            }
        );
        assert_eq!(
            parse_query("@modified"),
            (
                String::new(),
                Filters {
                    modified: true,
                    restart: false
                }
            )
        );
        assert!(!parse_query("font").1.any());
        assert_eq!(toggle_filter("font", Filters::MODIFIED), "font @modified");
        assert_eq!(toggle_filter("font @modified", Filters::MODIFIED), "font");
        assert_eq!(toggle_filter("", Filters::RESTART), "@restart");
        assert_eq!(toggle_filter("@MODIFIED font", Filters::MODIFIED), "font");
    }

    #[test]
    fn a_section_without_rows_under_the_filters_leaves_its_page_out() {
        let (_dir, store) = store();
        let entries = catalog();
        let sections = section_fields(&entries, &store.schema());
        let only = |wanted: &'static str| move |section: &str| section == wanted;
        let pages: Vec<_> = search(
            &entries,
            &sections,
            "",
            &|_| String::new(),
            &only("terminal"),
        )
        .into_iter()
        .map(|(page, section)| (page.id, section))
        .collect();
        assert_eq!(pages, [("terminal", "terminal")]);
    }

    #[test]
    fn the_sidebar_namespace_is_part_of_the_projects_sidebar_section() {
        let (_dir, store) = store();
        let entries = catalog();
        let sections = section_fields(&entries, &store.schema());
        let pages = page_fields(&sections);
        let sidebar = entries.iter().find(|e| e.namespace == "sidebar").unwrap();
        assert_eq!(sidebar.section, "panels.sessions");
        assert_eq!(sidebar.page, "panels.sessions");
        assert!(!sidebar.is_section());
        assert!(!sections.contains_key("sidebar"));
        assert!(!pages.contains_key("sidebar"));
        let fields = &sections["panels.sessions"];
        let organize = fields.iter().find(|f| f.dotted() == "organize").unwrap();
        assert_eq!(organize.namespace, "sidebar");
        assert_eq!(organize.key_path(), "sidebar.organize");
        assert_eq!(
            organize.kind,
            FieldKind::Enum(vec!["workspaces".into(), "projects".into()])
        );
        let group_by = fields.iter().find(|f| f.dotted() == "group_by").unwrap();
        assert_eq!(group_by.namespace, "panels.sessions");
        // The page holds the section's fields, then those of the sections after it.
        assert!(pages["panels.sessions"].len() > fields.len());
        assert_eq!(found(&entries, &sections, "organize"), ["panels.sessions"]);
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
