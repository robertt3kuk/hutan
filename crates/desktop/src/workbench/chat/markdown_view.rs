//! Markdown blocks as GPUI elements: paragraphs and headings as one styled text each, fenced
//! code with a copy button, tables, lists (task boxes included), quotes and rules. In a chat
//! links open in the browser and images are their alt text; a [`Doc`] (a Markdown file's
//! preview) says what its links open, which images are drawn and how code is coloured.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClipboardItem, ElementId, Font, FontStyle, FontWeight, InteractiveElement,
    InteractiveText, IntoElement, ObjectFit, ParentElement, SharedString,
    StatefulInteractiveElement, StrikethroughStyle, Styled, StyledImage as _, StyledText, TextRun,
    UnderlineStyle, Window, div, font, img, px,
};
use hutan_chat_view::markdown::{Align, Block, Inline, Links, Span, flatten_links};

use super::view::Regions;
use crate::theme::{DensityExt as _, Theme, Tokens};
use crate::workbench::render::probe;
use crate::workbench::style::{TerminalStyle, UiStyle};
use crate::workbench::ui::{Button, StyledExt as _, prose_font};
use crate::workbench::view::Region;

/// Opens the target of a link.
pub type OpenLink = Box<dyn Fn(&str, &mut Window, &mut App)>;
/// The file an image's url names.
pub type ImageFile = Box<dyn Fn(&str) -> Option<PathBuf>>;
/// The text runs of a block of code in a language.
pub type CodeRuns = Box<dyn Fn(&str, Option<&str>) -> Option<Vec<TextRun>>>;

/// What a Markdown file's preview adds to a message's look.
pub struct Doc {
    /// Opens the target of a link (any text a link had, as written).
    pub open: OpenLink,
    /// The file an image's url names, when the preview may draw it (`None`: its alt text).
    pub image: ImageFile,
    /// The text runs of a fenced block of `lang` (`None`: plain), colored by its syntax.
    pub code: CodeRuns,
}

impl std::fmt::Debug for Doc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Doc").finish_non_exhaustive()
    }
}

/// What the chat view draws with: chrome colors, density tokens and the monospace font. Message
/// text is prose (the prose face, the UI face by default, at 15 px / 1.6) unless the look is [`sans`](Look::sans).
#[derive(Debug, Clone)]
pub struct Look {
    pub theme: Theme,
    pub tokens: Tokens,
    pub mono: SharedString,
    serif: bool,
    doc: Option<Rc<Doc>>,
}

impl Look {
    pub fn of(cx: &App) -> Self {
        let style = UiStyle::get(cx);
        Self {
            theme: style.theme,
            tokens: style.density.ui(),
            mono: super::super::ui::code_family()
                .unwrap_or_else(|| TerminalStyle::get(cx).font_family.into()),
            serif: true,
            doc: None,
        }
    }

    /// The same look for a Markdown file's preview: links, images and code as `doc` says.
    #[must_use]
    pub fn document(&self, doc: Rc<Doc>) -> Self {
        Self {
            doc: Some(doc),
            ..self.clone()
        }
    }

    /// The same look with message text in the UI sans (the user's own bubble).
    #[must_use]
    pub fn sans(&self) -> Self {
        Self {
            serif: false,
            ..self.clone()
        }
    }

    /// A container for one paragraph: prose type for the serif look, UI type for the sans one.
    fn text_box(&self) -> gpui::Div {
        let tokens = &self.tokens;
        if self.serif {
            div().prose(tokens)
        } else {
            div()
                .text_size(px(tokens.font_size + 1.0))
                .line_height(gpui::relative(1.5))
        }
    }

    /// The face of running text: the prose face, or the UI font.
    fn text_face(&self) -> Font {
        if self.serif {
            prose_font()
        } else {
            gpui::font(super::super::ui::ui_family())
        }
    }
}

/// Hands out element ids that are unique within one entry, and records where its Copy buttons
/// are drawn.
pub struct Ids {
    scope: String,
    next: usize,
    copies: usize,
    probe: Option<(Regions, usize)>,
}

