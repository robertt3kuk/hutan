//! Mawas's memory as Markdown (protocol 0.46, capability `memory.files`; `docs/assistant.md`
//! § Memory files): the text the daemon writes under the scope's folder, the same text
//! `hutan memory export` writes, and the reader of both — the daemon's (an edited file, an
//! import) and the CLI's.
//!
//! One file per category (`profile.md`, `facts.md`, `notes.md`):
//!
//! ```text
//! <!-- Written by Hutan: Mawas memory (global). … -->
//! # Facts
//!
//! <!-- mawas:id=01K7F3B5N2Q9W8E4R6T1Y0V3KM rev=3 -->
//! Uses jj for version control.
//!
//! <!-- mawas:id=01K7F3B5N2Q9W8E4R6T1Y0V3KN rev=1 -->
//! Staging is on `stage.example.com`.
//! ```
//!
//! An entry is the text after its marker line up to the next marker or heading. A marker
//! `<!-- mawas:new -->` (or text under the heading before any marker) starts a new entry. The
//! reader is forgiving about layout and exact about meaning: it never invents an id, and
//! anything it cannot use is a [`Problem`] with a reason, never a guess.
//!
//! Lines that would read as a marker or a category heading inside an entry's text are written
//! with a leading backslash and read back without it, so any body survives a round trip.

use serde::{Deserialize, Serialize};

use crate::memory::{MemoryCategory, MemoryEntry, MemoryScope, MemoryState};

/// The folder of the global memory files, below the Home folder.
pub const GLOBAL_DIR: &str = ".mawas/memory";
/// The folder of a workspace's memory files, below its project's root.
pub const PROJECT_DIR: &str = ".hutan/memory";
/// Marker of a section without an id: what the reader turns into a new entry.
pub const NEW_MARKER: &str = "<!-- mawas:new -->";
/// Format version of the JSON export.
pub const JSON_VERSION: u32 = 1;
/// `format` of the JSON export.
pub const JSON_FORMAT: &str = "hutan-memory";

const MARKER_PREFIX: &str = "<!-- mawas:";
const MARKER_SUFFIX: &str = "-->";

/// The file of `category`: `profile.md`, `facts.md`, `notes.md`.
pub const fn file_name(category: MemoryCategory) -> &'static str {
    match category {
        MemoryCategory::Profile => "profile.md",
        MemoryCategory::Fact => "facts.md",
        MemoryCategory::Note => "notes.md",
    }
}

/// The category a file name (or the end of a path) stands for.
pub fn category_of_file(name: &str) -> Option<MemoryCategory> {
    let name = name.rsplit('/').next().unwrap_or(name);
    MemoryCategory::ALL
        .into_iter()
        .find(|category| file_name(*category).eq_ignore_ascii_case(name))
}

/// The heading of a category's section: `Profile`, `Facts`, `Notes`.
pub const fn heading(category: MemoryCategory) -> &'static str {
    match category {
        MemoryCategory::Profile => "Profile",
        MemoryCategory::Fact => "Facts",
        MemoryCategory::Note => "Notes",
    }
}

fn category_of_heading(line: &str) -> Option<MemoryCategory> {
    let title = line.strip_prefix("# ")?.trim();
    MemoryCategory::ALL
        .into_iter()
        .find(|category| heading(*category).eq_ignore_ascii_case(title))
}

/// One entry to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item<'a> {
    /// Absent for text that is not an entry yet (an edit waiting for approval).
    pub id: Option<&'a str>,
    pub revision: u64,
    pub body: &'a str,
}

fn scope_words(scope: MemoryScope) -> &'static str {
    match scope {
        MemoryScope::Global => "global",
        MemoryScope::Workspace => "this workspace",
    }
}

/// The line at the top of every file.
fn banner(scope: MemoryScope) -> String {
    format!(
        "<!-- Written by Hutan: Mawas memory ({}). Change a section, add one under a `{NEW_MARKER}` line or delete one, and Hutan turns it into a proposal you approve in the Memory page; nothing is applied from a file on its own. Keep the id comments. -->",
        scope_words(scope)
    )
}

/// Whether `line` would be read as structure if it stood in an entry's text.
fn reserved(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with(MARKER_PREFIX) || category_of_heading(trimmed).is_some()
}

/// A line that is already an escaped reserved line (`\\<!-- mawas:…`, `\\\\# Facts`, …).
fn escaped(line: &str) -> bool {
    line.strip_prefix('\\')
        .is_some_and(|rest| reserved(rest) || escaped(rest))
}

fn push_body(out: &mut String, body: &str) {
    for line in body.trim().lines() {
        if reserved(line) || escaped(line) {
            out.push('\\');
        }
        out.push_str(line);
        out.push('\n');
    }
}

