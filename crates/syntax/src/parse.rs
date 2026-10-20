//! The blocking parse core: one parser per language and cancellation. [`crate::Syntax`] runs this
//! on its worker thread; [`crate::Snapshot::parse`] runs it on the caller's.

use std::ops::ControlFlow;

use tree_sitter::{Parser, Tree};

use crate::config::Config;
use crate::error::SyntaxError;
use crate::text::TextProvider;

/// The parser of one language.
pub(crate) struct Parsers {
    parser: Parser,
}

impl Parsers {
    pub fn new(config: &Config) -> Result<Self, SyntaxError> {
        let mut parser = Parser::new();
        parser
            .set_language(&config.grammar)
            .map_err(|e| SyntaxError::Grammar {
                language: config.language,
                message: e.to_string(),
            })?;
        Ok(Self { parser })
    }

    /// Parses `text`, reusing `old` (a tree already edited to match `text`). `cancelled` is polled
    /// while parsing; when it returns true the parse stops and `Err(Cancelled)` is returned.
    pub fn parse(
        &mut self,
        text: &dyn TextProvider,
        old: Option<&Tree>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Tree, SyntaxError> {
        // Reading nothing ends the input at once, so a cancelled parse unwinds quickly even before
        // tree-sitter next polls the progress callback.
        let nothing: &[u8] = &[];
        let mut read = |offset: usize, _| {
            if cancelled() {
                nothing
            } else {
                text.chunk(offset)
            }
        };
        let mut progress = |_: &tree_sitter::ParseState| {
            if cancelled() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let options = tree_sitter::ParseOptions::new().progress_callback(&mut progress);
        let tree = self
            .parser
            .parse_with_options(&mut read, old, Some(options));
        if cancelled() || tree.is_none() {
            // A halted parse would otherwise resume at the next call.
            self.parser.reset();
            return Err(SyntaxError::Cancelled);
        }
        tree.ok_or(SyntaxError::Cancelled)
    }
}
