//! Layout operations on [`WorkbenchState`].

use super::{
    Axis, Direction, DockPosition, Edge, FocusTarget, ItemRef, ModelError, Pane, PaneId, PaneTree,
    PanelId, Rect, WorkbenchState,
};

pub const MIN_DOCK_SIZE: f32 = 120.0;
pub const MAX_DOCK_SIZE: f32 = 1200.0;

/// Where a dragged/moved item currently lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemSource {
    Pane { pane: PaneId, index: usize },
    Dock { pos: DockPosition, panel: PanelId },
}

/// Where an item is dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropTarget {
    /// Insert into a pane's tab strip at `index` (before the tab currently at `index`).
    Pane { pane: PaneId, index: usize },
    /// Create a new pane at an edge of `pane`.
    Split { pane: PaneId, edge: Edge },
    /// Insert into a dock's panel stack. Only panels may be dropped here.
    Dock { pos: DockPosition, index: usize },
}

/// Destination for [`WorkbenchState::move_panel`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelDest {
    Dock(DockPosition),
    /// As a tab in the active pane ("Open as tab").
    Center,
}

/// Remove `index` from `pane.items`, keeping the active tab sensible.
fn remove_pane_item(pane: &mut Pane, index: usize) -> ItemRef {
    let item = pane.items.remove(index);
    if pane.items.is_empty() {
        pane.active = 0;
    } else if index < pane.active {
        pane.active -= 1;
    } else {
        pane.active = pane.active.min(pane.items.len() - 1);
    }
    item
}

impl WorkbenchState {
    // ---- focus & selection -------------------------------------------------------------

    pub fn focus_pane(&mut self, id: PaneId) -> Result<(), ModelError> {
        if self.center.pane(id).is_none() {
            return Err(ModelError::NoSuchPane(id));
        }
        self.focus = FocusTarget::Pane(id);
        if self.zoomed.is_some() {
            self.zoomed = Some(id);
        }
        Ok(())
    }

    pub fn focus_dock(&mut self, pos: DockPosition) {
        self.focus = FocusTarget::Dock(pos);
    }

    pub fn set_active_item(&mut self, pane: PaneId, index: usize) -> Result<(), ModelError> {
        let p = self
            .center
            .pane_mut(pane)
            .ok_or(ModelError::NoSuchPane(pane))?;
        if index >= p.items.len() {
            return Err(ModelError::IndexOutOfRange {
                index,
                len: p.items.len(),
            });
        }
        p.active = index;
        Ok(())
    }

    /// Activate the `n`-th (0-based) tab of the active pane; out of range is a no-op.
    pub fn select_tab(&mut self, n: usize) -> bool {
        let id = self.active_pane_id();
        match self.center.pane_mut(id) {
            Some(pane) if n < pane.items.len() => {
                pane.active = n;
                self.focus = FocusTarget::Pane(id);
                true
            }
            _ => false,
        }
    }

    /// Activate the next (`forward`) or previous tab, wrapping around, of whatever has the
    /// focus: a dock cycles its panels, else the active center pane its tabs. One tab or none
    /// is a no-op.
    pub fn cycle_tab(&mut self, forward: bool) -> bool {
        let step = |index: usize, len: usize| {
            if forward {
                (index + 1) % len
            } else {
                (index + len - 1) % len
            }
        };
        if let FocusTarget::Dock(pos) = self.focus {
            let dock = self.docks.get_mut(pos);
            let len = dock.panels.len();
            if len < 2 {
                return false;
            }
            let index = dock
                .active
                .as_ref()
                .and_then(|a| dock.panels.iter().position(|p| p == a))
                .unwrap_or(0);
            dock.active = Some(dock.panels[step(index, len)].clone());
            return true;
        }
        let id = self.active_pane_id();
        match self.center.pane_mut(id) {
            Some(pane) if pane.items.len() > 1 => {
                pane.active = step(pane.active.min(pane.items.len() - 1), pane.items.len());
                self.focus = FocusTarget::Pane(id);
                true
            }
            _ => false,
        }
    }

    pub fn set_dock_active(
        &mut self,
        pos: DockPosition,
        panel: &PanelId,
    ) -> Result<(), ModelError> {
        let dock = self.docks.get_mut(pos);
        if !dock.contains(panel) {
            return Err(ModelError::NoSuchPanel(panel.clone()));
        }
        dock.active = Some(panel.clone());
        Ok(())
    }