impl std::fmt::Debug for Ids {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ids").field("scope", &self.scope).finish()
    }
}

impl Ids {
    pub fn new(scope: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
            next: 0,
            copies: 0,
            probe: None,
        }
    }

    /// Records the Copy buttons of entry `entry` in `regions`.
    #[must_use]
    pub fn probing(mut self, regions: Regions, entry: usize) -> Self {
        self.probe = Some((regions, entry));
        self
    }

    /// An invisible child that records the bounds of the next Copy button.
    fn copy_probe(&mut self) -> Option<AnyElement> {
        let (regions, entry) = self.probe.as_ref()?;
        let region = Region::ChatCopy(*entry, self.copies);
        self.copies += 1;
        Some(probe(regions, region).into_any_element())
    }

    pub fn next(&mut self, what: &str) -> ElementId {
        self.next += 1;
        ElementId::from(SharedString::from(format!(
            "{}-{what}-{}",
            self.scope, self.next
        )))
    }
}

fn run(font: Font, len: usize, color: gpui::Hsla) -> TextRun {
    TextRun {
        len,
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// Text runs for `styled`'s spans. A heading (`Some(weight)`) is set in the UI sans at that
/// weight, a paragraph in the look's text face.
fn runs(spans: &[Span], look: &Look, heading: Option<FontWeight>) -> Vec<TextRun> {
    let theme = &look.theme;
    spans
        .iter()
        .map(|span| {
            let style = &span.style;
            let mut face = if style.code {
                font(look.mono.clone())
            } else if heading.is_some() {
                Font::default()
            } else {
                look.text_face()
            };
            if let Some(weight) = heading {
                face.weight = weight;
            } else if style.bold {
                face.weight = FontWeight::BOLD;
            }
            if style.italic {
                face.style = FontStyle::Italic;
            }
            let mut text_run = run(
                face,
                span.range.len(),
                if style.link.is_some() {
                    theme.link
                } else {
                    theme.fg
                },
            );
            if style.code {
                text_run.background_color = Some(theme.code_bg);
            }
            if style.strike {
                text_run.strikethrough = Some(StrikethroughStyle {
                    thickness: px(1.0),
                    color: Some(text_run.color),
                });
            }
            if style.link.is_some() {
                text_run.underline = Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(theme.link),
                    wavy: false,
                });
            }
            text_run
        })
        .collect()
}

/// One paragraph or heading: the inlines as a single styled text; link ranges are clickable
/// (in a document every link is, and the document opens it).
pub fn inline_text(content: &[Inline], look: &Look, bold: bool, id: ElementId) -> AnyElement {
    text_element(content, look, bold.then_some(FontWeight::MEDIUM), None, id)
}

/// [`inline_text`] for a heading of a document: `size` pixels at `weight`.
fn heading_text(
    content: &[Inline],
    look: &Look,
    size: f32,
    weight: FontWeight,
    id: ElementId,
) -> AnyElement {
    text_element(content, look, Some(weight), Some(size), id)
}

/// The text of `content` as one element. `heading`: set as a heading at that weight; `size`: a
/// size of its own (else the look's paragraph type).
fn text_element(
    content: &[Inline],
    look: &Look,
    heading: Option<FontWeight>,
    size: Option<f32>,
    id: ElementId,
) -> AnyElement {
    let policy = if look.doc.is_some() {
        Links::Any
    } else {
        Links::Web
    };
    let styled = flatten_links(content, policy);
    if styled.text.is_empty() {
        return div().into_any_element();
    }
    let text_runs = runs(&styled.spans, look, heading);
    let links: Vec<(std::ops::Range<usize>, String)> = styled
        .spans
        .iter()
        .filter_map(|span| span.style.link.clone().map(|url| (span.range.clone(), url)))
        .collect();
    let text = StyledText::new(styled.text).with_runs(text_runs);
    let container = match size {
        Some(size) => div().text_size(px(size)).line_height(gpui::relative(1.3)),
        None => look.text_box(),
    };
    if links.is_empty() {
        return container.child(text).into_any_element();
    }
    let ranges = links.iter().map(|(range, _)| range.clone()).collect();
    let urls: Vec<String> = links.into_iter().map(|(_, url)| url).collect();
    let doc = look.doc.clone();
    container
        .child(
            InteractiveText::new(id, text).on_click(ranges, move |index, window, cx| {
                let Some(url) = urls.get(index) else {
                    return;
                };
                match &doc {
                    Some(doc) => (doc.open)(url, window, cx),
                    None => cx.open_url(url),
                }
            }),
        )
        .into_any_element()
}

