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
    StatefulInteractiveElement, Styled, StyledImage as _, StyledText, TextRun, UnderlineStyle,
    Window, div, font, img, px,
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
            mono: TerminalStyle::get(cx).font_family.into(),
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
            Font::default()
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

/// Text runs for `styled`'s spans. A `heading` is set in the UI sans at medium weight, a
/// paragraph in the look's text face.
fn runs(spans: &[Span], look: &Look, heading: bool) -> Vec<TextRun> {
    let theme = &look.theme;
    spans
        .iter()
        .map(|span| {
            let style = &span.style;
            let mut face = if style.code {
                font(look.mono.clone())
            } else if heading {
                Font::default()
            } else {
                look.text_face()
            };
            if heading {
                face.weight = FontWeight::MEDIUM;
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
    let policy = if look.doc.is_some() {
        Links::Any
    } else {
        Links::Web
    };
    let styled = flatten_links(content, policy);
    if styled.text.is_empty() {
        return div().into_any_element();
    }
    let text_runs = runs(&styled.spans, look, bold);
    let links: Vec<(std::ops::Range<usize>, String)> = styled
        .spans
        .iter()
        .filter_map(|span| span.style.link.clone().map(|url| (span.range.clone(), url)))
        .collect();
    let text = StyledText::new(styled.text).with_runs(text_runs);
    if links.is_empty() {
        return look.text_box().child(text).into_any_element();
    }
    let ranges = links.iter().map(|(range, _)| range.clone()).collect();
    let urls: Vec<String> = links.into_iter().map(|(_, url)| url).collect();
    let doc = look.doc.clone();
    look.text_box()
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

fn picture(item: &Picture<'_>, look: &Look, ids: &mut Ids) -> AnyElement {
    let label = |alt: &str, url: &str| {
        let shown = if alt.is_empty() { url } else { alt };
        vec![Inline::Text(shown.to_owned())]
    };
    let file = look.doc.as_ref().and_then(|doc| (doc.image)(item.url));
    let Some(file) = file else {
        // Not a file this preview may draw (the web, another computer, not an image): the
        // alt text, linked when the image was.
        let text = label(item.alt, item.url);
        let content = match item.link {
            Some(url) => vec![Inline::Link {
                content: text,
                url: url.to_owned(),
            }],
            None => text,
        };
        return inline_text(&content, look, false, ids.next("alt"));
    };
    let alt = item.alt.to_owned();
    let (theme, tokens) = (look.theme, look.tokens);
    let drawn = img(file)
        .max_w_full()
        .max_h(px(IMAGE_MAX_HEIGHT))
        .rounded(px(tokens.radius))
        .object_fit(ObjectFit::Contain)
        .with_fallback(move || {
            div()
                .text_color(theme.muted)
                .text_size(px(tokens.secondary_font_size))
                .child(SharedString::from(format!("[{alt}]")))
                .into_any_element()
        });
    match (item.link, &look.doc) {
        (Some(url), Some(doc)) => {
            let (url, doc) = (url.to_owned(), doc.clone());
            div()
                .id(ids.next("img"))
                .cursor_pointer()
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
                    .min_w_0()
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
    div()
        .flex()
        .flex_col()
        .w_full()
        .rounded(px(tokens.radius))
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        .children(lines)
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

    use super::{has_text, is_task, pictures};

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
}