fn push_section(out: &mut String, category: MemoryCategory, items: &[Item<'_>]) {
    out.push_str("# ");
    out.push_str(heading(category));
    out.push('\n');
    for item in items {
        out.push('\n');
        match item.id {
            Some(id) => {
                out.push_str(&format!(
                    "{MARKER_PREFIX}id={id} rev={} {MARKER_SUFFIX}\n",
                    item.revision
                ));
            }
            None => {
                out.push_str(NEW_MARKER);
                out.push('\n');
            }
        }
        push_body(out, item.body);
    }
}

/// The text of `category`'s file. Deterministic: the same entries give the same bytes.
pub fn render_file(scope: MemoryScope, category: MemoryCategory, items: &[Item<'_>]) -> String {
    let mut out = banner(scope);
    out.push('\n');
    push_section(&mut out, category, items);
    out
}

/// One document with all three categories: what `hutan memory export` writes in Markdown.
pub fn render_document(
    scope: MemoryScope,
    by_category: &[(MemoryCategory, Vec<Item<'_>>)],
) -> String {
    let mut out = banner(scope);
    out.push('\n');
    for (index, (category, items)) in by_category.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        push_section(&mut out, *category, items);
    }
    out
}

/// One section of a file, or one entry of an export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub category: Option<MemoryCategory>,
    /// The entry the section is about; absent for a new one.
    pub id: Option<String>,
    /// The revision the section was written at.
    pub revision: Option<u64>,
    /// The text, trimmed.
    pub body: String,
    /// Where it starts (1-based line, or the entry's index in a JSON export).
    pub line: usize,
}

impl Section {
    /// How a problem names the section: its id, else `line N`.
    pub fn label(&self) -> String {
        self.id
            .clone()
            .unwrap_or_else(|| format!("line {}", self.line))
    }
}

/// Something in a text that cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub line: usize,
    pub reason: String,
}

/// What [`parse`] found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Parsed {
    pub sections: Vec<Section>,
    pub problems: Vec<Problem>,
}

#[derive(Debug)]
struct Open {
    category: Option<MemoryCategory>,
    id: Option<String>,
    revision: Option<u64>,
    line: usize,
    body: Vec<String>,
}

/// Reads Markdown as [`render_file`] and [`render_document`] write it (and as a person edits
/// it). `default_category` is the category of sections no heading precedes (the file's own).
pub fn parse(text: &str, default_category: Option<MemoryCategory>) -> Parsed {
    let mut parsed = Parsed::default();
    let mut category = default_category;
    let mut open: Option<Open> = None;
    let mut banner_open = false;
    // After a marker that could not be read, its text belongs to nothing.
    let mut skipping = false;

    let close = |open: &mut Option<Open>, parsed: &mut Parsed| {
        let Some(section) = open.take() else { return };
        let body = section.body.join("\n").trim().to_owned();
        if body.is_empty() && section.id.is_none() {
            return;
        }
        parsed.sections.push(Section {
            category: section.category,
            id: section.id,
            revision: section.revision,
            body,
            line: section.line,
        });
    };

    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let line = raw.trim_end();
        let trimmed = line.trim();

        if banner_open {
            // The banner comment may run over several lines (a person re-wrapped it).
            if trimmed.contains("-->") {
                banner_open = false;
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix(MARKER_PREFIX) {
            close(&mut open, &mut parsed);
            skipping = true;
            let Some(inner) = rest.strip_suffix(MARKER_SUFFIX) else {
                parsed.problems.push(Problem {
                    line: number,
                    reason: "a `<!-- mawas:` comment is not closed with `-->` on its line"
                        .to_owned(),
                });
                continue;
            };
            let mut id = None;
            let mut revision = None;
            let mut bad = None;
            for token in inner.split_whitespace() {
                if token == "new" {
                    // An entry that has no id yet.
                } else if let Some(value) = token.strip_prefix("id=") {
                    if !value.is_empty() {
                        id = Some(value.to_owned());
                    }
                } else if let Some(value) = token.strip_prefix("rev=") {
                    match value.parse::<u64>() {
                        Ok(rev) => revision = Some(rev),
                        Err(_) => bad = Some(format!("`{token}` is not a revision number")),
                    }
                } else {
                    bad = Some(format!("unknown `{token}` in a `<!-- mawas:` comment"));
                }
            }
            if let Some(reason) = bad {
                parsed.problems.push(Problem {
                    line: number,
                    reason,
                });
                continue;
            }
            skipping = false;
            open = Some(Open {
                category,
                id,
                revision,
                line: number,
                body: Vec::new(),
            });
            continue;
        }
        if let Some(found) = category_of_heading(trimmed) {
            close(&mut open, &mut parsed);
            skipping = false;
            category = Some(found);
            continue;
        }
        if skipping {
            continue;
        }
        match open.as_mut() {
            Some(section) => {
                section.body.push(unescape(line).to_owned());
            }
            None => {
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed.starts_with("<!--") && open.is_none() && parsed.sections.is_empty() {
                    // The banner (or another comment) before anything else.
                    if !trimmed.contains("-->") {
                        banner_open = true;
                    }
                    continue;
                }
                // Text under the heading that no marker introduced: a new entry.
                open = Some(Open {
                    category,
                    id: None,
                    revision: None,
                    line: number,
                    body: vec![unescape(line).to_owned()],
                });
            }
        }
    }
    close(&mut open, &mut parsed);
    parsed
}

fn unescape(line: &str) -> &str {
    match line.strip_prefix('\\') {
        Some(rest) if reserved(rest) || escaped(rest) => rest,
        _ => line,
    }
}

// JSON export ----------------------------------------------------------------------------------

/// One entry of the JSON export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub category: MemoryCategory,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    /// `active` (absent means active) or `archived`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<MemoryState>,
}

