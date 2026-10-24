//! The block index: prompt + command + output units built from OSC 133 marks (M10, protocol
//! 0.5).
//!
//! This module is pure bookkeeping. [`crate::Emulator`] decides *where* a mark happened (the
//! cursor's absolute line once the engine has executed every byte before the mark), reads the
//! command text off the grid and tells the index about it; the index keeps the blocks, their
//! state machine and the changes to announce.
//!
//! Marks are treated leniently, the way the shell scripts can produce them (see
//! `docs/shell-integration.md`):
//!
//! | Mark | Effect |
//! |---|---|
//! | `A` | A prompt block starts. A prompt that is redrawn (resize, `Ctrl-L`) or abandoned (empty Enter) reuses the open prompt block; a command that never reported `D` is finished without an exit code. |
//! | `B` | Records where the command input starts (the prompt block is announced again with it). Without an `A` it also creates the prompt block. |
//! | `C` | The command runs (`running`). Without a prompt block one is created; a second `C` while running is ignored. |
//! | `D` | Finishes a running block. A `D` with no running block (empty Enter, an inner shell's `D`) is ignored. |

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use hutan_protocol::blocks::{Block, BlockState};

/// Blocks kept per terminal; the oldest are dropped first.
pub(crate) const MAX_BLOCKS: usize = 2_000;
/// Longest command text kept (bytes).
pub(crate) const MAX_COMMAND_BYTES: usize = 4 * 1024;

/// Why [`crate::Emulator::block_text`] has no text to give.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BlockTextError {
    /// No such block: never existed, or dropped from the bounded list.
    #[error("no such block")]
    NotFound,
    /// The block's rows are in the primary screen, which the alternate screen hides right now.
    #[error("the terminal shows the alternate screen")]
    Unavailable,
}

/// Text of one block part (see [`crate::Emulator::block_text`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockText {
    pub text: String,
    /// Cut at the byte limit, or the first lines were evicted or erased.
    pub truncated: bool,
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Between `A` and `C`.
    Prompt,
    /// Between `C` and `D`.
    Running,
}

/// The block the next marks belong to.
#[derive(Debug, Clone, Copy)]
struct Current {
    id: u64,
    phase: Phase,
    /// Line and column of the end of the prompt (`B`).
    input_at: Option<(u64, u16)>,
}

#[derive(Debug, Default)]
pub(crate) struct BlockIndex {
    /// Oldest first; ids are consecutive, so the position of `id` is `id - front.id`.
    blocks: VecDeque<Block>,
    next_id: u64,
    /// `blocks[..scanned]` were already compared with the retained history's first line.
    scanned: usize,
    /// The last working directory the shell reported.
    cwd: Option<PathBuf>,
    current: Option<Current>,
    /// Changed blocks, in the order they changed.
    events: Vec<Block>,
}

impl BlockIndex {
    pub(crate) fn new() -> Self {
        Self {
            next_id: 1,
            ..Self::default()
        }
    }

    fn position(&self, id: u64) -> Option<usize> {
        let first = self.blocks.front()?.id;
        let index = usize::try_from(id.checked_sub(first)?).ok()?;
        (index < self.blocks.len()).then_some(index)
    }

    fn emit(&mut self, index: usize) {
        if let Some(block) = self.blocks.get(index) {
            self.events.push(block.clone());
        }
    }

    fn push(&mut self, line: u64, now: u64) -> usize {
        let block = Block {
            id: self.next_id,
            prompt_line: line,
            command_line: None,
            output_start: None,
            end_line: None,
            command: None,
            exit_code: None,
            started_at_ms: now,
            finished_at_ms: None,
            cwd: self.cwd.clone(),
            state: BlockState::Prompt,
        };
        self.next_id += 1;
        self.blocks.push_back(block);
        if self.blocks.len() > MAX_BLOCKS {
            self.blocks.pop_front();
            self.scanned = self.scanned.saturating_sub(1);
        }
        self.blocks.len() - 1
    }

