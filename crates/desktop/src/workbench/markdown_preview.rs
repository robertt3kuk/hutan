//! The Markdown preview (MD-PREVIEW): a Markdown file's text drawn with the chat's renderer
//! (`chat/markdown_view.rs`): headings, lists with task boxes, tables, fenced code coloured by
//! its language's syntax, links that open files, anchors and the web, and images that sit in
//! the repository. No language server is involved.
//!
//! One [`MarkdownPreview`] is a view of one file's buffer. A Markdown file tab holds one of its
//! own while it shows the rendered page (`file_view.rs`, ⇧⌘V), and the *Open Preview to the
//! Side* tab (`ItemRef::Preview`, `markdown_open.rs`) is one on its own. It follows the buffer's
//! edits: a change is parsed 120 ms after the last one, off the main thread, and only the
//! blocks that differ are replaced in the list (a [`gpui::ListState`] draws just the blocks in
//! view), so typing in a long file costs one parse and a few blocks. It follows its source
//! view's scrolling by block: the block that holds the source's first line is at the top.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, ListAlignment, ListOffset, ListState,
    ParentElement, Render, SharedString, Styled, Subscription, Task, TextRun, WeakEntity, Window,
    div, list, px,
};
use hutan_chat_view::markdown::{Block, Located, parse_located, plain_text, slug};
use hutan_code_view::{CodeStyle, CodeView, FileBuffer, LoadState};
use hutan_syntax::{Language, Snapshot};

use super::chat::{Doc, Ids, Look, block_element};
use crate::theme::DensityExt as _;
use crate::workbench::style::UiStyle;

/// A parse starts this long after the last edit.
pub const DEBOUNCE: Duration = Duration::from_millis(120);
/// Text beyond this many bytes is not previewed (a notice ends the page).
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
/// A document of this size or less is parsed where it is, so the first frame has the page.
const INLINE_BYTES: usize = 64 * 1024;
/// A fenced block longer than this is not coloured.
const MAX_HIGHLIGHT_BYTES: usize = 64 * 1024;
/// An image file larger than this is not drawn.
const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;
/// The page is at most this wide, in pixels.
const PAGE_WIDTH: f32 = 860.0;
/// What a block is tall before it is measured: the scrollbar's thumb.
const BLOCK_ESTIMATE: f32 = 64.0;

/// What a preview asks of its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownPreviewEvent {
    /// A link that is not an anchor of the page was clicked: the url as written.
    OpenLink(String),
}

/// The blocks of one version of a document, and where in the text each one starts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    /// The buffer's version it was read at.
    pub version: u64,
    pub blocks: Vec<Located>,
    /// Heading anchors (`#getting-started`) to the index of their block.
    pub anchors: HashMap<String, usize>,
    /// The text was longer than [`MAX_BYTES`]: the page ends with a notice.
    pub truncated: bool,
}

impl Parsed {
    /// Parses `text` (read at `version`): at most [`MAX_BYTES`] of it, cut at a line end.
    pub fn of(text: &str, version: u64) -> Self {
        let (text, truncated) = if text.len() > MAX_BYTES {
            let mut end = MAX_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            let end = text[..end].rfind('\n').unwrap_or(end);
            (&text[..end], true)
        } else {
            (text, false)
        };
        let blocks = parse_located(text);
        let mut anchors = HashMap::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        for (index, located) in blocks.iter().enumerate() {
            let Block::Heading { content, .. } = &located.block else {
                continue;
            };
            let base = slug(&plain_text(content));
            if base.is_empty() {
                continue;
            }
            // GitHub numbers the second heading of a name `-1`, the third `-2`.
            let count = seen.entry(base.clone()).or_insert(0);
            let anchor = if *count == 0 {
                base
            } else {
                format!("{base}-{count}")
            };
            *count += 1;
            anchors.entry(anchor).or_insert(index);
        }
        Self {
            version,
            blocks,
            anchors,
            truncated,
        }
    }

    /// The block that holds source line `line` (0-based): the last one that starts at or
    /// before it.
    pub fn block_at_line(&self, line: usize) -> usize {
        self.blocks
            .partition_point(|located| located.line <= line)
            .saturating_sub(1)
    }

