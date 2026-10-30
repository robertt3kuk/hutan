//! The models of the harnesses as a person reads them: their display names, kept between runs
//! so a chat reads "Claude Haiku 5.5" after a restart without asking the harness again, and the
//! order, labels and current mark of the model pickers (Start sheet, Switch model).
//!
//! The names come from the harness's catalog (`chat.models`, which the daemon caches); every
//! answer the desktop gets is remembered in [`ModelNames`] and saved next to the layout
//! ([`MODEL_NAMES_FILE`]). Nothing here asks the daemon or spawns anything. GPUI-free.

use std::cmp::{Ordering, Reverse};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use hutan_core::agent::ModelInfo;
use serde::{Deserialize, Serialize};

/// The file of the remembered model names, next to the layout's.
pub const MODEL_NAMES_FILE: &str = "model-names.json";
/// Harnesses whose names are kept at most.
const MAX_AGENTS: usize = 16;
/// Models kept per harness at most.
const MAX_MODELS: usize = 1024;
/// Ids and names longer than this (in bytes) are not kept.
const MAX_TEXT: usize = 256;

/// The display names of the harnesses' models, by harness (`omp`, `claude`, `codex`) and id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelNames {
    #[serde(default)]
    agents: BTreeMap<String, BTreeMap<String, String>>,
}

impl ModelNames {
    /// The names saved at `path`; none when the file is missing, unreadable or corrupt. A file
    /// edited by hand is held to the same bounds as one this desktop wrote.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(saved) = serde_json::from_str::<Self>(&text) else {
            return Self::default();
        };
        let mut names = Self::default();
        for (agent, models) in saved.agents {
            if names.agents.len() >= MAX_AGENTS || !fits(&agent) {
                continue;
            }
            let kept: BTreeMap<String, String> = models
                .into_iter()
                .filter(|(id, name)| fits(id) && fits(name) && !name.trim().is_empty())
                .take(MAX_MODELS)
                .collect();
            names.agents.insert(agent, kept);
        }
        names
    }

    /// The names as saved.
    pub fn to_json(&self) -> String {
        // A map of strings always serializes.
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Takes `models` as the catalog of `agent`: their names replace what was known, and a
    /// model the catalog no longer lists keeps its name while there is room (an older chat may
    /// still run it). Whether anything changed.
    pub fn learn(&mut self, agent: &str, models: &[ModelInfo]) -> bool {
        if !fits(agent) || (!self.agents.contains_key(agent) && self.agents.len() >= MAX_AGENTS) {
            return false;
        }
        let mut next: BTreeMap<String, String> = models
            .iter()
            .filter(|m| fits(&m.id) && fits(&m.display_name) && !m.display_name.trim().is_empty())
            .take(MAX_MODELS)
            .map(|m| (m.id.clone(), m.display_name.trim().to_owned()))
            .collect();
        if let Some(known) = self.agents.get(agent) {
            for (id, name) in known {
                if next.len() >= MAX_MODELS {
                    break;
                }
                next.entry(id.clone()).or_insert_with(|| name.clone());
            }
        }
        if self.agents.get(agent) == Some(&next) {
            return false;
        }
        self.agents.insert(agent.to_owned(), next);
        true
    }

    /// The display name of `id` in the catalog of `agent`. A model reported by its bare id
    /// (`claude-haiku-5-5` for OMP's `anthropic/claude-haiku-5-5`) is found when exactly one
    /// catalog entry ends with it.
    pub fn name(&self, agent: &str, id: &str) -> Option<&str> {
        let models = self.agents.get(agent)?;
        if let Some(name) = models.get(id) {
            return Some(name);
        }
        let tail = tail_of(id);
        if tail.is_empty() {
            return None;
        }
        let mut found = models.iter().filter(|(known, _)| tail_of(known) == tail);
        let (_, name) = found.next()?;
        found.next().is_none().then_some(name.as_str())
    }

    /// The model as a person reads it: its display name, else the id as reported.
    pub fn label(&self, agent: &str, id: &str) -> String {
        self.name(agent, id).unwrap_or(id).to_owned()
    }
}

/// Writes `json` to `path` atomically (a crash never leaves a torn file).
pub fn save(path: &Path, json: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(json.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)
}

fn fits(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_TEXT
}

