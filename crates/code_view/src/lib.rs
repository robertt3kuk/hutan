//! Hutan's code view (ADR 0012, slices E-5, E-6 and E-8): the file editor drawn by its own GPUI
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
//! - Vim (E-8, `modal.rs`): `hutan-vim` over the rope ([`BufferHost`]), one state per view,
//!   modes with a block or bar caret, the Escape rule for input methods, and the text half of
//!   the `:` line ([`ex`]: `:s :g :normal :set :N`); the owner draws the line and runs the
//!   commands about files and windows.
//! - Agent changes (E-10, [`agent`]): the hunks an agent's turn left in the file (its base
//!   rebased over the user's own edits, then diffed against the text), drawn as a tint and a
//!   gutter bar, rejected one at a time as an undoable edit, recomputed in the background after
//!   edits (less often for large or minified files).
//! - Language servers (E-12a): diagnostics on the buffer ([`Diagnostic`], anchored to the text
//!   until the next publish) drawn with an underline style and a gutter glyph shape per
//!   severity and the caret line's message after its end; the hover popover ([`HoverState`]);
//!   `gd` / ⌘-click / F12, `]d` `[d` / F8, `K`, and `⌃O` back across files. The owner talks to
//!   the daemon ([`CodeViewEvent::HoverRequested`], [`CodeViewEvent::GoToDefinition`]).
//! - Completion and signature help (E-12b, `completion.rs`): the popup with kind glyphs,
//!   client-side filtering and the selected item's documentation, the snippet subset with tab
//!   stops, and signature help above the caret ([`CodeViewEvent::CompletionRequested`],
//!   [`CodeViewEvent::SignatureHelpRequested`]).
//!
//! GPUI is linked here (a UI crate, ADR 0010); the text model and the parser are GPUI-free.

pub mod actions;
pub mod agent;
pub mod blame;
mod buffer;
mod completion;
pub mod diagnostics;
mod editing;
mod element;
pub mod ex;
mod find;
pub mod git_gutter;
mod git_peek;
mod hover;
mod input;
mod layout;
mod modal;
mod motion;
mod rules;
mod snippet;
mod soft_wrap;
mod style;
mod view;
mod vim_host;
mod wrap;

pub use actions::*;
pub use agent::{AgentChanges, AgentHunk, AgentLabel, HunkKind};
pub use blame::{BlameCommit, BlameData, BlameLine};
pub use buffer::{
    BufferEdited, Coalesce, Conflict, EditKind, FailReason, FileBuffer, LoadState,
    TYPING_GROUP_PAUSE, file_name, language_label,
};
pub use completion::{
    COMPLETION_DEBOUNCE, COMPLETION_ROWS, CompletionEdit, CompletionItem, CompletionKind,
    CompletionList, CompletionState, CompletionTriggers, Signature, SignatureHelp,
};
pub use diagnostics::{
    Diagnostic, DiagnosticCounts, DiagnosticInput, DrawnDiagnostic, GlyphShape, Severity,
    UnderlineStyle,
};
pub use find::{FindQuery, FindResult, FindState, MAX_FIND_MATCHES};
pub use git_gutter::{GitBase, GitBaseKind};
pub use hover::{HOVER_DELAY, HoverBlock, HoverSource, HoverState, markdown_blocks};
pub use hutan_buffer::Utf16Point;
pub use hutan_vim::{Key as VimKey, Mode as VimMode};
pub use layout::TAB_SIZE;
pub use modal::{ExRun, VimMessage, VimSettings, vim_key_of, vim_key_of_keystroke};
pub use rules::IndentUnit;
pub use style::{CodeStyle, rgb, rgba};
pub use view::{
    CodeView, CodeViewEvent, DrawnRow, Layout, MAX_COLUMN_LINES, OVERSCAN_LINES, PaintTimes,
    ViewStats, VimWindow, WindowDirection,
};
pub use vim_host::{BufferHost, HostStep};
pub use wrap::{
    Break as WrapBreak, DEFAULT_WRAP_COLUMN, MAX_WRAP_BYTES, MIN_WRAP_COLS, RowGeom, WrapMap,
    WrapMode, WrapParams, wrap_line,
};

/// The key context of a focused code view.
pub const KEY_CONTEXT: &str = "CodeView";
/// A second name of the same context, for keymaps written for editors in general.
pub const EDITOR_CONTEXT: &str = "Editor";
/// Set in the view's context while the completion popup shows: its keys (↑ ↓ ⌃N ⌃P Return Tab).
pub const COMPLETION_CONTEXT: &str = "showing_completions";
/// Set in the view's context while a snippet's tab stops are being filled in (Tab, ⇧Tab).
pub const SNIPPET_CONTEXT: &str = "in_snippet";
/// The key of the view's context that holds the Vim mode (`normal`, `insert`, `visual`) while
/// `editor.vim_mode` is on.
pub const VIM_MODE_KEY: &str = "vim_mode";
/// Set in the view's context while Vim's `/` or `?` line is typed: the user's Vim mappings do
/// not apply to it.
pub const VIM_COMMAND_LINE_CONTEXT: &str = "vim_command_line";

/// The value of [`VIM_MODE_KEY`] for `mode`: `normal`, `insert`, or `visual` for all three
/// Visual modes. File tabs, the chat composer and the overlays' fields carry it alike.
pub fn vim_mode_value(mode: VimMode) -> &'static str {
    match mode {
        VimMode::Normal => "normal",
        VimMode::Insert => "insert",
        VimMode::Visual | VimMode::VisualLine | VimMode::VisualBlock => "visual",
    }
}

/// Where the editor's own keys apply that Vim's Normal and Visual modes answer themselves
/// (motions, Return, Tab, deletion, the ⌃ line keys): everywhere but there.
pub const TYPING_CONTEXT: &str = "CodeView && vim_mode != normal && vim_mode != visual";
/// Where the editor's own Escape applies (back to one caret): only without Vim, whose Escape
/// leaves Insert and Visual.
pub const PLAIN_CONTEXT: &str = "CodeView && !vim_mode";
