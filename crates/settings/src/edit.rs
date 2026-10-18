//! Comment-preserving edits of `settings.toml` (`toml_edit`).

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, TableLike};

/// Sets `key_path` (dotted) to `value` inside `text` and returns the new document text.
///
/// Everything not on the edited key keeps its comments and formatting; replacing an existing
/// value keeps the comments attached to that key. Missing intermediate tables are created.
pub(crate) fn set_key(text: &str, key_path: &str, value: &toml::Value) -> Result<String, String> {
    let segments: Vec<&str> = key_path.split('.').collect();
    if segments.iter().any(|s| s.trim().is_empty()) {
        return Err(format!(
            "invalid key path `{key_path}`: empty segment (expected e.g. `terminal.font_size`)"
        ));
    }
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| format!("invalid TOML: {e}"))?;

    // A document holding only comments keeps them as "trailing" text, which would end up below
    // the first key we add; detach them here and put them back on top afterwards.
    let header = if doc.as_table().is_empty() {
        let header = doc.trailing().as_str().unwrap_or_default().to_owned();
        doc.set_trailing("");
        header
    } else {
        String::new()
    };

    let (last, parents) = segments
        .split_last()
        .ok_or_else(|| "empty key path".to_owned())?;
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    let mut walked = String::new();
    for segment in parents {
        if !walked.is_empty() {
            walked.push('.');
        }
        walked.push_str(segment);
        if !table.contains_key(segment) {
            let mut created = Table::new();
            created.set_implicit(true);
            table.insert(segment, Item::Table(created));
        }
        table = table
            .get_mut(segment)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| format!("`{walked}` exists but is not a table"))?;
    }

    let mut item = to_item(value)?;
    match table.get_mut(last) {
        Some(existing) => {
            copy_decor(existing, &mut item);
            *existing = item;
        }
        None => {
            table.insert(last, item);
        }
    }
    Ok(format!("{header}{doc}"))
}

/// Removes `key_path` (dotted) from `text`. `Ok(None)` when the key is not there. Comments and
/// formatting elsewhere survive; a table the removal leaves empty is dropped too unless a
/// comment is attached to it.
pub(crate) fn remove_key(text: &str, key_path: &str) -> Result<Option<String>, String> {
    let segments: Vec<&str> = key_path.split('.').collect();
    if segments.iter().any(|s| s.trim().is_empty()) {
        return Err(format!(
            "invalid key path `{key_path}`: empty segment (expected e.g. `terminal.font_size`)"
        ));
    }
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| format!("invalid TOML: {e}"))?;
    if !remove_in(doc.as_table_mut(), &segments) {
        return Ok(None);
    }
    Ok(Some(doc.to_string()))
}

/// Removes the key `path` below `table`; `false` when it is not there.
fn remove_in(table: &mut dyn TableLike, path: &[&str]) -> bool {
    let Some((first, rest)) = path.split_first() else {
        return false;
    };
    if rest.is_empty() {
        return table.remove(first).is_some();
    }
    let Some(child) = table.get_mut(first).and_then(Item::as_table_like_mut) else {
        return false;
    };
    if !remove_in(child, rest) {
        return false;
    }
    let emptied = child.is_empty();
    let commented = child_has_comment(table.get(first));
    if emptied && !commented {
        table.remove(first);
    }
    true
}

/// Whether the comments of `item` (a table) are more than whitespace.
fn child_has_comment(item: Option<&Item>) -> bool {
    let prefix = match item {
        Some(Item::Table(table)) => table.decor().prefix().and_then(|p| p.as_str()),
        Some(Item::Value(toml_edit::Value::InlineTable(table))) => {
            table.decor().prefix().and_then(|p| p.as_str())
        }
        _ => None,
    };
    prefix.is_some_and(|text| !text.trim().is_empty())
}

fn copy_decor(from: &Item, to: &mut Item) {
    match (from, to) {
        (Item::Value(from), Item::Value(to)) => *to.decor_mut() = from.decor().clone(),
        (Item::Table(from), Item::Table(to)) => *to.decor_mut() = from.decor().clone(),
        _ => {}
    }
}

fn to_item(value: &toml::Value) -> Result<Item, String> {
    Ok(match value {
        toml::Value::Table(map) => {
            let mut table = Table::new();
            for (key, value) in map {
                table.insert(key, to_item(value)?);
            }
            Item::Table(table)
        }
        other => Item::Value(to_value(other)?),
    })
}

