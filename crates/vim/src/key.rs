//! The vocabulary a view speaks to the modal layer.

/// Vim's modes (Visual is charwise only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Typing inserts text.
    #[default]
    Insert,
    /// Keys are commands.
    Normal,
    /// Motions extend a selection; operators act on it.
    Visual,
}

impl Mode {
    /// The word of the mode pill.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Insert => "INSERT",
            Mode::Normal => "NORMAL",
            Mode::Visual => "VISUAL",
        }
    }

    /// Whether the caret is a block (Normal, Visual) rather than a bar (Insert).
    pub fn block_caret(self) -> bool {
        self != Mode::Insert
    }
}

/// A key press, reduced to what the modal layer answers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A typed character (shift already applied: `W`, `$`).
    Char(char),
    /// ⌃ and a letter.
    Ctrl(char),
    Escape,
    Enter,
    Tab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// What [`Vim::key`](crate::Vim::key) did with a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The key was used (possibly as a no-op: Normal mode swallows keys it does not know).
    Handled,
    /// Not the modal layer's: the field's own key handling applies (Enter, Tab, Escape in Normal
    /// mode, and in Insert mode every key when the field types for itself).
    Ignored,
}

/// How the field the keys go to behaves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// A single-line field: `j`/`k` do nothing, lines are the whole text (`dd` clears it).
    pub single_line: bool,
    /// The field types for itself in Insert mode: only Escape is the modal layer's there.
    pub insert_passthrough: bool,
    /// Yanks and deletes also go to the system clipboard, and `p` pastes from it.
    pub use_system_clipboard: bool,
}
