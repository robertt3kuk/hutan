//! Hutan's code view (ADR 0012, slices E-5 and E-6): the file editor drawn by its own GPUI
//! element.
//!
//! # Pieces
//! - [`FileBuffer`]: the text of one open file (a `hutan-buffer` rope with its undo history),
//!   its load state, its conflict with the disk and, while a view of it is on screen, its
//!   `hutan-syntax` tree, which follows every edit incrementally. One buffer per file, shared by
//!   every view; the desktop's buffer store owns them and reads and writes files through the
//!   daemon (`fs.read`, `fs.write`).
//! - [`CodeView`]: one view of a buffer — scroll position, carets and selections (several at
//!   once), the input method's marked text, find matches — and its element, which shapes only
//!   the lines that intersect the view (cached by what they show, so an edit reshapes the lines
//!   it changed), colors them with the syntax tokens of the color pack (E-4, [`CodeStyle`]) and
//!   paints the gutter, the selections, the carets and the matches. Typing reaches it through
//!   the platform's text input (`EntityInputHandler`, UTF-16), so input methods compose in
//!   place.
//! - The editing commands ([`actions`]): carets and selections, multi-cursor (⌘D, ⌥-click,
//!   ⌥⇧-drag), typing with auto-pairs, auto-indent, line commands, comments, the clipboard and
//!   undo by steps; every command on every caret is one transaction.
//! - [`FindQuery`] / [`FindState`]: find in file, searched in the background over a snapshot of
//!   the rope, and replace (one match, or all as one undo step).
//!
//! GPUI is linked here (a UI crate, ADR 0010); the text model and the parser are GPUI-free.

pub mod actions;
mod buffer;
mod editing;
mod element;
mod find;
mod input;
mod layout;
mod motion;
mod rules;
mod style;
mod view;

pub use actions::*;
pub use buffer::{
    BufferEdited, Coalesce, Conflict, EditKind, FailReason, FileBuffer, LoadState,
    TYPING_GROUP_PAUSE, file_name, language_label,
};
pub use find::{FindQuery, FindResult, FindState, MAX_FIND_MATCHES};
pub use layout::TAB_SIZE;
pub use rules::IndentUnit;
pub use style::{CodeStyle, rgb, rgba};
pub use view::{
    CodeView, CodeViewEvent, Layout, MAX_COLUMN_LINES, OVERSCAN_LINES, PaintTimes, ViewStats,
};

/// The key context of a focused code view.
pub const KEY_CONTEXT: &str = "CodeView";