    /// The line block `index` starts at (0-based).
    pub fn line_of_block(&self, index: usize) -> usize {
        self.blocks.get(index).map_or(0, |located| located.line)
    }
}

/// What a link of a page means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// `#heading` in this page (lower case, decoded).
    Anchor(String),
    /// `http`, `https` or `mailto`.
    Web(String),
    /// A file, relative to the page's folder (or absolute), with the line a `#L12` names.
    File { path: PathBuf, line: Option<u32> },
}

/// Reads a link as the page at folder `dir` means it; `None` for what is not opened (another
/// scheme: `javascript:`, `file:`, `data:`; an empty link).
pub fn classify_link(url: &str, dir: &Path) -> Option<LinkTarget> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    if let Some(anchor) = url.strip_prefix('#') {
        return Some(LinkTarget::Anchor(percent_decode(anchor).to_lowercase()));
    }
    if has_scheme(url) {
        let lower = url.to_ascii_lowercase();
        return ["http:", "https:", "mailto:"]
            .iter()
            .any(|scheme| lower.starts_with(scheme))
            .then(|| LinkTarget::Web(url.to_owned()));
    }
    let (target, fragment) = url.split_once('#').unwrap_or((url, ""));
    let target = target.split('?').next().unwrap_or("");
    if target.is_empty() {
        return None;
    }
    let decoded = percent_decode(target);
    let joined = if decoded.starts_with('/') {
        PathBuf::from(decoded)
    } else {
        dir.join(decoded)
    };
    Some(LinkTarget::File {
        path: normalize(&joined),
        line: github_line(fragment),
    })
}

/// `L12` and `L12-L20` (GitHub's line fragments) as the first line.
fn github_line(fragment: &str) -> Option<u32> {
    let rest = fragment.strip_prefix('L')?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|line| *line > 0)
}

