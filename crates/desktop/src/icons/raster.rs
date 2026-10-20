//! The GPUI SVG atlas has no public SVG eviction API. Render validated artwork into our own
//! byte-bounded LRU of RenderImages instead, so eviction also calls GPUI's public drop_image.
use gpui::{
    App, Bounds, DevicePixels, Hsla, Pixels, RenderImage, Rgba, SvgRenderer, SvgSize, Window, size,
};
use hutan_settings::themes::icons::IconAsset;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::Arc;

const LIMIT: usize = 64 * 1024 * 1024;
struct Entry {
    key: Arc<str>,
    side: u32,
    color: u32,
    bytes: usize,
    image: Arc<RenderImage>,
}
struct Cache {
    renderer: SvgRenderer,
    entries: VecDeque<Entry>,
    bytes: usize,
}
thread_local! { static CACHE: RefCell<Cache> = RefCell::new(Cache {renderer:SvgRenderer::new(Arc::new(super::Assets)),entries:VecDeque::new(),bytes:0}); }

pub(super) fn paint(
    asset: &IconAsset,
    bounds: Bounds<Pixels>,
    color: Hsla,
    window: &mut Window,
    cx: &mut App,
) {
    let side = (f32::from(bounds.size.width).min(f32::from(bounds.size.height))
        * window.scale_factor()
        * 2.0)
        .ceil()
        .clamp(1.0, 512.0) as u32;
    let rgba = Rgba::from(color);
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    let color = (channel(rgba.r) << 24)
        | (channel(rgba.g) << 16)
        | (channel(rgba.b) << 8)
        | channel(rgba.a);
    let image = CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let position = cache
            .entries
            .iter()
            .position(|e| e.key == asset.key && e.side == side && e.color == color);
        if let Some(position) = position {
            let entry = cache.entries.remove(position)?;
            let image = Arc::clone(&entry.image);
            cache.entries.push_back(entry);
            return Some(image);
        }
        // This substitution operates only on the sanitizer's currentColor token. No XML/CSS
        // supplied by a pack is interpreted here and no file/network lookup is possible.
        let text = std::str::from_utf8(&asset.bytes)
            .ok()?
            .replace("currentColor", &format!("#{color:08x}"));
        let parsed = match cache.renderer.parse_svg(text.as_bytes()) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("cannot decode validated icon: {e}");
                return None;
            }
        };
        let image = match cache.renderer.render_parsed(
            &parsed,
            SvgSize::ExactSize(size(DevicePixels(side as i32), DevicePixels(side as i32))),
        ) {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!("cannot rasterize validated icon: {e}");
                return None;
            }
        };
        let bytes = side as usize * side as usize * 4;
        // Entries in the current frame have an extra reference retained until the next frame.
        // Evict only unreferenced tiles; transient frame references are outside the LRU budget.
        while cache.bytes + bytes > LIMIT {
            let Some(position) = cache
                .entries
                .iter()
                .position(|e| Arc::strong_count(&e.image) == 1)
            else {
                break;
            };
            if let Some(entry) = cache.entries.remove(position) {
                cache.bytes -= entry.bytes;
                cx.drop_image(entry.image, Some(window));
            }
        }
        cache.bytes += bytes;
        cache.entries.push_back(Entry {
            key: Arc::clone(&asset.key),
            side,
            color,
            bytes,
            image: Arc::clone(&image),
        });
        Some(image)
    });
    if let Some(image) = image {
        if let Err(e) = window.paint_image(
            bounds,
            bounds,
            Default::default(),
            Arc::clone(&image),
            0,
            false,
        ) {
            tracing::warn!("cannot paint icon: {e}");
        }
        window.on_next_frame(move |_, _| drop(image));
    }
}

/// Whether this thread's renderer has decoded and rasterized `key` (renderer fixtures use it to
/// prove a themed control painted the user's validated SVG, not just resolved its id).
pub fn rasterized(key: &str) -> bool {
    CACHE.with(|cache| {
        cache
            .borrow()
            .entries
            .iter()
            .any(|entry| entry.key.as_ref() == key)
    })
}
