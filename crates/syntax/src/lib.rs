//! Syntax for Hutan's editor: tree-sitter parsing that follows edits, highlights reduced to a
//! small token table, and the structure queries an editor asks (nodes, expand/shrink selection,
//! folds, bracket matching, indentation). GPUI-free and I/O-free; the text arrives through
//! [`TextProvider`], so the crate does not depend on the buffer (the `buffer` feature adds the
//! impl for `hutan_buffer::Text`).
//!
//! # Pieces
//! - [`Language`]: the registry of up to 15 languages (Rust, TypeScript, TSX, JavaScript, Python,
//!   Go, JSON, TOML, YAML, Markdown, Bash, HTML, CSS, C, C++), each behind a cargo feature of the
//!   same name (`typescript` covers TSX); detection by path, name or shebang.
//! - [`Snapshot`]: an immutable parsed version. Every read is here: [`Snapshot::highlights`],
//!   [`Snapshot::node_at`], [`Snapshot::expand_selection`], [`Snapshot::shrink_selection`],
//!   [`Snapshot::folds`], [`Snapshot::bracket_match`], [`Snapshot::indent_level`]. Blocking
//!   [`Snapshot::parse`] / [`Snapshot::reparse`] are the synchronous entry points.
//! - [`Syntax`]: the handle an open file keeps. A worker thread parses the latest version; a newer
//!   version cancels the parse in flight; [`Syntax::view`] always answers at once with the newest
//!   finished tree and highlights carried across the edits made since.
//! - [`Token`]: the token table highlights are reduced to (`syntax.keyword`, …), themed by the
//!   color packs (E-4).
//! - [`TextEdit`]: an edit with the rows and columns tree-sitter needs.
//!
//! All offsets are **byte offsets** into UTF-8 text; rows and columns are zero-based with byte
//! columns. Query data (`queries/`) is vendored from the grammar crates verbatim, with Hutan's
//! own overlays next to it; see `queries/NOTICE.md`.
//!
//! ```
//! use hutan_syntax::{Language, Snapshot, Token};
//!
//! let source = "fn main() { let x = 1; }";
//! let snapshot = Snapshot::parse(Language::Rust, source.to_string(), 0)?;
//! let spans = snapshot.highlights(0..source.len());
//! assert_eq!(spans[0].token, Token::Keyword);
//! assert_eq!(&source[spans[0].range.clone()], "fn");
//! # Ok::<(), hutan_syntax::SyntaxError>(())
//! ```

mod config;
mod error;
mod language;
mod map;
mod parse;
mod snapshot;
mod syntax;
mod text;
mod token;

#[cfg(feature = "buffer")]
mod buffer_text;

pub use error::SyntaxError;
pub use language::Language;
pub use snapshot::{BracketPair, Fold, HighlightSpan, Snapshot, SyntaxNode};
pub use syntax::{MAX_LOGGED_EDITS, Syntax, SyntaxStats, SyntaxView};
pub use text::{Point, TextEdit, TextProvider};
pub use token::Token;

#[cfg(feature = "buffer")]
pub use buffer_text::edits_of_transaction;
