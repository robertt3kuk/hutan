//! The composer's rich input (protocol 0.23, capability `chat.rich_input`): files and images
//! dropped on the chat, picked with the `+` menu or pasted, long pastes folded into chips, and
//! how those chips are drawn (thumbnail or icon, name, size, a refusal, ✕).
//!
//! Nothing is read here but a file's first bytes and its size (`rich`); the daemon reads the
//! attachments when the message is sent and refuses what it cannot take. What the session is
//! known to refuse (an agent that takes no images) is shown on the chip before that.

use std::path::PathBuf;

use gpui::{
    AnyElement, App, ClipboardEntry, Context, ExternalPaths, InteractiveElement, IntoElement,
    ObjectFit, ParentElement, PathPromptOptions, SharedString, StatefulInteractiveElement, Styled,
    StyledImage as _, Window, div, img, prelude::*, px,
};
use hutan_protocol::assistant::AttachmentKind;

use super::context::{ContextChip, Thumb};
use super::markdown_view::Look;
use super::rich::{chip_for_image_data, chip_for_path, chip_for_text, is_long_paste};
use super::view::ChatView;
use crate::workbench::harnesses::harness_name;
use crate::workbench::render::probe;
use crate::workbench::ui::IconButton;
use crate::workbench::view::Region;

/// Side of an image chip's thumbnail.
const THUMB: f32 = 22.0;
/// Tallest the open text of a folded paste grows before it scrolls.
const PASTE_MAX_HEIGHT: f32 = 160.0;
/// Name of the group whose drag-over shows the drop overlay.
pub(super) const DROP_GROUP: &str = "chat-drop";

impl ChatView {
    /// Whether the daemon takes rich attachments (capability `chat.rich_input`).
    fn rich_input_available(&self, cx: &App) -> bool {
        self.sessions.read(cx).connection().supports_rich_input()
    }

    /// Why the session's agent cannot take images, when it is known (`ChatInfo.images`).
    fn image_refusal(&self, cx: &App) -> Option<String> {
        let chat = self.sessions.read(cx).session(self.session)?.chat.clone()?;
        (chat.images == Some(false))
            .then(|| format!("{} does not take images", harness_name(&chat.agent)))
    }

    /// Why `chip` cannot be sent: its own reason, or the session's.
    pub fn chip_refusal(&self, chip: &ContextChip, cx: &App) -> Option<String> {
        chip.refused.clone().or_else(|| {
            (chip.kind() == AttachmentKind::Image)
                .then(|| self.image_refusal(cx))
                .flatten()
        })
    }

    /// The reason of the first chip of the draft that cannot be sent, if any.
    pub fn refused_attachment(&self, cx: &App) -> Option<String> {
        self.model
            .read(cx)
            .context()
            .iter()
            .find_map(|chip| self.chip_refusal(chip, cx))
    }

