//! Terminal splits: a tab that holds a tree of terminals (Ghostty / iTerm / WezTerm style).
//!
//! A tab with one terminal stays [`ItemRef::Terminal`]; splitting it turns it into
//! [`ItemRef::TerminalSplit`], whose [`TermSplit`] is a [`PaneTree`] reused for its geometry
//! (ratios, dividers, neighbours): every leaf is a pane of exactly one terminal item, with an id
//! that is only unique inside the tab. Closing the second-to-last split turns the tab back into
//! a plain terminal. Pane splits (a new pane with its own tab strip) are a different thing and
//! stay for mixing kinds.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use super::ops::neighbor_in;
use super::{
    Direction, Edge, FocusTarget, ItemRef, ModelError, Pane, PaneId, PaneTree, Rect, WorkbenchState,
};

/// The smallest a split may be dragged to, in character cells.
pub const MIN_SPLIT_COLS: u16 = 6;
/// See [`MIN_SPLIT_COLS`].
pub const MIN_SPLIT_ROWS: u16 = 3;

/// The terminals of a tab, as a split tree plus which one has the keyboard.
#[derive(Clone, Debug, PartialEq)]
pub struct TermSplit {
    /// Leaves are panes holding one [`ItemRef::Terminal`].
    pub tree: PaneTree,
    /// The leaf that has keyboard focus (its session is the tab's title and `focused_session`).
    pub focus: PaneId,
    /// The focused leaf fills the tab (⌘⇧↩); the others keep their place in `tree`.
    pub zoomed: bool,
}

// Ratios are finite (the tree normalizes them), so equality is reflexive.
impl Eq for TermSplit {}

impl Hash for TermSplit {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for session in self.sessions() {
            session.hash(state);
        }
        self.focus.hash(state);
        self.zoomed.hash(state);
    }
}

fn terminal(session: &str) -> ItemRef {
    ItemRef::Terminal {
        session: session.to_owned(),
    }
}

fn leaf_session(pane: &Pane) -> Option<&str> {
    match pane.items.first() {
        Some(ItemRef::Terminal { session }) => Some(session),
        _ => None,
    }
}

impl TermSplit {
    /// One leaf showing `session`.
    pub fn single(session: &str) -> Self {
        let id = PaneId(1);
        Self {
            tree: PaneTree::Pane(Pane::new(id, vec![terminal(session)])),
            focus: id,
            zoomed: false,
        }
    }

    /// `(leaf, session)` in tree order.
    pub fn leaves(&self) -> Vec<(PaneId, &str)> {
        self.tree
            .panes()
            .into_iter()
            .filter_map(|pane| leaf_session(pane).map(|s| (pane.id, s)))
            .collect()
    }

    /// The sessions of the leaves, in tree order.
    pub fn sessions(&self) -> Vec<&str> {
        self.leaves().into_iter().map(|(_, s)| s).collect()
    }

    /// How many terminals the tab holds.
    pub fn len(&self) -> usize {
        self.leaves().len()
    }

    /// Never true for a tab the model holds (it has at least one leaf).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The session of leaf `id`.
    pub fn session_of(&self, id: PaneId) -> Option<&str> {
        self.tree.pane(id).and_then(leaf_session)
    }

    /// The session of the focused leaf.
    pub fn focused_session(&self) -> Option<&str> {
        self.session_of(self.focus)
    }

    /// The leaf showing `session`.
    pub fn leaf_of(&self, session: &str) -> Option<PaneId> {
        self.leaves()
            .into_iter()
            .find_map(|(id, s)| (s == session).then_some(id))
    }

    fn next_id(&self) -> PaneId {
        PaneId(self.tree.panes().iter().map(|p| p.id.0).max().unwrap_or(0) + 1)
    }

    /// Places a new terminal next to leaf `target` on `edge` and focuses it. Returns its leaf.
    pub fn split(&mut self, target: PaneId, edge: Edge, session: &str) -> Option<PaneId> {
        let id = self.next_id();
        let mut new = Some(Pane::new(id, vec![terminal(session)]));
        if !self.tree.split_at(target, edge, &mut new) {
            return None;
        }
        self.focus = id;
        self.zoomed = false;
        Some(id)
    }