    /// Index of the open prompt block, creating one at `line` when there is none.
    fn open_prompt(&mut self, line: u64, now: u64) -> Option<usize> {
        match self.current {
            Some(Current {
                id,
                phase: Phase::Prompt,
                ..
            }) => self.position(id),
            Some(_) => None,
            None => {
                let index = self.push(line, now);
                self.current = Some(Current {
                    id: self.blocks[index].id,
                    phase: Phase::Prompt,
                    input_at: None,
                });
                self.emit(index);
                Some(index)
            }
        }
    }

    /// `OSC 133;A` at `line`. `end` is where the output of a command that is still running
    /// would end (the line after the cursor's, when the cursor is not in column 0).
    pub(crate) fn prompt(&mut self, line: u64, end: u64, now: u64) {
        if let Some(Current {
            id,
            phase: Phase::Running,
            ..
        }) = self.current
        {
            self.finish_block(id, None, end, now);
        }
        if let Some(Current {
            id,
            phase: Phase::Prompt,
            ..
        }) = self.current
            && let Some(index) = self.position(id)
        {
            // The same prompt drawn again: it moves, the block stays.
            let block = &mut self.blocks[index];
            let changed = block.prompt_line != line || block.state != BlockState::Prompt;
            block.prompt_line = line;
            block.command_line = None;
            block.state = BlockState::Prompt;
            block.started_at_ms = now;
            block.cwd.clone_from(&self.cwd);
            self.scanned = self.scanned.min(index);
            self.current = Some(Current {
                id,
                phase: Phase::Prompt,
                input_at: None,
            });
            if changed {
                self.emit(index);
            }
            return;
        }
        self.current = None;
        self.open_prompt(line, now);
    }

    /// `OSC 133;B` at `line`, `col`: the command input starts there.
    pub(crate) fn input(&mut self, line: u64, col: u16, now: u64) {
        let Some(index) = self.open_prompt(line, now) else {
            return;
        };
        let changed = self.blocks[index].command_line != Some(line);
        self.blocks[index].command_line = Some(line);
        if let Some(current) = &mut self.current {
            current.input_at = Some((line, col));
        }
        if changed {
            self.emit(index);
        }
    }

    /// The line the prompt that is open now (between `A` and `C`) starts at.
    pub(crate) fn open_prompt_line(&self) -> Option<u64> {
        match self.current {
            Some(Current {
                id,
                phase: Phase::Prompt,
                ..
            }) => self
                .position(id)
                .map(|index| self.blocks[index].prompt_line),
            _ => None,
        }
    }

    /// Where the open prompt's command input starts, if `B` was seen.
    pub(crate) fn input_at(&self) -> Option<(u64, u16)> {
        match self.current {
            Some(Current {
                phase: Phase::Prompt,
                input_at,
                ..
            }) => input_at,
            _ => None,
        }
    }

    /// `OSC 133;C` at `line`: the command runs; `command` is its text.
    pub(crate) fn run(&mut self, line: u64, command: Option<String>, now: u64) {
        let Some(index) = self.open_prompt(line, now) else {
            return;
        };
        let block = &mut self.blocks[index];
        block.output_start = Some(line);
        block.command = command;
        block.started_at_ms = now;
        if block.state != BlockState::Truncated {
            block.state = BlockState::Running;
        }
        let id = block.id;
        self.current = Some(Current {
            id,
            phase: Phase::Running,
            input_at: None,
        });
        self.emit(index);
    }

    /// `OSC 133;D`: the running command ended; its output ends before line `end`.
    pub(crate) fn finish(&mut self, exit_code: Option<i32>, end: u64, now: u64) {
        if let Some(Current {
            id,
            phase: Phase::Running,
            ..
        }) = self.current
        {
            self.finish_block(id, exit_code, end, now);
        }
    }

    fn finish_block(&mut self, id: u64, exit_code: Option<i32>, end: u64, now: u64) {
        self.current = None;
        let Some(index) = self.position(id) else {
            return;
        };
        let block = &mut self.blocks[index];
        block.exit_code = exit_code;
        block.end_line = Some(end);
        block.finished_at_ms = Some(now);
        if block.state != BlockState::Truncated {
            block.state = BlockState::Finished;
        }
        self.emit(index);
    }

