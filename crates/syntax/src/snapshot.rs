//! An immutable parsed version of a text and everything that reads it: highlights, nodes,
//! structural selection, folds, bracket matching and indentation.

use std::cmp::Reverse;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use tree_sitter::{QueryCursor, StreamingIterator, Tree};

use crate::config::{Config, Paint, config};
use crate::error::SyntaxError;
use crate::language::Language;
use crate::parse::Parsers;
use crate::text::{NodeText, Point, TextEdit, TextProvider, from_ts_point};
use crate::token::Token;

/// A run of text with one token. Spans of a [`Snapshot::highlights`] result are sorted and do not
/// overlap; text between them has no token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HighlightSpan {
    /// Byte range.
    pub range: Range<usize>,
    /// What the range is.
    pub token: Token,
}

/// A foldable region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fold {
    /// The bytes a folded view replaces by a placeholder: the inside of a bracketed node
    /// (`{` and `}` stay visible), the rest of the node after its first line, or a body below its
    /// header line.
    pub range: Range<usize>,
    /// Row of `range.start`, the header row that stays visible.
    pub start_row: usize,
    /// Row of `range.end`.
    pub end_row: usize,
}

/// Two matching brackets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BracketPair {
    /// The opening bracket.
    pub open: Range<usize>,
    /// The closing bracket.
    pub close: Range<usize>,
}

/// A syntax tree node borrowed from a [`Snapshot`].
#[derive(Clone, Copy)]
pub struct SyntaxNode<'a>(tree_sitter::Node<'a>);

impl fmt::Debug for SyntaxNode<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyntaxNode")
            .field("kind", &self.kind())
            .field("range", &self.range())
            .finish()
    }
}

impl<'a> SyntaxNode<'a> {
    /// The grammar's name of the node (`function_item`, `{`).
    pub fn kind(&self) -> &'a str {
        self.0.kind()
    }

    /// Byte range.
    pub fn range(&self) -> Range<usize> {
        self.0.byte_range()
    }

    /// Row and byte column of the start.
    pub fn start_point(&self) -> Point {
        from_ts_point(self.0.start_position())
    }

    /// Row and byte column of the end.
    pub fn end_point(&self) -> Point {
        from_ts_point(self.0.end_position())
    }

    /// Whether the grammar names this node (as opposed to punctuation and keywords).
    pub fn is_named(&self) -> bool {
        self.0.is_named()
    }

    /// Whether this is an `ERROR` node (text the grammar could not parse).
    pub fn is_error(&self) -> bool {
        self.0.is_error()
    }

    /// Whether the parser inserted this node to recover (zero width).
    pub fn is_missing(&self) -> bool {
        self.0.is_missing()
    }

    /// Whether this node or a descendant is an error or missing.
    pub fn has_error(&self) -> bool {
        self.0.has_error()
    }

    /// The parent, `None` at the root.
    pub fn parent(&self) -> Option<SyntaxNode<'a>> {
        self.0.parent().map(SyntaxNode)
    }

    /// The next sibling.
    pub fn next_sibling(&self) -> Option<SyntaxNode<'a>> {
        self.0.next_sibling().map(SyntaxNode)
    }

    /// The previous sibling.
    pub fn prev_sibling(&self) -> Option<SyntaxNode<'a>> {
        self.0.prev_sibling().map(SyntaxNode)
    }

    /// All children, anonymous ones included.
    pub fn children(&self) -> Vec<SyntaxNode<'a>> {
        let mut cursor = self.0.walk();
        self.0.children(&mut cursor).map(SyntaxNode).collect()
    }

    /// The named children.
    pub fn named_children(&self) -> Vec<SyntaxNode<'a>> {
        let mut cursor = self.0.walk();
        self.0.named_children(&mut cursor).map(SyntaxNode).collect()
    }

    /// The child a grammar field names (`name`, `body`), if present.
    pub fn child_by_field_name(&self, field: &str) -> Option<SyntaxNode<'a>> {
        self.0.child_by_field_name(field).map(SyntaxNode)
    }
}

/// A text parsed at one version. Immutable and shareable across threads; every query reads the
/// tree it was built from, so results are in the coordinates of [`Snapshot::text`] at
/// [`Snapshot::version`].
pub struct Snapshot {
    config: &'static Config,
    version: u64,
    tree: Tree,
    text: Arc<dyn TextProvider>,
}