    /// Removes leaf `id`; focus moves to the previous leaf (the next when it was the first).
    /// Refuses (returns `false`) when it is the only leaf or absent.
    pub fn remove_leaf(&mut self, id: PaneId) -> bool {
        let order: Vec<PaneId> = self.tree.panes().iter().map(|p| p.id).collect();
        let Some(at) = order.iter().position(|leaf| *leaf == id) else {
            return false;
        };
        if order.len() < 2 {
            return false;
        }
        let survivor = if at > 0 { order[at - 1] } else { order[1] };
        if let Some(pane) = self.tree.pane_mut(id) {
            pane.items.clear();
        }
        self.tree.normalize(survivor);
        if self.focus == id {
            self.focus = survivor;
        }
        if self.zoomed || self.len() < 2 {
            self.zoomed = false;
        }
        true
    }

    /// Restores the invariants of a split read from disk or edited: one terminal per leaf, every
    /// session once, leaf ids unique, a focus that exists. Returns how many terminals remain.
    pub fn normalize(&mut self) -> usize {
        let mut sessions: HashSet<String> = HashSet::new();
        for pane in self.tree.panes_mut() {
            let keep = pane.items.drain(..).find(|item| match item {
                ItemRef::Terminal { session } => sessions.insert(session.clone()),
                _ => false,
            });
            pane.items = keep.into_iter().collect();
            pane.active = 0;
        }
        let first = self.tree.panes().iter().map(|p| p.id).min();
        if let Some(first) = first {
            self.tree.normalize(first);
        }
        // Leaf ids unique inside the tab.
        let mut next = self.next_id().0;
        let mut seen = HashSet::new();
        for pane in self.tree.panes_mut() {
            if !seen.insert(pane.id) {
                pane.id = PaneId(next);
                next += 1;
                seen.insert(pane.id);
            }
        }
        let len = self.leaves().len();
        if self
            .tree
            .pane(self.focus)
            .is_none_or(|p| leaf_session(p).is_none())
            && let Some((first, _)) = self.leaves().first().copied()
        {
            self.focus = first;
        }
        if len < 2 {
            self.zoomed = false;
        }
        len
    }

    /// The leaf next to `from` in direction `dir`, by geometry.
    pub fn neighbor(&self, from: PaneId, dir: Direction) -> Option<PaneId> {
        neighbor_in(&self.tree.layout(Rect::UNIT), from, dir)
    }

    /// Moves the nearest divider of the focused leaf along `dir` by `delta` (a share of that
    /// split). Returns whether anything moved.
    pub fn resize(&mut self, dir: Direction, delta: f32) -> bool {
        self.tree.resize_leaf(self.focus, dir, delta)
    }

    /// The tab's ids of the leaves and their rectangles in the unit square.
    pub fn layout(&self) -> Vec<(PaneId, Rect)> {
        self.tree.layout(Rect::UNIT)
    }
}

/// Normalizes the terminal splits among `pane`'s tabs: a tab with one terminal left turns back
/// into a plain terminal tab, a tab without any goes. Keeps the active tab where it was.
pub(super) fn normalize_pane_splits(pane: &mut Pane) {
    if !pane
        .items
        .iter()
        .any(|item| matches!(item, ItemRef::TerminalSplit { .. }))
    {
        return;
    }
    let active = pane.active;
    let mut out: Vec<ItemRef> = Vec::with_capacity(pane.items.len());
    let mut new_active = None;
    for (index, item) in std::mem::take(&mut pane.items).into_iter().enumerate() {
        let item = match item {
            ItemRef::TerminalSplit { mut split } => match split.normalize() {
                0 => {
                    if index == active {
                        new_active = Some(out.len().saturating_sub(1));
                    }
                    continue;
                }
                1 => match split.sessions().first() {
                    Some(session) => terminal(session),
                    None => continue,
                },
                _ => ItemRef::TerminalSplit { split },
            },
            other => other,
        };
        if index == active {
            new_active = Some(out.len());
        }
        out.push(item);
    }
    pane.active = new_active.unwrap_or(0).min(out.len().saturating_sub(1));
    pane.items = out;
}

impl WorkbenchState {
    fn term_item_mut(&mut self, pane: PaneId, index: usize) -> Result<&mut ItemRef, ModelError> {
        let p = self
            .center
            .pane_mut(pane)
            .ok_or(ModelError::NoSuchPane(pane))?;
        let len = p.items.len();
        p.items
            .get_mut(index)
            .ok_or(ModelError::IndexOutOfRange { index, len })
    }

    /// The terminal splits of the active tab of the focused pane, with where they are.
    pub fn active_term_split(&self) -> Option<(PaneId, usize, &TermSplit)> {
        let FocusTarget::Pane(_) = self.focus else {
            return None;
        };
        let pane = self.active_pane()?;
        match pane.active_item()? {
            ItemRef::TerminalSplit { split } => Some((pane.id, pane.active, split)),
            _ => None,
        }
    }

