//! The small token table highlights are reduced to. Grammar queries use hundreds of capture names
//! (`@function.builtin`, `@text.title`, `@keyword.control.return`); the editor themes only the
//! tokens below (`syntax.*` in the color packs, E-4), so every capture name is mapped to one of
//! them by [`Token::from_capture`] or dropped.

use std::fmt;

/// A themable syntax token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Token {
    /// `syntax.comment`
    Comment,
    /// `syntax.string` (also character literals, raw text, dates)
    String,
    /// `syntax.string.escape`
    StringEscape,
    /// `syntax.number`
    Number,
    /// `syntax.constant` (`true`, `null`, `NONE`, enum-like constants)
    Constant,
    /// `syntax.keyword` (also `self`/`this`)
    Keyword,
    /// `syntax.operator`
    Operator,
    /// `syntax.function` (definitions, calls, macros)
    Function,
    /// `syntax.function.method`
    FunctionMethod,
    /// `syntax.type` (types, constructors, namespaces, lifetimes)
    Type,
    /// `syntax.type.builtin`
    TypeBuiltin,
    /// `syntax.variable`
    Variable,
    /// `syntax.variable.parameter`
    VariableParameter,
    /// `syntax.property` (fields, keys)
    Property,
    /// `syntax.attribute`
    Attribute,
    /// `syntax.tag` (HTML/JSX tag names, CSS selectors)
    Tag,
    /// `syntax.punctuation` (delimiters, separators)
    Punctuation,
    /// `syntax.punctuation.bracket`
    PunctuationBracket,
    /// `syntax.markup.heading`
    MarkupHeading,
    /// `syntax.markup.bold`
    MarkupBold,
    /// `syntax.markup.italic`
    MarkupItalic,
    /// `syntax.markup.link`
    MarkupLink,
}

impl Token {
    /// Every token, in declaration order.
    pub const ALL: [Token; 22] = [
        Token::Comment,
        Token::String,
        Token::StringEscape,
        Token::Number,
        Token::Constant,
        Token::Keyword,
        Token::Operator,
        Token::Function,
        Token::FunctionMethod,
        Token::Type,
        Token::TypeBuiltin,
        Token::Variable,
        Token::VariableParameter,
        Token::Property,
        Token::Attribute,
        Token::Tag,
        Token::Punctuation,
        Token::PunctuationBracket,
        Token::MarkupHeading,
        Token::MarkupBold,
        Token::MarkupItalic,
        Token::MarkupLink,
    ];

