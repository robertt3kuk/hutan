//! The language server's bar over a file tab (slice E-12a): quiet, one line, like the agent
//! and conflict bars, in a module of its own.
//!
//! - **Trust.** The file's server waits for the user to trust its folder (`untrusted` with
//!   `trust_pending`). Trust is per folder and lets every server below it start, so the bar
//!   names every server known to wait there: "Language servers for Rust and Plain Text run code
//!   from this folder (build scripts, tools). Trust ~/src/app?" with **Trust** (`lsp.trust`,
//!   user-only; the daemon starts them) and **Not now** (nothing is decided; no bar asks about
//!   the folder again in this window, whatever the language of the next file).
//! - **Install.** The server is not installed: its install command, copyable, never run; the
//!   bar closes for good in this window once dismissed.

use std::path::Path;

use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::lsp::{LspBar, LspStore};
use super::regions::Regions;
use super::render::probe;
use super::style::TextTooltip;
use super::ui;
use super::view::Region;
use crate::theme::{Theme, Tokens};

/// What the bar says (tests read it too).
pub fn bar_text(bar: &LspBar) -> String {
    match bar {
        LspBar::Trust {
            folder,
            languages,
            servers,
        } => {
            let (who, run) = if *servers > 1 {
                ("Language servers", "run")
            } else {
                ("Language server", "runs")
            };
            format!(
                "{who} for {} {run} code from this folder (build scripts, tools). Trust {}?",
                super::lsp::spoken_list(languages),
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

/// The bar for `path`, if its servers ask for something.
pub fn render(
    lsp: &Entity<LspStore>,
    path: &str,
    regions: &Regions,
    theme: &Theme,
    tokens: &Tokens,
    cx: &App,
) -> Option<AnyElement> {
    let store = lsp.read(cx);
    let bar = store.bar(path)?;
    let message = bar_text(&bar);
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
        LspBar::Trust { folder, .. } => {
            let busy = store.is_trusting(&folder);
            let trust_store = lsp.clone();
            let dismiss_store = lsp.clone();
            let not_now_folder = folder.clone();
            let trust = ui::Button::new("file-lsp-trust", "Trust", theme, tokens)
                .compact()
                .primary()
                .loading(busy)
                .tooltip(
                    "Let language servers run this project's build scripts, proc macros and the tools its settings name (`hutan lsp trust`)",
                )
                .on_click(move |_, _, cx| {
                    let folder = folder.clone();
                    trust_store.update(cx, |lsp, cx| lsp.trust(folder, cx));
                })
                .child(probe(regions, Region::FileLsp("trust")));
            let not_now = ui::Button::new("file-lsp-not-now", "Not now", theme, tokens)
                .compact()
                .ghost()
                .on_click(move |_, _, cx| {
                    let folder = not_now_folder.clone();
                    dismiss_store.update(cx, |lsp, cx| lsp.not_now(folder, cx));
                })
                .child(probe(regions, Region::FileLsp("not-now")));
            row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.fg)
                    .child(SharedString::from(message)),
            )
            .child(not_now)
            .child(trust)
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
            let copy = ui::Button::new("file-lsp-copy", "Copy command", theme, tokens)
                .compact()
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(command.clone()));
                    copy_store.update(cx, |lsp, cx| lsp.dismiss(&copy_id, cx));
                })
                .child(probe(regions, Region::FileLsp("copy")));
            let close = ui::Button::new("file-lsp-dismiss", "Dismiss", theme, tokens)
                .compact()
                .ghost()
                .on_click(move |_, _, cx| {
                    dismiss_store.update(cx, |lsp, cx| lsp.dismiss(&server_id, cx));
                })
                .child(probe(regions, Region::FileLsp("dismiss")));
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
            .child(close)
            .child(copy)
        }
    };
    Some(element.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_names_every_server_of_the_folder_and_what_it_runs() {
        let one = LspBar::Trust {
            folder: "/w/app".into(),
            languages: vec!["Rust".into()],
            servers: 1,
        };
        assert_eq!(
            bar_text(&one),
            "Language server for Rust runs code from this folder (build scripts, tools). Trust /w/app?"
        );
        let two = LspBar::Trust {
            folder: "/w/app".into(),
            languages: vec!["Rust".into(), "Plain Text".into()],
            servers: 2,
        };
        assert_eq!(
            bar_text(&two),
            "Language servers for Rust and Plain Text run code from this folder (build scripts, tools). Trust /w/app?"
        );
        let install = LspBar::Install {
            server_id: "gopls:/w".into(),
            name: "gopls".into(),
            command: "go install golang.org/x/tools/gopls@latest".into(),
            setting: None,
        };
        assert_eq!(
            bar_text(&install),
            "gopls is not installed. Install it with: go install golang.org/x/tools/gopls@latest"
        );
    }

    #[test]
    fn lists_read_aloud() {
        use crate::workbench::lsp::spoken_list;
        assert_eq!(spoken_list::<&str>(&[]), "");
        assert_eq!(spoken_list(&["Rust"]), "Rust");
        assert_eq!(spoken_list(&["Rust", "Go"]), "Rust and Go");
        assert_eq!(spoken_list(&["C", "C++", "Go"]), "C, C++ and Go");
    }
}
