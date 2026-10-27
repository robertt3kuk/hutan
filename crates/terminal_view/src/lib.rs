//! GPUI terminal view for Hutan (M2.3): draws the daemon's render stream and turns keyboard,
//! mouse and input-method events into PTY bytes.
//!
//! The daemon is the only VT parser (ADR 0004). [`TerminalView`] is passive: an owner feeds it
//! [`TerminalRender`]s, history pages and a [`ViewStatus`], and implements [`TerminalBackend`] to
//! carry the view's input, resizes, acks and history requests to the daemon.
//!
//! GPUI-free building blocks (unit tested without a window): [`model`], [`blocks`], [`keys`],
//! [`mouse`], [`paste`], [`image_paste`], [`links`], [`find`], [`selection`] and [`theme`].

pub mod blocks;
mod convert;
mod element;
pub mod find;
pub mod image_paste;
pub mod keys;
pub mod links;
pub mod model;
pub mod mouse;
pub mod paste;
pub mod selection;
mod settings;
mod shape;
mod shared;
pub mod theme;
pub mod trace;
mod view;

pub use find::{FindHighlights, FindSpan, FoundCells};
use gpui::Task;
pub use hutan_protocol::blocks::BlockPart;
pub use hutan_protocol::messages::TerminalSize;
pub use hutan_protocol::render::{Row, TerminalRender};
pub use image_paste::{ImagePasteTarget, PasteAgent};
pub use links::LinkTarget;
pub use settings::{
    BlockSettings, CursorSettings, CursorStyle, FindSettings, OptionAsMeta, TerminalViewSettings,
    Tint,
};
pub use shared::{SharedEvent, SharedTerminal};
pub use theme::{DefaultColors, Rgb, TerminalPalette};
pub use view::{
    BlockActions, Copy, CopyBlockCommand, CopyBlockOutput, IMAGE_PASTE_NOTICE, JumpToNextPrompt,
    JumpToPreviousPrompt, KEY_CONTEXT, Paste, PasteChoice, RerunBlock, ScrollToBottom, ScrollToTop,
    Segment, SelectBlockOutput, SendBlockToAgent, TerminalView, TerminalViewEvent,
    ToggleBlockCollapse, ViewMode, ViewStatus, bind_default_keys,
};

/// What the owner of a [`TerminalView`] provides. Called on the UI thread; implementations hand
/// the work to their connection and must not block.
pub trait TerminalBackend: 'static {
    /// Bytes the user typed, pasted or that mouse/focus reporting produced.
    fn input(&self, bytes: Vec<u8>);
    /// The grid the view can show changed (debounced; the first size is sent at once).
    fn resize(&self, size: TerminalSize);
    /// The render `frame` has been painted; the daemon may send the next one.
    fn ack(&self, frame: u64);
    /// Fetch `count` history rows starting `from_line` lines above the viewport; the answer goes
    /// to [`TerminalView::apply_history`].
    fn request_history(&self, from_line: u32, count: u32);
    /// Plain text of a block's command or output (`terminal.block_text`); `None` when the daemon
    /// cannot answer. The view copies it to the clipboard.
    fn block_text(&self, block_id: u64, part: BlockPart) -> Task<Option<String>>;
    /// The colors the view draws defaults with, so OSC 10/11/12 replies match (called on creation
    /// and when they change; the protocol message arrives with a later wave).
    fn set_default_colors(&self, colors: DefaultColors);
    /// The folder the shell is in now, for relative paths in output that no block places
    /// (`None`: unknown; such paths are not links).
    fn working_dir(&self) -> Option<std::path::PathBuf> {
        None
    }
    /// Where this terminal's pasted images are saved and which agent reads them (`None`: the
    /// owner has no folder for them, so an image on the clipboard is refused with a notice).
    /// Asked at every paste, so the answer follows the session.
    fn image_paste(&self) -> Option<image_paste::ImagePasteTarget> {
        None
    }
}