    /// Append `item` to `pane`, activate it, and focus the pane. Returns its tab index.
    pub fn add_item(&mut self, pane: PaneId, item: ItemRef) -> Result<usize, ModelError> {
        if let Some(panel) = item.as_panel().filter(|p| p.is_left_only()) {
            return Err(ModelError::LeftOnly(panel.clone()));
        }
        if let Some(panel) = item.as_panel()
            && self.find_panel(panel).is_some()
        {
            return Err(ModelError::Invalid(format!(
                "panel `{panel}` is already open"
            )));
        }
        let p = self
            .center
            .pane_mut(pane)
            .ok_or(ModelError::NoSuchPane(pane))?;
        p.items.push(item);
        p.active = p.items.len() - 1;
        let index = p.active;
        self.focus = FocusTarget::Pane(pane);
        if self.zoomed.is_some() {
            self.zoomed = Some(pane);
        }
        Ok(index)
    }

    /// The first tab (in tree order) showing exactly `item`, as `(pane, index)`.
    pub fn find_item(&self, item: &ItemRef) -> Option<(PaneId, usize)> {
        self.center.panes().into_iter().find_map(|pane| {
            pane.items
                .iter()
                .position(|i| i == item)
                .map(|index| (pane.id, index))
        })
    }

    /// Replace the first tab showing `old` with `new`, keeping its place, activation and focus.
    /// Returns whether `old` was found. Panels are never replaced (they must stay unique).
    pub fn replace_item(&mut self, old: &ItemRef, new: ItemRef) -> bool {
        if old.as_panel().is_some() || new.as_panel().is_some() {
            return false;
        }
        let Some((pane, index)) = self.find_item(old) else {
            return false;
        };
        match self
            .center
            .pane_mut(pane)
            .and_then(|p| p.items.get_mut(index))
        {
            Some(slot) => {
                *slot = new;
                true
            }
            None => false,
        }
    }

    // ---- splitting & moving ------------------------------------------------------------

    /// Split `pane` at `edge`; the new pane holds `item` and takes focus.
    pub fn split_pane(
        &mut self,
        pane: PaneId,
        edge: Edge,
        item: ItemRef,
    ) -> Result<PaneId, ModelError> {
        if self.center.pane(pane).is_none() {
            return Err(ModelError::NoSuchPane(pane));
        }
        if let Some(panel) = item.as_panel().filter(|p| p.is_left_only()) {
            return Err(ModelError::LeftOnly(panel.clone()));
        }
        if let Some(panel) = item.as_panel()
            && self.find_panel(panel).is_some()
        {
            return Err(ModelError::Invalid(format!(
                "panel `{panel}` is already open"
            )));
        }
        let id = self.alloc_pane_id();
        let mut new = Some(Pane::new(id, vec![item]));
        if !self.center.split_at(pane, edge, &mut new) {
            return Err(ModelError::NoSuchPane(pane));
        }
        self.zoomed = None;
        self.focus = FocusTarget::Pane(id);
        self.normalize();
        Ok(id)
    }