/// The last segment of a model id: `anthropic/claude-haiku-5-5` → `claude-haiku-5-5`.
fn tail_of(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id)
}

/// Whether `a` and `b` name the same model: equal, or one is the other without its provider.
pub fn same_model(a: &str, b: &str) -> bool {
    a == b || ((!a.contains('/') || !b.contains('/')) && tail_of(a) == tail_of(b))
}

/// The group of a model in OMP's pickers: its provider (the id is `provider/model`). Other
/// harnesses' models are not grouped.
pub fn model_group(agent: &str, model: &ModelInfo) -> Option<String> {
    (agent == "omp")
        .then(|| {
            model
                .id
                .split_once('/')
                .map(|(provider, _)| provider.to_owned())
        })
        .flatten()
}

/// What the order of a picker reads from a model id.
#[derive(Debug, PartialEq, Eq)]
struct Shape {
    /// Opus, Sonnet, Haiku, then the rest.
    family: u8,
    /// The words of the id (`gpt-mini`), which order the rest among themselves.
    words: String,
    version: Vec<u32>,
    /// `20251001` of a dated id.
    date: Option<String>,
}

fn shape(id: &str) -> Shape {
    let tail = tail_of(id).to_lowercase();
    let mut words: Vec<&str> = Vec::new();
    let mut version: Vec<u32> = Vec::new();
    let mut date = None;
    let tokens: Vec<&str> = tail
        .split(['-', '_', '@'])
        .filter(|t| !t.is_empty())
        .collect();
    let digits = |t: &str, len: usize| t.len() == len && t.bytes().all(|b| b.is_ascii_digit());
    let mut at = 0;
    while at < tokens.len() {
        let token = tokens[at];
        at += 1;
        // `20251001`, or `2025-08-07` as OpenAI writes it.
        if digits(token, 8) && token.starts_with("20") {
            date = Some(token.to_owned());
            continue;
        }
        if digits(token, 4)
            && token.starts_with("20")
            && tokens.get(at).is_some_and(|t| digits(t, 2))
            && tokens.get(at + 1).is_some_and(|t| digits(t, 2))
        {
            date = Some(format!("{token}{}{}", tokens[at], tokens[at + 1]));
            at += 2;
            continue;
        }
        let parts: Vec<&str> = token.split('.').collect();
        let numbers: Option<Vec<u32>> = parts
            .iter()
            .map(|p| {
                (!p.is_empty() && p.len() <= 4 && p.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| p.parse().ok())
                    .flatten()
            })
            .collect();
        match numbers {
            Some(numbers) => version.extend(numbers),
            None => words.push(token),
        }
    }
    let family = if words.contains(&"opus") {
        0
    } else if words.contains(&"sonnet") {
        1
    } else if words.contains(&"haiku") {
        2
    } else {
        3
    };
    let words = if family == 3 {
        words.join("-")
    } else {
        String::new()
    };
    Shape {
        family,
        words,
        version,
        date,
    }
}

/// The order of two models of one provider: by family (Opus, Sonnet, Haiku, the rest by
/// name), the default first in its family, then the newest version, an alias before its dated
/// ids (and newer dates first), then the id.
fn picker_order(a: &ModelInfo, b: &ModelInfo) -> Ordering {
    let (sa, sb) = (shape(&a.id), shape(&b.id));
    sa.family
        .cmp(&sb.family)
        .then_with(|| sa.words.cmp(&sb.words))
        .then_with(|| b.is_default.cmp(&a.is_default))
        .then_with(|| Reverse(&sa.version).cmp(&Reverse(&sb.version)))
        .then_with(|| sa.date.is_some().cmp(&sb.date.is_some()))
        .then_with(|| Reverse(&sa.date).cmp(&Reverse(&sb.date)))
        .then_with(|| a.id.cmp(&b.id))
}

/// One row of a model picker.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRow<'a> {
    pub model: &'a ModelInfo,
    /// The provider section the row is in (OMP), `None` when the picker is not grouped.
    pub group: Option<String>,
    /// What tells the row apart from another of the same display name: `(20251001)` for a
    /// dated id, shown muted after the name (the catalog's name is untouched).
    pub variant: Option<String>,
    /// The model in use: the chat's, or the one the Start sheet would start with.
    pub current: bool,
}