/// `scheme:` at the start (letters, digits, `+`, `-`, `.`).
fn has_scheme(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// `%20` and the other escapes of a url's path as text (bytes that are not UTF-8 stay as they
/// were).
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |at: usize| bytes.get(at).and_then(|b| (*b as char).to_digit(16));
        if bytes[i] == b'%'
            && let (Some(high), Some(low)) = (hex(i + 1), hex(i + 2))
        {
            out.push((high * 16 + low) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// `path` without `.` and `..` (the disk is not asked, links are lexical).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The file an image's `url` names, when a page whose file is in `dir` may draw it: a path on
/// this computer (a page of another computer's daemon has none), in a format the renderer
/// reads, a file of at most 25 MiB. The web is never fetched (the page would tell whoever
/// serves the image that it was read).
pub fn resolve_image(url: &str, dir: &Path, local: bool) -> Option<PathBuf> {
    if !local || has_scheme(url) || url.starts_with("//") {
        return None;
    }
    let target = url.split(['#', '?']).next().unwrap_or("");
    if target.is_empty() {
        return None;
    }
    let decoded = percent_decode(target);
    let path = if decoded.starts_with('/') {
        PathBuf::from(decoded)
    } else {
        normalize(&dir.join(decoded))
    };
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if !gpui::Img::extensions().contains(&extension.as_str()) {
        return None;
    }
    let meta = std::fs::metadata(&path).ok()?;
    (meta.is_file() && meta.len() <= MAX_IMAGE_BYTES).then_some(path)
}

/// The text runs of fenced code of `lang`, coloured by the syntax's tokens; `None` for a
/// language Hutan has no grammar for, or text too long to be worth it.
pub fn code_runs(style: &CodeStyle, text: &str, lang: Option<&str>) -> Option<Vec<TextRun>> {
    if text.is_empty() || text.len() > MAX_HIGHLIGHT_BYTES {
        return None;
    }
    let language = Language::from_name(lang?)?;
    if !language.is_compiled() {
        return None;
    }
    let snapshot = Snapshot::parse(language, text.to_owned(), 0).ok()?;
    let spans = snapshot.highlights(0..text.len());
    let mut runs = Vec::with_capacity(spans.len() * 2 + 1);
    let mut at = 0;
    let mut push = |range: Range<usize>, token| {
        if !range.is_empty() {
            runs.push(TextRun {
                len: range.len(),
                font: style.font(token),
                color: style.token_color(token),
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
    };
    for span in spans {
        push(at..span.range.start, None);
        push(span.range.clone(), Some(span.token));
        at = span.range.end;
    }
    push(at..text.len(), None);
    Some(runs)
}

/// A hash of what a fenced block says, to look its colours up again.
fn code_key(text: &str, lang: Option<&str>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (text, lang).hash(&mut hasher);
    hasher.finish()
}

/// A Markdown file's page.
pub struct MarkdownPreview {
    buffer: Entity<FileBuffer>,
    style: CodeStyle,
    /// Images are files of this computer (a daemon on another one has none to show).
    local: bool,
    doc: Rc<Doc>,
    /// What the page's images resolved to; cleared when the text is parsed again.
    images: Rc<RefCell<HashMap<String, Option<PathBuf>>>>,
    parsed: Arc<Parsed>,
    list: ListState,
    bar: hutan_ui::Scrollbar,
    pending: Option<Task<()>>,
    /// The view the page follows: its first line decides the block at the top.
    source: Option<Entity<CodeView>>,
    followed: Option<usize>,
    /// An edit puts the page back at the source's first line (see [`Self::set_source`]).
    follow_edits: bool,
    focus: FocusHandle,
    /// How many times the text was parsed (tests).
    parses: usize,
    /// How many blocks the last change replaced (tests).
    replaced: usize,
    /// How many blocks were drawn (tests).
    built: usize,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for MarkdownPreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarkdownPreview")
            .field("blocks", &self.parsed.blocks.len())
            .finish_non_exhaustive()
    }
}

impl EventEmitter<MarkdownPreviewEvent> for MarkdownPreview {}

impl Focusable for MarkdownPreview {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl MarkdownPreview {
    pub fn new(
        buffer: Entity<FileBuffer>,
        style: CodeStyle,
        local: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let list = ListState::new(0, ListAlignment::Top, px(600.0));
        let images = Rc::new(RefCell::new(HashMap::new()));
        let mut this = Self {
            doc: Self::doc(cx.weak_entity(), &buffer, &style, local, &images, cx),
            images,
            buffer: buffer.clone(),
            style,
            local,
            parsed: Arc::new(Parsed::default()),
            bar: hutan_ui::Scrollbar::for_list(&list, BLOCK_ESTIMATE),
            list,
            pending: None,
            source: None,
            followed: None,
            follow_edits: false,
            focus: cx.focus_handle(),
            parses: 0,
            replaced: 0,
            built: 0,
            _subscriptions: Vec::new(),
        };
        this._subscriptions
            .push(cx.observe(&buffer, |this, _, cx| this.buffer_changed(cx)));
        this.buffer_changed(cx);
        this
    }

    /// What the renderer asks of its page: links, images and code of this file.
    fn doc(
        this: WeakEntity<Self>,
        buffer: &Entity<FileBuffer>,
        style: &CodeStyle,
        local: bool,
        images: &Rc<RefCell<HashMap<String, Option<PathBuf>>>>,
        cx: &App,
    ) -> Rc<Doc> {
        let dir = buffer
            .read(cx)
            .path()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let images = images.clone();
        let style = style.clone();
        let colours: RefCell<HashMap<u64, Option<Vec<TextRun>>>> = RefCell::new(HashMap::new());
        Rc::new(Doc {
            open: Box::new(move |url, _, cx| {
                this.update(cx, |preview, cx| preview.open_link(url, cx))
                    .ok();
            }),
            image: Box::new(move |url| {
                images
                    .borrow_mut()
                    .entry(url.to_owned())
                    .or_insert_with(|| resolve_image(url, &dir, local))
                    .clone()
            }),
            code: Box::new(move |text, lang| {
                colours
                    .borrow_mut()
                    .entry(code_key(text, lang))
                    .or_insert_with(|| code_runs(&style, text, lang))
                    .clone()
            }),
        })
    }

    /// A theme or editor font change: the code is coloured again.
    pub fn set_style(&mut self, style: CodeStyle, cx: &mut Context<Self>) {
        if self.style == style {
            return;
        }
        self.style = style;
        self.doc = Self::doc(
            cx.weak_entity(),
            &self.buffer,
            &self.style,
            self.local,
            &self.images,
            cx,
        );
        // The measured heights were those of the old fonts.
        self.list.remeasure();
        cx.notify();
    }

    pub fn buffer(&self) -> &Entity<FileBuffer> {
        &self.buffer
    }

    /// The blocks as last parsed.
    pub fn parsed(&self) -> &Parsed {
        &self.parsed
    }

    /// The buffer's version the page shows.
    pub fn shown_version(&self) -> u64 {
        self.parsed.version
    }

    /// How many times the text was parsed, and how many blocks the last change replaced.
    pub fn work(&self) -> (usize, usize) {
        (self.parses, self.replaced)
    }

    /// How many blocks the list has asked this page to draw so far (a frame draws only the
    /// blocks in view).
    pub fn blocks_built(&self) -> usize {
        self.built
    }

    /// The images the page asked about since its text was last parsed: the url as written and
    /// the file it resolved to, if any.
    pub fn resolved_images(&self) -> Vec<(String, Option<PathBuf>)> {
        let mut found: Vec<_> = self
            .images
            .borrow()
            .iter()
            .map(|(url, file)| (url.clone(), file.clone()))
            .collect();
        found.sort();
        found
    }

    /// Whether the page shows the buffer as it is now.
    pub fn is_current(&self, cx: &App) -> bool {
        let buffer = self.buffer.read(cx);
        buffer.is_ready() && self.parsed.version == buffer.version() && self.pending.is_none()
    }

    // ---- following the text ------------------------------------------------------------------

    /// The buffer changed (it loaded, was edited, reloaded): parse again soon.
    fn buffer_changed(&mut self, cx: &mut Context<Self>) {
        let (ready, version, small) = {
            let buffer = self.buffer.read(cx);
            (
                buffer.is_ready(),
                buffer.version(),
                buffer.text().len() <= INLINE_BYTES,
            )
        };
        cx.notify();
        if !ready {
            return;
        }
        if self.parses == 0 && small {
            // The first page needs no waiting.
            let text = self.buffer.read(cx).text().to_string();
            self.apply(Parsed::of(&text, version), cx);
            return;
        }
        if self.parsed.version == version && self.parses > 0 {
            return;
        }
        self.pending = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let Ok((version, text)) = this.update(cx, |this, cx| {
                let buffer = this.buffer.read(cx);
                (buffer.version(), buffer.text().to_string())
            }) else {
                return;
            };
            let parsed = cx
                .background_spawn(async move { Parsed::of(&text, version) })
                .await;
            this.update(cx, |this, cx| {
                this.pending = None;
                this.apply(parsed, cx);
            })
            .ok();
        }));
    }

    /// Swaps the page for `parsed`: the blocks in common at the start and at the end stay
    /// (and stay measured), the ones between are replaced.
    fn apply(&mut self, parsed: Parsed, cx: &mut Context<Self>) {
        let old = &self.parsed.blocks;
        let new = &parsed.blocks;
        let same = |a: &Located, b: &Located| a.block == b.block;
        let prefix = old.iter().zip(new).take_while(|(a, b)| same(a, b)).count();
        let room = old.len().min(new.len()) - prefix;
        let suffix = old
            .iter()
            .rev()
            .zip(new.iter().rev())
            .take(room)
            .take_while(|(a, b)| same(a, b))
            .count();
        let removed = prefix..old.len() - suffix;
        let added = new.len() - prefix - suffix;
        self.replaced = added;
        if self.parses == 0 {
            self.list.reset(new.len());
        } else if !removed.is_empty() || added > 0 {
            self.list.splice(removed, added);
        }
        let first = self.parses == 0;
        self.parses += 1;
        self.images.borrow_mut().clear();
        self.parsed = Arc::new(parsed);
        // The text moved under the reader: the page goes back to where the text it follows is
        // (a page that is not beside its text — in its own tab — keeps its place).
        if first || self.follow_edits {
            self.followed = None;
        }
        self.follow_source(cx);
        cx.notify();
    }

    // ---- following the source's scroll -------------------------------------------------------

    /// The view whose first line decides where the page is (`None`: the page scrolls alone).
    /// `live`: the text is on screen beside the page (a split), so after an edit the page goes
    /// back to the block of the text's first line; a page that took the text's place does not
    /// move when the file changes under it.
    pub fn set_source(
        &mut self,
        source: Option<Entity<CodeView>>,
        live: bool,
        cx: &mut Context<Self>,
    ) {
        self.follow_edits = live;
        if self.source == source {
            return;
        }
        // The observation of the old source goes with the list's tail; keep the buffer's.
        self._subscriptions.truncate(1);
        if let Some(code) = &source {
            self._subscriptions
                .push(cx.observe(code, |this, _, cx| this.follow_source(cx)));
        }
        self.source = source;
        self.followed = None;
        self.follow_source(cx);
    }

    /// Scrolls to the block that holds the source's first line, when that line changed.
    fn follow_source(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.source.clone() else {
            return;
        };
        let line = source.read(cx).top_line();
        if self.followed == Some(line) {
            return;
        }
        self.followed = Some(line);
        self.scroll_to_line(line);
        cx.notify();
    }

    /// Puts the block that holds source line `line` (0-based) at the top.
    pub fn scroll_to_line(&self, line: usize) {
        if self.parsed.blocks.is_empty() {
            return;
        }
        self.list.scroll_to(ListOffset {
            item_ix: self.parsed.block_at_line(line),
            offset_in_item: px(0.0),
        });
    }

    /// The line (0-based) the block at the top of the page starts at.
    pub fn top_line(&self) -> usize {
        self.parsed
            .line_of_block(self.list.logical_scroll_top().item_ix)
    }

    /// The index of the block at the top of the page (tests).
    pub fn top_block(&self) -> usize {
        self.list.logical_scroll_top().item_ix
    }

    // ---- links -------------------------------------------------------------------------------

    /// A click on a link: `#heading` scrolls here, the rest is the owner's to open.
    fn open_link(&mut self, url: &str, cx: &mut Context<Self>) {
        let dir = self
            .buffer
            .read(cx)
            .path()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        match classify_link(url, &dir) {
            Some(LinkTarget::Anchor(anchor)) => {
                if let Some(index) = self.parsed.anchors.get(&anchor) {
                    self.list.scroll_to(ListOffset {
                        item_ix: *index,
                        offset_in_item: px(0.0),
                    });
                    cx.notify();
                }
            }
            Some(_) => cx.emit(MarkdownPreviewEvent::OpenLink(url.to_owned())),
            None => {}
        }
    }

    /// Clicks the link `url` as if it were on the page (tests).
    pub fn activate_link(&mut self, url: &str, cx: &mut Context<Self>) {
        self.open_link(url, cx);
    }

    // ---- keys and drawing --------------------------------------------------------------------

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.platform || keystroke.modifiers.control || keystroke.modifiers.alt {
            return;
        }
        let page = f32::from(self.list.viewport_bounds().size.height).max(120.0) * 0.9;
        let by = match keystroke.key.as_str() {
            "down" => 48.0,
            "up" => -48.0,
            "pagedown" => page,
            "pageup" => -page,
            "space" if keystroke.modifiers.shift => -page,
            "space" => page,
            "home" => {
                self.list.scroll_to(ListOffset {
                    item_ix: 0,
                    offset_in_item: px(0.0),
                });
                cx.notify();
                cx.stop_propagation();
                return;
            }
            "end" => {
                self.list.scroll_to_end();
                cx.notify();
                cx.stop_propagation();
                return;
            }
            _ => return,
        };
        self.list.scroll_by(px(by));
        cx.notify();
        cx.stop_propagation();
    }

    fn render_block(&mut self, index: usize, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.built += 1;
        let look = Look::of(cx).document(self.doc.clone());
        let tokens = look.tokens;
        let blocks = &self.parsed.blocks;
        let Some(located) = blocks.get(index) else {
            return div().into_any_element();
        };
        let mut ids = Ids::new(format!("md{}-{index}", cx.entity_id().as_u64()));
        let last = index + 1 == blocks.len();
        let page = div().w_full().max_w(px(PAGE_WIDTH)).child(block_element(
            &located.block,
            &look,
            &mut ids,
        ));
        let page = if last && self.parsed.truncated {
            div()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing * 3.0))
                .child(page)
                .child(
                    div()
                        .text_color(look.theme.muted)
                        .text_size(px(tokens.secondary_font_size))
                        .child(SharedString::from(format!(
                            "The preview stops after {} MiB; the file goes on.",
                            MAX_BYTES / (1024 * 1024)
                        ))),
                )
                .into_any_element()
        } else {
            page.into_any_element()
        };
        div()
            .w_full()
            .flex()
            .justify_center()
            .px(px(tokens.spacing * 6.0))
            .when(index == 0, |s| s.pt(px(tokens.spacing * 5.0)))
            .pb(px(if last {
                tokens.spacing * 10.0
            } else {
                tokens.spacing * 3.0
            }))
            .child(page)
            .into_any_element()
    }
}

