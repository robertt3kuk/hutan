//! One terminal model per terminal, many views.
//!
//! [`SharedTerminal`] is the GPUI entity an owner feeds with the daemon's renders; any number of
//! [`TerminalView`](crate::TerminalView)s (a pane tab, an overview preview) draw from it. It owns
//! the [`TerminalModel`], the ack that is due, and the [`ViewStatus`] every view shows.
//!
//! Acks are paced by painting: the newest applied frame is acked after the *first* view that
//! shows it has painted it. While no view is visible nothing is painted, so nothing is acked and
//! the daemon stops sending after one frame; the first paint after that acks it and the daemon
//! catches the view up with a single coalesced frame.

use std::cell::{Ref, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;

use gpui::{Context, EventEmitter};
use hutan_client::BlockList;
use hutan_protocol::blocks::Block;
use hutan_protocol::render::{Row, TerminalRender};

use crate::ViewStatus;
use crate::model::{Applied, TerminalModel};

/// What views learn from a [`SharedTerminal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharedEvent {
    /// A snapshot or frame was applied to the model.
    Applied(Applied),
    /// A `terminal.history` answer arrived (`empty`: the daemon has no lines that far back).
    History { from_line: u32, empty: bool },
    /// The status the views show changed.
    Status,
    /// The model was replaced by an empty one (the terminal was swapped for another).
    Reset,
    /// The block list or the set of folded blocks changed.
    Blocks,
    /// A view folded or unfolded a block: the set to persist changed (restoring a saved set
    /// emits [`SharedEvent::Blocks`] only).
    Collapsed,
}

/// A cheap, cloneable read handle on the model for code that has no GPUI context at hand.
#[derive(Debug, Clone)]
pub(crate) struct ModelRef(Rc<RefCell<TerminalModel>>);

impl ModelRef {
    pub(crate) fn get(&self) -> Ref<'_, TerminalModel> {
        self.0.borrow()
    }
}

/// The model of one terminal plus the pacing and status state its views share.
#[derive(Debug)]
pub struct SharedTerminal {
    model: Rc<RefCell<TerminalModel>>,
    ack_due: Option<u64>,
    status: ViewStatus,
}

impl EventEmitter<SharedEvent> for SharedTerminal {}

impl Default for SharedTerminal {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedTerminal {
    pub fn new() -> Self {
        Self {
            model: Rc::new(RefCell::new(TerminalModel::new())),
            ack_due: None,
            status: ViewStatus::Live,
        }
    }

    pub fn model(&self) -> Ref<'_, TerminalModel> {
        self.model.borrow()
    }

    pub(crate) fn model_ref(&self) -> ModelRef {
        ModelRef(Rc::clone(&self.model))
    }

    pub fn status(&self) -> &ViewStatus {
        &self.status
    }

    /// The terminal title the application set, if any.
    pub fn title(&self) -> Option<String> {
        self.model.borrow().title().map(str::to_owned)
    }

    /// The newest frame that was applied but not yet acked.
    pub fn ack_pending(&self) -> Option<u64> {
        self.ack_due
    }

    /// Applies a snapshot or frame. The frame is acked once a view has painted it.
    pub fn apply_render(&mut self, render: TerminalRender, cx: &mut Context<Self>) {
        let frame = match &render {
            TerminalRender::Snapshot(s) => s.frame,
            TerminalRender::Frame(f) => f.frame,
        };
        // Even a frame that could not be applied is acked, or the daemon would wait for it.
        self.ack_due = Some(frame);
        let applied = self.model.borrow_mut().apply(render);
        if let Some(applied) = applied {
            cx.emit(SharedEvent::Applied(applied));
        }
        cx.notify();
    }

    /// Delivers a `terminal.history` answer: `from_line` 0 is the line directly above the
    /// viewport, `rows` are newest first.
    pub fn apply_history(&mut self, from_line: u32, rows: Vec<Row>, cx: &mut Context<Self>) {
        self.apply_history_at(None, from_line, rows, cx);
    }

    /// [`Self::apply_history`] for a page counted from the viewport whose first row is the
    /// absolute line `top` (see `TerminalModel::apply_history_at`).
    pub fn apply_history_at(
        &mut self,
        top: Option<u64>,
        from_line: u32,
        rows: Vec<Row>,
        cx: &mut Context<Self>,
    ) {
        let empty = rows.is_empty();
        if !empty {
            self.model
                .borrow_mut()
                .apply_history_at(top, from_line, rows);
        }
        cx.emit(SharedEvent::History { from_line, empty });
        cx.notify();
    }

    /// Replaces the block list: an initial fetch, or one after a reconnect.
    pub fn set_blocks(&mut self, blocks: BlockList, cx: &mut Context<Self>) {
        self.model.borrow_mut().set_blocks(blocks);
        cx.emit(SharedEvent::Blocks);
        cx.notify();
    }

    /// Folds one `terminal.block` event into the list.
    pub fn apply_block(&mut self, block: Block, cx: &mut Context<Self>) {
        self.model.borrow_mut().apply_block(block);
        cx.emit(SharedEvent::Blocks);
        cx.notify();
    }

    /// Restores the folded blocks the owner saved.
    pub fn set_collapsed(&mut self, ids: BTreeSet<u64>, cx: &mut Context<Self>) {
        if *self.model.borrow().collapsed() == ids {
            return;
        }
        self.model.borrow_mut().set_collapsed(ids);
        cx.emit(SharedEvent::Blocks);
        cx.notify();
    }

    /// Folds or unfolds a block for every view of this terminal (view-only: the daemon never
    /// hears of it). Returns whether anything changed.
    pub fn set_block_collapsed(
        &mut self,
        id: u64,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let changed = self.model.borrow_mut().set_block_collapsed(id, collapsed);
        if changed {
            cx.emit(SharedEvent::Blocks);
            cx.emit(SharedEvent::Collapsed);
            cx.notify();
        }
        changed
    }

    /// Replaces the status every view shows (live, reconnecting, ended).
    pub fn set_status(&mut self, status: ViewStatus, cx: &mut Context<Self>) {
        if status == self.status {
            return;
        }
        self.status = status;
        cx.emit(SharedEvent::Status);
        cx.notify();
    }

    /// Forgets everything shown so far: the owner re-binds to another terminal (a restarted
    /// session) whose first snapshot starts from scratch.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        *self.model.borrow_mut() = TerminalModel::new();
        self.ack_due = None;
        cx.emit(SharedEvent::Reset);
        cx.notify();
    }

    pub(crate) fn take_ack(&mut self) -> Option<u64> {
        self.ack_due.take()
    }
}