/// The JSON export document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonExport {
    pub format: String,
    pub version: u32,
    pub scope: MemoryScope,
    pub entries: Vec<JsonEntry>,
}

/// `entries` as the JSON export (pretty, newline-terminated, in the order given).
pub fn render_json(scope: MemoryScope, entries: &[MemoryEntry]) -> String {
    let export = JsonExport {
        format: JSON_FORMAT.to_owned(),
        version: JSON_VERSION,
        scope,
        entries: entries
            .iter()
            .map(|entry| JsonEntry {
                id: Some(entry.id.clone()),
                category: entry.category,
                body: entry.body.clone(),
                revision: Some(entry.revision),
                state: Some(entry.state),
            })
            .collect(),
    };
    let mut text = serde_json::to_string_pretty(&export).unwrap_or_default();
    text.push('\n');
    text
}

/// Reads the JSON export (or a bare array of its entries).
pub fn parse_json(text: &str) -> Result<Parsed, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let entries = match value {
        serde_json::Value::Array(_) => value,
        serde_json::Value::Object(mut object) => {
            if let Some(format) = object.get("format").and_then(serde_json::Value::as_str)
                && format != JSON_FORMAT
            {
                return Err(format!("`format` is `{format}`, not `{JSON_FORMAT}`"));
            }
            if object
                .get("version")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|v| v > u64::from(JSON_VERSION))
            {
                return Err("written by a newer Hutan: update to import it".to_owned());
            }
            object
                .remove("entries")
                .ok_or_else(|| "no `entries`".to_owned())?
        }
        _ => return Err("expected an object with `entries`, or an array of entries".to_owned()),
    };
    let serde_json::Value::Array(entries) = entries else {
        return Err("`entries` is not an array".to_owned());
    };
    let mut parsed = Parsed::default();
    for (index, value) in entries.into_iter().enumerate() {
        match serde_json::from_value::<JsonEntry>(value) {
            Ok(entry) => {
                if entry.state == Some(MemoryState::Archived) {
                    continue;
                }
                parsed.sections.push(Section {
                    category: Some(entry.category),
                    id: entry.id.filter(|id| !id.is_empty()),
                    revision: entry.revision,
                    body: entry.body.trim().to_owned(),
                    line: index + 1,
                });
            }
            Err(e) => parsed.problems.push(Problem {
                line: index + 1,
                reason: format!("entry {} is not usable: {e}", index + 1),
            }),
        }
    }
    Ok(parsed)
}