impl Render for MarkdownPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let (state, name) = {
            let buffer = self.buffer.read(cx);
            (buffer.state().clone(), buffer.file_name())
        };
        let message = match state {
            LoadState::Ready if self.parsed.blocks.is_empty() => Some(format!("{name} is empty.")),
            LoadState::Ready => None,
            LoadState::Loading => Some(format!("Opening {name}…")),
            LoadState::NeedsConfirm { .. } => {
                Some(format!("{name} is large: open it as text first."))
            }
            LoadState::Failed { .. } => Some(format!("{name} cannot be shown.")),
        };
        let body = match message {
            Some(text) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted)
                .child(SharedString::from(text))
                .into_any_element(),
            None => div()
                .relative()
                .size_full()
                .child(
                    list(self.list.clone(), cx.processor(Self::render_block))
                        .size_full()
                        .into_any_element(),
                )
                .child(self.bar.element())
                .into_any_element(),
        };
        div()
            .id("markdown-preview")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .size_full()
            .bg(theme.bg)
            .text_size(px(tokens.font_size))
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        PathBuf::from("/repo/docs")
    }

    #[test]
    fn links_are_read_the_way_a_page_in_its_folder_means_them() {
        assert_eq!(
            classify_link("#Getting-Started", &dir()),
            Some(LinkTarget::Anchor("getting-started".into()))
        );
        assert_eq!(
            classify_link("https://example.com/a?b#c", &dir()),
            Some(LinkTarget::Web("https://example.com/a?b#c".into()))
        );
        assert_eq!(
            classify_link("mailto:a@b.c", &dir()),
            Some(LinkTarget::Web("mailto:a@b.c".into()))
        );
        assert_eq!(
            classify_link("../src/main.rs#L42-L50", &dir()),
            Some(LinkTarget::File {
                path: PathBuf::from("/repo/src/main.rs"),
                line: Some(42)
            })
        );
        assert_eq!(
            classify_link("./my%20file.md?plain=1#intro", &dir()),
            Some(LinkTarget::File {
                path: PathBuf::from("/repo/docs/my file.md"),
                line: None
            })
        );
        assert_eq!(
            classify_link("/etc/hosts", &dir()),
            Some(LinkTarget::File {
                path: PathBuf::from("/etc/hosts"),
                line: None
            })
        );
        // What is not opened: other schemes and nothing.
        for link in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,x",
            " ",
            "",
        ] {
            assert_eq!(classify_link(link, &dir()), None, "{link}");
        }
    }

    #[test]
    fn percent_escapes_decode_and_broken_ones_stay() {
        assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%e2%9c%93"), "✓");
        // Not UTF-8: left as written.
        assert_eq!(percent_decode("%ff"), "%ff");
    }

    #[test]
    fn images_resolve_beside_the_file_and_only_on_this_computer() {
        let home = tempfile::tempdir().expect("a directory");
        let dir = home.path();
        std::fs::create_dir(dir.join("img")).expect("a folder");
        std::fs::write(dir.join("img/a b.png"), b"x").expect("a file");
        std::fs::write(dir.join("notes.txt"), b"x").expect("a file");
        assert_eq!(
            resolve_image("img/a%20b.png", dir, true),
            Some(dir.join("img/a b.png"))
        );
        assert_eq!(
            resolve_image("./img/../img/a%20b.png#x", dir, true),
            Some(dir.join("img/a b.png"))
        );
        assert_eq!(resolve_image("img/a%20b.png", dir, false), None);
        assert_eq!(resolve_image("img/missing.png", dir, true), None);
        assert_eq!(resolve_image("notes.txt", dir, true), None, "not an image");
        assert_eq!(resolve_image("https://x.y/a.png", dir, true), None);
        assert_eq!(resolve_image("//x.y/a.png", dir, true), None);
        assert_eq!(resolve_image("data:image/png;base64,AAAA", dir, true), None);
    }

    #[test]
    fn a_page_knows_the_block_of_a_line_and_the_anchors_of_its_headings() {
        let parsed = Parsed::of(
            "# Intro\n\ntext\n\n## Usage\n\nmore\n\n## Usage\n\n```rust\nfn x() {}\n```\n",
            7,
        );
        assert_eq!(parsed.version, 7);
        assert_eq!(parsed.blocks.len(), 6);
        assert_eq!(parsed.block_at_line(0), 0);
        assert_eq!(parsed.block_at_line(2), 1);
        assert_eq!(
            parsed.block_at_line(3),
            1,
            "a blank line belongs to the block above"
        );
        assert_eq!(parsed.block_at_line(4), 2);
        assert_eq!(parsed.block_at_line(11), 5);
        assert_eq!(parsed.block_at_line(999), 5);
        assert_eq!(parsed.line_of_block(2), 4);
        assert_eq!(parsed.anchors.get("intro"), Some(&0));
        assert_eq!(parsed.anchors.get("usage"), Some(&2));
        assert_eq!(
            parsed.anchors.get("usage-1"),
            Some(&4),
            "GitHub numbers repeats"
        );
    }

    #[test]
    fn a_page_of_a_huge_file_stops_at_a_line_end() {
        let line = "word ".repeat(19) + "end\n";
        let text = line.repeat(MAX_BYTES / line.len() + 10);
        let parsed = Parsed::of(&text, 1);
        assert!(parsed.truncated);
        let Some(Located {
            block: Block::Paragraph(content),
            ..
        }) = parsed.blocks.last()
        else {
            panic!("a paragraph");
        };
        let shown = plain_text(content);
        assert!(shown.len() <= MAX_BYTES);
        assert!(
            shown.ends_with("end"),
            "cut at a line end: …{}",
            &shown[shown.len() - 10..]
        );
        assert!(!Parsed::of("short", 1).truncated);
    }

    #[test]
    fn fenced_code_is_coloured_by_its_language_and_other_code_is_not() {
        let theme =
            hutan_settings::themes::builtin_theme(hutan_settings::themes::Builtin::HutanDark)
                .editor();
        let style = CodeStyle::new(theme, "Menlo", 13.0, 1.4);
        let text = "fn main() { let x = 1; }";
        let runs = code_runs(&style, text, Some("rust")).expect("rust has a grammar");
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), text.len());
        assert!(runs.len() > 3, "keywords, names and numbers differ");
        let colours: std::collections::HashSet<String> =
            runs.iter().map(|r| format!("{:?}", r.color)).collect();
        assert!(colours.len() > 1, "more than one colour");
        assert!(code_runs(&style, text, Some("brainfuck")).is_none());
        assert!(code_runs(&style, text, None).is_none());
        assert!(code_runs(&style, "", Some("rust")).is_none());
    }
}
