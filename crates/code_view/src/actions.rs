//! The actions of a code view (bound in the desktop's keymap to the [`KEY_CONTEXT`] and
//! `FileView` contexts) and the handlers that run them.
//!
//! [`KEY_CONTEXT`]: crate::KEY_CONTEXT

use gpui::{Context, Div, InteractiveElement as _, Stateful};

use crate::motion::{Motion, Target};
use crate::view::CodeView;

gpui::actions!(
    code_view,
    [
        /// Move the carets one character left (or to the start of their selections).
        MoveLeft,
        /// Move the carets one character right (or to the end of their selections).
        MoveRight,
        /// Move the carets one line up.
        MoveUp,
        /// Move the carets one line down.
        MoveDown,
        /// Move the carets to the start of the word.
        MoveWordLeft,
        /// Move the carets to the end of the word.
        MoveWordRight,
        /// Move the carets to the first character of the line, or to its very start.
        MoveLineStart,
        /// Move the carets to the end of the line.
        MoveLineEnd,
        /// Move the caret to the start of the file.
        MoveToStart,
        /// Move the caret to the end of the file.
        MoveToEnd,
        /// Move the carets one page up.
        PageUp,
        /// Move the carets one page down.
        PageDown,
        /// Extend the selections one character left.
        SelectLeft,
        /// Extend the selections one character right.
        SelectRight,
        /// Extend the selections one line up.
        SelectUp,
        /// Extend the selections one line down.
        SelectDown,
        /// Extend the selections to the start of the word.
        SelectWordLeft,
        /// Extend the selections to the end of the word.
        SelectWordRight,
        /// Extend the selections to the start of the line.
        SelectLineStart,
        /// Extend the selections to the end of the line.
        SelectLineEnd,
        /// Extend the selection to the start of the file.
        SelectToStart,
        /// Extend the selection to the end of the file.
        SelectToEnd,
        /// Extend the selections one page up.
        SelectPageUp,
        /// Extend the selections one page down.
        SelectPageDown,
        /// Select the whole file.
        SelectAll,
        /// Select the word under the caret, then add the next occurrence of the selection.
        SelectNextOccurrence,
        /// Select every occurrence of the selection (or of the word under the caret).
        SelectAllOccurrences,
        /// Back to one caret (the primary), without a selection.
        Cancel,
        /// Break the line, indenting the new one.
        Newline,
        /// Indent the selected lines, or insert one level of indentation at the carets.
        Tab,
        /// Indent the lines of the selections.
        Indent,
        /// Outdent the lines of the selections.
        Outdent,
        /// Delete the selection, or the character before the caret.
        DeleteBackward,
        /// Delete the selection, or the character after the caret.
        DeleteForward,
        /// Delete to the start of the word.
        DeleteWordBackward,
        /// Delete to the end of the word.
        DeleteWordForward,
        /// Delete to the start of the line.
        DeleteToLineStart,
        /// Delete to the end of the line.
        DeleteToLineEnd,
        /// Delete the lines of the selections.
        DeleteLine,
        /// Copy the lines of the selections above them.
        DuplicateLineUp,
        /// Copy the lines of the selections below them.
        DuplicateLineDown,
        /// Move the lines of the selections up by one.
        MoveLineUp,
        /// Move the lines of the selections down by one.
        MoveLineDown,
        /// Comment the lines of the selections out, or in again.
        ToggleComment,
        /// Cut the selections (the lines, without one).
        Cut,
        /// Copy the selections (the lines, without one).
        Copy,
        /// Paste at every caret (one piece each when as many were copied).
        Paste,
        /// Undo the last change.
        Undo,
        /// Redo the change undone last.
        Redo,
        /// Save the file (the owner of the view writes it).
        Save,
        /// Open find in file (the owner of the view draws the find bar).
        Find,
        /// Open find with replace.
        FindReplace,
        /// Go to the next match.
        FindNext,
        /// Go to the previous match.
        FindPrevious,
        /// Close find in file.
        CloseFind,
    ]
);

