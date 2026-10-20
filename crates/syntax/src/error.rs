use crate::language::Language;

/// Why syntax could not be set up or a parse did not finish.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SyntaxError {
    /// The grammar of this language is not compiled in (its cargo feature is off).
    #[error("the {} grammar is not compiled in (enable the `{}` feature)", .0.name(), .0.feature())]
    NotCompiled(Language),
    /// The grammar's ABI is not supported by the linked tree-sitter runtime.
    #[error("the {} grammar cannot be loaded: {message}", .language.name())]
    Grammar {
        /// The language.
        language: Language,
        /// The runtime's message.
        message: String,
    },
    /// One of the language's query files does not compile against its grammar.
    #[error("the {} {query} query does not compile: {message}", .language.name())]
    Query {
        /// The language.
        language: Language,
        /// Which query (`highlights`, `folds`, `indents`, `inline highlights`).
        query: &'static str,
        /// The runtime's message with the row and column.
        message: String,
    },
    /// The parse was cancelled (a newer version replaced it) or the runtime gave up.
    #[error("the parse was cancelled")]
    Cancelled,
    /// The parse thread could not be started.
    #[error("cannot start the parse thread: {0}")]
    Thread(String),
    /// An edit does not fit the text it was applied to (range past the end, wrong order).
    #[error("invalid edit: {0}")]
    InvalidEdit(&'static str),
}