    /// The first tab (in tree order) that shows `session`: a terminal or chat tab of it, or a
    /// tab of terminal splits one of which it is.
    pub fn find_session_tab(&self, session: &str) -> Option<(PaneId, usize)> {
        self.center.panes().into_iter().find_map(|pane| {
            pane.items
                .iter()
                .position(|item| item.shows_session(session))
                .map(|index| (pane.id, index))
        })
    }

    /// Splits terminal tab `index` of `pane` at `edge` of leaf `target` (the focused leaf when
    /// `None`), the new split showing `session`; a plain terminal tab becomes a tab of splits.
    /// The tab becomes active, the pane focused and the new split focused.
    pub fn split_terminal(
        &mut self,
        pane: PaneId,
        index: usize,
        target: Option<PaneId>,
        edge: Edge,
        session: &str,
    ) -> Result<PaneId, ModelError> {
        let item = self.term_item_mut(pane, index)?;
        let leaf = match item {
            ItemRef::Terminal { session: first } => {
                let mut split = TermSplit::single(first);
                let target = split.focus;
                let leaf = split
                    .split(target, edge, session)
                    .ok_or_else(|| ModelError::Invalid("no such split".to_owned()))?;
                *item = ItemRef::TerminalSplit {
                    split: Box::new(split),
                };
                leaf
            }
            ItemRef::TerminalSplit { split } => {
                let target = target.unwrap_or(split.focus);
                split
                    .split(target, edge, session)
                    .ok_or_else(|| ModelError::Invalid(format!("no such split: {target}")))?
            }
            _ => return Err(ModelError::Invalid("not a terminal tab".to_owned())),
        };
        if let Some(p) = self.center.pane_mut(pane) {
            p.active = index;
        }
        self.focus = FocusTarget::Pane(pane);
        if self.zoomed.is_some() {
            self.zoomed = Some(pane);
        }
        self.normalize();
        Ok(leaf)
    }

    /// Puts `closed` back where it was in the tab of splits `before` (the tab as it stood when
    /// `closed` was closed): the same place in the tree with the same shares, focused. Only when
    /// that tab is still there with exactly the other splits it had then, and `closed` shows
    /// nowhere; returns `false` otherwise and changes nothing.
    pub fn restore_term_split(&mut self, closed: &str, before: &TermSplit) -> bool {
        let Some(leaf) = before.leaf_of(closed) else {
            return false;
        };
        if self.find_session_tab(closed).is_some() {
            return false;
        }
        let mut others: Vec<&str> = before
            .sessions()
            .into_iter()
            .filter(|session| *session != closed)
            .collect();
        others.sort_unstable();
        let Some(first) = others.first() else {
            return false;
        };
        let Some((pane, index)) = self.find_session_tab(first) else {
            return false;
        };
        let now: Vec<&str> = match self.center.pane(pane).and_then(|p| p.items.get(index)) {
            Some(ItemRef::Terminal { session }) => vec![session.as_str()],
            Some(ItemRef::TerminalSplit { split }) => {
                let mut sessions = split.sessions();
                sessions.sort_unstable();
                sessions
            }
            _ => return false,
        };
        if now != others {
            return false;
        }
        let mut restored = before.clone();
        restored.zoomed = before.zoomed && before.focus == leaf;
        restored.focus = leaf;
        let Ok(item) = self.term_item_mut(pane, index) else {
            return false;
        };
        *item = ItemRef::TerminalSplit {
            split: Box::new(restored),
        };
        if let Some(p) = self.center.pane_mut(pane) {
            p.active = index;
        }
        self.focus = FocusTarget::Pane(pane);
        self.normalize();
        true
    }

    /// Gives the keyboard to the split of `session` in tab `index` of `pane`.
    pub fn focus_term_split(
        &mut self,
        pane: PaneId,
        index: usize,
        session: &str,
    ) -> Result<(), ModelError> {
        match self.term_item_mut(pane, index)? {
            ItemRef::TerminalSplit { split } => {
                let leaf = split
                    .leaf_of(session)
                    .ok_or_else(|| ModelError::Invalid(format!("no split of {session}")))?;
                split.focus = leaf;
                Ok(())
            }
            _ => Err(ModelError::Invalid("not a tab of splits".to_owned())),
        }
    }

