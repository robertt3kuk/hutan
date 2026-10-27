//! The editor's scrollbars (SCROLLBARS): a vertical thumb along the display rows (so a wrapped
//! file's thumb counts its wrapped rows), a horizontal one while the view does not wrap and a
//! line is wider than the text area, and colored ticks on the vertical track for diagnostics, git
//! changes and find matches (a lite overview ruler). The bars are `hutan_ui::Scrollbar`s over a
//! [`Scroller`] that reads and moves the view's own pixel offsets.
//!
//! The ticks are computed when the text, the diagnostics, the git hunks, the find result, the
//! wrapping or the colors change, never per frame: the view keeps the last answer with the key it
//! was computed for.

use std::rc::Rc;

use gpui::{App, Bounds, Hsla, WeakEntity, Window, point, size};
use hutan_ui::{Axis, Mark, Metrics, Region, Scrollbar, Scroller};

use crate::agent::HunkKind;
use crate::diagnostics::Severity;
use crate::style::rgba;
use crate::view::CodeView;

/// Ticks per kind are merged into this many buckets down the track, so a file with ten thousand
/// matches draws a few hundred quads.
const BUCKETS: usize = 512;

/// The bars of one view.
pub(crate) struct Bars {
    pub(crate) y: Scrollbar,
    pub(crate) x: Scrollbar,
}

impl Bars {
    pub(crate) fn new(view: WeakEntity<CodeView>) -> Self {
        Self {
            // The vertical track stops short of the corner the horizontal bar takes.
            y: Scrollbar::of(Vertical(view.clone()), Axis::Vertical).leave_corner(),
            x: Scrollbar::of(Horizontal(view), Axis::Horizontal),
        }
    }
}

struct Vertical(WeakEntity<CodeView>);

impl Scroller for Vertical {
    fn metrics(&self, cx: &App) -> Metrics {
        let Some(view) = self.0.upgrade() else {
            return Metrics::FITS;
        };
        let view = view.read(cx);
        let Some(layout) = view.layout else {
            return Metrics::FITS;
        };
        Metrics {
            viewport: f32::from(layout.bounds.size.height),
            max: view.max_scroll_y(cx),
            offset: view.scroll_y.max(0.0),
        }
    }

    fn set_offset(&self, offset: f32, _window: &mut Window, cx: &mut App) {
        if let Some(view) = self.0.upgrade() {
            view.update(cx, |view, cx| view.scroll_to(offset, cx));
        }
    }
}

struct Horizontal(WeakEntity<CodeView>);

impl Scroller for Horizontal {
    fn metrics(&self, cx: &App) -> Metrics {
        let Some(view) = self.0.upgrade() else {
            return Metrics::FITS;
        };
        let view = view.read(cx);
        let Some(layout) = view.layout else {
            return Metrics::FITS;
        };
        let visible = f32::from(layout.bounds.right() - layout.text_left).max(0.0);
        // Only a line that really is wider than the text area gives a bar; the room the view
        // scrolls past the widest line (half a screen) then counts as content.
        if view.is_wrapping(cx) || (view.widest as f32) * f32::from(layout.cell_width) <= visible {
            return Metrics::FITS;
        }
        Metrics {
            viewport: visible,
            max: view.max_scroll_x(cx),
            offset: view.scroll_x.max(0.0),
        }
    }

    fn region(&self, cx: &App) -> Option<Region> {
        let layout = self.0.upgrade()?.read(cx).layout?;
        let text = Bounds::new(
            point(layout.text_left, layout.bounds.origin.y),
            size(
                (layout.bounds.right() - layout.text_left).max(gpui::px(0.0)),
                layout.bounds.size.height,
            ),
        );
        Some(Region {
            extent: text,
            edge: layout.bounds,
        })
    }

    fn set_offset(&self, offset: f32, _window: &mut Window, cx: &mut App) {
        if let Some(view) = self.0.upgrade() {
            view.update(cx, |view, cx| view.scroll_to_x(offset, cx));
        }
    }
}

/// What the ticks were computed for: any change of one part recomputes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MarkKey {
    version: u64,
    diagnostics: u64,
    hunks: usize,
    find: (u64, u64, usize, Option<usize>),
    rows: usize,
    style: u64,
}

/// The ticks and the key they answer.
#[derive(Default)]
pub(crate) struct MarkCache {
    cached: Option<(MarkKey, Rc<[Mark]>)>,
}

/// The kinds of tick, in paint order: later ones cover earlier ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Lane {
    Find,
    Git,
    Diagnostic,
}

/// Collects ticks, one per kind and bucket (the strongest of those that fall into it).
struct Collector {
    rows: f32,
    seen: Vec<(Lane, usize, u8, Hsla)>,
}

impl Collector {
    fn add(&mut self, lane: Lane, first_row: usize, last_row: usize, strength: u8, color: Hsla) {
        let bucket =
            |row: usize| ((row as f32 / self.rows * BUCKETS as f32) as usize).min(BUCKETS - 1);
        for at in bucket(first_row)..=bucket(last_row) {
            self.seen.push((lane, at, strength, color));
        }
    }