/// A block of code (or a command's output): monospace, scrolls sideways, with a Copy button.
pub fn code_block(text: &str, lang: Option<&str>, look: &Look, ids: &mut Ids) -> AnyElement {
    let theme = &look.theme;
    let tokens = &look.tokens;
    let copy_id = ids.next("copy");
    let body_id = ids.next("code");
    let copy_probe = ids.copy_probe();
    let copied = text.to_owned();
    // A document's fenced block is coloured by its language's syntax.
    let colored = look
        .doc
        .as_ref()
        .and_then(|doc| (doc.code)(text, lang))
        .map(|runs| StyledText::new(text.to_owned()).with_runs(runs));
    div()
        .relative()
        .card(theme, tokens)
        .overflow_hidden()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px(px(tokens.spacing * 2.0))
                .h(px(tokens.font_size + 10.0))
                .border_b_1()
                .border_color(theme.border)
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .child(SharedString::from(lang.unwrap_or("text").to_owned()))
                .child(
                    Button::new(copy_id, "Copy", theme, tokens)
                        .ghost()
                        .compact()
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
                        })
                        .children(copy_probe),
                ),
        )
        .child({
            let body = div()
                .id(body_id)
                .overflow_x_scroll()
                .px(px(tokens.spacing * 2.0))
                .py(px(tokens.spacing * 1.5))
                .font_family(look.mono.clone())
                .text_size(px(tokens.font_size))
                .text_color(theme.fg)
                .whitespace_nowrap();
            match colored {
                Some(text) => body.child(text),
                None => body.child(SharedString::from(text.to_owned())),
            }
        })
        .into_any_element()
}

/// The tallest an image of a document is drawn, in pixels; wider than the page, it shrinks.
const IMAGE_MAX_HEIGHT: f32 = 480.0;

/// An image of a paragraph: its alt text and url, and the link around it, if any.
struct Picture<'a> {
    alt: &'a str,
    url: &'a str,
    link: Option<&'a str>,
}