impl fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Snapshot")
            .field("language", &self.config.language)
            .field("version", &self.version)
            .field("len", &self.text.len())
            .finish()
    }
}

impl Snapshot {
    /// Parses `text` from scratch, on the calling thread.
    pub fn parse(
        language: Language,
        text: impl TextProvider + 'static,
        version: u64,
    ) -> Result<Snapshot, SyntaxError> {
        Self::parse_shared(language, Arc::new(text), version, &|| false)
    }

    /// Re-parses after `edits` (in order, each in the coordinates after the previous one) turned
    /// the text of this snapshot into `text`, reusing the unchanged subtrees. The result equals a
    /// fresh parse of `text`.
    pub fn reparse(
        &self,
        edits: &[TextEdit],
        text: impl TextProvider + 'static,
        version: u64,
    ) -> Result<Snapshot, SyntaxError> {
        self.reparse_shared(edits, Arc::new(text), version, &|| false)
    }

    pub(crate) fn parse_shared(
        language: Language,
        text: Arc<dyn TextProvider>,
        version: u64,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Snapshot, SyntaxError> {
        let config = config(language)?;
        let mut parsers = Parsers::new(config)?;
        Self::parse_with(config, &mut parsers, None, text, version, cancelled)
    }

    pub(crate) fn reparse_shared(
        &self,
        edits: &[TextEdit],
        text: Arc<dyn TextProvider>,
        version: u64,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Snapshot, SyntaxError> {
        let mut parsers = Parsers::new(self.config)?;
        let tree = self.edited_tree(edits);
        Self::parse_with(
            self.config,
            &mut parsers,
            Some(&tree),
            text,
            version,
            cancelled,
        )
    }

    /// This snapshot's tree with `edits` applied: the old tree of the next parse.
    pub(crate) fn edited_tree(&self, edits: &[TextEdit]) -> Tree {
        let mut tree = self.tree.clone();
        for edit in edits {
            tree.edit(&edit.to_input_edit());
        }
        tree
    }

    /// The worker's entry point: a long-lived `Parsers`, an already edited old tree.
    pub(crate) fn parse_with(
        config: &'static Config,
        parsers: &mut Parsers,
        old: Option<&Tree>,
        text: Arc<dyn TextProvider>,
        version: u64,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Snapshot, SyntaxError> {
        let tree = parsers.parse(&*text, old, cancelled)?;
        Ok(Snapshot {
            config,
            version,
            tree,
            text,
        })
    }

    /// The version tag this snapshot was parsed for.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The language.
    pub fn language(&self) -> Language {
        self.config.language
    }

    /// The text this snapshot was parsed from.
    pub fn text(&self) -> &dyn TextProvider {
        &*self.text
    }

    /// The tree's root node.
    pub fn root(&self) -> SyntaxNode<'_> {
        SyntaxNode(self.tree.root_node())
    }

    /// Whether the text has syntax errors (the tree is still usable).
    pub fn has_errors(&self) -> bool {
        self.tree.root_node().has_error()
    }

    /// The tree as an S-expression, for tests and debugging. (Markdown's inline layer is parsed
    /// per highlight request and is not part of it.)
    pub fn to_sexp(&self) -> String {
        self.tree.root_node().to_sexp()
    }

    // ---- highlights -------------------------------------------------------------------------

    /// The highlighted spans intersecting `range`, clipped to it, sorted and non-overlapping.
    ///
    /// Where several captures cover the same text the innermost node wins, and between captures
    /// of the same node the later pattern of the query wins. Cost is proportional to the nodes in
    /// `range` (plus the path to them), not to the file.
    pub fn highlights(&self, range: Range<usize>) -> Vec<HighlightSpan> {
        let len = self.text.len();
        let range = range.start.min(len)..range.end.min(len);
        if range.start >= range.end {
            return Vec::new();
        }
        let mut captures = Vec::new();
        self.collect_captures(
            &self.config.highlights,
            &self.config.highlight_tokens,
            &self.tree,
            0,
            &range,
            &mut captures,
        );
        self.collect_inline_captures(&range, &mut captures);
        resolve(captures, &range)
    }

    /// Markdown: parses the inline content (emphasis, links, code spans) of the paragraphs that
    /// touch `range` with the inline grammar and adds its captures. Done per request on the
    /// requested part only, so a large document costs what its viewport costs.
    fn collect_inline_captures(&self, range: &Range<usize>, out: &mut Vec<RawCapture>) {
        let Some(inline) = &self.config.inline else {
            return;
        };
        let mut ranges = Vec::new();
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());
        let mut matches =
            cursor.matches(&inline.nodes, self.tree.root_node(), NodeText(&*self.text));
        while let Some(found) = matches.next() {
            for capture in found.captures() {
                push_inline_ranges(capture.node, &mut ranges);
            }
        }
        if ranges.is_empty() {
            return;
        }
        ranges.sort_by_key(|r| r.start_byte);
        ranges.dedup_by_key(|r| r.start_byte);
        let mut parser = tree_sitter::Parser::new();
        if parser.set_language(&inline.grammar).is_err()
            || parser.set_included_ranges(&ranges).is_err()
        {
            // Ranges come from one tree, sorted and disjoint; refusing them would be a runtime
            // bug, and the block layer alone is still a correct (if plainer) highlight.
            return;
        }
        let text = &*self.text;
        let Some(tree) = parser.parse_with_options(&mut |offset, _| text.chunk(offset), None, None)
        else {
            return;
        };
        self.collect_captures(
            &inline.highlights,
            &inline.highlight_tokens,
            &tree,
            INLINE_PRIORITY,
            range,
            out,
        );
    }

    fn collect_captures(
        &self,
        query: &tree_sitter::Query,
        tokens: &[Paint],
        tree: &Tree,
        priority_base: u32,
        range: &Range<usize>,
        out: &mut Vec<RawCapture>,
    ) {
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());
        let mut matches = cursor.matches(query, tree.root_node(), NodeText(&*self.text));
        while let Some(found) = matches.next() {
            for capture in found.captures() {
                let token = match tokens.get(capture.index as usize) {
                    Some(Paint::Token(token)) => Some(*token),
                    Some(Paint::Clear) => None,
                    _ => continue,
                };
                let node = capture.node.byte_range();
                if node.start >= node.end || node.end <= range.start || node.start >= range.end {
                    continue;
                }
                out.push(RawCapture {
                    start: node.start,
                    end: node.end,
                    priority: priority_base + found.pattern_index as u32,
                    token,
                });
            }
        }
    }

