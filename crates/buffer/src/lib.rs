//! The text model of Hutan's editor: a rope with byte offsets, atomic transactions, an undo tree
//! with groups, multi-cursor selections, anchors, UTF-16 coordinates and search. GPUI-free, no
//! I/O beyond reading and writing bytes the caller provides, no global state.
//!
//! # Pieces
//! - [`Text`]: the rope. `O(1)` clone (a snapshot for background parsing or LSP sync), slicing,
//!   lines, and every coordinate conversion: byte ↔ char ↔ [`Point`] (line, column) ↔ UTF-16
//!   (offsets and [`Utf16Point`], what LSP and GPUI's text input speak).
//! - [`Buffer`]: a `Text` plus line ending, BOM, version counter, dirty tracking, the undo tree and
//!   tracked anchors. Loads bytes ([`Buffer::from_bytes`] refuses binary and non-UTF-8 input with a
//!   typed [`BufferError`]) and writes them back ([`Buffer::write_to`]) in the original format.
//! - [`Transaction`] / [`Edit`]: ordered edits on byte ranges, applied atomically; compose, invert
//!   and map offsets. [`Buffer::apply`] returns an [`Applied`] describing what changed.
//! - Undo: [`Buffer::undo`], [`Buffer::redo`], groups ([`Buffer::begin_group`]) so one user action
//!   is one step, branches, and [`HistoryLimits`] bounds.
//! - [`Selections`] / [`Selection`]: ordered non-overlapping ranges with a primary; [`Anchor`] with
//!   a [`Bias`]: positions that follow their text through transactions.
//! - [`Buffer::apply_external`]: a minimal diff against new content (an agent edited the file),
//!   applied as one transaction.
//! - [`Text::search`]: literal or regex search over the rope with bounded results.
//!
//! All offsets are **byte offsets** unless a name says otherwise (`*_utf16`, `*_char`, `column`
//! of [`Point`]).
//!
//! ```
//! use hutan_buffer::{Anchor, Buffer};
//!
//! let mut buffer = Buffer::from_content("hello world\n");
//! let world = buffer.track_anchor(Anchor::right(6))?;
//! buffer.insert(0, "big ")?;
//! assert_eq!(buffer.anchor(world).map(|a| a.offset), Some(10));
//! assert_eq!(buffer.text().to_string(), "big hello world\n");
//! buffer.undo();
//! assert_eq!(buffer.text().to_string(), "hello world\n");
//! # Ok::<(), hutan_buffer::BufferError>(())
//! ```

mod anchor;
mod buffer;
mod diff;
mod edit;
mod error;
mod history;
mod search;
mod selection;
mod text;
mod transaction;

pub use anchor::{Anchor, AnchorId, Bias};
pub use buffer::{Applied, Buffer, LineEnding, MAX_ANCHORS, normalize_newlines};
pub use edit::Edit;
pub use error::BufferError;
pub use history::HistoryLimits;
pub use search::{MAX_PATTERN_BYTES, MAX_SEARCH_MATCHES, SearchMatches, SearchQuery};
pub use selection::{MAX_SELECTIONS, Selection, Selections};
pub use text::{Point, Text, Utf16Point};
pub use transaction::{Change, Transaction};
