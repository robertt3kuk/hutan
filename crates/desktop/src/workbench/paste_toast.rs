//! The toast of an image paste into a terminal (TERM-IMAGE-PASTE): where the image was saved,
//! with *Reveal*. The terminal view saves the file and types its path
//! (`hutan_terminal_view::TerminalViewEvent::ImageSaved`); this only tells the user.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, div,
    px,
};

use super::render::probe;
use super::ui::motion;
use super::ui::{Button, IconButton, StyledExt as _};
use super::view::{Region, Workbench};
use crate::theme::DensityExt as _;

/// How long the toast stays.
pub const PASTE_TOAST_TIME: Duration = Duration::from_secs(8);
/// The most of the path the toast shows: the end of it, which holds the file's name.
const SHOWN_CHARS: usize = 56;

/// The toast: one line, *Reveal*, a dismiss button.
#[derive(Debug, Clone)]
pub struct PasteToast {
    pub text: SharedString,
    /// The saved file.
    pub path: PathBuf,
    rev: u64,
}

/// `Image saved to …/scratch/<session>/paste-1.png`: the end of the path, from a `/`, when it is
/// long.
fn toast_text(path: &Path) -> String {
    let full = path.display().to_string();
    let count = full.chars().count();
    if count <= SHOWN_CHARS {
        return format!("Image saved to {full}");
    }
    let tail: String = full.chars().skip(count - SHOWN_CHARS).collect();
    // Start at a folder boundary rather than in the middle of a name.
    let tail = tail.find('/').map_or(tail.as_str(), |at| &tail[at..]);
    format!("Image saved to …{tail}")
}

impl Workbench {
    /// The toast of the last image pasted, while it shows (tests, inspection).
    pub fn paste_toast(&self) -> Option<&PasteToast> {
        self.paste_toast.as_ref()
    }

    /// An image was saved as `path` and its path typed into a terminal.
    pub(crate) fn show_paste_toast(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.toast_rev += 1;
        let rev = self.toast_rev;
        self.paste_toast = Some(PasteToast {
            text: toast_text(&path).into(),
            path,
            rev,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PASTE_TOAST_TIME).await;
            this.update(cx, |this, cx| {
                if this.paste_toast.as_ref().is_some_and(|t| t.rev == rev) {
                    this.paste_toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Takes the toast off.
    pub fn dismiss_paste_toast(&mut self, cx: &mut Context<Self>) {
        if self.paste_toast.take().is_some() {
            cx.notify();
        }
    }

    /// The toast's *Reveal*: the saved image in Finder.
    pub fn reveal_pasted_image(&mut self, cx: &mut Context<Self>) {
        let Some(toast) = self.paste_toast.take() else {
            return;
        };
        if toast.path.exists() {
            (self.daemon.reveal.clone())(&toast.path, cx);
        } else {
            self.set_notice(
                "The image is gone: it is removed when its session is closed",
                cx,
            );
        }
        cx.notify();
    }

    pub(crate) fn render_paste_toast(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let toast =
            self.paste_toast_exit
                .follow(self.paste_toast.as_ref(), motion::Kind::Toast, cx)?;
        let (theme, tokens) = (self.theme, self.density.ui());
        let (wanted, toast) = (toast.wanted, toast.value);
        Some(
            motion::present(
                "paste-toast-motion",
                motion::Kind::Toast,
                wanted,
                motion::Slide::Rise,
                div()
                    .id("paste-toast")
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 3.0))
                    .toast(&theme, &tokens)
                    .px(px(tokens.spacing * 3.0))
                    .py(px(tokens.spacing * 2.0))
                    .child(div().min_w_0().truncate().child(toast.text.clone()))
                    .child(
                        Button::new("paste-toast-reveal", "Reveal", &theme, &tokens)
                            .link()
                            .on_click(cx.listener(|this, _, _, cx| this.reveal_pasted_image(cx)))
                            .child(probe(&self.regions, Region::PasteToast("reveal"))),
                    )
                    .child(
                        IconButton::new(
                            "paste-toast-dismiss",
                            crate::icons::Icon::ActionClose,
                            "Dismiss",
                            &theme,
                            &tokens,
                        )
                        .compact()
                        .on_click(cx.listener(|this, _, _, cx| this.dismiss_paste_toast(cx)))
                        .child(probe(&self.regions, Region::PasteToast("dismiss"))),
                    )
                    .child(probe(&self.regions, Region::PasteToast("toast"))),
            )
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_path_shows_its_end() {
        assert_eq!(
            toast_text(Path::new("/tmp/s/paste-1.png")),
            "Image saved to /tmp/s/paste-1.png"
        );
        let long = format!(
            "/Users/me/Library/Application Support/Hutan/scratch/{}/paste-12.png",
            "0".repeat(26)
        );
        let text = toast_text(Path::new(&long));
        assert!(text.starts_with("Image saved to …"), "{text}");
        assert!(text.ends_with("/paste-12.png"), "{text}");
        assert!(
            text.chars().count() <= "Image saved to …".chars().count() + SHOWN_CHARS,
            "{text}"
        );
        assert!(
            text.starts_with("Image saved to …/"),
            "starts at a folder: {text}"
        );
    }
}
