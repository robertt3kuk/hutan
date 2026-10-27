//! The hover popover of a code view (slice E-12a): the diagnostics under the pointer (or the
//! caret) and what the language server says about the symbol there.
//!
//! - **When.** The pointer rests on text for [`HOVER_DELAY`], or `K` (Vim's Normal mode) /
//!   *Editor: Show Hover* (⌘K ⌘I in the Zed keymap) at the caret. The view shows the
//!   diagnostics it has at once and asks its owner for the server's text
//!   ([`CodeViewEvent::HoverRequested`]); the answer arrives with [`CodeView::set_hover_text`].
//! - **Dismissed** when the pointer moves off the word, the caret moves away from where `K`
//!   showed it, the text changes, or Escape.
//! - **Markdown** is drawn as plain styled text: fenced code blocks in the code font, headings
//!   and `**bold**` in a heavier weight, emphasis and inline code markers dropped
//!   ([`markdown_blocks`]).

use std::ops::Range;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, anchored, deferred, div, point, px,
};

use crate::diagnostics::Diagnostic;
use crate::style::rgba;
use crate::view::{CodeView, CodeViewEvent};

/// How long the pointer rests on text before the popover shows.
pub const HOVER_DELAY: Duration = Duration::from_millis(500);
/// Most characters of a server's text the popover keeps.
const MAX_HOVER_CHARS: usize = 8 * 1024;

/// A block of a hover text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoverBlock {
    /// A paragraph or a heading (`strong`), markers removed.
    Text { text: String, strong: bool },
    /// A fenced (or indented) code block, drawn in the code font.
    Code(String),
}

/// What showed the popover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoverSource {
    /// The pointer rested on the text.
    Pointer,
    /// `K` or *Show Hover* at the caret.
    Caret,
}

/// The popover while it shows.
#[derive(Debug, Clone, PartialEq)]
pub struct HoverState {
    /// The byte the popover is about.
    pub offset: usize,
    /// The word around it: the pointer may move inside it.
    pub(crate) word: Range<usize>,
    pub source: HoverSource,
    /// The diagnostics about `offset`, the most serious first.
    pub diagnostics: Vec<Diagnostic>,
    /// The server's text, once it answered (`None` while asked or when it has nothing).
    pub blocks: Option<Vec<HoverBlock>>,
    /// The server was asked and has not answered yet.
    pub pending: bool,
}

impl HoverState {
    /// Whether there is anything to draw.
    fn has_content(&self) -> bool {
        !self.diagnostics.is_empty() || self.blocks.as_ref().is_some_and(|b| !b.is_empty())
    }

    /// The popover's text, its blocks joined by blank lines (tests).
    pub fn text(&self) -> String {
        let mut parts: Vec<String> = self
            .diagnostics
            .iter()
            .map(|d| match d.origin() {
                Some(origin) => format!("{} {} {}", d.severity.symbol(), d.message, origin),
                None => format!("{} {}", d.severity.symbol(), d.message),
            })
            .collect();
        for block in self.blocks.iter().flatten() {
            parts.push(match block {
                HoverBlock::Text { text, .. } | HoverBlock::Code(text) => text.clone(),
            });
        }
        parts.join("\n\n")
    }
}

