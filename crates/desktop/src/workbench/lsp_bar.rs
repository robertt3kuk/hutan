//! The language server's bar over a file tab (slice E-12a): quiet, one line, like the agent
//! and conflict bars, in a module of its own.
//!
//! - **Trust.** The file's server is installed and waits for the user to trust its folder
//!   (`untrusted` with `trust_pending`; the daemon says `not_installed` instead while there is
//!   no binary to run). Trust is per folder and lets every server below it start, so the bar
//!   names every server known to wait there and says what each may run: "rust-analyzer starts
//!   in ~/src/app for Rust and can run build scripts and procedural macros there. Trust this
//!   folder?" or "taplo starts in ~/src/app for TOML and only reads the files. …", with
//!   **Trust** (`lsp.trust`, user-only; the daemon starts them) and **Not now** (nothing is
//!   decided; no bar asks about the folder again in this window, whatever the language of the
//!   next file; the status bar's `untrusted` brings it back). One bar per window for a folder:
//!   the first pane that needs it shows it (`Workbench::sync_lsp_bars`).
//! - **Install.** The server is not installed: its install command, copyable, never run; the
//!   bar closes for good in this window once dismissed.

use std::path::Path;

use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::lsp::{LspBar, LspStore, TrustServer};
use super::regions::Regions;
use super::render::probe;
use super::style::TextTooltip;
use super::ui;
use super::view::Region;
use crate::theme::{Theme, Tokens};

/// What the bar says (tests read it too).
pub fn bar_text(bar: &LspBar) -> String {
    match bar {
        LspBar::Trust { folder, servers } => trust_text(servers, &display_folder(folder)),
        LspBar::Install { name, command, .. } => {
            format!("{name} is not installed. Install it with: {command}")
        }
    }
}

/// What a server may run from the folder it is started in, by its name in `daemon.toml`; `None`
/// for a server of the user's own, which can be anything.
fn runs(name: &str) -> Option<Runs> {
    Some(match name {
        "rust-analyzer" => Runs::Code("build scripts and procedural macros"),
        "typescript-language-server" => Runs::Code("the project's TypeScript plugins"),
        "pyright" | "basedpyright" => Runs::Code("the project's Python interpreter"),
        "gopls" => Runs::Code("the go command on the project"),
        "bash-language-server" => Runs::Code("shellcheck on the scripts"),
        "clangd" | "taplo" | "marksman" | "vscode-json-languageserver" => Runs::Reads,
        _ => return None,
    })
}

#[derive(Clone, Copy)]
enum Runs {
    /// Starts programs of the project or its tools.
    Code(&'static str),
    /// Only reads the files.
    Reads,
}

/// The trust bar's line: which servers start where, and what each may run.
fn trust_text(servers: &[TrustServer], folder: &str) -> String {
    let languages = |s: &TrustServer| -> String { super::lsp::spoken_list(&s.languages) };
    if let [one] = servers {
        let does = match runs(&one.name) {
            Some(Runs::Code(what)) => format!("can run {what} there"),
            Some(Runs::Reads) => "only reads the files".to_owned(),
            None => "runs the program you set for it in daemon.toml".to_owned(),
        };
        return format!(
            "{} starts in {folder} for {} and {does}. Trust this folder?",
            one.name,
            languages(one)
        );
    }
    let names = super::lsp::spoken_list(
        &servers
            .iter()
            .map(|s| format!("{} ({})", s.name, languages(s)))
            .collect::<Vec<_>>(),
    );
    let mut may: Vec<String> = Vec::new();
    for server in servers {
        match runs(&server.name) {
            Some(Runs::Code(what)) => may.push(format!("{} can run {what}.", server.name)),
            Some(Runs::Reads) => {}
            None => may.push(format!("{} runs a program you configured.", server.name)),
        }
    }
    let tail = if may.is_empty() {
        "They only read the files.".to_owned()
    } else {
        may.join(" ")
    };
    format!("{names} start in {folder}. {tail} Trust this folder?")
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
        let server = |name: &str, languages: &[&str]| TrustServer {
            name: name.into(),
            languages: languages.iter().map(|l| (*l).to_owned()).collect(),
        };
        let one = LspBar::Trust {
            folder: "/w/app".into(),
            servers: vec![server("rust-analyzer", &["Rust"])],
        };
        assert_eq!(
            bar_text(&one),
            "rust-analyzer starts in /w/app for Rust and can run build scripts and procedural macros there. Trust this folder?"
        );
        // A server that only reads files says so; it is not a build-script warning (F-10).
        let plain = LspBar::Trust {
            folder: "/w/app".into(),
            servers: vec![server("taplo", &["TOML"])],
        };
        assert_eq!(
            bar_text(&plain),
            "taplo starts in /w/app for TOML and only reads the files. Trust this folder?"
        );
        let two = LspBar::Trust {
            folder: "/w/app".into(),
            servers: vec![
                server("rust-analyzer", &["Rust"]),
                server("taplo", &["TOML"]),
            ],
        };
        assert_eq!(
            bar_text(&two),
            "rust-analyzer (Rust) and taplo (TOML) start in /w/app. rust-analyzer can run build scripts and procedural macros. Trust this folder?"
        );
        let reading = LspBar::Trust {
            folder: "/w/app".into(),
            servers: vec![
                server("taplo", &["TOML"]),
                server("marksman", &["Markdown"]),
            ],
        };
        assert_eq!(
            bar_text(&reading),
            "taplo (TOML) and marksman (Markdown) start in /w/app. They only read the files. Trust this folder?"
        );
        // A server of your own can be anything.
        let own = LspBar::Trust {
            folder: "/w/app".into(),
            servers: vec![server("fake", &["Plain Text"])],
        };
        assert_eq!(
            bar_text(&own),
            "fake starts in /w/app for Plain Text and runs the program you set for it in daemon.toml. Trust this folder?"
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
