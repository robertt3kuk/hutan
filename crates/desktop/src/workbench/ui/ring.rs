//! A ring meter: a track circle and an arc over it that starts at the top and runs clockwise
//! for `fraction` of the way round (STATUS-METERS: the status bar's usage rings). Pure paint, no
//! state; whatever sits inside it (a harness's glyph) is the caller's.

use std::f32::consts::{FRAC_PI_2, TAU};

use gpui::{Hsla, IntoElement, PathBuilder, Pixels, Styled as _, canvas, point, px};

/// Segments of a full turn: enough for a smooth 18 px ring.
const SEGMENTS: usize = 64;
/// An arc under this share of the turn is drawn this long, so a window that has hardly begun
/// still shows as something.
const MIN_FRACTION: f32 = 0.04;

/// The points of an arc of `radius` round `center`, from the top, `sweep` radians clockwise.
fn arc_points(
    center: gpui::Point<Pixels>,
    radius: f32,
    sweep: f32,
) -> impl Iterator<Item = gpui::Point<Pixels>> {
    let steps = ((SEGMENTS as f32 * sweep / TAU).ceil() as usize).max(2);
    (0..=steps).map(move |step| {
        let angle = -FRAC_PI_2 + sweep * step as f32 / steps as f32;
        point(
            center.x + px(radius * angle.cos()),
            center.y + px(radius * angle.sin()),
        )
    })
}

/// A ring `size` px square with a `stroke` px line: the whole circle in `track`, and
/// `fraction` (0 to 1) of it in `fill`.
pub fn ring(size: f32, stroke: f32, fraction: f32, track: Hsla, fill: Hsla) -> impl IntoElement {
    let fraction = fraction.clamp(0.0, 1.0);
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let center = bounds.center();
            let radius = size / 2.0 - stroke / 2.0;
            let mut paint = |sweep: f32, color: Hsla| {
                let mut builder = PathBuilder::stroke(px(stroke));
                for (index, at) in arc_points(center, radius, sweep).enumerate() {
                    if index == 0 {
                        builder.move_to(at);
                    } else {
                        builder.line_to(at);
                    }
                }
                if let Ok(path) = builder.build() {
                    window.paint_path(path, color);
                }
            };
            paint(TAU, track);
            if fraction > 0.0 {
                paint(TAU * fraction.max(MIN_FRACTION), fill);
            }
        },
    )
    .size(px(size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_arc_starts_at_the_top_and_runs_clockwise() {
        let center = point(px(10.0), px(10.0));
        let points: Vec<_> = arc_points(center, 8.0, TAU / 4.0).collect();
        let first = points[0];
        assert!((f32::from(first.x) - 10.0).abs() < 1e-3, "{first:?}");
        assert!(
            (f32::from(first.y) - 2.0).abs() < 1e-3,
            "the top: {first:?}"
        );
        // A quarter turn clockwise ends at three o'clock.
        let last = points[points.len() - 1];
        assert!((f32::from(last.x) - 18.0).abs() < 1e-3, "{last:?}");
        assert!((f32::from(last.y) - 10.0).abs() < 1e-3, "{last:?}");
    }
}