/// `markdown` as blocks of plain text: paragraphs (lines joined), headings, code blocks.
pub fn markdown_blocks(markdown: &str) -> Vec<HoverBlock> {
    let markdown = match markdown.char_indices().nth(MAX_HOVER_CHARS) {
        Some((cut, _)) => &markdown[..cut],
        None => markdown,
    };
    let mut blocks = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut code: Option<Vec<String>> = None;
    let flush = |paragraph: &mut Vec<String>, blocks: &mut Vec<HoverBlock>| {
        if !paragraph.is_empty() {
            let text = paragraph.join(" ");
            blocks.push(HoverBlock::Text {
                text: inline(&text),
                strong: false,
            });
            paragraph.clear();
        }
    };
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            match code.take() {
                Some(lines) => blocks.push(HoverBlock::Code(lines.join("\n"))),
                None => {
                    flush(&mut paragraph, &mut blocks);
                    code = Some(Vec::new());
                }
            }
            continue;
        }
        if let Some(lines) = &mut code {
            lines.push(line.to_owned());
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut paragraph, &mut blocks);
            continue;
        }
        if trimmed.chars().all(|c| matches!(c, '-' | '*' | '_')) && trimmed.len() >= 3 {
            // A rule: a paragraph break.
            flush(&mut paragraph, &mut blocks);
            continue;
        }
        if let Some(heading) = heading(trimmed) {
            flush(&mut paragraph, &mut blocks);
            blocks.push(HoverBlock::Text {
                text: inline(heading),
                strong: true,
            });
            continue;
        }
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            // A list item stays a line of its own.
            flush(&mut paragraph, &mut blocks);
            paragraph.push(format!("• {}", &trimmed[2..]));
            flush(&mut paragraph, &mut blocks);
            continue;
        }
        paragraph.push(trimmed.to_owned());
    }
    if let Some(lines) = code {
        blocks.push(HoverBlock::Code(lines.join("\n")));
    }
    flush(&mut paragraph, &mut blocks);
    blocks.retain(|b| match b {
        HoverBlock::Text { text, .. } => !text.trim().is_empty(),
        HoverBlock::Code(text) => !text.trim().is_empty(),
    });
    blocks
}

fn heading(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    (1..=6)
        .contains(&hashes)
        .then(|| line[hashes..].strip_prefix(' '))
        .flatten()
}