    /// The theme token name (`syntax.keyword`), the key of the color packs.
    pub const fn name(self) -> &'static str {
        match self {
            Token::Comment => "syntax.comment",
            Token::String => "syntax.string",
            Token::StringEscape => "syntax.string.escape",
            Token::Number => "syntax.number",
            Token::Constant => "syntax.constant",
            Token::Keyword => "syntax.keyword",
            Token::Operator => "syntax.operator",
            Token::Function => "syntax.function",
            Token::FunctionMethod => "syntax.function.method",
            Token::Type => "syntax.type",
            Token::TypeBuiltin => "syntax.type.builtin",
            Token::Variable => "syntax.variable",
            Token::VariableParameter => "syntax.variable.parameter",
            Token::Property => "syntax.property",
            Token::Attribute => "syntax.attribute",
            Token::Tag => "syntax.tag",
            Token::Punctuation => "syntax.punctuation",
            Token::PunctuationBracket => "syntax.punctuation.bracket",
            Token::MarkupHeading => "syntax.markup.heading",
            Token::MarkupBold => "syntax.markup.bold",
            Token::MarkupItalic => "syntax.markup.italic",
            Token::MarkupLink => "syntax.markup.link",
        }
    }

    /// The token a query capture name (without the `@`) stands for, or `None` when the capture
    /// has no visual meaning (`@spell`, `@embedded`, `@local.*`, names starting with `_`,
    /// unknown names). `@none` is not a token either, but the highlighter treats it as "remove
    /// the highlight of the enclosing captures".
    ///
    /// The longest dotted prefix wins, so `keyword.control.return` is a keyword and
    /// `function.method.call` a method; the older `text.*` names of `nvim-treesitter` are
    /// understood too.
    pub fn from_capture(capture: &str) -> Option<Token> {
        if capture.starts_with('_') {
            return None;
        }
        let mut prefix = capture;
        loop {
            if let Some(found) = exact(prefix) {
                return found;
            }
            let dot = prefix.rfind('.')?;
            prefix = &prefix[..dot];
        }
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// `Some(Some(token))` mapped, `Some(None)` explicitly invisible, `None` unknown (try a shorter
/// prefix).
fn exact(name: &str) -> Option<Option<Token>> {
    let token = match name {
        "comment" => Token::Comment,
        "string.escape" | "escape" => Token::StringEscape,
        "string.special.key" => Token::Property,
        "string" | "character" | "text.literal" | "markup.raw" => Token::String,
        "number" | "float" => Token::Number,
        "constant" | "boolean" => Token::Constant,
        "keyword" | "conditional" | "repeat" | "include" | "exception" | "variable.builtin" => {
            Token::Keyword
        }
        "operator" => Token::Operator,
        "function.method" | "method" => Token::FunctionMethod,
        "function" => Token::Function,
        "type.builtin" => Token::TypeBuiltin,
        "type" | "constructor" | "namespace" | "module" | "label" => Token::Type,
        "variable.parameter" | "parameter" => Token::VariableParameter,
        "variable" => Token::Variable,
        "property" | "field" => Token::Property,
        "attribute" | "tag.attribute" => Token::Attribute,
        "tag" => Token::Tag,
        "punctuation.bracket" | "tag.delimiter" => Token::PunctuationBracket,
        "punctuation" | "delimiter" => Token::Punctuation,
        "markup.heading" | "text.title" => Token::MarkupHeading,
        "markup.bold" | "markup.strong" | "text.strong" => Token::MarkupBold,
        "markup.italic" | "markup.emphasis" | "text.emphasis" => Token::MarkupItalic,
        "markup.link" | "text.uri" | "text.reference" | "link_text" | "link_uri" => {
            Token::MarkupLink
        }
        "none" | "spell" | "nospell" | "embedded" | "conceal" | "local" => return Some(None),
        _ => return None,
    };
    Some(Some(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_prefix_wins() {
        assert_eq!(Token::from_capture("keyword"), Some(Token::Keyword));
        assert_eq!(
            Token::from_capture("keyword.control.return"),
            Some(Token::Keyword)
        );
        assert_eq!(
            Token::from_capture("function.method.call"),
            Some(Token::FunctionMethod)
        );
        assert_eq!(Token::from_capture("function.macro"), Some(Token::Function));
        assert_eq!(
            Token::from_capture("string.special.key"),
            Some(Token::Property)
        );
        assert_eq!(Token::from_capture("string.special"), Some(Token::String));
        assert_eq!(
            Token::from_capture("punctuation.delimiter"),
            Some(Token::Punctuation)
        );
        assert_eq!(
            Token::from_capture("punctuation.bracket"),
            Some(Token::PunctuationBracket)
        );
        assert_eq!(
            Token::from_capture("variable.builtin"),
            Some(Token::Keyword)
        );
        assert_eq!(
            Token::from_capture("text.title"),
            Some(Token::MarkupHeading)
        );
    }

    #[test]
    fn invisible_and_unknown_captures_map_to_nothing() {
        for name in [
            "none",
            "spell",
            "embedded",
            "local.definition",
            "_hidden",
            "whatever",
        ] {
            assert_eq!(Token::from_capture(name), None, "{name}");
        }
    }

    #[test]
    fn names_are_unique_and_namespaced() {
        let mut names: Vec<_> = Token::ALL.iter().map(|t| t.name()).collect();
        assert!(names.iter().all(|n| n.starts_with("syntax.")));
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Token::ALL.len());
    }
}