    /// Move an item between tab strips, into a new split, or into a dock (panels only).
    pub fn move_item(&mut self, source: ItemSource, target: DropTarget) -> Result<(), ModelError> {
        let item = match &source {
            ItemSource::Pane { pane, index } => {
                let p = self
                    .center
                    .pane(*pane)
                    .ok_or(ModelError::NoSuchPane(*pane))?;
                p.items
                    .get(*index)
                    .cloned()
                    .ok_or(ModelError::IndexOutOfRange {
                        index: *index,
                        len: p.items.len(),
                    })?
            }
            ItemSource::Dock { pos, panel } => {
                if !self.docks.get(*pos).contains(panel) {
                    return Err(ModelError::NoSuchPanel(panel.clone()));
                }
                ItemRef::Panel {
                    panel: panel.clone(),
                }
            }
        };
        // The left-only panels (the Assistant) stay pages of the left sidebar.
        if let Some(panel) = item.as_panel().filter(|p| p.is_left_only())
            && !matches!(
                &target,
                DropTarget::Dock {
                    pos: DockPosition::Left,
                    ..
                }
            )
        {
            return Err(ModelError::LeftOnly(panel.clone()));
        }
        match &target {
            DropTarget::Pane { pane, index } => {
                let p = self
                    .center
                    .pane(*pane)
                    .ok_or(ModelError::NoSuchPane(*pane))?;
                if *index > p.items.len() {
                    return Err(ModelError::IndexOutOfRange {
                        index: *index,
                        len: p.items.len(),
                    });
                }
            }
            DropTarget::Split { pane, .. } => {
                if self.center.pane(*pane).is_none() {
                    return Err(ModelError::NoSuchPane(*pane));
                }
            }
            DropTarget::Dock { pos, index } => {
                if item.as_panel().is_none() {
                    return Err(ModelError::NotAPanel);
                }
                let len = self.docks.get(*pos).panels.len();
                if *index > len {
                    return Err(ModelError::IndexOutOfRange { index: *index, len });
                }
            }
        }

        // Reorders inside one container and splitting a pane off its only tab.
        match (&source, &target) {
            (
                ItemSource::Pane {
                    pane: sp,
                    index: si,
                },
                DropTarget::Pane {
                    pane: tp,
                    index: ti,
                },
            ) if sp == tp => {
                if let Some(pane) = self.center.pane_mut(*sp) {
                    let moved = pane.items.remove(*si);
                    let at = if ti > si { ti - 1 } else { *ti };
                    pane.items.insert(at, moved);
                    pane.active = at;
                }
                self.focus = FocusTarget::Pane(*sp);
                return Ok(());
            }
            (ItemSource::Pane { pane: sp, .. }, DropTarget::Split { pane: tp, .. })
                if sp == tp && self.center.pane(*sp).is_some_and(|p| p.items.len() == 1) =>
            {
                return Ok(());
            }
            (ItemSource::Dock { pos: sp, panel }, DropTarget::Dock { pos: tp, index })
                if sp == tp =>
            {
                let dock = self.docks.get_mut(*sp);
                if let Some(from) = dock.panels.iter().position(|p| p == panel) {
                    let moved = dock.panels.remove(from);
                    let at = if *index > from { index - 1 } else { *index };
                    dock.panels.insert(at, moved);
                    dock.active = dock.panels.get(at).cloned();
                }
                self.focus = FocusTarget::Dock(*sp);
                return Ok(());
            }
            _ => {}
        }

        // Detach from the source.
        match &source {
            ItemSource::Pane { pane, index } => {
                if let Some(p) = self.center.pane_mut(*pane) {
                    remove_pane_item(p, *index);
                }
            }
            ItemSource::Dock { pos, panel } => {
                let dock = self.docks.get_mut(*pos);
                dock.panels.retain(|p| p != panel);
                if dock.active.as_ref() == Some(panel) {
                    dock.active = dock.panels.first().cloned();
                }
            }
        }

        // Attach to the target.
        let dest_pane = match target {
            DropTarget::Pane { pane, index } => {
                if let Some(p) = self.center.pane_mut(pane) {
                    p.items.insert(index, item);
                    p.active = index;
                }
                self.focus = FocusTarget::Pane(pane);
                Some(pane)
            }
            DropTarget::Split { pane, edge } => {
                let id = self.alloc_pane_id();
                let mut new = Some(Pane::new(id, vec![item]));
                self.center.split_at(pane, edge, &mut new);
                self.focus = FocusTarget::Pane(id);
                Some(id)
            }
            DropTarget::Dock { pos, index } => {
                if let Some(panel) = item.as_panel().cloned() {
                    let dock = self.docks.get_mut(pos);
                    dock.panels.insert(index, panel.clone());
                    dock.active = Some(panel);
                    dock.visible = true;
                }
                self.focus = FocusTarget::Dock(pos);
                None
            }
        };
        if self.zoomed.is_some() && self.zoomed != dest_pane {
            self.zoomed = None;
        }
        self.normalize();
        Ok(())
    }

    // ---- panels ------------------------------------------------------------------------

    /// Where `panel` is open, if anywhere.
    pub fn find_panel(&self, panel: &PanelId) -> Option<ItemSource> {
        if let Some(pos) = self.docks.find(panel) {
            return Some(ItemSource::Dock {
                pos,
                panel: panel.clone(),
            });
        }
        self.center.panes().into_iter().find_map(|pane| {
            pane.items
                .iter()
                .position(|i| i.as_panel() == Some(panel))
                .map(|index| ItemSource::Pane {
                    pane: pane.id,
                    index,
                })
        })
    }