    /// `OSC 7`: the shell's working directory; the open prompt shows it, a running command
    /// keeps the directory it started in.
    pub(crate) fn set_cwd(&mut self, cwd: PathBuf) {
        if let Some(Current {
            id,
            phase: Phase::Prompt,
            ..
        }) = self.current
            && let Some(index) = self.position(id)
        {
            self.blocks[index].cwd = Some(cwd.clone());
        }
        self.cwd = Some(cwd);
    }

    fn truncate_index(&mut self, index: usize) {
        if self.blocks[index].state != BlockState::Truncated {
            self.blocks[index].state = BlockState::Truncated;
            self.emit(index);
        }
    }

    /// Marks every block whose prompt line is before `first_retained` (evicted or erased) as
    /// truncated.
    pub(crate) fn truncate_below(&mut self, first_retained: u64) {
        while self.scanned < self.blocks.len()
            && self.blocks[self.scanned].prompt_line < first_retained
        {
            self.truncate_index(self.scanned);
            self.scanned += 1;
        }
    }

    /// Everything on screen and in the history is gone (`ESC c`).
    pub(crate) fn truncate_all(&mut self) {
        for index in 0..self.blocks.len() {
            self.truncate_index(index);
        }
        self.scanned = self.blocks.len();
    }

    /// Highest line any block refers to.
    fn max_line(block: &Block) -> u64 {
        [
            Some(block.prompt_line),
            block.command_line,
            block.output_start,
            block.end_line,
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(block.prompt_line)
    }

    /// True when the newest block refers to a line at or after `line` (lines only grow with
    /// ids, so older blocks cannot).
    pub(crate) fn reaches(&self, line: u64) -> bool {
        self.blocks
            .back()
            .is_some_and(|block| Self::max_line(block) >= line)
    }

    /// Rows from `line` on changed in a way the index cannot follow: every block that refers to
    /// them is truncated.
    pub(crate) fn invalidate_from(&mut self, line: u64) {
        for index in (0..self.blocks.len()).rev() {
            if self.blocks[index].state == BlockState::Truncated {
                continue;
            }
            if Self::max_line(&self.blocks[index]) >= line {
                self.truncate_index(index);
            }
        }
    }

    /// Rewrites every line number at or after `from` with `map` (rows were reflowed).
    pub(crate) fn remap(&mut self, from: u64, map: impl Fn(u64) -> u64) {
        let apply = |line: &mut u64| {
            if *line >= from {
                *line = map(*line);
            }
        };
        for block in self.blocks.iter_mut().rev() {
            if Self::max_line(block) < from {
                if block.state != BlockState::Truncated {
                    break;
                }
                continue;
            }
            apply(&mut block.prompt_line);
            for line in [
                &mut block.command_line,
                &mut block.output_start,
                &mut block.end_line,
            ]
            .into_iter()
            .flatten()
            {
                apply(line);
            }
        }
        if let Some(Current {
            input_at: Some((line, _)),
            ..
        }) = &mut self.current
        {
            apply(line);
        }
    }

    pub(crate) fn get(&self, id: u64) -> Option<&Block> {
        self.position(id).map(|index| &self.blocks[index])
    }

    /// Blocks newest first: at most `limit`, ids below `before_id` when given.
    pub(crate) fn page(&self, before_id: Option<u64>, limit: usize) -> Vec<Block> {
        self.blocks
            .iter()
            .rev()
            .filter(|block| before_id.is_none_or(|before| block.id < before))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Where each block lives, oldest first: `(prompt_line, end_line, id)`.
    pub(crate) fn spans(&self) -> Vec<(u64, Option<u64>, u64)> {
        self.blocks
            .iter()
            .map(|block| (block.prompt_line, block.end_line, block.id))
            .collect()
    }

    /// Changes since the last call, oldest first.
    pub(crate) fn take_events(&mut self) -> Vec<Block> {
        std::mem::take(&mut self.events)
    }
}