    /// Paint order: by kind, then down the track.
    fn finish(mut self) -> Vec<Mark> {
        self.seen
            .sort_by_key(|&(lane, bucket, strength, _)| (lane, bucket, strength));
        let len = 1.0 / BUCKETS as f32;
        let mut marks = Vec::new();
        for (index, &(lane, bucket, _, color)) in self.seen.iter().enumerate() {
            // Entries of one kind and bucket are in ascending strength: keep the last.
            let next = self.seen.get(index + 1).map(|n| (n.0, n.1));
            if next != Some((lane, bucket)) {
                marks.push(Mark {
                    at: bucket as f32 * len,
                    len,
                    color,
                });
            }
        }
        marks
    }
}

impl CodeView {
    /// How far the view may scroll sideways, in pixels: the widest line seen can reach the middle
    /// of the text area; a wrapped view never scrolls sideways.
    pub fn max_scroll_x(&self, cx: &App) -> f32 {
        let Some(layout) = self.layout else {
            return 0.0;
        };
        if self.is_wrapping(cx) {
            return 0.0;
        }
        let visible = f32::from(layout.bounds.right() - layout.text_left).max(0.0);
        (self.widest as f32 * f32::from(layout.cell_width) - visible * 0.5).max(0.0)
    }

    /// The vertical scrollbar (tests and probes read where its thumb is).
    pub fn vertical_scrollbar(&self) -> &Scrollbar {
        &self.bars.y
    }

    /// The horizontal scrollbar; it has a thumb while a line is wider than the text area.
    pub fn horizontal_scrollbar(&self) -> &Scrollbar {
        &self.bars.x
    }

    /// Scrolls to `x` pixels from the left edge of the text area (the horizontal bar).
    pub fn scroll_to_x(&mut self, x: f32, cx: &mut gpui::Context<Self>) {
        self.scroll_x = x;
        self.clamp_scroll(cx);
        cx.notify();
    }

    /// The ticks of the vertical track: diagnostics, git changes and find matches, one display
    /// row each (a hunk spans its rows), computed again only when one of them changed.
    pub fn scroll_marks(&mut self, cx: &App) -> Rc<[Mark]> {
        let (buffer, hunks) = {
            let buffer = self.buffer().read(cx);
            (buffer.version(), buffer.git_hunks())
        };
        let key = MarkKey {
            version: buffer,
            diagnostics: self.buffer().read(cx).diagnostics_epoch(),
            // An empty list is a fresh allocation each time it is asked for: it has no identity.
            hunks: if hunks.is_empty() {
                0
            } else {
                hunks.as_ptr() as usize
            },
            find: (
                self.find.generation,
                self.find.version,
                self.find.result.matches.len(),
                self.find.current,
            ),
            rows: self.display_rows(cx),
            style: self.style_generation,
        };
        if let Some((known, marks)) = &self.marks.cached
            && *known == key
        {
            return Rc::clone(marks);
        }
        let marks: Rc<[Mark]> = Rc::from(self.compute_marks(cx));
        self.marks.cached = Some((key, Rc::clone(&marks)));
        marks
    }

    fn compute_marks(&self, cx: &App) -> Vec<Mark> {
        let buffer = self.buffer().read(cx);
        if !buffer.is_ready() {
            return Vec::new();
        }
        let text = buffer.text();
        let theme = &self.style.theme;
        let solid = |color: hutan_settings::Rgba| rgba(hutan_settings::Rgba { a: 1.0, ..color });
        let mut collector = Collector {
            rows: self.display_rows(cx).max(1) as f32,
            seen: Vec::new(),
        };
        let row_of = |at: usize| self.row_of_offset(at.min(text.len()), cx);

        for (index, found) in self.find.result.matches.iter().enumerate() {
            let color = if self.find.current == Some(index) {
                theme.search_match_active
            } else {
                theme.search_match
            };
            let row = row_of(found.start);
            collector.add(Lane::Find, row, row, 0, solid(color));
        }
        for hunk in buffer.git_hunks().iter() {
            let color = match hunk.kind() {
                HunkKind::Added => theme.diff_added,
                HunkKind::Modified => theme.diff_modified,
                HunkKind::Removed => theme.diff_removed,
            };
            let first = row_of(hunk.bytes.start);
            let last = if hunk.bytes.is_empty() {
                first
            } else {
                row_of(hunk.bytes.end.saturating_sub(1)).max(first)
            };
            collector.add(Lane::Git, first, last, 0, solid(color));
        }
        for diagnostic in buffer.diagnostics().iter() {
            let color = self.style.diagnostic_color(diagnostic.severity);
            let row = row_of(diagnostic.bytes.start);
            // The worse severity of a bucket is the one drawn.
            let strength = match diagnostic.severity {
                Severity::Hint => 0,
                Severity::Info => 1,
                Severity::Warning => 2,
                Severity::Error => 3,
            };
            collector.add(Lane::Diagnostic, row, row, strength, color);
        }
        collector.finish()
    }
}