/// A line with its inline markers dropped: `**` and `__`, backticks, `*emphasis*` (a `*` that
/// opens before a word and closes after one; a pointer type's `*const` stays), link targets
/// (`[text](url)` → `text`), backslash escapes. A single `_` stays (`snake_case`).
fn inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut emphasis = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let before = i.checked_sub(1).map(|j| chars[j]);
        match c {
            '\\' if next.is_some_and(|n| n.is_ascii_punctuation()) => {
                out.extend(next);
                i += 2;
                continue;
            }
            '`' => {}
            '*' | '_' if next == Some(c) => {
                i += 2;
                continue;
            }
            '*' if emphasis && before.is_some_and(|b| !b.is_whitespace()) => emphasis = false,
            '*' if !emphasis
                && next.is_some_and(|n| !n.is_whitespace())
                && chars[i + 1..]
                    .windows(2)
                    .any(|w| w[1] == '*' && !w[0].is_whitespace()) =>
            {
                emphasis = true;
            }
            '[' => {
                if let Some(close) = chars[i..].iter().position(|c| *c == ']')
                    && chars.get(i + close + 1) == Some(&'(')
                    && let Some(end) = chars[i + close..].iter().position(|c| *c == ')')
                {
                    out.extend(&chars[i + 1..i + close]);
                    i += close + end + 1;
                    continue;
                }
                out.push(c);
            }
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

impl CodeView {
    /// The popover, while it shows.
    pub fn hover(&self) -> Option<&HoverState> {
        self.hover.as_ref()
    }

    /// Shows the popover for byte `offset`: the diagnostics there at once, and asks the owner
    /// for the server's text.
    pub fn show_hover_at(&mut self, offset: usize, source: HoverSource, cx: &mut Context<Self>) {
        self.hover_task = None;
        let (diagnostics, word) = {
            let buffer = self.buffer().read(cx);
            if !buffer.is_ready() {
                return;
            }
            let text = buffer.text();
            let offset = offset.min(text.len());
            (
                buffer.diagnostics_at(offset),
                crate::view::word_at(text, offset),
            )
        };
        self.hover = Some(HoverState {
            offset,
            word,
            source,
            diagnostics,
            blocks: None,
            pending: true,
        });
        cx.emit(CodeViewEvent::HoverRequested { offset });
        cx.notify();
    }

    /// *Show Hover* / `K`: the popover at the primary caret.
    pub fn show_hover_at_caret(&mut self, cx: &mut Context<Self>) {
        let head = self.selections.primary().head;
        self.show_hover_at(head, HoverSource::Caret, cx);
    }

    /// The server's answer for the popover at `offset` (`None`: it has nothing to say). A
    /// popover with nothing at all to show goes.
    pub fn set_hover_text(
        &mut self,
        offset: usize,
        markdown: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(hover) = &mut self.hover else {
            return;
        };
        if hover.offset != offset {
            return;
        }
        hover.pending = false;
        hover.blocks = markdown.map(markdown_blocks).filter(|b| !b.is_empty());
        if !hover.has_content() {
            self.hover = None;
        }
        cx.notify();
    }

    /// Closes the popover (and forgets a rest of the pointer that would open it). Returns
    /// whether one was showing.
    pub fn dismiss_hover(&mut self, cx: &mut Context<Self>) -> bool {
        self.hover_task = None;
        self.hover_pointer = None;
        // The git peek is dismissed the same way (Escape, a click in the text).
        let peek = self.git_peek.take().is_some();
        if self.hover.take().is_some() || peek {
            cx.notify();
            return true;
        }
        false
    }

    /// The pointer moved over the view (no button pressed): a popover it left goes, and a rest
    /// on a character opens one after [`HOVER_DELAY`].
    pub(crate) fn hover_pointer_moved(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let at = self.text_offset_under(position, cx);
        if let Some(hover) = &self.hover
            && hover.source == HoverSource::Pointer
            && at.is_none_or(|at| !(hover.word.contains(&at) || hover.word.start == at))
        {
            self.hover = None;
            cx.notify();
        }
        if at == self.hover_pointer {
            return;
        }
        self.hover_pointer = at;
        self.hover_task = None;
        let Some(at) = at else {
            return;
        };
        if self.hover.is_some() {
            return;
        }
        self.hover_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_DELAY).await;
            this.update(cx, |this, cx| {
                if this.hover_pointer == Some(at) {
                    this.show_hover_at(at, HoverSource::Pointer, cx);
                }
            })
            .ok();
        }));
    }

    /// The byte under window position `position` when it is over a character of the text
    /// (not the gutter, not past the end of a line or of the file).
    fn text_offset_under(
        &self,
        position: gpui::Point<gpui::Pixels>,
        cx: &gpui::App,
    ) -> Option<usize> {
        let layout = self.layout?;
        if !layout.bounds.contains(&position) || position.x < layout.text_left {
            return None;
        }
        let y = f32::from(position.y - layout.bounds.top()) + self.scroll_y;
        let row = (y / f32::from(layout.line_height)).floor() as usize;
        if row >= self.display_rows(cx) {
            return None;
        }
        let (at, past_row_end) = self.offset_at_affinity(position, cx)?;
        if past_row_end {
            return None;
        }
        let text = self.buffer().read(cx).text();
        let content = text.line_content_range(text.line_of_byte(at).ok()?).ok()?;
        (at < content.end).then_some(at)
    }

    /// The popover over a line's blame (GIT-HISTORY): the commit, who made it and when, below
    /// the line.
    pub(crate) fn render_blame_hover(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let row = self.blame_hover?;
        let layout = self.layout?;
        let area = self.blame_area?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
        let lines = {
            let buffer = self.buffer().read(cx);
            crate::blame::details(buffer.blame_line(row)?, now)
        };
        let theme = &self.style.theme;
        let mut border = rgba(theme.gutter_foreground);
        border.a *= 0.35;
        let y = layout.bounds.top()
            + layout.line_height * (self.first_row_of_line(row, cx) as f32 + 1.0)
            - gpui::px(self.scroll_y);
        let mut body = div()
            .id("code-blame-hover")
            .flex()
            .flex_col()
            .gap(px(2.0))
            .max_w(px(f32::from(layout.cell_width) * 72.0))
            .px(px(10.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(border)
            .bg(rgba(theme.gutter_background))
            .shadow_md()
            .text_size(px(self.style.font_size * 0.95))
            .text_color(rgba(theme.foreground))
            .on_mouse_move(|_, _, cx| cx.stop_propagation());
        for (index, line) in lines.into_iter().enumerate() {
            body = body.child(
                div()
                    .when(index == 0, |s| s.font_weight(FontWeight::SEMIBOLD))
                    .when(index > 0, |s| s.text_color(rgba(theme.gutter_foreground)))
                    .child(line),
            );
        }
        Some(
            deferred(anchored().position(point(area.left(), y)).child(body))
                .with_priority(1)
                .into_any_element(),
        )
    }

    /// The popover as an overlay below the line it is about.
    pub(crate) fn render_hover(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let hover = self.hover.as_ref().filter(|h| h.has_content())?;
        let layout = self.layout?;
        let at = self.point_of(hover.offset, cx)?;
        let theme = &self.style.theme;
        let fg = rgba(theme.foreground);
        let muted = rgba(theme.gutter_foreground);
        let mut border = rgba(theme.gutter_foreground);
        border.a *= 0.35;
        let mut body = div()
            .id("code-hover")
            .flex()
            .flex_col()
            .gap(px(6.0))
            .max_w(px(f32::from(layout.cell_width) * 72.0))
            .max_h(px(f32::from(layout.line_height) * 16.0))
            .overflow_y_scroll()
            .px(px(10.0))
            .py(px(8.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(border)
            .bg(rgba(theme.gutter_background))
            .shadow_md()
            .text_size(px(self.style.font_size * 0.95))
            .text_color(fg)
            // The pointer may move onto the popover without closing it.
            .on_mouse_move(|_, _, cx| cx.stop_propagation());
        for d in &hover.diagnostics {
            let color = self.style.diagnostic_color(d.severity);
            body = body.child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_none()
                            .text_color(color)
                            .child(d.severity.symbol()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(d.message.to_string())
                            .when_some(d.origin(), |s, origin| {
                                s.child(div().text_color(muted).child(origin))
                            }),
                    ),
            );
        }
        for block in hover.blocks.iter().flatten() {
            body = body.child(match block {
                HoverBlock::Text { text, strong } => div()
                    .when(*strong, |s| s.font_weight(FontWeight::SEMIBOLD))
                    .child(text.clone()),
                HoverBlock::Code(code) => div()
                    .font_family(self.style.font_family.clone())
                    .text_color(fg)
                    .whitespace_nowrap()
                    .children(code.lines().map(|l| div().child(l.to_owned()))),
            });
        }
        let below = point(at.x, at.y + layout.line_height);
        Some(
            deferred(anchored().position(below).child(body))
                .with_priority(1)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_becomes_plain_blocks() {
        let blocks = markdown_blocks(
            "```rust\nfn main()\n```\n---\n# Title\nSome **bold** and `code`\nnext line\n\n- item [link](http://x)\n",
        );
        assert_eq!(
            blocks,
            vec![
                HoverBlock::Code("fn main()".into()),
                HoverBlock::Text {
                    text: "Title".into(),
                    strong: true
                },
                HoverBlock::Text {
                    text: "Some bold and code next line".into(),
                    strong: false
                },
                HoverBlock::Text {
                    text: "• item link".into(),
                    strong: false
                },
            ]
        );
        // A pointer type and snake_case survive.
        assert_eq!(
            markdown_blocks("takes *const u8 and my_value"),
            vec![HoverBlock::Text {
                text: "takes *const u8 and my_value".into(),
                strong: false
            }]
        );
        assert!(markdown_blocks("  \n\n").is_empty());
    }
}