    // ---- nodes and structural selection ------------------------------------------------------

    /// The smallest named node covering `range` (for a cursor, an empty range).
    pub fn node_at(&self, range: Range<usize>) -> Option<SyntaxNode<'_>> {
        let len = self.text.len();
        let (start, end) = (range.start.min(len), range.end.min(len));
        self.tree
            .root_node()
            .named_descendant_for_byte_range(start, end.max(start))
            .map(SyntaxNode)
    }

    /// The smallest node, anonymous tokens included, covering `range`.
    pub fn leaf_at(&self, range: Range<usize>) -> Option<SyntaxNode<'_>> {
        let len = self.text.len();
        let (start, end) = (range.start.min(len), range.end.min(len));
        self.tree
            .root_node()
            .descendant_for_byte_range(start, end.max(start))
            .map(SyntaxNode)
    }

    /// The range of the smallest named node that strictly contains `range`: what "expand
    /// selection" selects. `None` when `range` already is the root.
    pub fn expand_selection(&self, range: Range<usize>) -> Option<Range<usize>> {
        let mut node = self.node_at(range.clone())?;
        loop {
            let candidate = node.range();
            if candidate.start <= range.start && candidate.end >= range.end && candidate != range {
                return Some(candidate);
            }
            node = node.parent()?;
        }
    }

    /// The range of the first named child of the node `range` selects exactly: what "shrink
    /// selection" selects. `None` when `range` is not a whole node or the node has no named child.
    ///
    /// Expanding and shrinking are not inverses (expansion skips parents with the same range, and
    /// shrinking always takes the first child); a caller that wants to return to the earlier
    /// selection keeps its own stack.
    pub fn shrink_selection(&self, range: Range<usize>) -> Option<Range<usize>> {
        let node = self.node_at(range.clone())?;
        if node.range() != range {
            return None;
        }
        node.named_children()
            .into_iter()
            .map(|child| child.range())
            .find(|child| *child != range)
    }

    // ---- brackets ---------------------------------------------------------------------------

    /// The bracket at `offset` (the character after the cursor) or, failing that, just before it,
    /// with its partner, found through the tree: brackets inside strings and comments are not
    /// tokens and never match, and a bracket without a partner (a comparison `<`) returns `None`.
    pub fn bracket_match(&self, offset: usize) -> Option<BracketPair> {
        self.bracket_at(offset).or_else(|| {
            offset
                .checked_sub(1)
                .and_then(|before| self.bracket_at(before))
        })
    }

    fn bracket_at(&self, offset: usize) -> Option<BracketPair> {
        if offset >= self.text.len() {
            return None;
        }
        let node = self
            .tree
            .root_node()
            .descendant_for_byte_range(offset, offset + 1)?;
        if node.child_count() != 0 || node.is_missing() {
            return None;
        }
        let kind = node.kind();
        let (open, close) = BRACKETS
            .iter()
            .copied()
            .find(|(open, close)| kind == *open || kind == *close)?;
        let forward = kind == open;
        let mut depth = 0usize;
        let mut sibling = if forward {
            node.next_sibling()
        } else {
            node.prev_sibling()
        };
        while let Some(candidate) = sibling {
            if !candidate.is_missing() {
                let same = if forward { open } else { close };
                let other = if forward { close } else { open };
                if candidate.kind() == same {
                    depth += 1;
                } else if candidate.kind() == other {
                    if depth == 0 {
                        let (open_node, close_node) = if forward {
                            (node, candidate)
                        } else {
                            (candidate, node)
                        };
                        return Some(BracketPair {
                            open: open_node.byte_range(),
                            close: close_node.byte_range(),
                        });
                    }
                    depth -= 1;
                }
            }
            sibling = if forward {
                candidate.next_sibling()
            } else {
                candidate.prev_sibling()
            };
        }
        None
    }

    // ---- folds ------------------------------------------------------------------------------

    /// The foldable regions intersecting `range`, outermost first, each at most once.
    pub fn folds(&self, range: Range<usize>) -> Vec<Fold> {
        let len = self.text.len();
        let range = range.start.min(len)..range.end.min(len);
        let mut folds = Vec::new();
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());
        let mut matches = cursor.matches(
            &self.config.folds,
            self.tree.root_node(),
            NodeText(&*self.text),
        );
        while let Some(found) = matches.next() {
            for capture in found.captures() {
                let body = Some(capture.index) == self.config.fold_body_capture;
                if !body && Some(capture.index) != self.config.fold_capture {
                    continue;
                }
                if let Some(fold) = self.fold_of(capture.node, body) {
                    folds.push(fold);
                }
            }
        }
        folds.sort_by_key(|f| (f.range.start, Reverse(f.range.end)));
        folds.dedup_by(|a, b| a.range == b.range);
        folds
    }

    fn fold_of(&self, node: tree_sitter::Node<'_>, body: bool) -> Option<Fold> {
        let (start, end) = (node.start_byte(), node.end_byte());
        let start_row = node.start_position().row;
        let end_row = node.end_position().row;
        if end <= start {
            return None;
        }
        let text = &*self.text;
        let (hidden, header_row) = if body {
            let line_start = text.line_start(start_row);
            let only_indent =
                (line_start..start).all(|i| matches!(byte_at(text, i), Some(b' ' | b'\t')));
            if only_indent && start_row > 0 {
                // A body on its own lines hides below the header line, even a one-line body.
                (line_end_before(text, line_start)?..end, start_row - 1)
            } else if start_row < end_row {
                (first_line_end(text, start, end)?..end, start_row)
            } else {
                return None;
            }
        } else {
            if start_row >= end_row {
                return None;
            }
            let open = byte_at(text, start)?;
            let last = byte_at(text, end - 1)?;
            let bracketed = matches!((open, last), (b'{', b'}') | (b'[', b']') | (b'(', b')'));
            if bracketed && end - start >= 2 {
                (start + 1..end - 1, start_row)
            } else {
                (first_line_end(text, start, end)?..end, start_row)
            }
        };
        if hidden.start >= hidden.end {
            return None;
        }
        Some(Fold {
            range: hidden,
            start_row: header_row,
            end_row,
        })
    }

    // ---- indentation ------------------------------------------------------------------------

    /// The structural indent level of line `row`: how many indent scopes of `indents.scm` started
    /// on an earlier row and still contain the line's first token, less one when that token
    /// closes a scope (`}`, `else:`). A level, not a column: multiply by the indent unit. Blank
    /// lines get the level their content would have.
    pub fn indent_level(&self, row: usize) -> usize {
        let text = &*self.text;
        let len = text.len();
        let line_start = text.line_start(row);
        let mut probe = line_start;
        while probe < len && matches!(byte_at(text, probe), Some(b' ' | b'\t')) {
            probe += 1;
        }
        let query_range = probe.saturating_sub(1)..(probe + 1).min(len).max(probe);
        let mut rows: Vec<usize> = Vec::new();
        let mut outdent = false;
        self.scan_indents(query_range, |node, is_indent| {
            if is_indent {
                let contains = node.start_byte() < probe
                    && node.start_position().row < row
                    && (node.end_byte() > probe
                        || (node.end_byte() == probe && ends_with_missing(node)));
                if contains {
                    push_distinct(&mut rows, node.start_position().row);
                }
            } else if node.start_byte() == probe && node.end_byte() > probe {
                outdent = true;
            }
        });
        rows.len().saturating_sub(usize::from(outdent))
    }

    /// The structural indent level of a line created by a line break inserted at `offset`: the
    /// scopes that started before `offset` and are still open there. Use [`Snapshot::indent_level`]
    /// on the new row after the break for the text that moved down with it (a closing `}`).
    pub fn indent_level_after(&self, offset: usize) -> usize {
        let len = self.text.len();
        let offset = offset.min(len);
        let mut rows: Vec<usize> = Vec::new();
        self.scan_indents(
            offset.saturating_sub(1)..(offset + 1).min(len).max(offset),
            |node, is_indent| {
                if is_indent
                    && node.start_byte() < offset
                    && (node.end_byte() > offset
                        || (node.end_byte() == offset && ends_with_missing(node)))
                {
                    push_distinct(&mut rows, node.start_position().row);
                }
            },
        );
        self.unclosed_brackets(offset, &mut rows);
        rows.len()
    }

    /// Brackets opened before `offset` and never closed inside an `ERROR` node (`fn main() {`
    /// without its `}` parses as an error, with no block to carry an indent scope): each opens a
    /// scope on its row.
    fn unclosed_brackets(&self, offset: usize, rows: &mut Vec<usize>) {
        let mut node = self
            .tree
            .root_node()
            .descendant_for_byte_range(offset.saturating_sub(1), offset);
        while let Some(current) = node {
            if current.is_error() {
                let mut open: Vec<(&str, usize)> = Vec::new();
                let mut cursor = current.walk();
                for child in current.children(&mut cursor) {
                    if child.end_byte() > offset {
                        break;
                    }
                    match child.kind() {
                        kind @ ("{" | "(" | "[") => {
                            open.push((kind, child.start_position().row));
                        }
                        "}" | ")" | "]" => {
                            open.pop();
                        }
                        _ => {}
                    }
                }
                for (_, row) in open {
                    push_distinct(rows, row);
                }
            }
            node = current.parent();
        }
    }

    fn scan_indents(
        &self,
        range: Range<usize>,
        mut visit: impl FnMut(tree_sitter::Node<'_>, bool),
    ) {
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range);
        let mut matches = cursor.matches(
            &self.config.indents,
            self.tree.root_node(),
            NodeText(&*self.text),
        );
        while let Some(found) = matches.next() {
            for capture in found.captures() {
                if Some(capture.index) == self.config.indent_capture {
                    visit(capture.node, true);
                } else if Some(capture.index) == self.config.outdent_capture {
                    visit(capture.node, false);
                }
            }
        }
    }
}

