//! Vim's keys before anything else of the window sees them (`editor.vim_mode`): `⌃[` is
//! Escape wherever Vim is on, so every Escape of the application (leaving Insert, closing a
//! dialog in Normal mode) answers it too. With Vim off nothing changes.

use gpui::{App, Keystroke, Subscription, Window};
use hutan_code_view::{VimKey, vim_key_of_keystroke};
use hutan_settings::SettingsStore;

use super::vim_ui::VimConfig;

/// `⌃[` (on any layout: a Russian one names the key `х`), not Escape itself.
fn is_ctrl_bracket(keystroke: &Keystroke) -> bool {
    keystroke.modifiers.control
        && keystroke.key != "escape"
        && vim_key_of_keystroke(keystroke, |_| true) == Some(VimKey::Escape)
}

/// Installs the window's interceptor.
pub(crate) fn install(settings: SettingsStore, window: &Window, cx: &mut App) -> Subscription {
    let window_handle = window.window_handle();
    cx.intercept_keystrokes(move |event, window, cx| {
        if window.window_handle() != window_handle || !VimConfig::of(&settings).enabled {
            return;
        }
        // A terminal reads `⌃[` itself (it sends ESC, as Escape does).
        let terminal = event
            .context_stack
            .iter()
            .any(|context| context.contains(hutan_terminal_view::KEY_CONTEXT));
        if terminal || !is_ctrl_bracket(&event.keystroke) {
            return;
        }
        cx.stop_propagation();
        let escape = Keystroke {
            modifiers: gpui::Modifiers::none(),
            key: "escape".into(),
            key_char: None,
        };
        window.defer(cx, move |window, cx| {
            window.dispatch_keystroke(escape, cx);
        });
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keystroke(text: &str) -> Keystroke {
        Keystroke::parse(text).expect("a keystroke")
    }

    #[test]
    fn ctrl_bracket_is_escape_on_a_latin_and_a_russian_layout() {
        assert!(is_ctrl_bracket(&keystroke("ctrl-[")));
        assert!(is_ctrl_bracket(&Keystroke {
            modifiers: gpui::Modifiers::control(),
            key: "х".into(),
            key_char: None,
        }));
        assert!(!is_ctrl_bracket(&keystroke("escape")));
        assert!(!is_ctrl_bracket(&keystroke("[")));
        assert!(!is_ctrl_bracket(&keystroke("cmd-[")));
        assert!(!is_ctrl_bracket(&keystroke("ctrl-]")));
    }
}
