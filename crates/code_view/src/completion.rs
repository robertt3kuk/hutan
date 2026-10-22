//! Completion and signature help in a code view (slice E-12b). The view decides when to ask
//! and draws the answers; its owner talks to the language server.
//!
//! - **When it asks.** A trigger character of the server ([`CompletionTriggers`]) asks at
//!   once; a word character asks after [`COMPLETION_DEBOUNCE`] when no popup shows; ⌃Space
//!   (*Show Completions*, and ⌃N / ⌃P in Vim's Insert mode) asks at the caret. Nothing is asked
//!   while an input method composes, with several carets, or outside Insert in Vim.
//!   [`CodeViewEvent::CompletionRequested`] goes to the owner, which answers with
//!   [`CodeView::set_completions`]; a newer request makes older answers stale.
//! - **Filtering.** Typing more word characters filters the list here, without asking again,
//!   unless the server said the list is incomplete (then it is asked again, with the old list
//!   shown filtered meanwhile). Ranking: the server's `sort_text` (else the label), then the
//!   fuzzy score, then the server's order. A caret that leaves the word, or a character that is
//!   neither a word character nor a trigger, closes the popup.
//! - **Keys** (bound while the popup shows, key context `showing_completions`): ↑ ↓ and ⌃P ⌃N
//!   move, Return and Tab accept, Escape closes (the first Escape; Vim leaves Insert on the
//!   next one; an input method's composition ends before either).
//! - **Accepting** replaces from the item's edit start (else the start of the word) to the
//!   caret with its text, and applies its additional edits (an import), as **one** undo step.
//!   A snippet ([`crate::snippet`]) puts the caret on its first tab stop (every occurrence
//!   selected); Tab / ⇧Tab move between stops (key context `in_snippet`) until the last one.
//! - **Documentation.** The selected item's detail and documentation show to the right of the
//!   list; an item the server can tell more about is resolved first
//!   ([`CodeViewEvent::CompletionResolve`], [`CodeView::set_resolved_completion`]).
//! - **Signature help** shows above the caret after a signature trigger (`(`, `,`), and is
//!   asked again at every keystroke while it shows ([`CodeViewEvent::SignatureHelpRequested`],
//!   [`CodeView::set_signature_help`]); the active parameter is bold and underlined.

use std::collections::HashSet;
use std::ops::Range;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Anchor, AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, anchored, deferred, div,
    point, px,
};
use hutan_buffer::{Edit, Selection, Selections, Utf16Point};
use hutan_syntax::Token;

use crate::buffer::Coalesce;
use crate::hover::{HoverBlock, markdown_blocks};
use crate::rules::is_word;
use crate::snippet::{self, Snippet};
use crate::style::rgba;
use crate::view::{CodeView, CodeViewEvent};

/// How long typing a word waits before asking for completions when no popup shows.
pub const COMPLETION_DEBOUNCE: Duration = Duration::from_millis(20);
/// Rows of the popup shown at once.
pub const COMPLETION_ROWS: usize = 10;
/// Characters of a detail drawn in a row.
const DETAIL_CHARS: usize = 40;

/// What a completion item is; each has a glyph of its own shape (not only a colour).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompletionKind {
    Text,
    Method,
    Function,
    Constructor,
    Field,
    Variable,
    Class,
    Interface,
    Module,
    Property,
    Unit,
    Value,
    Enum,
    Keyword,
    Snippet,
    Color,
    File,
    Reference,
    Folder,
    EnumMember,
    Constant,
    Struct,
    Event,
    Operator,
    TypeParameter,
    Other,
}