/// The rows of a model picker of `agent`: the models that are not hidden and match `query`
/// (name or id, any case). `ordered` sorts them (grouped by provider, then [`picker_order`]);
/// a provider's own list (a direct chat) keeps its order. `current` is marked.
pub fn picker_rows<'a>(
    agent: &str,
    models: &'a [ModelInfo],
    query: &str,
    current: Option<&str>,
    ordered: bool,
) -> Vec<ModelRow<'a>> {
    let visible: Vec<&ModelInfo> = models.iter().filter(|m| !m.hidden).collect();
    let query = query.trim().to_lowercase();
    let mut shown: Vec<&ModelInfo> = visible
        .iter()
        .copied()
        .filter(|m| {
            query.is_empty()
                || m.id.to_lowercase().contains(&query)
                || m.display_name.to_lowercase().contains(&query)
        })
        .collect();
    if ordered {
        shown.sort_by(|a, b| {
            model_group(agent, a)
                .cmp(&model_group(agent, b))
                .then_with(|| picker_order(a, b))
        });
    }
    let current = current.filter(|c| !c.is_empty());
    // The model in use, by its exact id first (a bare id only when nothing else matches).
    let exact = current.is_some_and(|c| shown.iter().any(|m| m.id == c));
    shown
        .into_iter()
        .map(|model| ModelRow {
            model,
            group: if ordered {
                model_group(agent, model)
            } else {
                None
            },
            variant: variant(model, &visible),
            current: current.is_some_and(|c| {
                if exact {
                    model.id == c
                } else {
                    same_model(&model.id, c)
                }
            }),
        })
        .collect()
}