/// The images of a paragraph that stand on their own (`![a](b)`, `[![a](b)](c)`).
fn pictures(content: &[Inline]) -> Vec<Picture<'_>> {
    content
        .iter()
        .filter_map(|node| match node {
            Inline::Image { alt, url } => Some(Picture {
                alt,
                url,
                link: None,
            }),
            Inline::Link { content, url } => match content.as_slice() {
                [Inline::Image { alt, url: image }] => Some(Picture {
                    alt,
                    url: image,
                    link: Some(url),
                }),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Whether a paragraph says anything but its pictures (spaces and line breaks between them do
/// not count).
fn has_text(content: &[Inline]) -> bool {
    content.iter().any(|node| match node {
        Inline::Text(text) => !text.trim().is_empty(),
        Inline::Image { .. } | Inline::LineBreak => false,
        Inline::Link { content, .. } => !matches!(content.as_slice(), [Inline::Image { .. }]),
        _ => true,
    })
}

/// Why an image is not drawn, in words: the web is never fetched, anything else is a file that
/// is not there (or not an image, or too large, or on another computer).
fn missing_reason(url: &str) -> &'static str {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        "Web images are not loaded"
    } else {
        "Image not found"
    }
}

/// What stands where an image cannot be drawn: a bordered box with the alt text (or the url
/// when it has none) and why, linked when the image was.
fn placeholder(
    alt: &str,
    url: &str,
    reason: &str,
    link: Option<&str>,
    look: &Look,
    ids: &mut Ids,
) -> AnyElement {
    let (theme, tokens) = (look.theme, look.tokens);
    let headline = if alt.is_empty() { url } else { alt };
    let boxed = div()
        .flex()
        .flex_col()
        .gap(px(tokens.spacing / 2.0))
        .px(px(tokens.spacing * 2.0))
        .py(px(tokens.spacing * 1.5))
        .max_w_full()
        .border_1()
        .border_color(theme.border)
        .rounded(px(tokens.radius))
        .text_size(px(tokens.secondary_font_size))
        .child(
            div()
                .text_color(theme.fg)
                .child(SharedString::from(headline.to_owned())),
        )
        .child(
            div()
                .text_color(theme.muted)
                .child(SharedString::from(format!("{reason}: {url}"))),
        );
    match (link, &look.doc) {
        (Some(target), Some(doc)) => {
            let (target, doc) = (target.to_owned(), doc.clone());
            div()
                .id(ids.next("img"))
                .on_click(move |_, window, cx| (doc.open)(&target, window, cx))
                .child(boxed)
                .into_any_element()
        }
        _ => boxed.into_any_element(),
    }
}

/// The natural size of an SVG file in pixels, from its `width` and `height` or else its
/// `viewBox`; `None` when it says neither (or is no SVG). Read once per file version.
fn svg_size(file: &std::path::Path) -> Option<(f32, f32)> {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::time::SystemTime;
    /// A file's size as read, with the time of the file it was read from.
    type Read = (Option<SystemTime>, Option<(f32, f32)>);
    thread_local! {
        static SIZES: RefCell<HashMap<PathBuf, Read>> = RefCell::new(HashMap::new());
    }
    let stamp = std::fs::metadata(file).ok()?.modified().ok();
    SIZES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((seen, size)) = cache.get(file)
            && *seen == stamp
        {
            return *size;
        }
        let size = read_svg_size(file);
        cache.insert(file.to_path_buf(), (stamp, size));
        size
    })
}

fn read_svg_size(file: &std::path::Path) -> Option<(f32, f32)> {
    use std::io::Read as _;
    let mut head = Vec::new();
    std::fs::File::open(file)
        .ok()?
        .take(64 * 1024)
        .read_to_end(&mut head)
        .ok()?;
    svg_size_of(&String::from_utf8_lossy(&head))
}

/// [`svg_size`] of the text of an SVG file.
pub fn svg_size_of(text: &str) -> Option<(f32, f32)> {
    let start = text.find("<svg")?;
    let tag = &text[start..start + text[start..].find('>')?];
    let attr = |name: &str| -> Option<String> {
        let mut from = 0;
        while let Some(found) = tag[from..].find(name) {
            let at = from + found;
            from = at + name.len();
            if !tag[..at].ends_with(char::is_whitespace) {
                continue;
            }
            let rest = tag[from..].trim_start().strip_prefix('=')?.trim_start();
            let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            let inner = &rest[1..];
            return Some(inner[..inner.find(quote)?].to_owned());
        }
        None
    };
    // A length in pixels (`120`, `120px`); a percentage or another unit says nothing here.
    let length = |value: String| -> Option<f32> {
        let number = value.trim().trim_end_matches("px").trim();
        number.parse::<f32>().ok().filter(|v| *v > 0.0)
    };
    let (width, height) = (
        attr("width").and_then(length),
        attr("height").and_then(length),
    );
    let view_box = attr("viewBox").and_then(|v| {
        let parts: Vec<f32> = v
            .split([' ', ','])
            .filter(|p| !p.is_empty())
            .filter_map(|p| p.parse().ok())
            .collect();
        match parts.as_slice() {
            [_, _, w, h] if *w > 0.0 && *h > 0.0 => Some((*w, *h)),
            _ => None,
        }
    });
    match (width, height, view_box) {
        (Some(w), Some(h), _) => Some((w, h)),
        (Some(w), None, Some((vw, vh))) => Some((w, w * vh / vw)),
        (None, Some(h), Some((vw, vh))) => Some((h * vw / vh, h)),
        (None, None, Some(size)) => Some(size),
        _ => None,
    }
}