impl CompletionKind {
    /// The glyph drawn before the label: a distinct character per kind (lowercase for values
    /// and members, uppercase for types).
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Text => "t",
            Self::Method => "m",
            Self::Function => "ƒ",
            Self::Constructor => "c",
            Self::Field | Self::Property => "p",
            Self::Variable => "v",
            Self::Class => "C",
            Self::Interface => "I",
            Self::Module => "M",
            Self::Unit => "u",
            Self::Value => "#",
            Self::Enum => "E",
            Self::Keyword => "k",
            Self::Snippet => "s",
            Self::Color => "◐",
            Self::File => "□",
            Self::Reference => "&",
            Self::Folder => "▸",
            Self::EnumMember => "e",
            Self::Constant => "K",
            Self::Struct => "S",
            Self::Event => "!",
            Self::Operator => "±",
            Self::TypeParameter => "T",
            Self::Other => "·",
        }
    }

    fn token(self) -> Token {
        match self {
            Self::Method => Token::FunctionMethod,
            Self::Function | Self::Constructor => Token::Function,
            Self::Class
            | Self::Interface
            | Self::Struct
            | Self::Enum
            | Self::TypeParameter
            | Self::Module => Token::Type,
            Self::Constant | Self::EnumMember | Self::Value | Self::Unit | Self::Color => {
                Token::Constant
            }
            Self::Keyword | Self::Snippet => Token::Keyword,
            Self::Operator => Token::Operator,
            Self::Text | Self::File | Self::Folder | Self::Reference => Token::String,
            Self::Field | Self::Property | Self::Variable | Self::Event | Self::Other => {
                Token::Variable
            }
        }
    }
}

/// A replacement of the text, in UTF-16 positions of the text the request was answered for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionEdit {
    pub start: Utf16Point,
    pub end: Utf16Point,
    pub text: String,
}

/// One completion item, as the owner hands it over.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompletionItem {
    pub label: String,
    pub kind: Option<CompletionKind>,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    pub sort_text: Option<String>,
    pub filter_text: Option<String>,
    pub insert_text: Option<String>,
    pub snippet: bool,
    pub edit: Option<CompletionEdit>,
    pub additional_edits: Vec<CompletionEdit>,
    pub preselect: bool,
    pub deprecated: bool,
    pub resolvable: bool,
}

/// An answer to a completion request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompletionList {
    /// The owner's name of the list ([`CodeViewEvent::CompletionResolve`] gives it back).
    pub id: u64,
    pub items: Vec<CompletionItem>,
    pub incomplete: bool,
}

/// What makes the view ask: whether the file has a language server, and its characters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompletionTriggers {
    pub enabled: bool,
    pub completion: Vec<char>,
    pub signature: Vec<char>,
}

/// One signature of a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub label: String,
    pub documentation: Option<String>,
    /// Byte ranges of the parameters in `label`.
    pub parameters: Vec<Range<usize>>,
    pub parameter_docs: Vec<Option<String>>,
    pub active_parameter: Option<usize>,
}

/// An answer to a signature help request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureHelp {
    pub signatures: Vec<Signature>,
    pub active: usize,
}

/// An item that matches what was typed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Match {
    index: usize,
    score: i32,
    /// Character indices of the label that matched.
    positions: Vec<usize>,
}

/// The completion popup while it is asked for or shows.
#[derive(Debug, Clone)]
pub struct CompletionState {
    request: u64,
    pending: bool,
    list: u64,
    /// Where the word being completed starts (bytes).
    word_start: usize,
    query: String,
    items: Vec<CompletionItem>,
    incomplete: bool,
    matches: Vec<Match>,
    selected: usize,
    top: usize,
    resolved: HashSet<usize>,
    resolving: Option<usize>,
    /// When a frame first drew this list's items (B16).
    drawn: Option<Instant>,
}

impl CompletionState {
    fn new(request: u64, word_start: usize) -> Self {
        Self {
            request,
            pending: true,
            list: 0,
            word_start,
            query: String::new(),
            items: Vec::new(),
            incomplete: false,
            matches: Vec::new(),
            selected: 0,
            top: 0,
            resolved: HashSet::new(),
            resolving: None,
            drawn: None,
        }
    }

    /// Whether the popup shows (some item matches).
    pub fn is_visible(&self) -> bool {
        !self.matches.is_empty()
    }

    /// The labels shown, best first (tests).
    pub fn labels(&self) -> Vec<String> {
        self.matches
            .iter()
            .map(|m| self.items[m.index].label.clone())
            .collect()
    }

    /// The selected item.
    pub fn selected(&self) -> Option<&CompletionItem> {
        self.matches
            .get(self.selected)
            .map(|m| &self.items[m.index])
    }

    /// The server was asked and has not answered yet.
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    /// When a frame first drew the items of the current answer.
    pub fn drawn_at(&self) -> Option<Instant> {
        self.drawn
    }