/// The position of row `at` among the children of a list that draws a header before each
/// group (what a scroll to that row needs).
pub fn row_child_index(rows: &[ModelRow<'_>], at: usize) -> usize {
    let mut child = 0;
    let mut group: Option<&Option<String>> = None;
    for (index, row) in rows.iter().enumerate() {
        if row.group.is_some() && group != Some(&row.group) {
            child += 1;
        }
        group = Some(&row.group);
        if index == at {
            break;
        }
        child += 1;
    }
    child
}

/// `(20251001)` for a dated model whose display name another visible model has too; an
/// undated twin of another undated one is told apart by its id.
fn variant(model: &ModelInfo, visible: &[&ModelInfo]) -> Option<String> {
    let name = model.display_name.trim();
    let twins: Vec<&&ModelInfo> = visible
        .iter()
        .filter(|m| m.id != model.id && m.display_name.trim().eq_ignore_ascii_case(name))
        .collect();
    if twins.is_empty() {
        return None;
    }
    match shape(&model.id).date {
        Some(date) => Some(format!("({date})")),
        // The alias of dated twins reads plainly; two undated twins show their ids.
        None if twins.iter().all(|t| shape(&t.id).date.is_some()) => None,
        None => Some(format!("({})", tail_of(&model.id))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, name: &str, default: bool) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            display_name: name.into(),
            description: String::new(),
            is_default: default,
            hidden: false,
            default_reasoning_effort: None,
            reasoning_efforts: Vec::new(),
        }
    }

    /// Part of OMP's catalog as `omp models` lists it (2026-10), shuffled.
    fn catalog() -> Vec<ModelInfo> {
        vec![
            model("apple/foundation", "Apple Foundation", false),
            model(
                "anthropic/claude-haiku-4-5-20251001",
                "Claude Haiku 4.5",
                false,
            ),
            model("anthropic/claude-sonnet-4-5", "Claude Sonnet 4.5", false),
            model(
                "anthropic/claude-opus-4-5-20251101",
                "Claude Opus 4.5",
                false,
            ),
            model("anthropic/claude-haiku-5-5", "Claude Haiku 5.5", false),
            model("anthropic/claude-opus-4-6", "Claude Opus 4.6", false),
            model(
                "anthropic/claude-3-5-sonnet-20241022",
                "Claude Sonnet 3.5",
                false,
            ),
            model("anthropic/claude-opus-5-5", "Claude Opus 5.5", true),
            model("anthropic/claude-haiku-4-5", "Claude Haiku 4.5", false),
            model("anthropic/claude-sonnet-5-5", "Claude Sonnet 5.5", false),
            model("anthropic/claude-opus-5-6", "Claude Opus 5.6", false),
            model(
                "anthropic/claude-sonnet-4-5-20250929",
                "Claude Sonnet 4.5",
                false,
            ),
            model("anthropic/claude-opus-4-5", "Claude Opus 4.5", false),
            model("anthropic/gpt-oss", "GPT OSS", false),
        ]
    }

    #[test]
    fn a_row_scrolls_to_its_place_among_the_headers_and_rows() {
        let models = catalog();
        let rows = picker_rows("omp", &models, "", None, true);
        // The first provider's header comes before its first row; each next group adds one.
        assert_eq!(row_child_index(&rows, 0), 1);
        assert_eq!(row_child_index(&rows, 1), 2);
        let second = rows
            .iter()
            .position(|r| r.group != rows[0].group)
            .expect("a second provider");
        assert_eq!(row_child_index(&rows, second), second + 2);
        // A provider's own list (not grouped) has no headers.
        let flat = picker_rows("direct", &models, "", None, false);
        assert_eq!(row_child_index(&flat, 3), 3);
    }

    #[test]
    fn the_order_is_by_provider_family_and_newest_with_the_default_first_in_its_family() {
        let models = catalog();
        let rows = picker_rows("omp", &models, "", None, true);
        let ids: Vec<&str> = rows.iter().map(|r| r.model.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                // Opus: the default first, then newest first, the alias before its dated id.
                "anthropic/claude-opus-5-5",
                "anthropic/claude-opus-5-6",
                "anthropic/claude-opus-4-6",
                "anthropic/claude-opus-4-5",
                "anthropic/claude-opus-4-5-20251101",
                // Sonnet
                "anthropic/claude-sonnet-5-5",
                "anthropic/claude-sonnet-4-5",
                "anthropic/claude-sonnet-4-5-20250929",
                "anthropic/claude-3-5-sonnet-20241022",
                // Haiku
                "anthropic/claude-haiku-5-5",
                "anthropic/claude-haiku-4-5",
                "anthropic/claude-haiku-4-5-20251001",
                // The rest
                "anthropic/gpt-oss",
                // The next provider
                "apple/foundation",
            ]
        );
        assert_eq!(rows[0].group.as_deref(), Some("anthropic"));
        assert_eq!(rows.last().unwrap().group.as_deref(), Some("apple"));
        // Claude Code and Codex lists are not grouped.
        assert!(
            picker_rows("claude", &models, "", None, true)
                .iter()
                .all(|r| r.group.is_none())
        );
    }

    #[test]
    fn a_provider_list_keeps_its_order() {
        let models = catalog();
        let ids: Vec<&str> = picker_rows("direct", &models, "", None, false)
            .iter()
            .map(|r| r.model.id.as_str())
            .collect();
        let given: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, given);
    }

    #[test]
    fn duplicate_names_show_the_date_of_the_dated_variant_and_keep_the_name() {
        let models = catalog();
        let rows = picker_rows("omp", &models, "", None, true);
        let variant = |id: &str| {
            rows.iter()
                .find(|r| r.model.id == id)
                .unwrap()
                .variant
                .clone()
        };
        assert_eq!(
            variant("anthropic/claude-haiku-4-5-20251001").as_deref(),
            Some("(20251001)")
        );
        assert_eq!(variant("anthropic/claude-haiku-4-5"), None);
        assert_eq!(
            variant("anthropic/claude-opus-4-5-20251101").as_deref(),
            Some("(20251101)")
        );
        assert_eq!(
            variant("anthropic/claude-sonnet-4-5-20250929").as_deref(),
            Some("(20250929)")
        );
        // A dated id whose name nothing else has needs nothing.
        assert_eq!(variant("anthropic/claude-3-5-sonnet-20241022"), None);
        let dated = rows
            .iter()
            .find(|r| r.model.id == "anthropic/claude-haiku-4-5-20251001")
            .unwrap();
        assert_eq!(dated.model.display_name, "Claude Haiku 4.5");
        // Two undated twins are told apart by their ids.
        let twins = vec![model("a/x-1", "X", false), model("b/x-2", "X", false)];
        let rows = picker_rows("codex", &twins, "", None, true);
        // Newest first: `x-2` before `x-1`.
        assert_eq!(rows[0].variant.as_deref(), Some("(x-2)"));
        assert_eq!(rows[1].variant.as_deref(), Some("(x-1)"));
    }

    #[test]
    fn ids_read_as_family_version_and_date() {
        let haiku = shape("anthropic/claude-haiku-4-5-20251001");
        assert_eq!((haiku.family, haiku.version), (2, vec![4, 5]));
        assert_eq!(haiku.date.as_deref(), Some("20251001"));
        let old = shape("claude-3-5-sonnet-20241022");
        assert_eq!((old.family, old.version), (1, vec![3, 5]));
        let mini = shape("gitlab-duo/gpt-5-mini-2025-08-07");
        assert_eq!((mini.family, mini.words.as_str()), (3, "gpt-mini"));
        assert_eq!(mini.version, vec![5]);
        assert_eq!(mini.date.as_deref(), Some("20250807"));
        let dotted = shape("gpt-5.1-2025-11-13");
        assert_eq!(dotted.version, vec![5, 1]);
        assert_eq!(dotted.date.as_deref(), Some("20251113"));
    }

    #[test]
    fn the_filter_matches_names_and_ids_and_the_current_model_is_marked() {
        let models = catalog();
        let by_name = picker_rows("omp", &models, "haiku 4.5", None, true);
        assert_eq!(by_name.len(), 2);
        // Filtered alone, the dated twin still says which one it is.
        let by_id = picker_rows("omp", &models, "20251001", None, true);
        assert_eq!(by_id.len(), 1);
        assert_eq!(by_id[0].variant.as_deref(), Some("(20251001)"));
        let rows = picker_rows("omp", &models, "", Some("anthropic/claude-haiku-5-5"), true);
        let current: Vec<&str> = rows
            .iter()
            .filter(|r| r.current)
            .map(|r| r.model.id.as_str())
            .collect();
        assert_eq!(current, ["anthropic/claude-haiku-5-5"]);
        // A chat that reports the bare id is found too.
        let rows = picker_rows("omp", &models, "", Some("claude-haiku-5-5"), true);
        assert_eq!(rows.iter().filter(|r| r.current).count(), 1);
        // The hidden ones are not offered.
        let mut hidden = catalog();
        hidden[0].hidden = true;
        assert_eq!(
            picker_rows("omp", &hidden, "", None, true).len(),
            models.len() - 1
        );
    }

    #[test]
    fn names_are_learned_found_by_bare_id_and_survive_a_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MODEL_NAMES_FILE);
        let mut names = ModelNames::load(&path);
        assert_eq!(names.name("omp", "anthropic/claude-haiku-5-5"), None);
        assert!(names.learn("omp", &catalog()));
        assert!(!names.learn("omp", &catalog()), "nothing new");
        assert_eq!(
            names.name("omp", "anthropic/claude-haiku-5-5"),
            Some("Claude Haiku 5.5")
        );
        assert_eq!(
            names.name("omp", "claude-haiku-5-5"),
            Some("Claude Haiku 5.5")
        );
        assert_eq!(names.label("omp", "claude-haiku-5-5"), "Claude Haiku 5.5");
        assert_eq!(names.name("claude", "anthropic/claude-haiku-5-5"), None);
        assert_eq!(names.label("omp", "unknown/m"), "unknown/m");
        save(&path, &names.to_json()).unwrap();
        let loaded = ModelNames::load(&path);
        assert_eq!(loaded, names);
        // A model the catalog dropped keeps its name.
        let mut names = loaded;
        assert!(names.learn("omp", &[model("anthropic/new", "New", false)]));
        assert_eq!(names.name("omp", "new"), Some("New"));
        assert_eq!(
            names.name("omp", "anthropic/claude-opus-4-6"),
            Some("Claude Opus 4.6")
        );
        // Missing and corrupt files hold none.
        assert_eq!(
            ModelNames::load(&dir.path().join("none.json")),
            ModelNames::default()
        );
        std::fs::write(&path, "{ nope").unwrap();
        assert_eq!(ModelNames::load(&path), ModelNames::default());
    }

    #[test]
    fn a_bare_id_that_several_providers_share_is_not_guessed() {
        let mut names = ModelNames::default();
        names.learn(
            "omp",
            &[model("a/m-1", "A M1", false), model("b/m-1", "B M1", false)],
        );
        assert_eq!(names.name("omp", "m-1"), None);
        assert_eq!(names.name("omp", "b/m-1"), Some("B M1"));
    }
}
