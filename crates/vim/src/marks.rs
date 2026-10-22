//! Marks and the jumplist (`:help mark-motions`, `:help jump-motions`), kept as lines and byte
//! columns like Vim's, and moved when lines are added or removed by an edit the layer makes.

/// A position: 0-based line and byte column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Pos {
    pub line: usize,
    pub col: usize,
}

/// How many entries the jumplist keeps (Vim's 100).
const JUMPS: usize = 100;

#[derive(Debug, Clone, Default)]
pub(crate) struct Marks {
    letters: [Option<Pos>; 26],
    /// `'.`: where the last change was.
    pub last_change: Option<Pos>,
    /// `'^`: where Insert mode was left.
    pub last_insert: Option<Pos>,
    /// `'[` and `']`: the first and last character of the last changed or yanked text.
    pub change_start: Option<Pos>,
    pub change_end: Option<Pos>,
    /// `'<` and `'>`.
    pub visual_start: Option<Pos>,
    pub visual_end: Option<Pos>,
    /// `''`: where the last jump started.
    pub previous: Option<Pos>,
    jumps: Vec<Pos>,
    /// Where `⌃O` / `⌃I` are in `jumps` (`jumps.len()` after a jump).
    index: usize,
    /// Entries pushed so far (every jump, the first `⌃O` after one included).
    pushed: u64,
}

/// What a mark name reads (`'a`, `''`, `'.`, …).
pub(crate) fn readable(c: char) -> bool {
    c.is_ascii_alphabetic() || matches!(c, '\'' | '`' | '.' | '^' | '[' | ']' | '<' | '>')
}

impl Marks {
    pub fn get(&self, name: char) -> Option<Pos> {
        match name {
            'a'..='z' => self.letters[name as usize - 'a' as usize],
            'A'..='Z' => self.letters[name.to_ascii_lowercase() as usize - 'a' as usize],
            '\'' | '`' => self.previous,
            '.' => self.last_change,
            '^' => self.last_insert,
            '[' => self.change_start,
            ']' => self.change_end,
            '<' => self.visual_start,
            '>' => self.visual_end,
            _ => None,
        }
    }

    /// `m{name}`; `false` for a name that cannot be set.
    pub fn set(&mut self, name: char, at: Pos) -> bool {
        match name {
            'a'..='z' | 'A'..='Z' => {
                self.letters[name.to_ascii_lowercase() as usize - 'a' as usize] = Some(at);
            }
            '\'' | '`' => self.jump_from(at),
            '[' => self.change_start = Some(at),
            ']' => self.change_end = Some(at),
            '<' => self.visual_start = Some(at),
            '>' => self.visual_end = Some(at),
            _ => return false,
        }
        true
    }

    /// A jump starts at `at`: it becomes `''` and the newest jumplist entry.
    pub fn jump_from(&mut self, at: Pos) {
        self.previous = Some(at);
        self.jumps.push(at);
        if self.jumps.len() > JUMPS {
            self.jumps.remove(0);
        }
        self.index = self.jumps.len();
        self.pushed += 1;
    }

    /// Entries pushed so far: it grows with every jump.
    pub fn pushed(&self) -> u64 {
        self.pushed
    }

    /// `⌃O` (`count` < 0) and `⌃I` (`count` > 0) from `at`: the entry to go to, or `None`
    /// at either end of the list.
    pub fn step(&mut self, at: Pos, count: isize) -> Option<Pos> {
        self.dedup();
        if self.jumps.is_empty() {
            return None;
        }
        let reach = |index: usize| index as isize + count;
        if reach(self.index) < 0 || reach(self.index) >= self.jumps.len() as isize {
            return None;
        }
        if self.index == self.jumps.len() {
            // The first ⌃O after a jump keeps where it started, to come back with ⌃I.
            self.jump_from(at);
            self.index -= 1;
            if reach(self.index) < 0 {
                return None;
            }
        }
        self.index = reach(self.index) as usize;
        self.jumps.get(self.index).copied()
    }

    /// One entry per line: an older entry on the line of a newer one goes.
    fn dedup(&mut self) {
        let mut kept = Vec::with_capacity(self.jumps.len());
        let mut index = self.index;
        for (i, pos) in self.jumps.iter().enumerate() {
            let newer = self.jumps[i + 1..].iter().any(|p| p.line == pos.line);
            if self.index == i {
                index = kept.len();
            }
            if !newer {
                kept.push(*pos);
            }
        }
        if self.index >= self.jumps.len() {
            index = kept.len();
        }
        self.jumps = kept;
        self.index = index.min(self.jumps.len());
    }

    /// Follows an edit's line change.
    pub fn apply(&mut self, change: crate::vim::LineChange) {
        match change {
            crate::vim::LineChange::Lines {
                first,
                removed,
                inserted,
            } => self.adjust(first, removed, inserted),
            crate::vim::LineChange::Merge {
                first,
                last,
                inserted,
            } => self.merge(first, last, inserted),
        }
    }

    /// Lines `first..=last` became `inserted + 1` lines: marks on the lines after `first` among
    /// them move to `first`, marks below move by the difference.
    fn merge(&mut self, first: usize, last: usize, inserted: usize) {
        let removed = last - first;
        if removed == inserted {
            return;
        }
        let moved = |pos: &mut Pos| {
            if pos.line > last {
                pos.line = pos.line + inserted - removed;
            } else if pos.line > first + inserted {
                pos.line = first + inserted;
            }
        };
        for pos in self
            .letters
            .iter_mut()
            .chain([
                &mut self.last_change,
                &mut self.last_insert,
                &mut self.change_start,
                &mut self.change_end,
                &mut self.visual_start,
                &mut self.visual_end,
                &mut self.previous,
            ])
            .flatten()
        {
            moved(pos);
        }
        for pos in &mut self.jumps {
            moved(pos);
        }
    }

    /// Lines `first..first + removed` were replaced by `inserted` lines: marks below move, and
    /// marks on removed lines go (letters, `'.`) or move to the first of them (the rest).
    pub fn adjust(&mut self, first: usize, removed: usize, inserted: usize) {
        if removed == inserted {
            return;
        }
        let end = first + removed;
        let shift = |line: usize| line + inserted - removed;
        let gone = |p: &mut Option<Pos>| {
            if let Some(pos) = p {
                if pos.line >= end {
                    pos.line = shift(pos.line);
                } else if pos.line >= first + inserted {
                    *p = None;
                }
            }
        };
        for mark in &mut self.letters {
            gone(mark);
        }
        gone(&mut self.last_change);
        let moved = |pos: &mut Pos| {
            if pos.line >= end {
                pos.line = shift(pos.line);
            } else if pos.line >= first + inserted {
                *pos = Pos {
                    line: first,
                    col: 0,
                };
            }
        };
        for mark in [
            &mut self.last_insert,
            &mut self.change_start,
            &mut self.change_end,
            &mut self.visual_start,
            &mut self.visual_end,
            &mut self.previous,
        ]
        .into_iter()
        .flatten()
        {
            moved(mark);
        }
        for pos in &mut self.jumps {
            moved(pos);
        }
    }
}