    fn rank(&mut self) {
        let query = self.query.as_str();
        let mut matches: Vec<Match> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let target = item.filter_text.as_deref().unwrap_or(&item.label);
                let (score, positions) = fuzzy(query, target)?;
                let positions = if target == item.label {
                    positions
                } else {
                    fuzzy(query, &item.label)
                        .map(|(_, p)| p)
                        .unwrap_or_default()
                };
                Some(Match {
                    index,
                    score,
                    positions,
                })
            })
            .collect();
        let items = &self.items;
        matches.sort_by(|a, b| {
            let key = |m: &Match| {
                let item = &items[m.index];
                item.sort_text.clone().unwrap_or_else(|| item.label.clone())
            };
            key(a)
                .cmp(&key(b))
                .then(b.score.cmp(&a.score))
                .then(a.index.cmp(&b.index))
        });
        self.selected = if query.is_empty() {
            matches
                .iter()
                .position(|m| items[m.index].preselect)
                .unwrap_or(0)
        } else {
            0
        };
        self.top = self.selected.saturating_sub(COMPLETION_ROWS - 1);
        self.matches = matches;
    }
}

/// `query` as a case-insensitive subsequence of `text`: a score (consecutive characters and
/// word starts count more, gaps and a late start less) and the character indices matched.
fn fuzzy(query: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    let query: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let chars: Vec<char> = text.chars().collect();
    let mut positions = Vec::with_capacity(query.len());
    let mut score = 0i32;
    let mut qi = 0;
    let mut prev: Option<usize> = None;
    for (i, c) in chars.iter().enumerate() {
        if qi == query.len() {
            break;
        }
        if c.to_lowercase().next() != Some(query[qi]) {
            continue;
        }
        let word_start = i == 0
            || !chars[i - 1].is_alphanumeric()
            || (chars[i - 1].is_lowercase() && c.is_uppercase());
        score += 10 + if word_start { 8 } else { 0 };
        score += match prev {
            Some(p) if p + 1 == i => 12,
            Some(p) => -((i - p - 1) as i32).min(6),
            None => -(i as i32).min(10),
        };
        prev = Some(i);
        positions.push(i);
        qi += 1;
    }
    (qi == query.len()).then_some((score, positions))
}

/// The tab stops of a snippet being filled in, in buffer bytes.
#[derive(Debug, Clone)]
pub(crate) struct SnippetSession {
    stops: Vec<Vec<Range<usize>>>,
    current: usize,
    /// The buffer version of the insertion: its own edit event is not mapped again.
    version: u64,
}

/// Signature help while asked for or shown.
#[derive(Debug, Clone)]
pub(crate) struct SignatureState {
    request: u64,
    help: Option<SignatureHelp>,
    line: usize,
}

impl CodeView {
    // ---- what the owner sets -----------------------------------------------------------------

    /// Whether the file has a language server, and its trigger characters.
    pub fn set_completion_triggers(&mut self, triggers: CompletionTriggers) {
        self.triggers = triggers;
    }

    /// What makes the view ask (tests).
    pub fn completion_triggers(&self) -> &CompletionTriggers {
        &self.triggers
    }

    /// The completion popup, while asked for or shown.
    pub fn completion(&self) -> Option<&CompletionState> {
        self.completion.as_ref()
    }

    /// Whether a snippet's tab stops are being filled in.
    pub fn in_snippet(&self) -> bool {
        self.snippet.is_some()
    }

    /// The signature help shown: the active signature's label and its active parameter's
    /// text (tests).
    pub fn signature_shown(&self) -> Option<(String, Option<String>)> {
        let help = self.signature.as_ref()?.help.as_ref()?;
        let signature = help.signatures.get(help.active)?;
        let parameter = signature
            .active_parameter
            .and_then(|i| signature.parameters.get(i))
            .and_then(|r| signature.label.get(r.clone()))
            .map(str::to_owned);
        Some((signature.label.clone(), parameter))
    }

    /// The answer to completion request `request` (`None`: it failed or there is no server).
    pub fn set_completions(
        &mut self,
        request: u64,
        list: Option<CompletionList>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.completion.as_mut().filter(|s| s.request == request) else {
            return;
        };
        let Some(list) = list else {
            self.completion = None;
            cx.notify();
            return;
        };
        state.pending = false;
        state.list = list.id;
        state.items = list.items;
        state.incomplete = list.incomplete;
        state.resolved.clear();
        state.resolving = None;
        state.drawn = None;
        state.rank();
        if !state.is_visible() && !state.incomplete {
            self.completion = None;
        }
        self.resolve_selected(cx);
        cx.notify();
    }

