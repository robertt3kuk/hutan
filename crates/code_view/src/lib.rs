//! Hutan's code view (ADR 0012, slice E-5): a read-only file viewer drawn by its own GPUI
//! element.
//!
//! # Pieces
//! - [`FileBuffer`]: the text of one open file (a `hutan-buffer` rope), its load state and, while
//!   a view of it is on screen, its `hutan-syntax` tree. One buffer per file, shared by every view;
//!   the desktop's buffer store owns them and reads files through the daemon (`fs.read`).
//! - [`CodeView`]: one view of a buffer — scroll position, mouse selection, find matches — and
//!   its element, which shapes only the lines that intersect the view (cached by line, text
//!   version, horizontal window and style), colors them with the syntax tokens of the color pack
//!   (E-4, [`CodeStyle`]) and paints the gutter, the selection and the matches.
//! - [`FindQuery`] / [`FindState`]: find in file, searched in the background over a snapshot of
//!   the rope.
//!
//! GPUI is linked here (a UI crate, ADR 0010); the text model and the parser are GPUI-free.

mod buffer;
mod element;
mod find;
mod layout;
mod style;
mod view;

pub use buffer::{FailReason, FileBuffer, LoadState, file_name, language_label};
pub use find::{FindQuery, FindResult, FindState, MAX_FIND_MATCHES};
pub use layout::TAB_SIZE;
pub use style::{CodeStyle, rgb, rgba};
pub use view::{CodeView, CodeViewEvent, Layout, OVERSCAN_LINES, PaintTimes, Selection, ViewStats};

/// The key context of a focused code view.
pub const KEY_CONTEXT: &str = "CodeView";

gpui::actions!(
    code_view,
    [
        /// Copy the selection.
        Copy,
        /// Select the whole file.
        SelectAll,
        /// Scroll one line up.
        LineUp,
        /// Scroll one line down.
        LineDown,
        /// Scroll one page up.
        PageUp,
        /// Scroll one page down.
        PageDown,
        /// Scroll to the start of the file.
        Top,
        /// Scroll to the end of the file.
        Bottom,
        /// Open find in file (the owner of the view draws the find bar).
        Find,
        /// Go to the next match.
        FindNext,
        /// Go to the previous match.
        FindPrevious,
        /// Close find in file.
        CloseFind,
    ]
);