    /// Moves the keyboard to the neighbouring split of the focused one. Returns whether it moved
    /// (`false` at the edge of the tab's splits, or when the focused tab has none).
    pub fn focus_term_neighbor(&mut self, dir: Direction) -> bool {
        let Some((pane, index, split)) = self.active_term_split() else {
            return false;
        };
        if split.zoomed {
            return false;
        }
        let Some(next) = split.neighbor(split.focus, dir) else {
            return false;
        };
        if let Ok(ItemRef::TerminalSplit { split }) = self.term_item_mut(pane, index) {
            split.focus = next;
            return true;
        }
        false
    }

    /// Keyboard resize of the focused split: its nearest divider along `dir` moves by `delta`.
    pub fn resize_term_split(&mut self, dir: Direction, delta: f32) -> bool {
        let Some((pane, index, split)) = self.active_term_split() else {
            return false;
        };
        if split.zoomed {
            return false;
        }
        match self.term_item_mut(pane, index) {
            Ok(ItemRef::TerminalSplit { split }) => split.resize(dir, delta),
            _ => false,
        }
    }

    /// Moves one divider of the split tree at `path` of the active tab of `pane`; each side keeps
    /// at least `min` of the pair (a share).
    pub fn set_term_divider(
        &mut self,
        pane: PaneId,
        path: &[usize],
        divider: usize,
        position: f32,
        min: f32,
    ) -> Result<(), ModelError> {
        let index = self
            .center
            .pane(pane)
            .ok_or(ModelError::NoSuchPane(pane))?
            .active;
        if let ItemRef::TerminalSplit { split } = self.term_item_mut(pane, index)?
            && split.tree.set_divider_min(path, divider, position, min)
        {
            return Ok(());
        }
        Err(ModelError::NoSuchSplit(path.to_vec()))
    }

    /// Shares the split at `path` of the active tab of `pane` equally among its children.
    pub fn equalize_term_split(&mut self, pane: PaneId, path: &[usize]) -> Result<(), ModelError> {
        let index = self
            .center
            .pane(pane)
            .ok_or(ModelError::NoSuchPane(pane))?
            .active;
        if let ItemRef::TerminalSplit { split } = self.term_item_mut(pane, index)?
            && split.tree.equalize_at(path)
        {
            return Ok(());
        }
        Err(ModelError::NoSuchSplit(path.to_vec()))
    }

    /// Zooms the focused split to fill the tab, or puts it back. Returns whether the focused tab
    /// has splits to zoom.
    pub fn toggle_term_zoom(&mut self) -> bool {
        let Some((pane, index, _)) = self.active_term_split() else {
            return false;
        };
        if let Ok(ItemRef::TerminalSplit { split }) = self.term_item_mut(pane, index) {
            split.zoomed = !split.zoomed;
            return true;
        }
        false
    }

    /// Drops the split showing `session` from its tab; the tab becomes a plain terminal when one
    /// remains. Returns whether a split was removed.
    pub fn remove_term_split(&mut self, session: &str) -> bool {
        let Some((pane, index)) = self.find_session_tab(session) else {
            return false;
        };
        let Ok(ItemRef::TerminalSplit { split }) = self.term_item_mut(pane, index) else {
            return false;
        };
        let Some(leaf) = split.leaf_of(session) else {
            return false;
        };
        let removed = split.remove_leaf(leaf);
        self.normalize();
        removed
    }

    /// Takes the split showing `session` out of its tab into a tab of its own, right after it.
    pub fn move_split_to_tab(&mut self, session: &str) -> Result<(), ModelError> {
        let (pane, index) = self
            .find_session_tab(session)
            .ok_or_else(|| ModelError::Invalid(format!("no tab shows {session}")))?;
        let ItemRef::TerminalSplit { split } = self.term_item_mut(pane, index)? else {
            return Err(ModelError::Invalid("not a tab of splits".to_owned()));
        };
        let leaf = split
            .leaf_of(session)
            .ok_or_else(|| ModelError::Invalid(format!("no split of {session}")))?;
        if !split.remove_leaf(leaf) {
            return Err(ModelError::Invalid("the only split".to_owned()));
        }
        // `normalize` may turn the remaining one back into a plain terminal; the new tab goes
        // right after wherever the old one is.
        self.normalize();
        if let Some(p) = self.center.pane_mut(pane) {
            let at = (index + 1).min(p.items.len());
            p.items.insert(at, terminal(session));
            p.active = at;
        }
        self.focus = FocusTarget::Pane(pane);
        self.normalize();
        Ok(())
    }

