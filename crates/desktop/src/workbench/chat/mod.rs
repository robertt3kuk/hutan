//! The agent chat view (M4.4): a chat tab over one agent session. `model` follows the daemon
//! (journal, then live events) and owns the composer; `view` draws it; `cards` draws the
//! transcript's cards; `markdown_view` draws message text; `chip` is what "Send to agent"
//! attaches; `input` is the composer's text editing. For assistant chats (M16-B) `context`
//! describes the structured attachments of a message and the `@` mentions that offer them, and
//! `attach` is the Attach menu and form that turn a choice into one. `rich` is the rich input of
//! every chat (protocol 0.23): dropped, picked and pasted files, images and long pastes.
//! `queue_view` draws the chat's queue above the composer (protocol 0.36).

mod attach;
mod cards;
mod chip;
mod context;
mod fork;
mod input;
mod markdown_view;
mod model;
mod question;
mod question_card;
mod queue_view;
mod retry;
mod review_turn;
mod rewind;
mod rich;
mod rich_view;
mod view;
pub mod vim;
pub(crate) mod widgets;

pub(crate) use cards::{RowFlow, counts_of, diff_row, diff_row_flow};
pub use chip::{Chip, cut_to};
pub use context::{
    ContextChip, DEFAULT_HITS, DEFAULT_LINES, DEFAULT_TURNS, Mention, MentionKind, Preview, Thumb,
    find_mention, is_gateway_chat, is_harness_chat, kind_label, parse_file_arg, split_context,
};
pub use fork::{EndedTurn, ended_turns};
pub use input::{TextInput, utf16_len, utf16_to_byte};
pub use markdown_view::{Doc, Look};
pub(crate) use markdown_view::{Ids, block_element, blocks};
pub use model::{ChatModel, LoadStatus};
pub use rewind::changed_text;
pub use rich::{LONG_PASTE_CHARS, LONG_PASTE_LINES, format_size, is_long_paste};
pub(crate) use view::model_label;
pub use view::{
    CatchUp, ChatView, FoldedChip, HeaderFit, KEY_CONTEXT, ModeChip, containment_chip, safety_chip,
    who_runs,
};
pub use vim::Vim;
pub use widgets::ButtonKind;
pub(crate) use widgets::{
    always_note, approval_group, approval_option, scope_tooltip, suggestion_box, suggestion_buttons,
};
