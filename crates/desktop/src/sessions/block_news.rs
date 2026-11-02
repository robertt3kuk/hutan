//! Which `terminal.block` events are news.
//!
//! A block is announced again whenever its lines move (a width change reflowed the rows) and
//! after a gap in the events (the daemon or the window made them up). Those events carry the
//! block's state as it was, so a finished command would count as finishing every time. Block
//! ids only grow within a terminal, and a block starts running and finishes once and in order,
//! so what was seen is a few numbers per terminal: the newest id seen finished and the id seen
//! running.

use hutan_protocol::blocks::{Block, BlockState};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct BlockNews {
    finished_through: u64,
    running: Option<u64>,
}

impl BlockNews {
    /// Whether `block` is a command that just started or just finished, as far as this
    /// terminal's events have said. Remembers what it answers true for.
    pub(super) fn is_news(&mut self, block: &Block) -> bool {
        if block.id <= self.finished_through {
            return false;
        }
        match block.state {
            BlockState::Finished => {
                self.finished_through = block.id;
                self.running = None;
                true
            }
            BlockState::Running => {
                let fresh = self.running != Some(block.id);
                self.running = Some(block.id);
                fresh
            }
            BlockState::Prompt | BlockState::Truncated => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(id: u64, state: BlockState) -> Block {
        Block {
            id,
            prompt_line: id,
            command_line: None,
            output_start: None,
            end_line: None,
            command: None,
            exit_code: None,
            started_at_ms: 0,
            finished_at_ms: None,
            cwd: None,
            state,
        }
    }

    #[test]
    fn a_command_counts_once_when_it_starts_and_once_when_it_finishes() {
        let mut news = BlockNews::default();
        assert!(!news.is_news(&block(1, BlockState::Prompt)));
        assert!(news.is_news(&block(1, BlockState::Running)));
        assert!(
            !news.is_news(&block(1, BlockState::Running)),
            "moved by a reflow"
        );
        assert!(news.is_news(&block(1, BlockState::Finished)));
        assert!(
            !news.is_news(&block(1, BlockState::Finished)),
            "moved by a reflow"
        );
        assert!(
            !news.is_news(&block(1, BlockState::Running)),
            "an old state is no news"
        );
    }

    #[test]
    fn announcing_every_block_again_counts_none_of_the_finished_ones() {
        let mut news = BlockNews::default();
        for id in 1..=5 {
            assert!(news.is_news(&block(id, BlockState::Finished)));
        }
        for id in 1..=5 {
            assert!(
                !news.is_news(&block(id, BlockState::Finished)),
                "block {id}"
            );
        }
        // Commands finished while events were lost are news once the blocks are told again.
        for id in 6..=8 {
            assert!(news.is_news(&block(id, BlockState::Finished)), "block {id}");
        }
        assert!(!news.is_news(&block(7, BlockState::Finished)));
    }

    #[test]
    fn truncated_and_prompt_blocks_are_never_news() {
        let mut news = BlockNews::default();
        assert!(!news.is_news(&block(1, BlockState::Truncated)));
        assert!(!news.is_news(&block(2, BlockState::Prompt)));
    }
}