    /// Move an open panel to a dock or into the center. No-op if it is already there.
    pub fn move_panel(&mut self, panel: &PanelId, dest: PanelDest) -> Result<(), ModelError> {
        let source = self
            .find_panel(panel)
            .ok_or_else(|| ModelError::NoSuchPanel(panel.clone()))?;
        match dest {
            PanelDest::Dock(pos) => {
                if matches!(&source, ItemSource::Dock { pos: p, .. } if *p == pos) {
                    return Ok(());
                }
                let index = self.docks.get(pos).panels.len();
                self.move_item(source, DropTarget::Dock { pos, index })
            }
            PanelDest::Center => {
                if matches!(source, ItemSource::Pane { .. }) {
                    return Ok(());
                }
                let pane = self.active_pane_id();
                let index = self.center.pane(pane).map_or(0, |p| p.items.len());
                self.move_item(source, DropTarget::Pane { pane, index })
            }
        }
    }

    /// Show `panel`: reveal and focus it where it is open, otherwise open it in `default_dock`
    /// (the left dock for a left-only panel, whatever the caller says).
    pub fn open_panel(&mut self, panel: &PanelId, default_dock: DockPosition) {
        let default_dock = if panel.is_left_only() {
            DockPosition::Left
        } else {
            default_dock
        };
        match self.find_panel(panel) {
            Some(ItemSource::Dock { pos, .. }) => {
                let dock = self.docks.get_mut(pos);
                dock.visible = true;
                dock.active = Some(panel.clone());
                self.focus = FocusTarget::Dock(pos);
            }
            Some(ItemSource::Pane { pane, index }) => {
                if let Some(p) = self.center.pane_mut(pane) {
                    p.active = index;
                }
                self.focus = FocusTarget::Pane(pane);
            }
            None => {
                let dock = self.docks.get_mut(default_dock);
                dock.panels.push(panel.clone());
                dock.active = Some(panel.clone());
                dock.visible = true;
                self.focus = FocusTarget::Dock(default_dock);
            }
        }
        self.normalize();
    }

    /// Close a panel wherever it is open.
    pub fn close_panel(&mut self, panel: &PanelId) -> Result<(), ModelError> {
        match self
            .find_panel(panel)
            .ok_or_else(|| ModelError::NoSuchPanel(panel.clone()))?
        {
            ItemSource::Dock { pos, panel } => {
                let dock = self.docks.get_mut(pos);
                dock.panels.retain(|p| *p != panel);
                if dock.active.as_ref() == Some(&panel) {
                    dock.active = dock.panels.first().cloned();
                }
                self.normalize();
                Ok(())
            }
            ItemSource::Pane { pane, index } => self.close_item(pane, index).map(|_| ()),
        }
    }

    // ---- closing -----------------------------------------------------------------------

    /// Close a tab. An emptied pane collapses (its split flattens, ratios renormalize) unless it
    /// is the only pane, which stays as an empty root.
    pub fn close_item(&mut self, pane: PaneId, index: usize) -> Result<ItemRef, ModelError> {
        let order: Vec<PaneId> = self.center.panes().iter().map(|p| p.id).collect();
        let p = self
            .center
            .pane_mut(pane)
            .ok_or(ModelError::NoSuchPane(pane))?;
        if index >= p.items.len() {
            return Err(ModelError::IndexOutOfRange {
                index,
                len: p.items.len(),
            });
        }
        let item = remove_pane_item(p, index);
        self.normalize();
        if self.center.pane(pane).is_none() && self.focus == FocusTarget::Pane(pane) {
            // Focus the nearest surviving pane in tree order.
            let at = order.iter().position(|id| *id == pane).unwrap_or(0);
            let survivor = order[..at]
                .iter()
                .rev()
                .chain(order[at + 1..].iter())
                .find(|id| self.center.pane(**id).is_some())
                .copied()
                .unwrap_or_else(|| self.first_pane_id());
            self.focus = FocusTarget::Pane(survivor);
        }
        Ok(item)
    }

    /// Close what has focus: the active tab of the focused pane or the active panel of the
    /// focused dock. Returns the closed item.
    pub fn close_active_item(&mut self) -> Option<ItemRef> {
        match self.focus {
            FocusTarget::Dock(pos) => {
                let panel = self.docks.get(pos).active.clone()?;
                self.close_panel(&panel).ok()?;
                Some(ItemRef::Panel { panel })
            }
            FocusTarget::Pane(_) => {
                let id = self.active_pane_id();
                let active = self.center.pane(id)?.active;
                if self.center.pane(id)?.items.is_empty() {
                    return None;
                }
                self.close_item(id, active).ok()
            }
        }
    }