const INLINE_PRIORITY: u32 = 1 << 20;

/// The ranges of one block-layer `inline` node the inline grammar parses: the node's own range
/// minus the container markers (`block_continuation`, a blockquote's `>`) inside it.
fn push_inline_ranges(node: tree_sitter::Node<'_>, ranges: &mut Vec<tree_sitter::Range>) {
    let mut at_byte = node.start_byte();
    let mut at_point = node.start_position();
    let mut walk = node.walk();
    for child in node.children(&mut walk) {
        if child.kind() != "block_continuation" {
            continue;
        }
        if child.start_byte() > at_byte {
            ranges.push(tree_sitter::Range {
                start_byte: at_byte,
                end_byte: child.start_byte(),
                start_point: at_point,
                end_point: child.start_position(),
            });
        }
        at_byte = child.end_byte();
        at_point = child.end_position();
    }
    if node.end_byte() > at_byte {
        ranges.push(tree_sitter::Range {
            start_byte: at_byte,
            end_byte: node.end_byte(),
            start_point: at_point,
            end_point: node.end_position(),
        });
    }
}

const BRACKETS: [(&str, &str); 5] = [("(", ")"), ("[", "]"), ("{", "}"), ("<", ">"), ("[[", "]]")];

struct RawCapture {
    start: usize,
    end: usize,
    priority: u32,
    /// `None` clears the highlight of enclosing captures (`@none`).
    token: Option<Token>,
}

