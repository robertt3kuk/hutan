//! The numbers of a scrollbar, free of GPUI: where the thumb sits for a scroll position, which
//! scroll position a thumb position means, and where a marker goes. Units are the scroller's own
//! (pixels for a scrolled element, lines for a terminal's scrollback); the track is in pixels.

/// The thumb's thickness; the hit area is a little wider.
pub const THICKNESS: f32 = 7.0;
/// Space between the thumb and the edge of what it scrolls.
pub const INSET: f32 = 2.0;
/// The shortest the thumb gets, so it can always be grabbed.
pub const MIN_THUMB: f32 = 28.0;
/// Width of the strip along the edge in which the pointer counts as "near" the bar.
pub const ZONE: f32 = 16.0;
/// How much more than the drawn thumb and track the pointer may be off and still hit them.
pub const SLOP: f32 = 2.0;
/// Width of a marker tick on the track.
pub const MARK_WIDTH: f32 = 5.0;
/// The least a marker is tall, so one line of a long file stays visible.
pub const MIN_MARK: f32 = 2.0;

/// What a scroller reports about its position, along one axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// How much of the content is in view.
    pub viewport: f32,
    /// How far the content can scroll (positive); `0` while everything fits.
    pub max: f32,
    /// How far it is scrolled from the start (`0..=max`).
    pub offset: f32,
}

impl Metrics {
    /// Nothing to scroll.
    pub const FITS: Self = Self {
        viewport: 0.0,
        max: 0.0,
        offset: 0.0,
    };

    /// Whether there is more content than room: a bar is shown only then.
    pub fn scrollable(&self) -> bool {
        self.max > 0.5 && self.viewport > 0.0
    }

    /// The whole content's extent.
    pub fn content(&self) -> f32 {
        self.viewport + self.max
    }
}

/// The thumb along a track, in pixels from the track's start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thumb {
    pub start: f32,
    pub len: f32,
    /// How far the thumb can travel along the track.
    pub travel: f32,
}

/// The thumb for `metrics` on a track `track` pixels long: as long as the share of the content in
/// view (never shorter than [`MIN_THUMB`]), as far along as the share scrolled. `None` while
/// everything fits.
pub fn thumb(metrics: Metrics, track: f32) -> Option<Thumb> {
    if !metrics.scrollable() {
        return None;
    }
    let len = (track * (metrics.viewport / metrics.content())).clamp(MIN_THUMB.min(track), track);
    let travel = track - len;
    let start = travel * (metrics.offset / metrics.max).clamp(0.0, 1.0);
    Some(Thumb { start, len, travel })
}

/// The scroll position a thumb whose start is `thumb_start` pixels along its track means.
pub fn offset_for(max: f32, travel: f32, thumb_start: f32) -> f32 {
    if travel <= 0.0 {
        return 0.0;
    }
    max * (thumb_start / travel).clamp(0.0, 1.0)
}

/// Where a click on the track beside the thumb scrolls to: a page (the viewport) towards it.
pub fn page_target(metrics: Metrics, towards_end: bool) -> f32 {
    let to = if towards_end {
        metrics.offset + metrics.viewport
    } else {
        metrics.offset - metrics.viewport
    };
    to.clamp(0.0, metrics.max)
}

/// Where a marker at `at` (`0..=1` of the content) spanning `len` of it goes on a track `track`
/// pixels long: its start and its height.
pub fn mark_span(at: f32, len: f32, track: f32) -> (f32, f32) {
    let height = (len.max(0.0) * track).max(MIN_MARK);
    let start = (at.clamp(0.0, 1.0) * track).min((track - height).max(0.0));
    (start, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(viewport: f32, max: f32, offset: f32) -> Metrics {
        Metrics {
            viewport,
            max,
            offset,
        }
    }

    #[test]
    fn nothing_to_scroll_has_no_thumb() {
        assert_eq!(thumb(metrics(400.0, 0.0, 0.0), 396.0), None);
        assert_eq!(
            thumb(metrics(400.0, 0.3, 0.0), 396.0),
            None,
            "sub-pixel slack"
        );
        assert_eq!(thumb(Metrics::FITS, 396.0), None);
    }

    #[test]
    fn the_thumb_is_the_share_in_view_at_the_share_scrolled() {
        // 400 of 1 200 in view: a third of the track.
        let top = thumb(metrics(400.0, 800.0, 0.0), 300.0).expect("scrollable");
        assert_eq!((top.start, top.len, top.travel), (0.0, 100.0, 200.0));
        let middle = thumb(metrics(400.0, 800.0, 400.0), 300.0).expect("scrollable");
        assert_eq!(middle.start, 100.0);
        let end = thumb(metrics(400.0, 800.0, 800.0), 300.0).expect("scrollable");
        assert_eq!(
            end.start + end.len,
            300.0,
            "the thumb ends at the track's end"
        );
    }

    #[test]
    fn the_thumb_never_gets_too_short_to_grab() {
        let t = thumb(metrics(400.0, 400_000.0, 0.0), 300.0).expect("scrollable");
        assert_eq!(t.len, MIN_THUMB);
        assert_eq!(t.travel, 300.0 - MIN_THUMB);
    }

    #[test]
    fn a_thumb_position_means_the_scroll_position_that_puts_it_there() {
        let t = thumb(metrics(400.0, 800.0, 250.0), 300.0).expect("scrollable");
        let back = offset_for(800.0, t.travel, t.start);
        assert!((back - 250.0).abs() < 0.01, "{back}");
        assert_eq!(
            offset_for(800.0, t.travel, -50.0),
            0.0,
            "clamped at the start"
        );
        assert_eq!(
            offset_for(800.0, t.travel, 9_999.0),
            800.0,
            "clamped at the end"
        );
        assert_eq!(offset_for(800.0, 0.0, 10.0), 0.0, "no travel, no scroll");
    }

    #[test]
    fn a_click_beside_the_thumb_pages_towards_it_and_stops_at_the_ends() {
        let m = metrics(400.0, 1000.0, 300.0);
        assert_eq!(page_target(m, true), 700.0);
        assert_eq!(page_target(m, false), 0.0);
        assert_eq!(page_target(metrics(400.0, 1000.0, 900.0), true), 1000.0);
    }

    #[test]
    fn a_marker_keeps_a_visible_height_and_stays_on_the_track() {
        assert_eq!(mark_span(0.5, 0.0, 200.0), (100.0, MIN_MARK));
        assert_eq!(mark_span(1.0, 0.0, 200.0), (200.0 - MIN_MARK, MIN_MARK));
        assert_eq!(mark_span(0.25, 0.1, 200.0), (50.0, 20.0));
    }
}
