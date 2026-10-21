//! Vim's modal editing over any text that implements [`VimHost`]: Insert, Normal, Visual
//! (charwise, linewise and blockwise) with counts, operators, motions and text objects,
//! registers, marks and the jumplist, macros, search, `.` repeat, undo, and a `:` line with a
//! small ex parser. Pure data: no GPUI, no clock, no I/O; a view translates keystrokes into
//! [`Key`]s and feeds them to [`Vim::key`].
//!
//! The behaviour follows Vim's documented model (`:help motion.txt`, `:help change.txt`,
//! `:help pattern`), not any editor's code, and is checked against a real Vim by the
//! differential fixtures in `tests/fixtures` (recorded with `examples/record.rs`). Words are
//! runs of letters, digits and `_` (or of other non-blank characters), classified by
//! [`char::is_alphanumeric`], so Cyrillic and other scripts are words like Latin ones.
//!
//! # Pieces
//! - [`Vim`]: the state of one field (mode, pending command, registers, marks, last change). It
//!   holds no text: every call takes the [`VimHost`] the keys are applied to.
//! - [`VimHost`]: what the layer needs from a text: byte-offset reads (character at, line of,
//!   slice), edits as [`Transaction`]s, undo grouping and the host's own Insert-mode editing keys.
//!   Reads never copy the text, so a rope can implement them directly; a host that keeps a
//!   `String` (every one-line field of the desktop) answers them from it.
//! - [`Key`], [`Options`], [`Mode`], [`Outcome`], [`Clipboard`]: the vocabulary of the view.
//! - [`ex`]: the `:` line's parser ([`ex::Ex`]); [`Pattern`]: Vim patterns on the `regex` crate.
//!
//! All offsets are byte offsets on character boundaries.

mod command;
pub mod ex;
mod exec;
mod host;
mod key;
mod marks;
mod normal;
mod ops;
mod pattern;
mod register;
mod text;
mod vim;

pub use host::{Clipboard, Edit, Transaction, VimHost};
pub use key::{Key, Mode, Options, Outcome};
pub use pattern::{CaseOptions, Found, Pattern, expand, with_previous};
pub use register::{Register, RegisterKind};
pub use vim::{Message, Settings, Vim};
