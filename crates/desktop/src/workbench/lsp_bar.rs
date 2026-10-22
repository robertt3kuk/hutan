//! The language server's bar over a file tab (slice E-12a): quiet, one line, like the agent
//! and conflict bars, in a module of its own.
//!
//! - **Trust.** The file's server waits for the user to trust its folder (`untrusted` with
//!   `trust_pending`): "Language server for Rust runs build scripts and proc macros from this
//!   folder. Trust ~/src/app?" with **Trust** (`lsp.trust`, user-only; the daemon starts the
//!   server) and **Not now** (the bar closes in this window; nothing is decided).
//! - **Install.** The server is not installed: its install command, copyable, never run; the
//!   bar closes for good in this window once dismissed.

use std::path::Path;

use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Entity, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    div, px,
};

use super::lsp::{LspBar, LspStore};
use super::style::TextTooltip;
use super::ui::{self, ButtonKind};
use crate::theme::{Theme, Tokens};

/// What the bar says (tests read it too).
pub fn bar_text(bar: &LspBar, language: &str) -> String {
    match bar {
        LspBar::Trust { name, folder, .. } => {
            let does = if name == "rust-analyzer" {
                "runs build scripts and proc macros from this folder"
            } else {
                "runs code from this folder (its tools and workspace settings)"
            };
            format!(
                "Language server for {language} {does}. Trust {}?",
                display_folder(folder)
            )
        }
        LspBar::Install { name, command, .. } => {
            format!("{name} is not installed. Install it with: {command}")
        }
    }
}

/// `folder` with the home folder as `~`.
fn display_folder(folder: &Path) -> String {
    match std::env::var_os("HOME").map(std::path::PathBuf::from) {
        Some(home) if folder.starts_with(&home) && folder != home => {
            format!(
                "~/{}",
                folder.strip_prefix(&home).unwrap_or(folder).display()
            )
        }
        _ => folder.display().to_string(),
    }
}

/// The bar for `path` (a file whose language is `language`), if its server asks for something.
pub fn render(
    lsp: &Entity<LspStore>,
    path: &str,
    language: &str,
    theme: &Theme,
    tokens: &Tokens,
    cx: &App,
) -> Option<AnyElement> {
    let store = lsp.read(cx);
    let bar = store.bar(path)?;
    let message = bar_text(&bar, language);
    let row = div()
        .id("file-lsp")
        .flex_none()
        .flex()
        .items_center()
        .gap(px(tokens.spacing * 2.0))
        .px(px(tokens.spacing * 2.0))
        .py(px(tokens.spacing))
        .border_b_1()
        .border_color(theme.border)
        .bg(theme.sidebar)
        .text_size(px(tokens.secondary_font_size))
        .child(
            div()
                .flex_none()
                .text_color(theme.warning)
                .child(crate::icons::glyph(crate::icons::Icon::StatusAttention)),
        );
    let element = match bar {
        LspBar::Trust {
            server_id, folder, ..
        } => {
            let busy = store.is_trusting(&folder);
            let trust_store = lsp.clone();
            let dismiss_store = lsp.clone();
            let trust = ui::button(
                "file-lsp-trust",
                if busy { "Trusting…" } else { "Trust" },
                ButtonKind::Secondary,
                !busy,
                theme,
                tokens,
            )
            .tooltip(|_, cx| {
                cx.new(|_| {
                    TextTooltip(
                        "Let language servers run this project's build scripts, proc macros and the tools its settings name (`hutan lsp trust`)"
                            .into(),
                    )
                })
                .into()
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, _, cx| {
                let folder = folder.clone();
                trust_store.update(cx, |lsp, cx| lsp.trust(folder, cx));
            });
            let not_now = ui::button(
                "file-lsp-not-now",
                "Not now",
                ButtonKind::Ghost,
                true,
                theme,
                tokens,
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, _, cx| {
                dismiss_store.update(cx, |lsp, cx| lsp.dismiss(&server_id, cx));
            });
            row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.fg)
                    .child(SharedString::from(message)),
            )
            .child(trust)
            .child(not_now)
        }
        LspBar::Install {
            server_id,
            command,
            setting,
            ..
        } => {
            let dismiss_store = lsp.clone();
            let copy_store = lsp.clone();
            let copy_id = server_id.clone();
            let tip = match &setting {
                Some(setting) => format!(
                    "Hutan never installs anything. Elsewhere already? Set `{setting}` in daemon.toml"
                ),
                None => "Hutan never installs anything".to_owned(),
            };
            let copy = ui::button(
                "file-lsp-copy",
                "Copy Command",
                ButtonKind::Secondary,
                true,
                theme,
                tokens,
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(command.clone()));
                copy_store.update(cx, |lsp, cx| lsp.dismiss(&copy_id, cx));
            });
            let close = ui::button(
                "file-lsp-dismiss",
                "Dismiss",
                ButtonKind::Ghost,
                true,
                theme,
                tokens,
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, _, cx| {
                dismiss_store.update(cx, |lsp, cx| lsp.dismiss(&server_id, cx));
            });
            row.child(
                div()
                    .id("file-lsp-message")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.fg)
                    .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone().into())).into())
                    .child(SharedString::from(message)),
            )
            .child(copy)
            .child(close)
        }
    };
    Some(element.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_says_what_the_server_runs_and_where() {
        let trust = LspBar::Trust {
            server_id: "rust-analyzer:/w".into(),
            name: "rust-analyzer".into(),
            folder: "/w/app".into(),
        };
        assert_eq!(
            bar_text(&trust, "Rust"),
            "Language server for Rust runs build scripts and proc macros from this folder. Trust /w/app?"
        );
        let install = LspBar::Install {
            server_id: "gopls:/w".into(),
            name: "gopls".into(),
            command: "go install golang.org/x/tools/gopls@latest".into(),
            setting: None,
        };
        assert_eq!(
            bar_text(&install, "Go"),
            "gopls is not installed. Install it with: go install golang.org/x/tools/gopls@latest"
        );
    }
}