    /// Attaches files the user dropped, picked or pasted: an image chip for a PNG, JPEG, GIF or
    /// WebP file, else a file chip. A folder, a binary file, an image that is too big or that
    /// the agent cannot read gets a chip that says so. Returns how many chips were added.
    pub fn attach_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) -> usize {
        if paths.is_empty() {
            return 0;
        }
        if !self.rich_input_available(cx) {
            self.model.update(cx, |model, cx| {
                model.set_error(
                    "This daemon cannot take attachments of files or images: restart hutand",
                    cx,
                );
            });
            return 0;
        }
        let mut added = 0;
        for path in paths {
            let chip = chip_for_path(path);
            if !self
                .model
                .update(cx, |model, cx| model.attach_context(chip, cx))
            {
                break;
            }
            added += 1;
        }
        self.attach = None;
        cx.notify();
        added
    }

    /// ⌘V: files copied in Finder and an image on the clipboard attach; a long text folds into a
    /// `Pasted · 312 lines` chip; any other text is typed. `false` when the clipboard is empty.
    pub fn paste(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else {
            return false;
        };
        if self.rich_input_available(cx) {
            let paths: Vec<PathBuf> = item
                .entries()
                .iter()
                .filter_map(|entry| match entry {
                    ClipboardEntry::ExternalPaths(paths) => Some(paths.paths().to_vec()),
                    _ => None,
                })
                .flatten()
                .collect();
            if !paths.is_empty() {
                self.attach_paths(&paths, cx);
                return true;
            }
            let image = item.entries().iter().find_map(|entry| match entry {
                ClipboardEntry::Image(image) => Some(image.clone()),
                _ => None,
            });
            if let Some(image) = image {
                let chip = chip_for_image_data(&image);
                self.model
                    .update(cx, |model, cx| model.attach_context(chip, cx));
                return true;
            }
        }
        let Some(text) = item.text() else {
            return false;
        };
        if self.rich_input_available(cx) && is_long_paste(&text) {
            let chip = chip_for_text(text);
            self.model
                .update(cx, |model, cx| model.attach_context(chip, cx));
        } else {
            self.model.update(cx, |model, cx| {
                model.edit(|input| input.insert(&text), cx);
            });
        }
        true
    }

    /// Backspace in the composer: at the start of an empty one it takes the last chip off.
    /// `true` when it did.
    pub(super) fn backspace_removes_chip(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.model.read(cx).input().is_empty() {
            return false;
        }
        self.model
            .update(cx, |model, cx| model.pop_last_attachment(cx))
    }

    /// "Attach file or image…": the system's open panel; what it returns is attached like a drop.
    pub fn pick_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            this.update(cx, |this, cx| {
                this.attach_paths(&paths, cx);
            })
            .ok();
        })
        .detach();
    }

    /// The thumbnail of an image chip, or the icon of the other kinds.
    fn chip_icon(&self, chip: &ContextChip, look: &Look) -> AnyElement {
        let theme = &look.theme;
        match &chip.thumb {
            Some(Thumb::Path(path)) => img(path.clone())
                .size(px(THUMB))
                .flex_none()
                .rounded(px(4.0))
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            Some(Thumb::Image(image)) => img(image.clone())
                .size(px(THUMB))
                .flex_none()
                .rounded(px(4.0))
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            None => {
                let glyph = crate::icons::Icon::ActionAddContext;
                div()
                    .flex_none()
                    .text_color(theme.muted)
                    .child(crate::icons::glyph(glyph))
                    .into_any_element()
            }
        }
    }

    /// The structured attachments of the draft: one chip each, with its thumbnail or icon, name
    /// and size, a refusal in red, a click that opens a folded paste, and ✕.
    pub(super) fn render_context_row(
        &self,
        chips: &[ContextChip],
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        div()
            .flex()
            .flex_wrap()
            .gap(px(tokens.spacing))
            .children(chips.iter().enumerate().map(|(index, chip)| {
                let refusal = self.chip_refusal(chip, cx);
                let refused = refusal.is_some();
                let color = if refused { theme.danger } else { theme.accent };
                let foldable = chip.kind() == AttachmentKind::PastedText;
                let open = chip.expanded;
                let rich = matches!(
                    chip.kind(),
                    AttachmentKind::Image | AttachmentKind::PastedText | AttachmentKind::File
                );
                div()
                    .id(SharedString::from(format!("chat-context-chip-{index}")))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing))
                    .pl(px(tokens.spacing * 2.0))
                    .pr(px(tokens.spacing))
                    .h(px(if rich {
                        THUMB + 8.0
                    } else {
                        tokens.sidebar_row - 4.0
                    }))
                    .rounded(px(crate::theme::pill_radius()))
                    .bg(if refused {
                        color.opacity(0.14)
                    } else {
                        theme.accent_soft
                    })
                    .font_family(look.mono.clone())
                    .text_size(px(tokens.secondary_font_size))
                    .when(foldable, |s| {
                        s.cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.model
                                    .update(cx, |model, cx| model.toggle_context_open(index, cx));
                            }))
                    })
                    .when(rich, |s| s.child(self.chip_icon(chip, look)))
                    .child(SharedString::from(chip.label()))
                    .when(foldable, |s| {
                        s.child(crate::icons::glyph(if open {
                            crate::icons::Icon::ActionExpand
                        } else {
                            crate::icons::Icon::ActionDisclose
                        }))
                    })
                    .when_some(refusal, |s, reason| {
                        s.child(
                            div()
                                .text_color(theme.danger)
                                .child(SharedString::from(format!("· refused: {reason}"))),
                        )
                    })
                    .child(
                        IconButton::new(
                            SharedString::from(format!("chat-context-remove-{index}")),
                            crate::icons::Icon::ActionClose,
                            "Remove",
                            theme,
                            tokens,
                        )
                        .compact()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.model
                                .update(cx, |model, cx| model.remove_context(index, cx));
                        })),
                    )
                    .child(probe(&self.regions, Region::ChatContextChip(index)))
            }))
            .into_any_element()
    }

    /// The text of the folded pastes the user opened, under the chips.
    pub(super) fn render_open_pastes(&self, chips: &[ContextChip], look: &Look) -> Vec<AnyElement> {
        let (theme, tokens) = (&look.theme, &look.tokens);
        chips
            .iter()
            .enumerate()
            .filter(|(_, chip)| chip.expanded)
            .filter_map(|(index, chip)| match &chip.attachment {
                hutan_protocol::assistant::Attachment::PastedText { text } => Some((index, text)),
                _ => None,
            })
            .map(|(index, text)| {
                div()
                    .id(SharedString::from(format!("chat-paste-{index}")))
                    .relative()
                    .max_h(px(PASTE_MAX_HEIGHT))
                    .overflow_y_scroll()
                    .p(px(tokens.spacing * 2.0))
                    .rounded(px(tokens.radius))
                    .bg(theme.bg)
                    .border_1()
                    .border_color(theme.border)
                    .font_family(look.mono.clone())
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(text.clone()))
                    .child(probe(&self.regions, Region::Chat("paste")))
                    .into_any_element()
            })
            .collect()
    }

    /// The highlight that covers the chat while files are dragged over it.
    pub(super) fn render_drop_overlay(&self, look: &Look) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let accent = theme.accent;
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .m(px(tokens.spacing * 2.0))
            .rounded(px(tokens.card_radius))
            .border_2()
            .bg(theme.accent_soft)
            .text_color(theme.accent)
            .opacity(0.0)
            // A drop target: the one place an accent edge is drawn.
            .group_drag_over::<ExternalPaths>(DROP_GROUP, move |style| {
                style.opacity(1.0).border_color(accent)
            })
            .child("Drop files or images to attach")
            .child(probe(&self.regions, Region::Chat("drop-overlay")))
            .into_any_element()
    }
}
