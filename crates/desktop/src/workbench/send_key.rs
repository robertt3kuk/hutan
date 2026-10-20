//! The send key (`chat.send_key`): which key sends a message from every composer, the control
//! the composers show for it, the quick-switch menu behind that control, and "Chat: Toggle Send
//! Key". The rule itself is [`SendKey::return_sends`]; each composer applies it to its own Return.
//!
//! Message composers (they follow the setting): the chat composer (harness chats, direct chats,
//! Mawas Home), the Start sheet's prompt, the review comment box and "send review" in the Changes
//! tray, and free-text answers in question cards. Forms that are not messages keep their keys:
//! the merge and delivery commit messages (⌘↩ confirms, Return is a newline), the memory editor
//! (⌘↩ saves), and every settings field.

use gpui::{
    Context, Div, InteractiveElement, ParentElement, Pixels, Point, SharedString, Stateful, Styled,
    Window, div, px,
};
use hutan_settings::{SettingsError, SettingsStore};

use super::view::Workbench;
use crate::settings::{ChatSettings, SendKey};
use crate::theme::{Theme, Tokens};

/// Saves `key` as the user's send key, and counts it as the answer to the first-run question.
pub(crate) fn write_send_key(settings: &SettingsStore, key: SendKey) -> Result<(), SettingsError> {
    settings.write_user("chat.send_key", toml::Value::String(key.key().to_owned()))?;
    stop_asking(settings)
}

/// The first-run question has been answered or dismissed: it is not asked again.
pub(crate) fn stop_asking(settings: &SettingsStore) -> Result<(), SettingsError> {
    if settings.get::<ChatSettings>().ask_send_key {
        settings.write_user("chat.ask_send_key", toml::Value::Boolean(false))?;
    }
    Ok(())
}

/// The small control a composer shows in place of a plain hint: the keys as text and a caret.
/// The caller adds the click that opens the menu and its region probe.
pub(crate) fn send_key_control(
    id: &'static str,
    text: impl Into<SharedString>,
    theme: &Theme,
    tokens: &Tokens,
) -> Stateful<Div> {
    let (hover, fg) = (theme.surface_hover, theme.fg);
    div()
        .id(id)
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(tokens.spacing))
        .px(px(tokens.spacing))
        .rounded(px(tokens.radius - 2.0))
        .cursor_pointer()
        .text_color(theme.muted)
        .text_size(px(tokens.secondary_font_size))
        .hover(move |s| s.bg(hover).text_color(fg))
        .child(text.into())
        .child(crate::icons::glyph(crate::icons::Icon::ActionExpand))
}

impl Workbench {
    /// The send key now in force.
    pub fn send_key(&self) -> SendKey {
        self.settings.get::<ChatSettings>().send_key
    }

    /// Switches the send key (the user's settings file); every composer follows at once.
    pub(crate) fn set_send_key(&mut self, key: SendKey, cx: &mut Context<Self>) {
        if let Err(error) = write_send_key(&self.settings, key) {
            tracing::warn!("cannot save chat.send_key: {error}");
            self.set_notice(format!("Cannot save chat.send_key: {error}"), cx);
        }
        cx.notify();
    }

    /// ⌘K "Chat: Toggle Send Key": flips the key and says what sends now.
    pub(crate) fn toggle_send_key(&mut self, cx: &mut Context<Self>) {
        let key = self.send_key().other();
        match write_send_key(&self.settings, key) {
            Ok(()) => self.set_notice(key.toast(), cx),
            Err(error) => {
                tracing::warn!("cannot save chat.send_key: {error}");
                self.set_notice(format!("Cannot save chat.send_key: {error}"), cx);
            }
        }
        cx.notify();
    }

    /// The menu behind a composer's send-key control: the two choices, the current one checked.
    pub fn show_send_key_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.send_key();
        let entries = SendKey::ALL
            .into_iter()
            .map(|key| {
                let mut entry = Self::entry(key.label(), move |this, _, cx| {
                    this.set_send_key(key, cx);
                });
                entry.checked = key == current;
                entry
            })
            .collect();
        self.open_menu(position, entries, window, cx);
    }
}