/// Whether `text` is the JSON export rather than Markdown.
pub fn looks_like_json(text: &str) -> bool {
    matches!(text.trim_start().chars().next(), Some('{' | '['))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items<'a>(list: &'a [(&'a str, u64, &'a str)]) -> Vec<Item<'a>> {
        list.iter()
            .map(|(id, revision, body)| Item {
                id: Some(id),
                revision: *revision,
                body,
            })
            .collect()
    }

    #[test]
    fn a_file_reads_back_as_it_was_written() {
        let list = [
            ("01A", 3, "Uses jj.\n\nSecond paragraph."),
            ("01B", 1, "Staging."),
        ];
        let text = render_file(MemoryScope::Global, MemoryCategory::Fact, &items(&list));
        assert!(text.starts_with("<!-- Written by Hutan"));
        assert!(text.contains("# Facts\n\n<!-- mawas:id=01A rev=3 -->\nUses jj.\n\nSecond paragraph.\n\n<!-- mawas:id=01B rev=1 -->\nStaging.\n"));
        let parsed = parse(&text, Some(MemoryCategory::Fact));
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        let got: Vec<_> = parsed
            .sections
            .iter()
            .map(|s| {
                (
                    s.id.as_deref().unwrap(),
                    s.revision.unwrap(),
                    s.body.as_str(),
                )
            })
            .collect();
        assert_eq!(got, list);
        // Deterministic.
        assert_eq!(
            text,
            render_file(MemoryScope::Global, MemoryCategory::Fact, &items(&list))
        );
    }

    #[test]
    fn text_that_looks_like_structure_survives_a_round_trip() {
        let body = "# Facts\n<!-- mawas:id=X rev=1 -->\n\\<!-- mawas:new -->\nplain";
        let list = [("01A", 1, body)];
        let text = render_file(MemoryScope::Global, MemoryCategory::Note, &items(&list));
        let parsed = parse(&text, Some(MemoryCategory::Note));
        assert_eq!(parsed.sections.len(), 1, "{parsed:?}");
        assert_eq!(parsed.sections[0].body, body);
    }

    #[test]
    fn text_under_a_heading_and_a_new_marker_are_new_entries() {
        let text = "# Facts\nA loose note.\n\n<!-- mawas:id=01A rev=2 -->\nKept.\n\n<!-- mawas:new -->\nAnother.\n";
        let parsed = parse(text, None);
        let sections: Vec<_> = parsed
            .sections
            .iter()
            .map(|s| (s.id.as_deref(), s.category, s.body.as_str()))
            .collect();
        assert_eq!(
            sections,
            [
                (None, Some(MemoryCategory::Fact), "A loose note."),
                (Some("01A"), Some(MemoryCategory::Fact), "Kept."),
                (None, Some(MemoryCategory::Fact), "Another."),
            ]
        );
    }

    #[test]
    fn a_document_keeps_each_entry_under_its_heading() {
        let a = items(&[("01A", 1, "p")]);
        let b = items(&[("01B", 2, "f")]);
        let text = render_document(
            MemoryScope::Global,
            &[(MemoryCategory::Profile, a), (MemoryCategory::Fact, b)],
        );
        let parsed = parse(&text, None);
        let got: Vec<_> = parsed
            .sections
            .iter()
            .map(|s| (s.id.as_deref().unwrap(), s.category.unwrap()))
            .collect();
        assert_eq!(
            got,
            [
                ("01A", MemoryCategory::Profile),
                ("01B", MemoryCategory::Fact)
            ]
        );
    }

    #[test]
    fn marker_mistakes_are_problems_with_a_line() {
        let text = "# Facts\n<!-- mawas:id=01A rev=x -->\nbody\n<!-- mawas:bogus -->\n<!-- mawas:id=01B rev=1\n";
        let parsed = parse(text, None);
        assert_eq!(parsed.problems.len(), 3, "{parsed:?}");
        assert_eq!(parsed.problems[0].line, 2);
        assert!(
            parsed.sections.is_empty(),
            "text after a broken marker is not an entry"
        );
    }

    #[test]
    fn json_round_trips_and_skips_archived_entries() {
        let entry = |id: &str, state| MemoryEntry {
            id: id.to_owned(),
            scope: MemoryScope::Global,
            workspace_id: None,
            category: MemoryCategory::Profile,
            state,
            revision: 2,
            body: format!("body {id}"),
            inferred: false,
            provenance: crate::memory::MemoryProvenance {
                author: crate::memory::MemoryAuthor::User,
                client: None,
                session_id: None,
                turn_id: None,
                at_ms: 0,
                origin: None,
            },
            created_ms: 0,
            updated_ms: 0,
        };
        let text = render_json(
            MemoryScope::Global,
            &[
                entry("01A", MemoryState::Active),
                entry("01B", MemoryState::Archived),
            ],
        );
        assert!(looks_like_json(&text));
        let parsed = parse_json(&text).unwrap();
        assert_eq!(parsed.sections.len(), 1);
        assert_eq!(parsed.sections[0].id.as_deref(), Some("01A"));
        assert_eq!(parsed.sections[0].body, "body 01A");
        assert!(parse_json("{\"format\":\"other\",\"entries\":[]}").is_err());
        assert!(
            parse_json("[{\"category\":\"fact\"}]")
                .unwrap()
                .problems
                .len()
                == 1
        );
    }

    #[test]
    fn file_names_map_to_categories() {
        assert_eq!(category_of_file("/x/Facts.md"), Some(MemoryCategory::Fact));
        assert_eq!(category_of_file("notes.md"), Some(MemoryCategory::Note));
        assert_eq!(category_of_file("other.md"), None);
    }
}