    /// Merges terminal tab `source` into tab `target` as a split at `edge` of leaf `leaf` (the
    /// focused one when `None`): the dropped terminal becomes a leaf of the target tab and the
    /// source tab goes. Only a plain terminal tab can be merged, and not into itself.
    pub fn merge_terminal_tab(
        &mut self,
        source: (PaneId, usize),
        target: (PaneId, usize),
        leaf: Option<PaneId>,
        edge: Edge,
    ) -> Result<(), ModelError> {
        if source == target {
            return Err(ModelError::Invalid("a tab cannot split itself".to_owned()));
        }
        let session = match self.term_item_mut(source.0, source.1)? {
            ItemRef::Terminal { session } => session.clone(),
            _ => return Err(ModelError::Invalid("only a terminal tab merges".to_owned())),
        };
        if !matches!(
            self.term_item_mut(target.0, target.1)?,
            ItemRef::Terminal { .. } | ItemRef::TerminalSplit { .. }
        ) {
            return Err(ModelError::Invalid("not a terminal tab".to_owned()));
        }
        // Remove the source first; the target's index shifts when it sits after it.
        let mut target = target;
        if let Some(p) = self.center.pane_mut(source.0) {
            p.items.remove(source.1);
            if p.active >= p.items.len() {
                p.active = p.items.len().saturating_sub(1);
            }
        }
        if target.0 == source.0 && target.1 > source.1 {
            target.1 -= 1;
        }
        let result = self.split_terminal(target.0, target.1, leaf, edge, &session);
        if result.is_err() {
            // Put the source back where it was.
            if let Some(p) = self.center.pane_mut(source.0) {
                p.items
                    .insert(source.1.min(p.items.len()), terminal(&session));
            }
        }
        self.normalize();
        // The source pane may have emptied and collapsed; keep the focus on the target's pane.
        if self.center.pane(target.0).is_some() {
            self.focus = FocusTarget::Pane(target.0);
        }
        result.map(|_| ())
    }

