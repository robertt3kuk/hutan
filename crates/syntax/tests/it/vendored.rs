//! The vendored query files are byte-for-byte what the pinned grammar crates ship, so a grammar
//! bump that changes upstream queries shows up here and in `queries/NOTICE.md`'s review.

fn vendored(path: &str) -> String {
    let path = crate::common::manifest_dir()
        .join("queries/vendored")
        .join(path);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

macro_rules! same_as_upstream {
    ($($feature:literal, $name:ident, $file:literal, $upstream:expr;)*) => {$(
        #[cfg(feature = $feature)]
        #[test]
        fn $name() {
            assert_eq!(vendored($file), $upstream, "{} drifted from the crate", $file);
        }
    )*};
}

same_as_upstream! {
    "rust", rust, "rust/highlights.scm", tree_sitter_rust::HIGHLIGHTS_QUERY;
    "javascript", javascript, "javascript/highlights.scm", tree_sitter_javascript::HIGHLIGHT_QUERY;
    "javascript", javascript_jsx, "javascript/highlights-jsx.scm", tree_sitter_javascript::JSX_HIGHLIGHT_QUERY;
    "typescript", typescript, "typescript/highlights.scm", tree_sitter_typescript::HIGHLIGHTS_QUERY;
    "python", python, "python/highlights.scm", tree_sitter_python::HIGHLIGHTS_QUERY;
    "go", go, "go/highlights.scm", tree_sitter_go::HIGHLIGHTS_QUERY;
    "json", json, "json/highlights.scm", tree_sitter_json::HIGHLIGHTS_QUERY;
    "toml", toml, "toml/highlights.scm", tree_sitter_toml_ng::HIGHLIGHTS_QUERY;
    "yaml", yaml, "yaml/highlights.scm", tree_sitter_yaml::HIGHLIGHTS_QUERY;
    "markdown", markdown_block, "markdown/block-highlights.scm", tree_sitter_md::HIGHLIGHT_QUERY_BLOCK;
    "markdown", markdown_inline, "markdown/inline-highlights.scm", tree_sitter_md::HIGHLIGHT_QUERY_INLINE;
    "bash", bash, "bash/highlights.scm", tree_sitter_bash::HIGHLIGHT_QUERY;
    "html", html, "html/highlights.scm", tree_sitter_html::HIGHLIGHTS_QUERY;
    "css", css, "css/highlights.scm", tree_sitter_css::HIGHLIGHTS_QUERY;
    "c", c, "c/highlights.scm", tree_sitter_c::HIGHLIGHT_QUERY;
    "cpp", cpp, "cpp/highlights.scm", tree_sitter_cpp::HIGHLIGHT_QUERY;
}

#[test]
fn every_vendored_file_is_listed_in_the_notice() {
    let notice = std::fs::read_to_string(crate::common::manifest_dir().join("queries/NOTICE.md"))
        .expect("queries/NOTICE.md");
    let root = crate::common::manifest_dir().join("queries/vendored");
    let mut missing = Vec::new();
    for dir in std::fs::read_dir(&root).expect("vendored dir") {
        let dir = dir.expect("entry").path();
        for file in std::fs::read_dir(&dir).expect("language dir") {
            let file = file.expect("entry").path();
            let relative = format!(
                "vendored/{}/{}",
                dir.file_name().and_then(|n| n.to_str()).unwrap_or_default(),
                file.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
            );
            if !notice.contains(&relative) {
                missing.push(relative);
            }
        }
    }
    assert!(
        missing.is_empty(),
        "not attributed in NOTICE.md: {missing:?}"
    );
}

#[test]
fn every_license_text_is_present_and_named_in_the_notice() {
    let notice = std::fs::read_to_string(crate::common::manifest_dir().join("queries/NOTICE.md"))
        .expect("queries/NOTICE.md");
    let dir = crate::common::manifest_dir().join("queries/licenses");
    let mut count = 0;
    for file in std::fs::read_dir(&dir).expect("licenses dir") {
        let path = file.expect("entry").path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let text = std::fs::read_to_string(&path).expect("license");
        assert!(
            text.contains("Permission is hereby granted"),
            "{name} is not an MIT text"
        );
        assert!(
            notice.contains(&format!("licenses/{name}")),
            "{name} not named in NOTICE.md"
        );
        count += 1;
    }
    assert_eq!(count, 14, "one license text per grammar crate");
}