/// Flattens nested captures into sorted, disjoint spans: inside a capture the innermost wins,
/// among captures of the same range the one from the later pattern.
fn resolve(mut captures: Vec<RawCapture>, range: &Range<usize>) -> Vec<HighlightSpan> {
    captures.sort_by_key(|c| (c.start, Reverse(c.end), c.priority));
    let mut out: Vec<HighlightSpan> = Vec::new();
    let mut stack: Vec<(usize, Option<Token>)> = Vec::new();
    let mut pos = range.start;

    let emit = |from: usize, to: usize, token: Option<Token>, out: &mut Vec<HighlightSpan>| {
        let Some(token) = token else {
            return;
        };
        let (from, to) = (from.max(range.start), to.min(range.end));
        if from >= to {
            return;
        }
        if let Some(last) = out.last_mut()
            && last.token == token
            && last.range.end == from
        {
            last.range.end = to;
            return;
        }
        out.push(HighlightSpan {
            range: from..to,
            token,
        });
    };

    for capture in &captures {
        while let Some(&(end, token)) = stack.last() {
            if end > capture.start {
                break;
            }
            emit(pos, end, token, &mut out);
            pos = pos.max(end);
            stack.pop();
        }
        if let Some(&(_, token)) = stack.last() {
            emit(pos, capture.start, token, &mut out);
        }
        pos = pos.max(capture.start);
        // Nodes nest or are disjoint, so a capture never outlives the one below it; clamping
        // keeps the stack well-formed even if a query ever produced partial overlaps.
        let end = stack
            .last()
            .map_or(capture.end, |&(outer, _)| capture.end.min(outer));
        stack.push((end, capture.token));
    }
    while let Some((end, token)) = stack.pop() {
        emit(pos, end, token, &mut out);
        pos = pos.max(end);
    }
    out
}

