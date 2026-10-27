//! Whether macOS keeps its scroll bars on screen (System Settings ▸ Appearance ▸ Show scroll
//! bars): `NSScroller.preferredScrollerStyle` is the legacy style (bars that stay, on a track)
//! for "Always" and the overlay style (bars that appear while scrolling) for "When scrolling" and
//! "Automatically based on mouse or trackpad", which also follows whether a mouse or a trackpad
//! is in use. It is read at startup and again on `NSPreferredScrollerStyleDidChangeNotification`;
//! `appearance.scrollbars = system` follows it live, `always` and `scrolling` override it
//! (`hutan_ui::scrollbar`).

use tokio::sync::mpsc;

/// What a scroller style means: `true` for the legacy style (bars that stay), `false` for the
/// overlay style. `NSScrollerStyleLegacy` is 0 and `NSScrollerStyleOverlay` is 1; anything
/// unknown counts as an overlay, the macOS default.
pub fn style_keeps_bars(style: isize) -> bool {
    style == 0
}

/// Whether the system keeps its scroll bars on screen; `false` off macOS and off the main thread.
pub fn system_keeps_bars() -> bool {
    system_style().is_some_and(style_keeps_bars)
}

#[cfg(target_os = "macos")]
fn system_style() -> Option<isize> {
    let mtm = objc2::MainThreadMarker::new()?;
    Some(objc2_app_kit::NSScroller::preferredScrollerStyle(mtm).0)
}

#[cfg(not(target_os = "macos"))]
fn system_style() -> Option<isize> {
    None
}

/// Keeps the observer of the scroller-style notification alive; dropping it removes it.
#[cfg(target_os = "macos")]
pub struct ScrollerObserver {
    center: objc2::rc::Retained<objc2_foundation::NSNotificationCenter>,
    token:
        objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_foundation::NSObjectProtocol>>,
}

#[cfg(target_os = "macos")]
impl std::fmt::Debug for ScrollerObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScrollerObserver").finish_non_exhaustive()
    }
}

#[cfg(target_os = "macos")]
impl Drop for ScrollerObserver {
    fn drop(&mut self) {
        // SAFETY: `token` is the object `addObserverForName…` returned for this center.
        #[allow(unsafe_code)]
        unsafe {
            self.center.removeObserver((*self.token).as_ref());
        }
    }
}

/// Nothing to keep off macOS.
#[cfg(not(target_os = "macos"))]
#[derive(Debug)]
pub struct ScrollerObserver;

/// Sends `()` on `changed` whenever the preferred scroller style changes (the receiver re-reads
/// [`system_keeps_bars`]). The send never blocks: a full queue already holds a pending re-read.
/// `None` off macOS.
#[cfg(target_os = "macos")]
pub fn observe(changed: mpsc::Sender<()>) -> Option<ScrollerObserver> {
    use std::ptr::NonNull;

    use objc2_app_kit::NSPreferredScrollerStyleDidChangeNotification;
    use objc2_foundation::{NSNotification, NSNotificationCenter};

    let center = NSNotificationCenter::defaultCenter();
    let block = block2::RcBlock::new(move |_: NonNull<NSNotification>| {
        // Runs on the posting thread (the main thread for the system's notification).
        let _ = changed.try_send(());
    });
    // SAFETY: the block only calls `try_send` on a `Sender`, which is `Send` + `Sync`; the
    // notification name is a valid constant; no queue means the block runs where it is posted.
    #[allow(unsafe_code)]
    let token = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSPreferredScrollerStyleDidChangeNotification),
            None,
            None,
            &block,
        )
    };
    Some(ScrollerObserver { center, token })
}

#[cfg(not(target_os = "macos"))]
pub fn observe(_changed: mpsc::Sender<()>) -> Option<ScrollerObserver> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_legacy_style_keeps_the_bars_on_screen() {
        assert!(style_keeps_bars(0), "NSScrollerStyleLegacy: Always");
        assert!(
            !style_keeps_bars(1),
            "NSScrollerStyleOverlay: When scrolling / automatic"
        );
        assert!(!style_keeps_bars(7), "an unknown style is an overlay");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_style_notification_wakes_the_observer_until_it_is_dropped() {
        use objc2_app_kit::NSPreferredScrollerStyleDidChangeNotification;
        use objc2_foundation::NSNotificationCenter;
        let (sender, mut receiver) = mpsc::channel(4);
        let observer = observe(sender.clone()).expect("macOS has the notification center");
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: a constant notification name, no object, no user info.
        #[allow(unsafe_code)]
        let post = || unsafe {
            center.postNotificationName_object(NSPreferredScrollerStyleDidChangeNotification, None);
        };
        post();
        assert_eq!(receiver.try_recv(), Ok(()));
        drop(observer);
        post();
        assert_eq!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty),
            "a dropped observer stays silent"
        );
    }

    #[test]
    fn the_style_is_readable_from_any_thread() {
        // Off the main thread there is no marker and the answer is the overlay default; on it,
        // whatever this Mac says. Either way the call must not crash.
        let _ = system_keeps_bars();
    }
}