    /// What the server added to item `index` of list `list` (`completionItem/resolve`).
    pub fn set_resolved_completion(
        &mut self,
        list: u64,
        index: usize,
        item: Option<CompletionItem>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.completion.as_mut().filter(|s| s.list == list) else {
            return;
        };
        if state.resolving == Some(index) {
            state.resolving = None;
        }
        state.resolved.insert(index);
        if let (Some(item), Some(old)) = (item, state.items.get_mut(index)) {
            old.detail = item.detail.or(old.detail.take());
            old.documentation = item.documentation.or(old.documentation.take());
            if !item.additional_edits.is_empty() {
                old.additional_edits = item.additional_edits;
            }
        }
        cx.notify();
    }

    /// The answer to signature help request `request`.
    pub fn set_signature_help(
        &mut self,
        request: u64,
        help: Option<SignatureHelp>,
        cx: &mut Context<Self>,
    ) {
        if self.signature.as_ref().is_none_or(|s| s.request != request) {
            return;
        }
        match help.filter(|h| !h.signatures.is_empty()) {
            Some(help) => {
                if let Some(state) = self.signature.as_mut() {
                    state.help = Some(help);
                }
            }
            None => self.signature = None,
        }
        cx.notify();
    }

    // ---- asking ------------------------------------------------------------------------------

    fn may_complete(&self, cx: &Context<Self>) -> bool {
        self.triggers.enabled
            && self.selections.len() == 1
            && self.marked.is_empty()
            && self.vim_accepts_text()
            && self.is_editable(cx)
    }

    /// Where the word that ends at `head` starts (on its line).
    fn word_start_before(&self, head: usize, cx: &Context<Self>) -> usize {
        let text = self.buffer().read(cx).text();
        let mut at = head;
        while let Some(c) = text.char_before(at) {
            if !is_word(c) {
                break;
            }
            at -= c.len_utf8();
        }
        at
    }

    fn request_completion(
        &mut self,
        trigger: Option<char>,
        incomplete: bool,
        cx: &mut Context<Self>,
    ) {
        self.completion_task = None;
        let head = self.selections.primary().head;
        self.completion_seq += 1;
        let request = self.completion_seq;
        match self.completion.as_mut() {
            Some(state) if incomplete => {
                state.request = request;
                state.pending = true;
            }
            _ => {
                let word_start = if trigger.is_some() {
                    head
                } else {
                    self.word_start_before(head, cx)
                };
                let mut state = CompletionState::new(request, word_start);
                state.query = self
                    .buffer()
                    .read(cx)
                    .text()
                    .slice(word_start..head)
                    .unwrap_or_default();
                self.completion = Some(state);
            }
        }
        cx.emit(CodeViewEvent::CompletionRequested {
            request,
            offset: head,
            trigger,
            incomplete,
        });
    }

    fn request_signature(&mut self, trigger: Option<char>, cx: &mut Context<Self>) {
        let head = self.selections.primary().head;
        let line = self
            .buffer()
            .read(cx)
            .text()
            .line_of_byte(head)
            .unwrap_or(0);
        self.completion_seq += 1;
        let request = self.completion_seq;
        let retrigger = self.signature.as_ref().is_some_and(|s| s.help.is_some());
        let help = self.signature.take().and_then(|s| s.help);
        self.signature = Some(SignatureState {
            request,
            help,
            line,
        });
        cx.emit(CodeViewEvent::SignatureHelpRequested {
            request,
            offset: head,
            trigger,
            retrigger,
        });
    }

    /// *Show Completions* (⌃Space).
    pub fn show_completions(&mut self, cx: &mut Context<Self>) {
        if !self.may_complete(cx) {
            return;
        }
        self.completion = None;
        self.request_completion(None, false, cx);
        cx.notify();
    }