fn to_value(value: &toml::Value) -> Result<toml_edit::Value, String> {
    Ok(match value {
        toml::Value::String(s) => s.as_str().into(),
        toml::Value::Integer(i) => (*i).into(),
        toml::Value::Float(f) => (*f).into(),
        toml::Value::Boolean(b) => (*b).into(),
        toml::Value::Datetime(d) => d
            .to_string()
            .parse::<toml_edit::Datetime>()
            .map_err(|e| format!("invalid datetime `{d}`: {e}"))?
            .into(),
        toml::Value::Array(items) => {
            let mut array = Array::new();
            for item in items {
                array.push(to_value(item)?);
            }
            array.into()
        }
        toml::Value::Table(map) => {
            let mut table = InlineTable::new();
            for (key, value) in map {
                table.insert(key, to_value(value)?);
            }
            table.into()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Hutan settings — edited by hand

[appearance]
# compact | comfortable
density = \"compact\"   # my favourite

[terminal]
font_family = \"Menlo\"
font_size = 13 # keep it readable
cursor = { style = \"block\", blink = true }

# layout stuff
[layout]
preset = \"default\"
";

    #[test]
    fn replacing_a_value_preserves_every_comment() {
        let out = set_key(
            SAMPLE,
            "appearance.density",
            &toml::Value::String("comfortable".into()),
        )
        .unwrap();
        let expected = SAMPLE.replace(
            "density = \"compact\"   # my favourite",
            "density = \"comfortable\"   # my favourite",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn replacing_an_integer_keeps_trailing_comment_and_everything_else() {
        let out = set_key(SAMPLE, "terminal.font_size", &toml::Value::Integer(15)).unwrap();
        assert_eq!(
            out,
            SAMPLE.replace(
                "font_size = 13 # keep it readable",
                "font_size = 15 # keep it readable"
            )
        );
    }

    #[test]
    fn adding_a_key_to_an_existing_table_leaves_the_rest_untouched() {
        let out = set_key(
            SAMPLE,
            "terminal.copy_on_select",
            &toml::Value::Boolean(true),
        )
        .unwrap();
        assert!(out.starts_with(
            "# Hutan settings — edited by hand\n\n[appearance]\n# compact | comfortable\n"
        ));
        assert!(out.contains("font_size = 13 # keep it readable\n"));
        assert!(out.contains("copy_on_select = true"));
        assert!(out.contains("# layout stuff\n[layout]\npreset = \"default\"\n"));
        let parsed: toml::Table = toml::from_str(&out).unwrap();
        assert_eq!(parsed["terminal"]["copy_on_select"].as_bool(), Some(true));
        assert_eq!(parsed["terminal"]["font_size"].as_integer(), Some(13));
    }

    #[test]
    fn creates_missing_tables_without_empty_headers() {
        let out = set_key(
            "",
            "panels.sessions.group_by",
            &toml::Value::String("state".into()),
        )
        .unwrap();
        assert_eq!(out, "[panels.sessions]\ngroup_by = \"state\"\n");
    }

    #[test]
    fn edits_inside_inline_tables() {
        let out = set_key(
            SAMPLE,
            "terminal.cursor.blink",
            &toml::Value::Boolean(false),
        )
        .unwrap();
        assert_eq!(out, SAMPLE.replace("blink = true", "blink = false"));
    }

    #[test]
    fn writes_arrays_and_tables() {
        let value: toml::Value = toml::from_str("left = [\"a\", \"b\"]\n[nested]\nx = 1\n")
            .map(toml::Value::Table)
            .unwrap();
        let out = set_key("# top\n", "layout.presets.focus", &value).unwrap();
        let parsed: toml::Table = toml::from_str(&out).unwrap();
        assert_eq!(parsed["layout"]["presets"]["focus"], value);
        assert!(out.starts_with("# top\n[layout.presets.focus]\n"), "{out}");
    }

    #[test]
    fn refuses_non_table_parents_and_bad_paths_and_bad_documents() {
        let v = toml::Value::Integer(1);
        assert!(
            set_key(SAMPLE, "terminal.font_size.deeper", &v)
                .unwrap_err()
                .contains("not a table")
        );
        assert!(set_key(SAMPLE, "", &v).is_err());
        assert!(set_key(SAMPLE, "a..b", &v).is_err());
        assert!(
            set_key("[broken", "a.b", &v)
                .unwrap_err()
                .contains("invalid TOML")
        );
    }
}

#[cfg(test)]
mod remove_tests {
    use super::*;

    #[test]
    fn removing_a_key_keeps_everything_else() {
        let text = "# top\n[terminal]\nfont_size = 13 # keep\ncopy_on_select = true\n";
        let out = remove_key(text, "terminal.copy_on_select")
            .unwrap()
            .unwrap();
        assert_eq!(out, "# top\n[terminal]\nfont_size = 13 # keep\n");
        assert_eq!(remove_key(text, "terminal.nope").unwrap(), None);
        assert_eq!(remove_key(text, "nope.font_size").unwrap(), None);
        assert!(remove_key("[broken", "a.b").is_err());
        assert!(remove_key(text, "a..b").is_err());
    }

    #[test]
    fn a_table_left_empty_goes_unless_it_carries_a_comment() {
        let out = remove_key(
            "[panels.sessions]\ngroup_by = \"state\"\n",
            "panels.sessions.group_by",
        )
        .unwrap()
        .unwrap();
        assert_eq!(out, "");
        let out = remove_key("[a]\nx = 1\n[b]\ny = 2\n", "a.x")
            .unwrap()
            .unwrap();
        assert_eq!(out, "[b]\ny = 2\n");
        let commented = "# my terminal\n[terminal]\nfont_size = 13\n";
        let out = remove_key(commented, "terminal.font_size")
            .unwrap()
            .unwrap();
        assert!(out.contains("# my terminal"), "{out}");
    }

    #[test]
    fn keys_inside_inline_tables_and_maps_can_go() {
        let text = "[terminal]\ncursor = { style = \"bar\", blink = false }\n[terminal.env]\nA = \"1\"\nB = \"2\"\n";
        let out = remove_key(text, "terminal.cursor.style").unwrap().unwrap();
        assert!(
            out.contains("blink = false") && !out.contains("style"),
            "{out}"
        );
        let out = remove_key(text, "terminal.env").unwrap().unwrap();
        assert!(!out.contains("env") && out.contains("cursor"), "{out}");
        let out = remove_key(text, "terminal.env.A").unwrap().unwrap();
        assert!(!out.contains("A = ") && out.contains("B = \"2\""), "{out}");
    }
}