    // ---- docks -------------------------------------------------------------------------

    pub fn set_dock_visible(&mut self, pos: DockPosition, visible: bool) {
        self.docks.get_mut(pos).visible = visible;
        if !visible && self.focus == FocusTarget::Dock(pos) {
            self.focus = FocusTarget::Pane(self.active_pane_id());
        }
    }

    pub fn toggle_dock(&mut self, pos: DockPosition) {
        let visible = !self.docks.get(pos).visible;
        self.set_dock_visible(pos, visible);
    }

    /// Hides every dock (⌘⌥Y); focus moves to the active pane.
    pub fn close_all_docks(&mut self) {
        for pos in DockPosition::ALL {
            self.set_dock_visible(pos, false);
        }
    }

    pub fn resize_dock(&mut self, pos: DockPosition, size: f32) {
        if size.is_finite() {
            self.docks.get_mut(pos).size = size.clamp(MIN_DOCK_SIZE, MAX_DOCK_SIZE);
        }
    }

    // ---- resizing splits ---------------------------------------------------------------

    /// Move one divider of the split at `path` (see [`PaneTree::set_divider`]).
    pub fn set_divider(
        &mut self,
        path: &[usize],
        divider: usize,
        position: f32,
    ) -> Result<(), ModelError> {
        if self.center.set_divider(path, divider, position) {
            Ok(())
        } else {
            Err(ModelError::NoSuchSplit(path.to_vec()))
        }
    }

    /// Move the divider of the nearest ancestor split along `dir`'s axis one step in `dir`.
    /// `delta` is a fraction of that split. Returns whether anything moved.
    pub fn resize_pane(&mut self, pane: PaneId, dir: Direction, delta: f32) -> bool {
        let Some(path) = self.center.path_to(pane) else {
            return false;
        };
        let axis: Axis = dir.axis();
        for depth in (0..path.len()).rev() {
            let split_path = &path[..depth];
            let child = path[depth];
            let Some(PaneTree::Split {
                axis: a, ratios, ..
            }) = self.center.node_at(split_path)
            else {
                continue;
            };
            if *a != axis {
                continue;
            }
            let n = ratios.len();
            let divider = if dir.is_positive() {
                if child + 1 < n {
                    child
                } else if child > 0 {
                    child - 1
                } else {
                    continue;
                }
            } else if child > 0 {
                child - 1
            } else if child + 1 < n {
                child
            } else {
                continue;
            };
            let before: f32 = ratios[..=divider].iter().sum();
            let step = if dir.is_positive() { delta } else { -delta };
            return self.center.set_divider(split_path, divider, before + step);
        }
        false
    }

    /// Keyboard resize of whatever has focus: a pane moves its nearest divider, a dock changes
    /// size (a left dock grows to the right, a right dock grows to the left, bottom grows up).
    pub fn resize_focused(&mut self, dir: Direction, dock_step: f32, split_step: f32) -> bool {
        match self.focus {
            FocusTarget::Pane(_) => {
                let id = self.active_pane_id();
                self.resize_pane(id, dir, split_step)
            }
            FocusTarget::Dock(pos) => {
                let grow = match (pos, dir) {
                    (DockPosition::Left, Direction::Right)
                    | (DockPosition::Right, Direction::Left)
                    | (DockPosition::Bottom, Direction::Up) => dock_step,
                    (DockPosition::Left, Direction::Left)
                    | (DockPosition::Right, Direction::Right)
                    | (DockPosition::Bottom, Direction::Down) => -dock_step,
                    _ => return false,
                };
                let size = self.docks.get(pos).size;
                self.resize_dock(pos, size + grow);
                true
            }
        }
    }

    // ---- zoom --------------------------------------------------------------------------

    pub fn zoom(&mut self, pane: PaneId) -> Result<(), ModelError> {
        if self.center.pane(pane).is_none() {
            return Err(ModelError::NoSuchPane(pane));
        }
        self.zoomed = Some(pane);
        self.focus = FocusTarget::Pane(pane);
        Ok(())
    }

    pub fn unzoom(&mut self) {
        self.zoomed = None;
    }

    /// Toggle zoom on the active pane.
    pub fn toggle_zoom(&mut self) {
        if self.zoomed.is_some() {
            self.zoomed = None;
        } else {
            let id = self.active_pane_id();
            let _ = self.zoom(id);
        }
    }

    // ---- geometric focus ---------------------------------------------------------------