    /// Plain typing (not an input method's composition) inserted `typed`.
    pub(crate) fn after_typing(&mut self, typed: &str, cx: &mut Context<Self>) {
        if !self.may_complete(cx) {
            self.dismiss_completion(cx);
            return;
        }
        let Some(c) = typed.chars().last() else {
            return;
        };
        if self.triggers.signature.contains(&c) {
            self.request_signature(Some(c), cx);
        } else if self.signature.is_some() {
            self.request_signature(None, cx);
        }
        if self.triggers.completion.contains(&c) {
            self.request_completion(Some(c), false, cx);
        } else if is_word(c) {
            // An open popup is filtered when the edit lands (`refilter_completion`).
            if self.completion.is_none() {
                self.completion_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(COMPLETION_DEBOUNCE).await;
                    this.update(cx, |this, cx| {
                        let head = this.selections.primary().head;
                        let word = this.buffer().read(cx).text().char_before(head);
                        if this.completion.is_none()
                            && this.may_complete(cx)
                            && word.is_some_and(is_word)
                        {
                            this.request_completion(None, false, cx);
                        }
                    })
                    .ok();
                }));
            }
        } else {
            self.dismiss_completion(cx);
        }
    }

    /// After any edit: the popup follows the word being typed, or closes.
    pub(crate) fn refilter_completion(&mut self, cx: &mut Context<Self>) {
        let head = self.selections.primary().head;
        let Some(state) = self.completion.as_ref() else {
            return;
        };
        let word_start = state.word_start;
        let query = {
            let text = self.buffer().read(cx).text();
            let same_line = text.line_of_byte(head).ok() == text.line_of_byte(word_start).ok();
            let query = (head >= word_start && same_line && self.selections.len() == 1)
                .then(|| text.slice(word_start..head).ok())
                .flatten();
            query.filter(|q| q.chars().all(is_word))
        };
        let Some(query) = query else {
            self.dismiss_completion(cx);
            return;
        };
        let Some(state) = self.completion.as_mut() else {
            return;
        };
        if state.query == query {
            return;
        }
        state.query = query;
        let ask_again = state.incomplete && !state.pending;
        state.rank();
        if ask_again {
            self.request_completion(None, true, cx);
        } else if !state.is_visible() && !state.pending {
            self.completion = None;
        }
        self.resolve_selected(cx);
        cx.notify();
    }

    fn resolve_selected(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.completion.as_mut() else {
            return;
        };
        let Some(index) = state.matches.get(state.selected).map(|m| m.index) else {
            return;
        };
        if !state.items[index].resolvable
            || state.resolved.contains(&index)
            || state.resolving == Some(index)
        {
            return;
        }
        state.resolving = Some(index);
        cx.emit(CodeViewEvent::CompletionResolve {
            list: state.list,
            index,
        });
    }

    /// Closes the completion popup. Returns whether one showed.
    pub fn dismiss_completion(&mut self, cx: &mut Context<Self>) -> bool {
        self.completion_task = None;
        let visible = self.completion.take().is_some_and(|s| s.is_visible());
        if visible {
            cx.notify();
        }
        visible
    }

    /// Closes the popups (completion and signature help). Returns whether one showed.
    pub(crate) fn dismiss_popups(&mut self, cx: &mut Context<Self>) -> bool {
        let completion = self.dismiss_completion(cx);
        let signature = self.signature.take().is_some_and(|s| s.help.is_some());
        if signature {
            cx.notify();
        }
        completion || signature
    }

    // ---- moving and accepting ----------------------------------------------------------------

    fn move_selection(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(state) = self.completion.as_mut().filter(|s| s.is_visible()) else {
            return;
        };
        let n = state.matches.len();
        state.selected = if down {
            (state.selected + 1) % n
        } else {
            (state.selected + n - 1) % n
        };
        if state.selected < state.top {
            state.top = state.selected;
        } else if state.selected >= state.top + COMPLETION_ROWS {
            state.top = state.selected + 1 - COMPLETION_ROWS;
        }
        self.resolve_selected(cx);
        cx.notify();
    }

    pub(crate) fn completion_next(&mut self, cx: &mut Context<Self>) {
        self.move_selection(true, cx);
    }

    pub(crate) fn completion_previous(&mut self, cx: &mut Context<Self>) {
        self.move_selection(false, cx);
    }

    /// Return / Tab while the popup shows: an input method's composition ends first.
    pub(crate) fn confirm_completion(&mut self, cx: &mut Context<Self>) {
        if self.end_composition(cx) {
            return;
        }
        let Some(state) = self.completion.take() else {
            return;
        };
        self.completion_task = None;
        let Some(item) = state.selected().cloned() else {
            cx.notify();
            return;
        };
        self.accept_completion(state.word_start, &item, cx);
    }

    fn accept_completion(
        &mut self,
        word_start: usize,
        item: &CompletionItem,
        cx: &mut Context<Self>,
    ) {
        let text = self.buffer().read(cx).text().clone();
        let caret = self.selections.primary().head;
        let (start, raw) = match &item.edit {
            Some(edit) => (
                text.utf16_point_to_byte_clamped(edit.start).min(caret),
                edit.text.clone(),
            ),
            None => (
                word_start.min(caret),
                item.insert_text
                    .clone()
                    .unwrap_or_else(|| item.label.clone()),
            ),
        };
        let snippet = if item.snippet {
            snippet::parse(&raw)
        } else {
            Snippet::plain(raw)
        };
        let mut edits = vec![Edit::new(start..caret, snippet.text.clone())];
        // Where the inserted text lands once the edits before it are applied.
        let mut shift = 0isize;
        for extra in &item.additional_edits {
            let a = text.utf16_point_to_byte_clamped(extra.start);
            let b = text.utf16_point_to_byte_clamped(extra.end).max(a);
            if a < caret.max(start + 1) && b > start || (a == start && b == start) {
                continue; // overlaps the completion itself
            }
            if b <= start {
                shift += extra.text.len() as isize - (b - a) as isize;
            }
            edits.push(Edit::new(a..b, extra.text.clone()));
        }
        edits.sort_by_key(|e| e.range.start);
        let base = start.saturating_add_signed(shift);
        let stops: Vec<Vec<Range<usize>>> = snippet
            .stops
            .iter()
            .map(|stop| stop.iter().map(|r| base + r.start..base + r.end).collect())
            .collect();
        let first = &stops[0];
        let selections = Selections::new(first.iter().map(|r| Selection::new(r.start, r.end)), 0)
            .unwrap_or_else(|_| Selections::cursor(base + snippet.text.len()));
        self.buffer().update(cx, |b, _| b.break_group());
        if !self.commit(edits, selections, Coalesce::Step, cx) {
            return;
        }
        self.buffer().update(cx, |b, _| b.break_group());
        self.snippet = (stops.len() > 1).then(|| SnippetSession {
            stops,
            current: 0,
            version: self.buffer().read(cx).version(),
        });
        // A call just inserted: what its parameters are.
        if snippet.text.contains('(') && self.triggers.signature.contains(&'(') {
            self.request_signature(None, cx);
        }
    }

    // ---- snippets ----------------------------------------------------------------------------

    fn select_stop(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(session) = self.snippet.as_mut() else {
            return;
        };
        session.current = index;
        let ranges = session.stops[index].clone();
        let last = index + 1 == session.stops.len();
        if last {
            self.snippet = None;
        }
        if let Ok(selections) =
            Selections::new(ranges.iter().map(|r| Selection::new(r.start, r.end)), 0)
        {
            self.set_selections(selections, cx);
        }
    }

    pub(crate) fn next_tab_stop(&mut self, cx: &mut Context<Self>) {
        if let Some(next) = self.snippet.as_ref().map(|s| s.current + 1) {
            self.select_stop(next, cx);
        }
    }

    pub(crate) fn previous_tab_stop(&mut self, cx: &mut Context<Self>) {
        if let Some(previous) = self.snippet.as_ref().map(|s| s.current.saturating_sub(1)) {
            self.select_stop(previous, cx);
        }
    }

    /// The tab stops follow an edit (typing into a stop grows it).
    pub(crate) fn map_snippet(&mut self, transaction: &hutan_buffer::Transaction, version: u64) {
        let Some(session) = self.snippet.as_mut() else {
            return;
        };
        if version <= session.version {
            return;
        }
        for stop in &mut session.stops {
            for range in stop.iter_mut() {
                let start = transaction.map_offset(range.start, hutan_buffer::Bias::Left);
                let end = transaction.map_offset(range.end, hutan_buffer::Bias::Right);
                *range = start..end.max(start);
            }
        }
    }

    /// A caret outside every tab stop ends the snippet; one on another line closes signature
    /// help.
    pub(crate) fn check_popups(&mut self, cx: &Context<Self>) {
        let head = self.selections.primary().head;
        if self.snippet.as_ref().is_some_and(|s| {
            !s.stops
                .iter()
                .flatten()
                .any(|r| r.start <= head && head <= r.end)
        }) {
            self.snippet = None;
        }
        let line = self
            .buffer()
            .read(cx)
            .text()
            .line_of_byte(head)
            .unwrap_or(0);
        if self.signature.as_ref().is_some_and(|s| s.line != line) {
            self.signature = None;
        }
    }

    // ---- drawing -----------------------------------------------------------------------------

    fn popup_surface(&self) -> gpui::Div {
        let theme = &self.style.theme;
        let mut border = rgba(theme.gutter_foreground);
        border.a *= 0.35;
        div()
            .rounded(px(6.0))
            .border_1()
            .border_color(border)
            .bg(rgba(theme.gutter_background))
            .text_size(px(self.style.font_size * 0.95))
            .text_color(rgba(theme.foreground))
    }

    /// The completion popup below the caret's line, with the selected item's documentation.
    pub(crate) fn render_completion(&mut self, cx: &Context<Self>) -> Option<AnyElement> {
        let layout = self.layout?;
        let state = self.completion.as_ref().filter(|s| s.is_visible())?;
        let at = self.point_of(state.word_start, cx)?;
        let theme = &self.style.theme;
        let muted = rgba(theme.gutter_foreground);
        let line_h = layout.line_height;
        let cell = f32::from(layout.cell_width);
        let mut list = self
            .popup_surface()
            .id("code-completion")
            .flex()
            .flex_col()
            .py(px(4.0))
            .min_w(px(cell * 24.0))
            .max_w(px(cell * 64.0))
            .on_mouse_move(|_, _, cx| cx.stop_propagation());
        let end = (state.top + COMPLETION_ROWS).min(state.matches.len());
        for (row, m) in state.matches[state.top..end].iter().enumerate() {
            let i = state.top + row;
            let item = &state.items[m.index];
            let kind = item.kind.unwrap_or(CompletionKind::Other);
            let label = item.label.chars().enumerate().fold(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap(),
                |row, (ci, c)| {
                    row.child(
                        div()
                            .when(m.positions.contains(&ci), |s| {
                                s.font_weight(FontWeight::BOLD)
                            })
                            .child(c.to_string()),
                    )
                },
            );
            let detail = item.detail.as_deref().map(|d| {
                let line = d.lines().next().unwrap_or_default();
                if line.chars().count() > DETAIL_CHARS {
                    format!("{}…", line.chars().take(DETAIL_CHARS).collect::<String>())
                } else {
                    line.to_owned()
                }
            });
            let mut glyph_border = self.style.token_color(Some(kind.token()));
            glyph_border.a *= 0.6;
            list = list.child(
                div()
                    .id(("completion-row", i))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .h(line_h)
                    .px(px(6.0))
                    .when(i == state.selected, |s| s.bg(rgba(theme.selection)))
                    .when(item.deprecated, |s| s.line_through())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            if let Some(state) = this.completion.as_mut() {
                                state.selected = i;
                            }
                            this.confirm_completion(cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(line_h * 0.8)
                            .h(line_h * 0.8)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(self.style.font_size * 0.8))
                            .rounded(px(3.0))
                            .border_1()
                            .border_color(glyph_border)
                            .text_color(self.style.token_color(Some(kind.token())))
                            .font_family(self.style.font_family.clone())
                            .child(kind.glyph()),
                    )
                    .child(label)
                    .when_some(detail, |s, d| {
                        s.child(div().flex_none().text_color(muted).child(d))
                    }),
            );
        }
        let docs = state.selected().and_then(|item| {
            let blocks = item
                .documentation
                .as_deref()
                .map(markdown_blocks)
                .unwrap_or_default();
            (item.detail.is_some() || !blocks.is_empty()).then(|| {
                let mut docs = self
                    .popup_surface()
                    .id("code-completion-docs")
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .px(px(10.0))
                    .py(px(8.0))
                    .w(px(cell * 44.0))
                    .max_h(line_h * COMPLETION_ROWS as f32)
                    .overflow_y_scroll()
                    .when_some(item.detail.clone(), |s, d| {
                        s.child(
                            div()
                                .font_family(self.style.font_family.clone())
                                .text_color(muted)
                                .child(d),
                        )
                    });
                for block in blocks {
                    docs = docs.child(match block {
                        HoverBlock::Text { text, strong } => div()
                            .when(strong, |s| s.font_weight(FontWeight::SEMIBOLD))
                            .child(text),
                        HoverBlock::Code(code) => div()
                            .font_family(self.style.font_family.clone())
                            .whitespace_nowrap()
                            .children(code.lines().map(|l| div().child(l.to_owned()))),
                    });
                }
                docs
            })
        });
        let below = point(at.x - px(cell * 3.0), at.y + line_h);
        let body = div()
            .flex()
            .items_start()
            .gap(px(4.0))
            .child(list)
            .children(docs);
        if let Some(state) = self.completion.as_mut()
            && state.drawn.is_none()
        {
            state.drawn = Some(Instant::now());
        }
        Some(
            deferred(anchored().position(below).child(body))
                .with_priority(2)
                .into_any_element(),
        )
    }

    /// Signature help above the caret's line.
    pub(crate) fn render_signature(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let help = self.signature.as_ref()?.help.as_ref()?;
        let signature = help.signatures.get(help.active)?;
        let head = self.selections.primary().head;
        let at = self.point_of(head, cx)?;
        let muted = rgba(self.style.theme.gutter_foreground);
        let active = signature
            .active_parameter
            .and_then(|i| signature.parameters.get(i))
            .filter(|r| {
                r.end <= signature.label.len()
                    && signature.label.is_char_boundary(r.start)
                    && signature.label.is_char_boundary(r.end)
            })
            .cloned();
        let label = &signature.label;
        let (before, current, after) = match &active {
            Some(r) => (&label[..r.start], &label[r.clone()], &label[r.end..]),
            None => (label.as_str(), "", ""),
        };
        let doc = signature
            .active_parameter
            .and_then(|i| signature.parameter_docs.get(i).cloned().flatten())
            .or_else(|| signature.documentation.clone())
            .and_then(|d| {
                markdown_blocks(&d).into_iter().find_map(|b| match b {
                    HoverBlock::Text { text, .. } => Some(text),
                    HoverBlock::Code(_) => None,
                })
            });
        let body = self
            .popup_surface()
            .id("code-signature")
            .flex()
            .flex_col()
            .gap(px(4.0))
            .px(px(10.0))
            .py(px(6.0))
            .child(
                div()
                    .flex()
                    .whitespace_nowrap()
                    .font_family(self.style.font_family.clone())
                    .child(before.to_owned())
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .underline()
                            .child(current.to_owned()),
                    )
                    .child(after.to_owned()),
            )
            .when_some(doc, |s, d| s.child(div().text_color(muted).child(d)));
        Some(
            deferred(
                anchored()
                    .anchor(Anchor::BottomLeft)
                    .position(point(at.x, at.y - px(2.0)))
                    .child(body),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, sort: Option<&str>) -> CompletionItem {
        CompletionItem {
            label: label.into(),
            sort_text: sort.map(Into::into),
            ..CompletionItem::default()
        }
    }

    #[test]
    fn ranking_is_sort_text_then_score_and_filtering_drops_misses() {
        let mut state = CompletionState::new(1, 0);
        state.items = vec![
            item("push_str", Some("2")),
            item("pop", Some("1")),
            item("as_ptr", Some("2")),
            item("len", Some("0")),
        ];
        state.query = "p".into();
        state.rank();
        // `len` does not match; `pop` sorts first; of the two `2`s, `push_str` scores higher
        // (a match at the start beats one inside a word).
        assert_eq!(state.labels(), ["pop", "push_str", "as_ptr"]);
        state.query = "ps".into();
        state.rank();
        assert_eq!(state.labels(), ["push_str"]);
        assert_eq!(state.matches[0].positions, vec![0, 2]);
    }

    #[test]
    fn fuzzy_prefers_word_starts_and_runs() {
        let (run, _) = fuzzy("hm", "HashMap").unwrap();
        let (gap, _) = fuzzy("hm", "hamster").unwrap();
        assert!(run > gap);
        assert!(fuzzy("x", "HashMap").is_none());
        assert_eq!(fuzzy("", "a").map(|(s, _)| s), Some(0));
    }
}