fn picture(item: &Picture<'_>, look: &Look, ids: &mut Ids) -> AnyElement {
    let file = look.doc.as_ref().and_then(|doc| (doc.image)(item.url));
    let Some(file) = file else {
        // Not a file this preview may draw (the web, another computer, not an image).
        return placeholder(
            item.alt,
            item.url,
            missing_reason(item.url),
            item.link,
            look,
            ids,
        );
    };
    let (alt, url) = (item.alt.to_owned(), item.url.to_owned());
    let (theme, tokens) = (look.theme, look.tokens);
    let svg = file
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
        .then(|| svg_size(&file))
        .flatten();
    let drawn = img(file)
        .max_w_full()
        .max_h(px(IMAGE_MAX_HEIGHT))
        .rounded(px(tokens.radius))
        .object_fit(ObjectFit::Contain)
        // An SVG has no pixels to ask: the box takes the shape of its viewBox, so that
        // `contain` leaves no blank band above and below.
        .when_some(svg, |image, (w, h)| image.w(px(w)).aspect_ratio(w / h))
        .with_fallback(move || {
            div()
                .px(px(tokens.spacing * 2.0))
                .py(px(tokens.spacing * 1.5))
                .border_1()
                .border_color(theme.border)
                .rounded(px(tokens.radius))
                .text_color(theme.muted)
                .text_size(px(tokens.secondary_font_size))
                .child(SharedString::from(format!(
                    "{}: {url}",
                    if alt.is_empty() {
                        "Image could not be drawn"
                    } else {
                        &alt
                    }
                )))
                .into_any_element()
        });
    match (item.link, &look.doc) {
        (Some(url), Some(doc)) => {
            let (url, doc) = (url.to_owned(), doc.clone());
            div()
                .id(ids.next("img"))
                .on_click(move |_, window, cx| (doc.open)(&url, window, cx))
                .child(drawn)
                .into_any_element()
        }
        _ => drawn.into_any_element(),
    }
}

/// A paragraph of a document that holds images: the images of a paragraph of nothing else in a
/// row; with text, the text and then the images that can be drawn (the others stay in the
/// text as their alt text).
fn paragraph_with_pictures(content: &[Inline], look: &Look, ids: &mut Ids) -> Option<AnyElement> {
    let found = pictures(content);
    if found.is_empty() {
        return None;
    }
    let tokens = &look.tokens;
    let alone = !has_text(content);
    let drawable: Vec<&Picture<'_>> = found
        .iter()
        .filter(|p| {
            look.doc
                .as_ref()
                .is_some_and(|doc| (doc.image)(p.url).is_some())
        })
        .collect();
    let row = |items: Vec<&Picture<'_>>, ids: &mut Ids| {
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .children(items.into_iter().map(|p| picture(p, look, ids)))
            .into_any_element()
    };
    if alone {
        return Some(row(found.iter().collect(), ids));
    }
    if drawable.is_empty() {
        return None;
    }
    let rest: Vec<Inline> = content
        .iter()
        .filter(|node| {
            !pictures(std::slice::from_ref(*node)).iter().any(|p| {
                look.doc
                    .as_ref()
                    .is_some_and(|doc| (doc.image)(p.url).is_some())
            })
        })
        .cloned()
        .collect();
    Some(
        div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 2.0))
            .child(inline_text(&rest, look, false, ids.next("p")))
            .child(row(drawable, ids))
            .into_any_element(),
    )
}

/// The narrowest a table's column is drawn, in pixels.
const TABLE_COLUMN_MIN: f32 = 110.0;