    /// Move focus to the geometrically adjacent pane or visible, non-empty dock. Returns whether
    /// focus changed. While a pane is zoomed there is nothing to navigate to.
    pub fn focus_neighbor(&mut self, dir: Direction) -> bool {
        const EPS: f32 = 1e-4;
        if self.zoomed.is_some() {
            return false;
        }
        let layout = self.center.layout(Rect::UNIT);
        let dock_ready = |state: &Self, pos: DockPosition| {
            let dock = state.docks.get(pos);
            dock.visible && !dock.panels.is_empty()
        };
        match self.focus {
            FocusTarget::Pane(_) => {
                let id = self.active_pane_id();
                let Some(cur) = layout.iter().find(|(p, _)| *p == id).map(|(_, r)| *r) else {
                    return false;
                };
                // Nearest along the axis, then most overlap, then top/left-most. Quantized so
                // float noise from ratio arithmetic cannot reorder ties.
                let quant = |v: f32| (v * 1e4).round() as i64;
                let mut best: Option<((i64, i64, i64), PaneId)> = None;
                for (pid, r) in &layout {
                    if *pid == id {
                        continue;
                    }
                    let (gap, overlap, across) = match dir {
                        Direction::Right => {
                            (r.x - (cur.x + cur.w), overlap(cur.y, cur.h, r.y, r.h), r.y)
                        }
                        Direction::Left => {
                            (cur.x - (r.x + r.w), overlap(cur.y, cur.h, r.y, r.h), r.y)
                        }
                        Direction::Down => {
                            (r.y - (cur.y + cur.h), overlap(cur.x, cur.w, r.x, r.w), r.x)
                        }
                        Direction::Up => {
                            (cur.y - (r.y + r.h), overlap(cur.x, cur.w, r.x, r.w), r.x)
                        }
                    };
                    if gap < -EPS || overlap <= EPS {
                        continue;
                    }
                    let key = (quant(gap), -quant(overlap), quant(across));
                    if best.as_ref().is_none_or(|(k, _)| key < *k) {
                        best = Some((key, *pid));
                    }
                }
                if let Some((_, pid)) = best {
                    self.focus = FocusTarget::Pane(pid);
                    return true;
                }
                let dock = match dir {
                    Direction::Left => Some(DockPosition::Left),
                    Direction::Right => Some(DockPosition::Right),
                    Direction::Down => Some(DockPosition::Bottom),
                    Direction::Up => None,
                };
                // Only leave the center through an edge the pane actually touches.
                let touches = match dir {
                    Direction::Left => cur.x <= EPS,
                    Direction::Right => cur.x + cur.w >= 1.0 - EPS,
                    Direction::Down => cur.y + cur.h >= 1.0 - EPS,
                    Direction::Up => false,
                };
                match dock {
                    Some(pos) if touches && dock_ready(self, pos) => {
                        self.focus = FocusTarget::Dock(pos);
                        true
                    }
                    _ => false,
                }
            }
            FocusTarget::Dock(pos) => {
                let pick = |select: &dyn Fn(&Rect) -> bool, order: &dyn Fn(&Rect) -> f32| {
                    layout
                        .iter()
                        .filter(|(_, r)| select(r))
                        .min_by(|a, b| order(&a.1).total_cmp(&order(&b.1)))
                        .map(|(id, _)| *id)
                };
                let target = match (pos, dir) {
                    (DockPosition::Left, Direction::Right) => pick(&|r| r.x <= EPS, &|r| r.y),
                    (DockPosition::Right, Direction::Left) => {
                        pick(&|r| r.x + r.w >= 1.0 - EPS, &|r| r.y)
                    }
                    (DockPosition::Bottom, Direction::Up) => {
                        pick(&|r| r.y + r.h >= 1.0 - EPS, &|r| r.x)
                    }
                    (DockPosition::Left | DockPosition::Right, Direction::Down)
                        if dock_ready(self, DockPosition::Bottom) =>
                    {
                        self.focus = FocusTarget::Dock(DockPosition::Bottom);
                        return true;
                    }
                    _ => None,
                };
                match target {
                    Some(id) => {
                        self.focus = FocusTarget::Pane(id);
                        true
                    }
                    None => false,
                }
            }
        }
    }
}

/// Length of the overlap of `[a, a + al]` and `[b, b + bl]`.
fn overlap(a: f32, al: f32, b: f32, bl: f32) -> f32 {
    (a + al).min(b + bl) - a.max(b)
}