    /// Removes every tab that shows `session` as its only terminal or chat, and the split of it
    /// in a tab of splits. Returns whether anything went.
    pub fn remove_session_tabs(&mut self, session: &str) -> bool {
        let mut changed = false;
        while self.remove_term_split(session) {
            changed = true;
        }
        let tabs = |s: &Self| {
            s.center.panes().into_iter().find_map(|pane| {
                pane.items
                    .iter()
                    .position(|i| {
                        matches!(i, ItemRef::Terminal { session: s } | ItemRef::Chat { session: s } if s == session)
                    })
                    .map(|index| (pane.id, index))
            })
        };
        while let Some((pane, index)) = tabs(self) {
            if self.close_item(pane, index).is_err() {
                break;
            }
            changed = true;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::model::Axis;

    fn tab(session: &str) -> ItemRef {
        terminal(session)
    }

    /// A state whose only pane holds the terminal tab `a`.
    fn with_terminal(a: &str) -> (WorkbenchState, PaneId) {
        let mut state = WorkbenchState::default();
        let pane = state.active_pane_id();
        assert!(state.replace_item(&ItemRef::placeholder("Welcome"), tab(a)));
        (state, pane)
    }

    fn split_of(state: &WorkbenchState, pane: PaneId, index: usize) -> TermSplit {
        state
            .pane(pane)
            .and_then(|p| p.items.get(index))
            .and_then(|i| i.as_term_split())
            .cloned()
            .expect("a tab of splits")
    }

    #[test]
    fn splitting_a_terminal_makes_a_tab_of_splits_with_the_new_one_focused() {
        let (mut state, pane) = with_terminal("a");
        state
            .split_terminal(pane, 0, None, Edge::Right, "b")
            .unwrap();
        let split = split_of(&state, pane, 0);
        assert_eq!(split.sessions(), ["a", "b"]);
        assert_eq!(split.focused_session(), Some("b"));
        assert_eq!(
            state
                .active_item()
                .and_then(|i| i.session().map(str::to_owned)),
            Some("b".to_owned())
        );
        // Down splits the focused one again: a nested split.
        state
            .split_terminal(pane, 0, None, Edge::Bottom, "c")
            .unwrap();
        let split = split_of(&state, pane, 0);
        assert_eq!(split.sessions(), ["a", "b", "c"]);
        assert_eq!(split.focused_session(), Some("c"));
        assert!(matches!(
            &split.tree,
            PaneTree::Split { axis: Axis::Horizontal, children, .. } if children.len() == 2
        ));
        // One tab only: nothing else changed.
        assert_eq!(state.pane(pane).unwrap().items.len(), 1);
    }

    #[test]
    fn only_terminal_tabs_split() {
        let mut state = WorkbenchState::default();
        let pane = state.active_pane_id();
        assert!(
            state
                .split_terminal(pane, 0, None, Edge::Right, "b")
                .is_err()
        );
        assert!(
            state
                .split_terminal(pane, 9, None, Edge::Right, "b")
                .is_err()
        );
    }

    #[test]
    fn closing_splits_collapses_to_a_plain_terminal_and_the_tab_closes_with_the_last() {
        let (mut state, pane) = with_terminal("a");
        state
            .split_terminal(pane, 0, None, Edge::Right, "b")
            .unwrap();
        state
            .split_terminal(pane, 0, None, Edge::Right, "c")
            .unwrap();
        assert!(state.remove_term_split("c"));
        let split = split_of(&state, pane, 0);
        assert_eq!(split.sessions(), ["a", "b"]);
        assert_eq!(
            split.focused_session(),
            Some("b"),
            "focus goes to the previous split"
        );
        assert!(state.remove_term_split("b"));
        assert_eq!(state.pane(pane).unwrap().items, vec![tab("a")]);
        // The last one is the tab: it goes the way tabs go.
        assert!(!state.remove_term_split("a"));
        assert!(state.remove_session_tabs("a"));
        assert!(state.pane(pane).unwrap().items.is_empty());
    }

    #[test]
    fn a_session_is_found_in_its_split_and_its_tab_comes_to_the_front() {
        let (mut state, pane) = with_terminal("a");
        state
            .split_terminal(pane, 0, None, Edge::Right, "b")
            .unwrap();
        assert_eq!(state.find_session_tab("a"), Some((pane, 0)));
        assert_eq!(state.find_session_tab("b"), Some((pane, 0)));
        assert_eq!(state.find_session_tab("z"), None);
        state.focus_term_split(pane, 0, "a").unwrap();
        assert_eq!(split_of(&state, pane, 0).focused_session(), Some("a"));
        assert!(state.focus_term_split(pane, 0, "z").is_err());
    }

    #[test]
    fn focus_moves_by_geometry_and_stops_at_the_edge() {
        let (mut state, pane) = with_terminal("a");
        state
            .split_terminal(pane, 0, None, Edge::Right, "b")
            .unwrap();
        state
            .split_terminal(pane, 0, None, Edge::Bottom, "c")
            .unwrap();
        // a | (b over c); focus is on c.
        assert!(state.focus_term_neighbor(Direction::Up));
        assert_eq!(split_of(&state, pane, 0).focused_session(), Some("b"));
        assert!(state.focus_term_neighbor(Direction::Left));
        assert_eq!(split_of(&state, pane, 0).focused_session(), Some("a"));
        assert!(!state.focus_term_neighbor(Direction::Left), "the edge");
        assert!(!state.focus_term_neighbor(Direction::Up), "the edge");
        assert!(state.focus_term_neighbor(Direction::Right));
        assert_eq!(split_of(&state, pane, 0).focused_session(), Some("b"));
        // A tab without splits has nothing to focus.
        let (mut plain, _) = with_terminal("x");
        assert!(!plain.focus_term_neighbor(Direction::Right));
    }

    #[test]
    fn resize_moves_the_divider_of_the_focused_split() {
        let (mut state, pane) = with_terminal("a");
        state
            .split_terminal(pane, 0, None, Edge::Right, "b")
            .unwrap();
        state.focus_term_split(pane, 0, "a").unwrap();
        assert!(state.resize_term_split(Direction::Right, 0.1));
        let PaneTree::Split { ratios, .. } = split_of(&state, pane, 0).tree else {
            panic!("a split");
        };
        assert!((ratios[0] - 0.6).abs() < 1e-4, "{ratios:?}");
        // Vertical keys have no divider in a horizontal split.
        assert!(!state.resize_term_split(Direction::Down, 0.1));
    }

    #[test]
    fn a_closed_split_comes_back_in_its_place_with_its_share() {
        let (mut state, pane) = with_terminal("a");
        state
            .split_terminal(pane, 0, None, Edge::Right, "b")
            .unwrap();
        state
            .split_terminal(pane, 0, None, Edge::Bottom, "c")
            .unwrap();
        state.set_term_divider(pane, &[], 0, 0.3, 0.1).unwrap();
        state.focus_term_split(pane, 0, "b").unwrap();
        let before = split_of(&state, pane, 0);
        state.remove_session_tabs("b");
        assert!(state.find_session_tab("b").is_none());
        assert!(state.restore_term_split("b", &before));
        let after = split_of(&state, pane, 0);
        assert_eq!(after, before, "same tree, shares and focus");
        assert_eq!(state.find_session_tab("b"), Some((pane, 0)));

        // The tab of two: closing one turns it into a plain terminal; the restore splits again.
        let (mut state, pane) = with_terminal("a");
        state
            .split_terminal(pane, 0, None, Edge::Right, "b")
            .unwrap();
        let before = split_of(&state, pane, 0);
        state.remove_session_tabs("b");
        assert!(matches!(
            state.pane(pane).unwrap().items[0],
            ItemRef::Terminal { .. }
        ));
        assert!(state.restore_term_split("b", &before));
        assert_eq!(split_of(&state, pane, 0), before);
    }

    #[test]
    fn a_split_is_not_restored_into_a_tab_that_changed() {
        let (mut state, pane) = with_terminal("a");
        state
            .split_terminal(pane, 0, None, Edge::Right, "b")
            .unwrap();
        let before = split_of(&state, pane, 0);
        state.remove_session_tabs("b");
        // "a" went too: nothing to put it back next to.
        state.remove_session_tabs("a");
        assert!(!state.restore_term_split("b", &before));
        // Shown elsewhere already.
        let (mut state, pane) = with_terminal("a");
        state
            .split_terminal(pane, 0, None, Edge::Right, "b")
            .unwrap();
        let before = split_of(&state, pane, 0);
        assert!(!state.restore_term_split("b", &before), "b is still shown");
    }

    #[test]
    fn dividers_keep_a_minimum_and_equalize() {
        let (mut state, pane) = with_terminal("a");
        state
            .split_terminal(pane, 0, None, Edge::Right, "b")
            .unwrap();
        state.set_term_divider(pane, &[], 0, 0.99, 0.2).unwrap();
        let PaneTree::Split { ratios, .. } = split_of(&state, pane, 0).tree else {
            panic!("a split");
        };
        assert!((ratios[1] - 0.2).abs() < 1e-4, "{ratios:?}");
        state.equalize_term_split(pane, &[]).unwrap();
        let PaneTree::Split { ratios, .. } = split_of(&state, pane, 0).tree else {
            panic!("a split");
        };
        assert_eq!(ratios, vec![0.5, 0.5]);
        assert!(state.set_term_divider(pane, &[3], 0, 0.5, 0.1).is_err());
    }

    #[test]
    fn zoom_toggles_and_a_new_split_unzooms() {
        let (mut state, pane) = with_terminal("a");
        assert!(
            !state.toggle_term_zoom(),
            "nothing to zoom in a plain terminal"
        );
        state
            .split_terminal(pane, 0, None, Edge::Right, "b")
            .unwrap();
        assert!(state.toggle_term_zoom());
        assert!(split_of(&state, pane, 0).zoomed);
        assert!(
            !state.focus_term_neighbor(Direction::Left),
            "zoomed: one split is visible"
        );
        state
            .split_terminal(pane, 0, None, Edge::Bottom, "c")
            .unwrap();
        assert!(!split_of(&state, pane, 0).zoomed);
        state.toggle_term_zoom();
        assert!(state.remove_term_split("c"));
        assert!(
            !split_of(&state, pane, 0).zoomed,
            "closing the zoomed split unzooms"
        );
    }

    #[test]
    fn a_split_moves_to_a_tab_of_its_own() {
        let (mut state, pane) = with_terminal("a");
        state
            .split_terminal(pane, 0, None, Edge::Right, "b")
            .unwrap();
        state.move_split_to_tab("b").unwrap();
        let items = state.pane(pane).unwrap().items.clone();
        assert_eq!(
            items,
            vec![tab("a"), tab("b")],
            "the tab of one split is a plain terminal again"
        );
        assert_eq!(state.pane(pane).unwrap().active, 1);
        // A plain terminal has no split to move.
        assert!(state.move_split_to_tab("b").is_err());
        // With three the rest stay together.
        state
            .split_terminal(pane, 0, None, Edge::Right, "c")
            .unwrap();
        state
            .split_terminal(pane, 0, None, Edge::Right, "d")
            .unwrap();
        state.move_split_to_tab("c").unwrap();
        let items = state.pane(pane).unwrap().items.clone();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].sessions(), ["a", "d"]);
        assert_eq!(items[1], tab("c"));
    }

