//! Per-language compiled state: the grammar and the queries, built once per process on first use.

use std::sync::OnceLock;

use tree_sitter::Query;

use crate::error::SyntaxError;
use crate::language::Language;
use crate::token::Token;

/// Includes a query file from `queries/`.
#[allow(unused_macros)] // unused when no grammar feature is enabled
macro_rules! q {
    ($path:literal) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/queries/", $path))
    };
}

/// The compiled queries and grammar of one language.
pub(crate) struct Config {
    pub language: Language,
    pub grammar: tree_sitter::Language,
    pub highlights: Query,
    /// What each capture index of `highlights` paints.
    pub highlight_tokens: Vec<Paint>,
    pub folds: Query,
    pub fold_capture: Option<u32>,
    pub fold_body_capture: Option<u32>,
    pub indents: Query,
    pub indent_capture: Option<u32>,
    pub outdent_capture: Option<u32>,
    /// Markdown's inline grammar, parsed over the block tree's `inline` nodes.
    pub inline: Option<InlineConfig>,
}

pub(crate) struct InlineConfig {
    pub grammar: tree_sitter::Language,
    /// `(inline) @inline` over the block grammar: the nodes whose text the inline grammar parses.
    pub nodes: Query,
    pub highlights: Query,
    pub highlight_tokens: Vec<Paint>,
}

/// What a highlight capture does to the text it covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Paint {
    /// Nothing: the capture has no visual meaning (`@spell`, unknown names).
    Skip,
    /// Removes the highlight of enclosing captures (`@none`).
    Clear,
    /// The text is this token.
    Token(Token),
}