/// Attaches the handlers of every action that runs in the view itself.
pub(crate) fn register(root: Stateful<Div>, cx: &mut Context<CodeView>) -> Stateful<Div> {
    macro_rules! motion {
        ($root:expr, $action:ty, $target:expr, $select:expr) => {
            $root.on_action(cx.listener(|this, _: &$action, _, cx| {
                this.motion(
                    Motion {
                        target: $target,
                        select: $select,
                    },
                    cx,
                )
            }))
        };
    }
    macro_rules! run {
        ($root:expr, $action:ty, $method:ident) => {
            $root.on_action(cx.listener(|this, _: &$action, _, cx| this.$method(cx)))
        };
    }
    let root = motion!(root, MoveLeft, Target::Left, false);
    let root = motion!(root, MoveRight, Target::Right, false);
    let root = motion!(root, MoveUp, Target::Up(1), false);
    let root = motion!(root, MoveDown, Target::Down(1), false);
    let root = motion!(root, MoveWordLeft, Target::WordLeft, false);
    let root = motion!(root, MoveWordRight, Target::WordRight, false);
    let root = motion!(root, MoveLineStart, Target::LineStart, false);
    let root = motion!(root, MoveLineEnd, Target::LineEnd, false);
    let root = motion!(root, MoveToStart, Target::Start, false);
    let root = motion!(root, MoveToEnd, Target::End, false);
    let root = motion!(root, PageUp, Target::PageUp, false);
    let root = motion!(root, PageDown, Target::PageDown, false);
    let root = motion!(root, SelectLeft, Target::Left, true);
    let root = motion!(root, SelectRight, Target::Right, true);
    let root = motion!(root, SelectUp, Target::Up(1), true);
    let root = motion!(root, SelectDown, Target::Down(1), true);
    let root = motion!(root, SelectWordLeft, Target::WordLeft, true);
    let root = motion!(root, SelectWordRight, Target::WordRight, true);
    let root = motion!(root, SelectLineStart, Target::LineStart, true);
    let root = motion!(root, SelectLineEnd, Target::LineEnd, true);
    let root = motion!(root, SelectToStart, Target::Start, true);
    let root = motion!(root, SelectToEnd, Target::End, true);
    let root = motion!(root, SelectPageUp, Target::PageUp, true);
    let root = motion!(root, SelectPageDown, Target::PageDown, true);
    let root = run!(root, SelectAll, select_all);
    let root = run!(root, SelectNextOccurrence, select_next_occurrence);
    let root = run!(root, SelectAllOccurrences, select_all_occurrences);
    let root = run!(root, Cancel, cancel);
    let root = run!(root, Newline, newline);
    let root = run!(root, Tab, tab);
    let root = run!(root, Indent, indent);
    let root = run!(root, Outdent, outdent);
    let root = run!(root, DeleteBackward, delete_backward);
    let root = run!(root, DeleteForward, delete_forward);
    let root = run!(root, DeleteWordBackward, delete_word_backward);
    let root = run!(root, DeleteWordForward, delete_word_forward);
    let root = run!(root, DeleteToLineStart, delete_to_line_start);
    let root = run!(root, DeleteToLineEnd, delete_to_line_end);
    let root = run!(root, DeleteLine, delete_line);
    let root = run!(root, DuplicateLineUp, duplicate_line_up);
    let root = run!(root, DuplicateLineDown, duplicate_line_down);
    let root = run!(root, MoveLineUp, move_line_up);
    let root = run!(root, MoveLineDown, move_line_down);
    let root = run!(root, ToggleComment, toggle_comment);
    let root = run!(root, Cut, cut);
    let root = run!(root, Copy, copy);
    let root = run!(root, Paste, paste);
    let root = run!(root, Undo, undo);
    run!(root, Redo, redo)
}
