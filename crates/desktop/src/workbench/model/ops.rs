//! Layout operations on [`WorkbenchState`].

use super::{
    Axis, Direction, DockMode, DockPosition, Edge, FocusTarget, ItemRef, ModelError, Pane, PaneId,
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

/// The path `path` has after `from` became `to`: `to` itself, what is below `from` moved below
/// `to`, anything else `None`.
pub fn retargeted(path: &str, from: &str, to: &str) -> Option<String> {
    if path == from {
        return Some(to.to_owned());
    }
    let rest = path.strip_prefix(from)?.strip_prefix('/')?;
    Some(format!("{}/{rest}", to.trim_end_matches('/')))
}

/// Points the file tabs of `center` — and the Markdown previews of files — at `from` (and below
/// it) at `to`. Only file tabs are reported in `moved` (their buffers are re-keyed; a preview
/// reads the buffer of its file).
fn retarget_in(
    center: &mut super::PaneTree,
    from: &str,
    to: &str,
    mut moved: Option<&mut Vec<(String, String)>>,
) {
    for pane in center.panes_mut() {
        for item in &mut pane.items {
            match item {
                ItemRef::File { path } => {
                    if let Some(new) = retargeted(path, from, to) {
                        if let Some(moved) = moved.as_deref_mut() {
                            moved.push((std::mem::replace(path, new.clone()), new));
                        } else {
                            *path = new;
                        }
                    }
                }
                ItemRef::Preview { path } => {
                    if let Some(new) = retargeted(path, from, to) {
                        *path = new;
                    }
                }
                _ => {}
            }
        }
    }
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

    /// A file or folder was renamed or moved (EX-7): every file tab at `from`, or below it when
    /// it is a folder, now shows the same place under `to`, keeping its position and activation.
    /// The live layout, the overview's stash and the layouts of the workspaces that are not shown
    /// all follow. Returns the `(old, new)` path of every tab that moved in the live layout.
    pub fn retarget_files(&mut self, from: &str, to: &str) -> Vec<(String, String)> {
        let mut moved = Vec::new();
        retarget_in(&mut self.center, from, to, Some(&mut moved));
        if let Some(saved) = self.stashed.as_deref_mut() {
            retarget_in(&mut saved.center, from, to, None);
        }
        for parked in self.parked.values_mut() {
            retarget_in(&mut parked.layout.center, from, to, None);
            if let Some(saved) = parked.stashed.as_deref_mut() {
                retarget_in(&mut saved.center, from, to, None);
            }
        }
        moved
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

    /// After `pane` was split at a left or right edge into `new`: the two keep at least `min`
    /// points of the `center_width` the center has, taken from the other panes beside them (a
    /// side view that is a sliver is no view). Nothing else changes.
    pub fn make_room_beside(&mut self, pane: PaneId, new: PaneId, center_width: f32, min: f32) {
        self.center
            .fit_pair([pane, new], Axis::Horizontal, center_width, min);
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
                    dock.set_mode(DockMode::Expanded);
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

    /// Show `panel`: reveal and focus it where it is open, otherwise open it in `default_dock`.
    pub fn open_panel(&mut self, panel: &PanelId, default_dock: DockPosition) {
        match self.find_panel(panel) {
            Some(ItemSource::Dock { pos, .. }) => {
                let dock = self.docks.get_mut(pos);
                dock.set_mode(DockMode::Expanded);
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
                dock.set_mode(DockMode::Expanded);
                self.focus = FocusTarget::Dock(default_dock);
            }
        }
        self.normalize();
    }

    /// Show `panel` in the dock `pos` and nowhere else: a panel that is open in another dock or
    /// as a tab of the center moves there first (Memory is a body of the left sidebar however
    /// the user arranged it before), then it is revealed and focused like [`Self::open_panel`].
    pub fn open_panel_in_dock(&mut self, panel: &PanelId, pos: DockPosition) {
        let elsewhere = match self.find_panel(panel) {
            Some(ItemSource::Dock { pos: at, .. }) => at != pos,
            Some(ItemSource::Pane { .. }) => true,
            None => false,
        };
        if elsewhere {
            // Moving an open panel to a dock cannot fail: the panel was just found.
            let _ = self.move_panel(panel, PanelDest::Dock(pos));
        }
        self.open_panel(panel, pos);
    }

    /// Show `panel` as a tab of the active pane: reveal and focus it where it is open (a dock or
    /// a tab), otherwise open it as the active pane's last tab, which becomes the active one.
    /// For a panel that is a page of the center, like Mawas Home.
    pub fn open_panel_in_center(&mut self, panel: &PanelId) {
        if self.find_panel(panel).is_some() {
            self.open_panel(panel, DockPosition::Left);
            return;
        }
        let pane = self.active_pane_id();
        if self
            .add_item(
                pane,
                ItemRef::Panel {
                    panel: panel.clone(),
                },
            )
            .is_ok()
        {
            self.normalize();
        }
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

    /// Puts dock `pos` in `mode`. A mode that is not [`DockMode::Expanded`] takes keyboard focus
    /// back from the dock to the active pane, so nothing hidden keeps it. The bottom dock has no
    /// rail: its `Compact` is `Hidden`. Collapsing never touches the dock's panels, the center,
    /// sessions or approvals.
    pub fn set_dock_mode(&mut self, pos: DockPosition, mode: DockMode) {
        let mode = match (pos, mode) {
            (DockPosition::Bottom, DockMode::Compact) => DockMode::Hidden,
            _ => mode,
        };
        self.docks.get_mut(pos).set_mode(mode);
        if mode != DockMode::Expanded && self.focus == FocusTarget::Dock(pos) {
            self.focus = FocusTarget::Pane(self.active_pane_id());
        }
    }

    /// The working toggle of a dock (⌘B, ⌘L, ⌘⌥J, the status bar buttons):
    ///
    /// | dock | Expanded | Compact | Hidden |
    /// |---|---|---|---|
    /// | left | → Compact | → Expanded | → the mode it was hidden from |
    /// | right | → Compact | → Expanded | → Expanded |
    /// | bottom | → Hidden | — | → Expanded |
    pub fn toggle_dock(&mut self, pos: DockPosition) {
        let dock = self.docks.get(pos);
        let next = match (pos, dock.mode()) {
            (DockPosition::Bottom, DockMode::Expanded) => DockMode::Hidden,
            (_, DockMode::Expanded) => DockMode::Compact,
            (_, DockMode::Compact) => DockMode::Expanded,
            (DockPosition::Left, DockMode::Hidden) => dock.last_shown(),
            (_, DockMode::Hidden) => DockMode::Expanded,
        };
        self.set_dock_mode(pos, next);
    }

    /// The titlebar's sidebar button: hides a shown dock, brings a hidden one back in the mode it
    /// was hidden from.
    pub fn toggle_dock_hidden(&mut self, pos: DockPosition) {
        let dock = self.docks.get(pos);
        let next = if dock.mode() == DockMode::Hidden {
            dock.last_shown()
        } else {
            DockMode::Hidden
        };
        self.set_dock_mode(pos, next);
    }

    /// Release of a drag of a side dock's edge past its minimum: the dock collapses to its rail.
    /// The expanded size stays what it was before the drag.
    pub fn collapse_dock(&mut self, pos: DockPosition) {
        if pos != DockPosition::Bottom {
            self.set_dock_mode(pos, DockMode::Compact);
        }
    }

    /// A click on the icon of `panel` in a dock's rail, `shown` being what the window draws of
    /// the dock (the user's mode, unless a narrow window collapsed it). The icon of the panel an
    /// expanded dock shows collapses the dock to its rail (focus goes to the center); any other
    /// icon makes `panel` the active one and expands the dock, focusing it. Returns whether the
    /// dock is expanded afterwards.
    pub fn click_rail_icon(&mut self, pos: DockPosition, panel: &PanelId, shown: DockMode) -> bool {
        let dock = self.docks.get(pos);
        if !dock.contains(panel) {
            return dock.is_expanded();
        }
        if shown == DockMode::Expanded && dock.active.as_ref() == Some(panel) {
            self.set_dock_mode(pos, DockMode::Compact);
            return false;
        }
        let dock = self.docks.get_mut(pos);
        dock.active = Some(panel.clone());
        self.set_dock_mode(pos, DockMode::Expanded);
        self.focus = FocusTarget::Dock(pos);
        true
    }

    /// Hides every dock (⌘⌥Y); focus moves to the active pane. Each keeps the mode it comes back in.
    pub fn close_all_docks(&mut self) {
        for pos in DockPosition::ALL {
            self.set_dock_mode(pos, DockMode::Hidden);
        }
    }

    /// Sets the expanded size of `pos`, clamped to [`DockPosition::size_range`].
    pub fn resize_dock(&mut self, pos: DockPosition, size: f32) {
        if size.is_finite() {
            let (min, max) = pos.size_range();
            self.docks.get_mut(pos).size = size.clamp(min, max);
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
        self.center.resize_leaf(pane, dir, delta)
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

    /// Every split of the center shares its space equally (Vim's `⌃W =`).
    pub fn equalize_panes(&mut self) {
        self.center.equalize();
    }

    /// The pane of the center that is geometrically next to `pane` in `dir` (the one that
    /// overlaps it most, the top-most on a tie), if there is one.
    pub fn pane_beside(&self, pane: PaneId, dir: Direction) -> Option<PaneId> {
        neighbor_in(&self.center.layout(Rect::UNIT), pane, dir)
    }

    /// Focus moves to the next pane of the center (`forward`, wrapping) or the previous one, in
    /// the order of the layout (Vim's `⌃W w` / `⌃W W`). Returns whether focus changed.
    pub fn cycle_pane(&mut self, forward: bool) -> bool {
        if self.zoomed.is_some() {
            return false;
        }
        let ids: Vec<PaneId> = self.center.panes().iter().map(|p| p.id).collect();
        if ids.len() < 2 {
            return false;
        }
        let current = self.active_pane_id();
        let at = ids.iter().position(|id| *id == current).unwrap_or(0);
        let next = if forward {
            (at + 1) % ids.len()
        } else {
            (at + ids.len() - 1) % ids.len()
        };
        self.focus_pane(ids[next]).is_ok()
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
            dock.is_expanded() && !dock.panels.is_empty()
        };
        match self.focus {
            FocusTarget::Pane(_) => {
                let id = self.active_pane_id();
                let Some(cur) = layout.iter().find(|(p, _)| *p == id).map(|(_, r)| *r) else {
                    return false;
                };
                if let Some(pid) = neighbor_in(&layout, id, dir) {
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

/// The leaf of `layout` next to `id` in direction `dir`: nearest along the axis, then the most
/// overlap, then the top/left-most. Quantized so float noise from ratio arithmetic cannot
/// reorder ties.
pub(super) fn neighbor_in(layout: &[(PaneId, Rect)], id: PaneId, dir: Direction) -> Option<PaneId> {
    const EPS: f32 = 1e-4;
    let cur = layout.iter().find(|(p, _)| *p == id).map(|(_, r)| *r)?;
    let quant = |v: f32| (v * 1e4).round() as i64;
    let mut best: Option<((i64, i64, i64), PaneId)> = None;
    for (pid, r) in layout {
        if *pid == id {
            continue;
        }
        let (gap, overlap, across) = match dir {
            Direction::Right => (r.x - (cur.x + cur.w), overlap(cur.y, cur.h, r.y, r.h), r.y),
            Direction::Left => (cur.x - (r.x + r.w), overlap(cur.y, cur.h, r.y, r.h), r.y),
            Direction::Down => (r.y - (cur.y + cur.h), overlap(cur.x, cur.w, r.x, r.w), r.x),
            Direction::Up => (cur.y - (r.y + r.h), overlap(cur.x, cur.w, r.x, r.w), r.x),
        };
        if gap < -EPS || overlap <= EPS {
            continue;
        }
        let key = (quant(gap), -quant(overlap), quant(across));
        if best.as_ref().is_none_or(|(k, _)| key < *k) {
            best = Some((key, *pid));
        }
    }
    best.map(|(_, pid)| pid)
}
