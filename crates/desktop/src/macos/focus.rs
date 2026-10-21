//! The macOS keyboard focus indicator color: the ring `ui.focus` draws when a pack leaves the
//! token out (D-C3, `docs/themes.md`). Read once when the app starts, never in tests: a test
//! window draws the ring in `ui.accent` instead (`crate::theme::set_system_focus`).

use gpui::Hsla;

/// `NSColor.keyboardFocusIndicatorColor` resolved for the light (`aqua`) and dark (`darkAqua`)
/// appearance, made opaque: AppKit's own value is the user's accent color at half alpha, and the
/// ring is a 2 px stroke that has to stand on its own (≥ 3:1). `None` off macOS or when AppKit
/// has no sRGB value for it.
#[cfg(target_os = "macos")]
pub fn system_colors() -> Option<(Hsla, Hsla)> {
    use std::cell::Cell;

    use objc2_app_kit::{NSAppearance, NSColor, NSColorSpace};
    use objc2_foundation::NSString;

    let read = |appearance: &str| -> Option<Hsla> {
        // The appearance constants are these same strings; naming them avoids the extern statics.
        let appearance = NSAppearance::appearanceNamed(&NSString::from_str(appearance))?;
        let rgb = Cell::new(None);
        let block = block2::RcBlock::new(|| {
            let color = NSColor::keyboardFocusIndicatorColor()
                .colorUsingColorSpace(&NSColorSpace::sRGBColorSpace());
            rgb.set(color.map(|c| (c.redComponent(), c.greenComponent(), c.blueComponent())));
        });
        appearance.performAsCurrentDrawingAppearance(&block);
        let (r, g, b) = rgb.get()?;
        #[allow(clippy::cast_possible_truncation)] // sRGB components in 0…1
        Some(Hsla::from(gpui::Rgba {
            r: r as f32,
            g: g as f32,
            b: b as f32,
            a: 1.0,
        }))
    };
    Some((
        read("NSAppearanceNameAqua")?,
        read("NSAppearanceNameDarkAqua")?,
    ))
}

/// No system focus color off macOS.
#[cfg(not(target_os = "macos"))]
pub fn system_colors() -> Option<(Hsla, Hsla)> {
    None
}
