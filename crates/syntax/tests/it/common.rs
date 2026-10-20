#![allow(dead_code)]

use std::path::PathBuf;

use hutan_syntax::{Language, Snapshot};

/// The fixture file of each language, relative to `tests/it/fixtures`.
pub fn fixture_name(language: Language) -> &'static str {
    match language {
        Language::Rust => "rust.rs",
        Language::TypeScript => "typescript.ts",
        Language::Tsx => "tsx.tsx",
        Language::JavaScript => "javascript.js",
        Language::Python => "python.py",
        Language::Go => "go.go",
        Language::Json => "json.json",
        Language::Toml => "toml.toml",
        Language::Yaml => "yaml.yaml",
        Language::Markdown => "markdown.md",
        Language::Bash => "bash.sh",
        Language::Html => "html.html",
        Language::Css => "css.css",
        Language::C => "c.c",
        Language::Cpp => "cpp.cpp",
    }
}

pub fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn fixture(language: Language) -> String {
    let path = manifest_dir()
        .join("tests/it/fixtures")
        .join(fixture_name(language));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

pub fn parse(language: Language, source: &str) -> Snapshot {
    Snapshot::parse(language, source.to_string(), 0).unwrap_or_else(|e| panic!("{language}: {e}"))
}