/// The sources a [`Config`] is built from.
struct Spec {
    grammar: tree_sitter::Language,
    highlights: Vec<&'static str>,
    folds: Vec<&'static str>,
    indents: Vec<&'static str>,
    inline: Option<(tree_sitter::Language, Vec<&'static str>)>,
}

// Every arm is feature-gated: with none enabled the match is only its fallback.
#[allow(unreachable_patterns, unreachable_code, unused_variables)]
fn spec(language: Language) -> Option<Spec> {
    Some(match language {
        #[cfg(feature = "rust")]
        Language::Rust => Spec {
            grammar: tree_sitter_rust::LANGUAGE.into(),
            highlights: vec![
                q!("vendored/rust/highlights.scm"),
                q!("hutan/rust/highlights.scm"),
            ],
            folds: vec![q!("hutan/rust/folds.scm")],
            indents: vec![q!("hutan/rust/indents.scm")],
            inline: None,
        },
        #[cfg(feature = "javascript")]
        Language::JavaScript => Spec {
            grammar: tree_sitter_javascript::LANGUAGE.into(),
            highlights: vec![
                q!("vendored/javascript/highlights.scm"),
                q!("vendored/javascript/highlights-params.scm"),
                q!("vendored/javascript/highlights-jsx.scm"),
                q!("hutan/javascript/highlights.scm"),
            ],
            folds: vec![
                q!("hutan/javascript/folds.scm"),
                q!("hutan/javascript/jsx-folds.scm"),
            ],
            indents: vec![
                q!("hutan/javascript/indents.scm"),
                q!("hutan/javascript/jsx-indents.scm"),
            ],
            inline: None,
        },
        #[cfg(feature = "typescript")]
        Language::TypeScript => Spec {
            grammar: tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            highlights: vec![
                q!("vendored/javascript/highlights.scm"),
                q!("vendored/typescript/highlights.scm"),
                q!("hutan/javascript/highlights.scm"),
                q!("hutan/typescript/highlights.scm"),
            ],
            folds: vec![
                q!("hutan/javascript/folds.scm"),
                q!("hutan/typescript/folds.scm"),
            ],
            indents: vec![
                q!("hutan/javascript/indents.scm"),
                q!("hutan/typescript/indents.scm"),
            ],
            inline: None,
        },
        #[cfg(feature = "typescript")]
        Language::Tsx => Spec {
            grammar: tree_sitter_typescript::LANGUAGE_TSX.into(),
            highlights: vec![
                q!("vendored/javascript/highlights.scm"),
                q!("vendored/typescript/highlights.scm"),
                q!("vendored/javascript/highlights-jsx.scm"),
                q!("hutan/javascript/highlights.scm"),
                q!("hutan/typescript/highlights.scm"),
            ],
            folds: vec![
                q!("hutan/javascript/folds.scm"),
                q!("hutan/typescript/folds.scm"),
                q!("hutan/javascript/jsx-folds.scm"),
            ],
            indents: vec![
                q!("hutan/javascript/indents.scm"),
                q!("hutan/typescript/indents.scm"),
                q!("hutan/javascript/jsx-indents.scm"),
            ],
            inline: None,
        },
        #[cfg(feature = "python")]
        Language::Python => Spec {
            grammar: tree_sitter_python::LANGUAGE.into(),
            highlights: vec![
                q!("vendored/python/highlights.scm"),
                q!("hutan/python/highlights.scm"),
            ],
            folds: vec![q!("hutan/python/folds.scm")],
            indents: vec![q!("hutan/python/indents.scm")],
            inline: None,
        },
        #[cfg(feature = "go")]
        Language::Go => Spec {
            grammar: tree_sitter_go::LANGUAGE.into(),
            highlights: vec![
                q!("vendored/go/highlights.scm"),
                q!("hutan/go/highlights.scm"),
            ],
            folds: vec![q!("hutan/go/folds.scm")],
            indents: vec![q!("hutan/go/indents.scm")],
            inline: None,
        },
        #[cfg(feature = "json")]
        Language::Json => Spec {
            grammar: tree_sitter_json::LANGUAGE.into(),
            highlights: vec![
                q!("vendored/json/highlights.scm"),
                q!("hutan/json/highlights.scm"),
            ],
            folds: vec![q!("hutan/json/folds.scm")],
            indents: vec![q!("hutan/json/indents.scm")],
            inline: None,
        },
        #[cfg(feature = "toml")]
        Language::Toml => Spec {
            grammar: tree_sitter_toml_ng::LANGUAGE.into(),
            highlights: vec![
                q!("vendored/toml/highlights.scm"),
                q!("hutan/toml/highlights.scm"),
            ],
            folds: vec![q!("hutan/toml/folds.scm")],
            indents: vec![q!("hutan/toml/indents.scm")],
            inline: None,
        },
        #[cfg(feature = "yaml")]
        Language::Yaml => Spec {
            grammar: tree_sitter_yaml::LANGUAGE.into(),
            highlights: vec![
                q!("vendored/yaml/highlights.scm"),
                q!("hutan/yaml/highlights.scm"),
            ],
            folds: vec![q!("hutan/yaml/folds.scm")],
            indents: vec![q!("hutan/yaml/indents.scm")],
            inline: None,
        },
        #[cfg(feature = "markdown")]
        Language::Markdown => Spec {
            grammar: tree_sitter_md::LANGUAGE.into(),
            highlights: vec![
                q!("vendored/markdown/block-highlights.scm"),
                q!("hutan/markdown/highlights.scm"),
            ],
            folds: vec![q!("hutan/markdown/folds.scm")],
            indents: vec![q!("hutan/markdown/indents.scm")],
            inline: Some((
                tree_sitter_md::INLINE_LANGUAGE.into(),
                vec![
                    q!("vendored/markdown/inline-highlights.scm"),
                    q!("hutan/markdown/inline-highlights.scm"),
                ],
            )),
        },
        #[cfg(feature = "bash")]
        Language::Bash => Spec {
            grammar: tree_sitter_bash::LANGUAGE.into(),
            highlights: vec![
                q!("vendored/bash/highlights.scm"),
                q!("hutan/bash/highlights.scm"),
            ],
            folds: vec![q!("hutan/bash/folds.scm")],
            indents: vec![q!("hutan/bash/indents.scm")],
            inline: None,
        },
        #[cfg(feature = "html")]
        Language::Html => Spec {
            grammar: tree_sitter_html::LANGUAGE.into(),
            highlights: vec![
                q!("vendored/html/highlights.scm"),
                q!("hutan/html/highlights.scm"),
            ],
            folds: vec![q!("hutan/html/folds.scm")],
            indents: vec![q!("hutan/html/indents.scm")],
            inline: None,
        },
        #[cfg(feature = "css")]
        Language::Css => Spec {
            grammar: tree_sitter_css::LANGUAGE.into(),
            highlights: vec![
                q!("vendored/css/highlights.scm"),
                q!("hutan/css/highlights.scm"),
            ],
            folds: vec![q!("hutan/css/folds.scm")],
            indents: vec![q!("hutan/css/indents.scm")],
            inline: None,
        },
        #[cfg(feature = "c")]
        Language::C => Spec {
            grammar: tree_sitter_c::LANGUAGE.into(),
            highlights: vec![
                q!("vendored/c/highlights.scm"),
                q!("hutan/c/highlights.scm"),
            ],
            folds: vec![q!("hutan/c/folds.scm")],
            indents: vec![q!("hutan/c/indents.scm")],
            inline: None,
        },
        #[cfg(feature = "cpp")]
        Language::Cpp => Spec {
            grammar: tree_sitter_cpp::LANGUAGE.into(),
            highlights: vec![
                q!("vendored/c/highlights.scm"),
                q!("vendored/cpp/highlights.scm"),
                q!("hutan/c/highlights.scm"),
                q!("hutan/cpp/highlights.scm"),
            ],
            folds: vec![q!("hutan/c/folds.scm"), q!("hutan/cpp/folds.scm")],
            indents: vec![q!("hutan/c/indents.scm"), q!("hutan/cpp/indents.scm")],
            inline: None,
        },
        _ => return None,
    })
}

fn compile(
    language: Language,
    grammar: &tree_sitter::Language,
    kind: &'static str,
    parts: &[&str],
) -> Result<Query, SyntaxError> {
    Query::new(grammar, &parts.join("\n")).map_err(|e| SyntaxError::Query {
        language,
        query: kind,
        message: e.to_string(),
    })
}

fn tokens(query: &Query) -> Vec<Paint> {
    query
        .capture_names()
        .iter()
        .map(|name| match Token::from_capture(name) {
            Some(token) => Paint::Token(token),
            None if *name == "none" => Paint::Clear,
            None => Paint::Skip,
        })
        .collect()
}

fn build(language: Language) -> Result<Config, SyntaxError> {
    let spec = spec(language).ok_or(SyntaxError::NotCompiled(language))?;
    let grammar = spec.grammar;
    // A grammar from a newer ABI than the runtime supports fails here, not at the first parse.
    tree_sitter::Parser::new()
        .set_language(&grammar)
        .map_err(|e| SyntaxError::Grammar {
            language,
            message: e.to_string(),
        })?;
    let highlights = compile(language, &grammar, "highlights", &spec.highlights)?;
    let folds = compile(language, &grammar, "folds", &spec.folds)?;
    let indents = compile(language, &grammar, "indents", &spec.indents)?;
    let inline = match spec.inline {
        None => None,
        Some((inline_grammar, parts)) => {
            let query = compile(language, &inline_grammar, "inline highlights", &parts)?;
            Some(InlineConfig {
                nodes: compile(language, &grammar, "inline nodes", &["(inline) @inline"])?,
                grammar: inline_grammar,
                highlight_tokens: tokens(&query),
                highlights: query,
            })
        }
    };
    Ok(Config {
        language,
        grammar,
        highlight_tokens: tokens(&highlights),
        highlights,
        fold_capture: folds.capture_index_for_name("fold"),
        fold_body_capture: folds.capture_index_for_name("fold.body"),
        folds,
        indent_capture: indents.capture_index_for_name("indent"),
        outdent_capture: indents.capture_index_for_name("outdent"),
        indents,
        inline,
    })
}

static CONFIGS: [OnceLock<Result<Config, SyntaxError>>; Language::ALL.len()] =
    [const { OnceLock::new() }; Language::ALL.len()];

/// The compiled grammar and queries of `language`, built on first use (compiling the queries of
/// a large grammar takes a few milliseconds).
pub(crate) fn config(language: Language) -> Result<&'static Config, SyntaxError> {
    let slot = &CONFIGS[language as usize];
    slot.get_or_init(|| build(language))
        .as_ref()
        .map_err(Clone::clone)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_index_follows_declaration_order() {
        for (index, language) in Language::ALL.into_iter().enumerate() {
            assert_eq!(language as usize, index, "{language}");
        }
    }
}