fn table(
    align: &[Align],
    header: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
    look: &Look,
    ids: &mut Ids,
) -> AnyElement {
    let (theme, tokens) = (&look.theme, &look.tokens);
    let row = |cells: &[Vec<Inline>], head: bool, first: bool, ids: &mut Ids| {
        div()
            .flex()
            .w_full()
            .when(head, |s| s.bg(theme.sidebar))
            .when(!first, |s| s.border_t_1().border_color(theme.border))
            .children(cells.iter().enumerate().map(|(column, content)| {
                let cell = div()
                    .flex_1()
                    .min_w(px(TABLE_COLUMN_MIN))
                    .px(px(tokens.spacing * 2.0))
                    .py(px(tokens.spacing))
                    .when(column > 0, |s| s.border_l_1().border_color(theme.border));
                let cell = match align.get(column) {
                    Some(Align::Center) => cell.text_center(),
                    Some(Align::Right) => cell.text_right(),
                    _ => cell,
                };
                cell.child(inline_text(content, look, head, ids.next("cell")))
            }))
            .into_any_element()
    };
    let mut lines = vec![row(header, true, true, ids)];
    for cells in rows {
        lines.push(row(cells, false, false, ids));
    }
    // A page narrower than its columns scrolls the table sideways rather than cutting words.
    let scroll_id = ids.next("table");
    let width = TABLE_COLUMN_MIN * align.len().max(1) as f32;
    div()
        .w_full()
        .rounded(px(tokens.radius))
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        .child(
            div().id(scroll_id).overflow_x_scroll().child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .min_w(px(width))
                    .children(lines),
            ),
        )
        .into_any_element()
}

/// Markdown blocks stacked.
pub fn blocks(items: &[Block], look: &Look, ids: &mut Ids) -> AnyElement {
    let tokens = &look.tokens;
    div()
        .flex()
        .flex_col()
        .gap(px(tokens.spacing * 3.0))
        .children(items.iter().map(|block| block_element(block, look, ids)))
        .into_any_element()
}

/// Whether a list item is a task (`- [ ] x`): its box stands in for the bullet.
fn is_task(item: &[Block]) -> bool {
    matches!(item.first(), Some(Block::Paragraph(first)) if matches!(first.first(), Some(Inline::Task(_))))
}