fn push_distinct(rows: &mut Vec<usize>, row: usize) {
    if !rows.contains(&row) {
        rows.push(row);
    }
}

/// Whether the node's last token is one the parser made up (a missing `}`) or a zero-width body
/// (an empty Python block): the node is unfinished, not complete before `offset`.
fn ends_with_missing(node: tree_sitter::Node<'_>) -> bool {
    let count = node.child_count();
    count > 0
        && node.child(count - 1).is_some_and(|last| {
            last.is_missing() || last.start_byte() == last.end_byte() || ends_with_missing(last)
        })
}

fn byte_at(text: &dyn TextProvider, offset: usize) -> Option<u8> {
    text.chunk(offset).first().copied()
}

/// The offset of the line break that ends the line before the line starting at `line_start`
/// (before a `\r\n`'s `\r`), `None` for the first line.
fn line_end_before(text: &dyn TextProvider, line_start: usize) -> Option<usize> {
    let newline = line_start.checked_sub(1)?;
    if newline > 0 && byte_at(text, newline - 1) == Some(b'\r') {
        Some(newline - 1)
    } else {
        Some(newline)
    }
}

/// The end of the line `from` is on (before its `\r\n`), searching up to `limit`.
fn first_line_end(text: &dyn TextProvider, from: usize, limit: usize) -> Option<usize> {
    let mut at = from;
    while at < limit {
        let chunk = text.chunk(at);
        if chunk.is_empty() {
            return None;
        }
        let window = &chunk[..chunk.len().min(limit - at)];
        if let Some(i) = window.iter().position(|b| *b == b'\n') {
            let newline = at + i;
            let end = if newline > from && byte_at(text, newline - 1) == Some(b'\r') {
                newline - 1
            } else {
                newline
            };
            return Some(end);
        }
        at += window.len();
    }
    None
}