    #[test]
    fn a_terminal_tab_merges_into_another_as_a_split() {
        let (mut state, pane) = with_terminal("a");
        state.add_item(pane, tab("b")).unwrap();
        state.add_item(pane, tab("c")).unwrap();
        // Drop c on the right edge of a.
        state
            .merge_terminal_tab((pane, 2), (pane, 0), None, Edge::Right)
            .unwrap();
        let items = state.pane(pane).unwrap().items.clone();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].sessions(), ["a", "c"]);
        assert_eq!(items[1], tab("b"));
        // Drop b (before nothing) onto the tab of splits, at the bottom of its split c.
        state
            .merge_terminal_tab((pane, 1), (pane, 0), None, Edge::Bottom)
            .unwrap();
        let items = state.pane(pane).unwrap().items.clone();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].sessions(), ["a", "c", "b"]);
        assert!(
            state
                .merge_terminal_tab((pane, 0), (pane, 0), None, Edge::Right)
                .is_err()
        );
    }

    #[test]
    fn merging_never_loses_the_tab_when_the_target_is_not_a_terminal() {
        let (mut state, pane) = with_terminal("a");
        state.add_item(pane, ItemRef::placeholder("P")).unwrap();
        assert!(
            state
                .merge_terminal_tab((pane, 0), (pane, 1), None, Edge::Right)
                .is_err()
        );
        assert_eq!(state.pane(pane).unwrap().items.len(), 2);
    }

    #[test]
    fn a_merge_across_panes_collapses_the_emptied_pane() {
        let (mut state, left) = with_terminal("a");
        let right = state.split_pane(left, Edge::Right, tab("b")).unwrap();
        state
            .merge_terminal_tab((right, 0), (left, 0), None, Edge::Bottom)
            .unwrap();
        assert!(state.pane(right).is_none(), "the emptied pane collapsed");
        assert_eq!(state.center.panes().len(), 1);
        assert_eq!(state.pane(left).unwrap().items[0].sessions(), ["a", "b"]);
        assert_eq!(state.focus, FocusTarget::Pane(left));
    }

    #[test]
    fn layout_round_trips_with_splits_and_old_layouts_load_unchanged() {
        let (mut state, pane) = with_terminal("a");
        state
            .split_terminal(pane, 0, None, Edge::Right, "b")
            .unwrap();
        state
            .split_terminal(pane, 0, None, Edge::Bottom, "c")
            .unwrap();
        state.toggle_term_zoom();
        let json = serde_json::to_string(&state).unwrap();
        let back: WorkbenchState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, state);
        // Before splits a terminal tab was this and only this.
        let old = r#"{"terminal":{"session":"s1"}}"#;
        let item: ItemRef = serde_json::from_str(old).unwrap();
        assert_eq!(item, tab("s1"));
        assert_eq!(serde_json::to_string(&item).unwrap(), old);
    }

    #[test]
    fn a_damaged_split_is_repaired_on_load() {
        let (mut state, pane) = with_terminal("a");
        state
            .split_terminal(pane, 0, None, Edge::Right, "b")
            .unwrap();
        // Hand-edit: a duplicate session, a leaf that holds a chat, a focus that does not exist.
        if let Some(ItemRef::TerminalSplit { split }) =
            state.center.pane_mut(pane).and_then(|p| p.items.get_mut(0))
        {
            let mut leaves = split.tree.panes_mut();
            leaves[1].items = vec![ItemRef::Chat {
                session: "a".to_owned(),
            }];
            split.focus = PaneId(99);
        }
        state.normalize();
        // The chat is no terminal, so one terminal remains: a plain tab.
        assert_eq!(state.pane(pane).unwrap().items, vec![tab("a")]);

        let (mut state, pane) = with_terminal("a");
        state
            .split_terminal(pane, 0, None, Edge::Right, "b")
            .unwrap();
        if let Some(ItemRef::TerminalSplit { split }) =
            state.center.pane_mut(pane).and_then(|p| p.items.get_mut(0))
        {
            split.tree.panes_mut()[1].items = vec![tab("a")];
            split.focus = PaneId(99);
        }
        state.normalize();
        assert_eq!(
            state.pane(pane).unwrap().items,
            vec![tab("a")],
            "a session shows once"
        );
    }

    #[test]
    fn every_split_session_is_part_of_the_open_items() {
        let (mut state, pane) = with_terminal("a");
        state
            .split_terminal(pane, 0, None, Edge::Right, "b")
            .unwrap();
        let sessions: Vec<String> = state
            .all_items()
            .iter()
            .flat_map(|i| i.sessions().into_iter().map(str::to_owned))
            .collect();
        assert_eq!(sessions, ["a", "b"]);
    }
}