/// One block as an element. A document's preview draws its blocks one by one with this.
pub fn block_element(block: &Block, look: &Look, ids: &mut Ids) -> AnyElement {
    let (theme, tokens) = (&look.theme, &look.tokens);
    match block {
        Block::Paragraph(content) => paragraph_with_pictures(content, look, ids)
            .unwrap_or_else(|| inline_text(content, look, false, ids.next("p"))),
        Block::Heading { level, content } if look.doc.is_some() => {
            // A page's headings are titles: sized and weighted by level, the first two with a
            // hairline under them (GitHub's page).
            let base = tokens.prose_font_size;
            let (size, weight) = match level {
                1 => (base + 12.0, FontWeight::BOLD),
                2 => (base + 8.0, FontWeight::BOLD),
                3 => (base + 4.0, FontWeight::SEMIBOLD),
                4 => (base + 2.0, FontWeight::SEMIBOLD),
                _ => (base, FontWeight::SEMIBOLD),
            };
            div()
                .mt(px(tokens.spacing * 2.0))
                .when(*level <= 2, |s| {
                    s.pb(px(tokens.spacing))
                        .border_b_1()
                        .border_color(theme.border)
                })
                .child(heading_text(content, look, size, weight, ids.next("h")))
                .into_any_element()
        }
        Block::Heading { level, content } => {
            let size = match level {
                1 => tokens.prose_font_size + 5.0,
                2 => tokens.prose_font_size + 3.0,
                3 => tokens.prose_font_size + 1.0,
                _ => tokens.prose_font_size,
            };
            div()
                .mt(px(tokens.spacing))
                .text_size(px(size))
                .child(inline_text(content, look, true, ids.next("h")))
                .into_any_element()
        }
        Block::Code { lang, text } => code_block(text, lang.as_deref(), look, ids),
        Block::Table {
            align,
            header,
            rows,
        } => table(align, header, rows, look, ids),
        Block::List {
            ordered,
            start,
            items,
        } => div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 1.5))
            .children(items.iter().enumerate().map(|(index, item)| {
                let marker = if is_task(item) {
                    String::new()
                } else if *ordered {
                    format!("{}.", start + index as u64)
                } else {
                    "•".to_owned()
                };
                div()
                    .flex()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        div()
                            .flex_none()
                            .min_w(px(if marker.is_empty() {
                                0.0
                            } else {
                                tokens.prose_font_size * 1.4
                            }))
                            .prose(tokens)
                            .text_color(theme.muted)
                            .child(SharedString::from(marker)),
                    )
                    .child(div().flex_1().min_w_0().child(blocks(item, look, ids)))
            }))
            .into_any_element(),
        Block::Quote(inner) => div()
            .pl(px(tokens.spacing * 2.5))
            .border_l_2()
            .border_color(theme.border)
            .text_color(theme.muted)
            .child(blocks(inner, look, ids))
            .into_any_element(),
        Block::Rule => div()
            .h(px(1.0))
            .w_full()
            .my(px(tokens.spacing))
            .bg(theme.border)
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use hutan_chat_view::markdown::{Block, parse};

    use super::{has_text, is_task, missing_reason, pictures, svg_size_of};

    fn paragraph(text: &str) -> Vec<hutan_chat_view::markdown::Inline> {
        match parse(text).into_iter().next() {
            Some(Block::Paragraph(content)) => content,
            other => panic!("a paragraph, not {other:?}"),
        }
    }

    #[test]
    fn a_row_of_badges_is_pictures_without_text() {
        let content = paragraph("[![ci](ci.svg)](https://x.y/ci) ![v](v.png)\n![w](w.png)");
        let found = pictures(&content);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].url, "ci.svg");
        assert_eq!(found[0].link, Some("https://x.y/ci"));
        assert_eq!(found[1].link, None);
        assert!(!has_text(&content));
    }

    #[test]
    fn words_make_a_paragraph_text() {
        let content = paragraph("see ![the logo](logo.png) here");
        assert_eq!(pictures(&content).len(), 1);
        assert!(has_text(&content));
        assert!(pictures(&paragraph("no pictures")).is_empty());
        // A linked word is text; a linked picture is not.
        assert!(has_text(&paragraph("[word](a.md)")));
    }

    #[test]
    fn a_task_item_has_no_bullet() {
        let Some(Block::List { items, .. }) = parse("- [x] done\n- plain").into_iter().next()
        else {
            panic!("a list")
        };
        assert!(is_task(&items[0]));
        assert!(!is_task(&items[1]));
    }

    #[test]
    fn an_svg_is_as_big_as_it_says_and_else_as_its_view_box() {
        assert_eq!(
            svg_size_of("<svg xmlns=\"x\" width=\"120\" height=\"40px\" viewBox=\"0 0 10 10\">"),
            Some((120.0, 40.0))
        );
        assert_eq!(
            svg_size_of("<?xml version=\"1.0\"?>\n<svg viewBox=\"0 0 400 100\" xmlns=\"x\">"),
            Some((400.0, 100.0))
        );
        // One side and a view box: the other follows the shape.
        assert_eq!(
            svg_size_of("<svg width='200' viewBox='0,0,400,100'>"),
            Some((200.0, 50.0))
        );
        assert_eq!(
            svg_size_of("<svg height=\"50\" viewBox=\"0 0 400 100\">"),
            Some((200.0, 50.0))
        );
        // Nothing to go by: a percentage, no size at all, not an SVG.
        assert_eq!(svg_size_of("<svg width=\"100%\" height=\"100%\">"), None);
        assert_eq!(svg_size_of("<svg>"), None);
        assert_eq!(svg_size_of("<html></html>"), None);
        // `data-width` is not `width`.
        assert_eq!(
            svg_size_of("<svg data-width=\"9\" viewBox=\"0 0 4 2\">"),
            Some((4.0, 2.0))
        );
    }

    #[test]
    fn a_missing_image_says_why() {
        assert_eq!(missing_reason("img/gone.png"), "Image not found");
        assert_eq!(
            missing_reason("https://x.y/a.png"),
            "Web images are not loaded"
        );
        assert_eq!(
            missing_reason("HTTP://x.y/a.png"),
            "Web images are not loaded"
        );
    }
}
