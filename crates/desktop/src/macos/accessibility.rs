//! macOS Reduce Motion (System Settings ▸ Accessibility ▸ Display ▸ Reduce motion), read live:
//! `NSWorkspace.accessibilityDisplayShouldReduceMotion` now, and
//! `NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification` when it (or another display
//! option) changes. The setting `appearance.reduce_motion` and the helpers that act on the
//! result live in `workbench::ui::motion`.

use tokio::sync::mpsc;

/// The `HUTAN_REDUCE_MOTION` variable (`0` / `1`) stands in for the system flag where set: for
/// tests, screenshots and machines without the setting.
fn env_override() -> Option<bool> {
    std::env::var_os("HUTAN_REDUCE_MOTION").map(|value| value == "1")
}

/// Whether the user asked macOS to reduce motion; `false` off macOS.
pub fn system_reduce_motion() -> bool {
    if let Some(value) = env_override() {
        return value;
    }
    system_flag()
}

#[cfg(target_os = "macos")]
fn system_flag() -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

#[cfg(not(target_os = "macos"))]
fn system_flag() -> bool {
    false
}

/// Keeps the observer of the display-options notification alive; dropping it removes it.
#[cfg(target_os = "macos")]
pub struct ReduceMotionObserver {
    center: objc2::rc::Retained<objc2_foundation::NSNotificationCenter>,
    token:
        objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_foundation::NSObjectProtocol>>,
}

#[cfg(target_os = "macos")]
impl std::fmt::Debug for ReduceMotionObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReduceMotionObserver")
            .finish_non_exhaustive()
    }
}

#[cfg(target_os = "macos")]
impl Drop for ReduceMotionObserver {
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
pub struct ReduceMotionObserver;

/// Sends `()` on `changed` whenever the system's display options change (Reduce Motion among
/// them; the receiver re-reads [`system_reduce_motion`]). The send never blocks: a full queue
/// already holds a pending re-read. `None` off macOS.
#[cfg(target_os = "macos")]
pub fn observe(changed: mpsc::Sender<()>) -> Option<ReduceMotionObserver> {
    use std::ptr::NonNull;

    use objc2_app_kit::{NSWorkspace, NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification};
    use objc2_foundation::NSNotification;

    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    let block = block2::RcBlock::new(move |_: NonNull<NSNotification>| {
        // Runs on the posting thread (the main thread for the system's notification).
        let _ = changed.try_send(());
    });
    // SAFETY: the block only calls `try_send` on a `Sender`, which is `Send` + `Sync`; the
    // notification name is a valid constant; no queue means the block runs where it is posted.
    #[allow(unsafe_code)]
    let token = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification),
            None,
            None,
            &block,
        )
    };
    Some(ReduceMotionObserver { center, token })
}

#[cfg(not(target_os = "macos"))]
pub fn observe(_changed: mpsc::Sender<()>) -> Option<ReduceMotionObserver> {
    None
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn a_display_options_notification_wakes_the_observer_until_it_is_dropped() {
        use objc2_app_kit::{
            NSWorkspace, NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification,
        };
        let (sender, mut receiver) = mpsc::channel(4);
        let observer = observe(sender.clone()).expect("macOS has the workspace");
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        // SAFETY: a constant notification name, no object, no user info.
        #[allow(unsafe_code)]
        let post = || unsafe {
            center.postNotificationName_object(
                NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification,
                None,
            );
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
    fn the_system_flag_is_readable() {
        // Any value is fine; the call must not crash off the main thread.
        let _ = system_flag();
    }
}
