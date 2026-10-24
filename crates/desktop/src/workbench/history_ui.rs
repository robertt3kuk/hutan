//! What the file history tab and the Changes view's History segment share (GIT-HISTORY): a
//! commit's patch as the Changes view draws it (the chat's diff rows under a hunk header), and a
//! commit row (when, who, what).

use gpui::AppContext as _;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, InteractiveElement as _, IntoElement, ParentElement, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled, div, px,
};
use hutan_chat_view::diff::{self, FileDiff, MAX_LINES};
use hutan_protocol::git::{GitCommit, GitShowResult};

use super::chat::{Look, diff_row};
use super::style::TextTooltip;

/// One file's patch of a commit, as a view holds it.
#[derive(Debug, Clone)]
pub enum Patch {
    Loading,
    Ready {
        diff: FileDiff,
        truncated: bool,
        binary: bool,
    },
    Failed(String),
}

impl Patch {
    /// The patch of the first file of `shown` (`git.show` limited to one file answers one).
    pub fn of(shown: &GitShowResult) -> Self {
        let binary = shown.files.first().is_some_and(|f| f.binary)
            || shown.patch.lines().any(|l| l.starts_with("Binary files "));
        let diff = diff::split_patch(&shown.patch)
            .into_iter()
            .next()
            .unwrap_or_else(|| diff::split_patch("diff --git a/x b/x\n").remove(0));
        Self::Ready {
            diff,
            truncated: shown.truncated,
            binary,
        }
    }
}

/// The hunks of a file's diff: the line ranges starting at each `@@` header.
pub fn hunk_ranges(diff: &FileDiff) -> Vec<std::ops::Range<usize>> {
    super::changes::hunk_ranges(diff)
}

/// A patch as a block of diff rows under hunk headers, scrolling sideways inside itself; its
/// messages while it loads or fails.
pub fn patch_block(
    id: &'static str,
    patch: Option<&Patch>,
    look: &Look,
    scroll: &ScrollHandle,
) -> AnyElement {
    let (theme, tokens) = (&look.theme, &look.tokens);
    let message = |text: String, color| {
        div()
            .text_color(color)
            .child(SharedString::from(text))
            .into_any_element()
    };
    match patch {
        None | Some(Patch::Loading) => message("Reading the patch…".to_owned(), theme.muted),
        Some(Patch::Failed(error)) => {
            message(format!("Cannot read the patch: {error}"), theme.danger)
        }
        Some(Patch::Ready {
            diff,
            truncated,
            binary,
        }) => {
            if *binary {
                return message(
                    "A binary file: there is no text to show.".to_owned(),
                    theme.muted,
                );
            }
            let ranges = hunk_ranges(diff);
            if ranges.is_empty() {
                return message(
                    "No text changes (a mode change, or an empty file).".to_owned(),
                    theme.muted,
                );
            }
            let mut block = div()
                .id(id)
                .overflow_x_scroll()
                .track_scroll(scroll)
                .flex()
                .flex_col()
                .items_start()
                .rounded(px(tokens.radius))
                .border_1()
                .border_color(theme.border)
                .bg(theme.bg)
                .font_family(look.mono.clone())
                .text_size(px(tokens.font_size - 0.5))
                .whitespace_nowrap()
                .py(px(tokens.spacing));
            let mut drawn = 0usize;
            'hunks: for range in &ranges {
                for line in &diff.lines[range.clone()] {
                    if drawn >= MAX_LINES {
                        break 'hunks;
                    }
                    block = block.child(diff_row(line, look));
                    drawn += 1;
                }
            }
            div()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing))
                .child(block)
                .when(drawn >= MAX_LINES, |s| {
                    s.child(
                        div()
                            .text_color(theme.muted)
                            .child(SharedString::from(format!(
                                "… the first {MAX_LINES} lines are shown"
                            ))),
                    )
                })
                .when(*truncated, |s| {
                    s.child(
                        div()
                            .text_color(theme.warning)
                            .child("This patch is larger than 2 MiB and is cut."),
                    )
                })
                .into_any_element()
        }
    }
}

/// `3 days ago` for a commit's author time.
pub fn when(commit: &GitCommit) -> String {
    hutan_client::time::relative_to_now(commit.time.max(0) as u64 * 1000)
}

/// The tooltip of a commit's relative time: the absolute local time.
pub fn when_tooltip(
    commit: &GitCommit,
) -> impl Fn(&mut gpui::Window, &mut gpui::App) -> gpui::AnyView + 'static {
    let text = SharedString::from(hutan_client::time::local_time_text(
        commit.time.max(0) as u64 * 1000,
    ));
    move |_, cx| cx.new(|_| TextTooltip(text.clone())).into()
}

/// The first seven characters of a hash.
pub fn short(id: &str) -> &str {
    id.get(..7).unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_cut_at_seven_characters() {
        assert_eq!(short("0123456789abcdef"), "0123456");
        assert_eq!(short("abc"), "abc");
    }
}
