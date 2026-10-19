//! GPUI-free layout model: docks, pane tree, focus, zoom, presets, persistence.
//!
//! Every mutator leaves the state satisfying the invariants documented on [`PaneTree`] (no empty
//! panes except a lone root pane, flat same-axis splits, ratios summing to 1, every panel placed at
//! most once). The GPUI shell only calls these methods; it never edits the tree directly.

mod ops;
mod persist;
mod preset;
mod responsive;
mod tree;
mod types;
mod workspace;

#[cfg(test)]
mod tests;

pub use ops::{DropTarget, ItemSource, MAX_DOCK_SIZE, MIN_DOCK_SIZE, PanelDest};
pub use persist::{LoadOutcome, STATE_VERSION};
pub use preset::{CenterLayout, LayoutPreset};
pub use responsive::{
    AUTO_COLLAPSE_HYSTERESIS, AUTO_COLLAPSE_LEFT_BELOW, AUTO_COLLAPSE_RIGHT_BELOW, AutoState,
    MIN_CENTER_WIDTH, Responsive,
};
pub use tree::MIN_RATIO;
pub use types::{
    Axis, Direction, Dock, DockMode, DockModes, DockPosition, Docks, Edge, FocusTarget, ItemRef,
    ModelError, Pane, PaneId, PaneTree, PanelId, Rect, RememberedDocks,
};
pub use workspace::WorkspaceLayout;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

/// Everything the workbench persists and presets rewrite.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkbenchState {
    pub docks: Docks,
    pub center: PaneTree,
    pub zoomed: Option<PaneId>,
    pub focus: FocusTarget,
    /// Name of the most recently applied preset.
    #[serde(default = "default_preset_name")]
    pub preset: String,
    /// Preset that was active before `preset`, used to toggle the overview back.
    #[serde(default)]
    pub previous_preset: Option<String>,
    /// The layout from before a center-override preset (the overview) was applied. Toggling the
    /// overview off restores it exactly; applying a preset that keeps the center discards it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stashed: Option<Box<SavedLayout>>,
    /// Next pane id to hand out; ids are never reused within a state, nor across the layouts of
    /// different workspaces.
    #[serde(default)]
    next_pane: u64,
    /// The workspace (daemon workspace id) the layout above belongs to; `None` until the
    /// desktop knows which workspace it shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_workspace: Option<String>,
    /// The layouts of the workspaces that are not shown right now, by workspace id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parked: BTreeMap<String, WorkspaceLayout>,
    /// What the user left the docks at in each preset they have left, by preset name: applying
    /// that preset again brings the modes and expanded sizes back (DK-1).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dock_memory: BTreeMap<String, RememberedDocks>,
}

/// A [`WorkbenchState`] minus its own stash and pane-id counter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedLayout {
    pub docks: Docks,
    pub center: PaneTree,
    pub zoomed: Option<PaneId>,
    pub focus: FocusTarget,
    #[serde(default = "default_preset_name")]
    pub preset: String,
    #[serde(default)]
    pub previous_preset: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dock_memory: BTreeMap<String, RememberedDocks>,
}

impl SavedLayout {
    fn of(state: &WorkbenchState) -> Self {
        Self {
            docks: state.docks.clone(),
            center: state.center.clone(),
            zoomed: state.zoomed,
            focus: state.focus,
            preset: state.preset.clone(),
            previous_preset: state.previous_preset.clone(),
            dock_memory: state.dock_memory.clone(),
        }
    }

    fn into_state(self, next_pane: u64) -> WorkbenchState {
        WorkbenchState {
            docks: self.docks,
            center: self.center,
            zoomed: self.zoomed,
            focus: self.focus,
            preset: self.preset,
            previous_preset: self.previous_preset,
            stashed: None,
            next_pane,
            active_workspace: None,
            parked: BTreeMap::new(),
            dock_memory: self.dock_memory,
        }
    }
}

fn default_preset_name() -> String {
    "default".to_owned()
}

impl Default for WorkbenchState {
    /// The `default` preset over a single welcome pane.
    fn default() -> Self {
        let first = PaneId(1);
        let mut state = Self {
            docks: LayoutPreset::default_preset().docks,
            center: PaneTree::single(Pane::new(first, vec![ItemRef::placeholder("Welcome")])),
            zoomed: None,
            focus: FocusTarget::Pane(first),
            preset: default_preset_name(),
            previous_preset: None,
            stashed: None,
            next_pane: 2,
            active_workspace: None,
            parked: BTreeMap::new(),
            dock_memory: BTreeMap::new(),
        };
        state.normalize();
        state
    }
}

impl WorkbenchState {
    /// A state with a custom center and the `default` preset's docks. Used by tests and by wave 2
    /// when it seeds real sessions.
    pub fn with_center(center: PaneTree) -> Self {
        let mut state = Self {
            docks: LayoutPreset::default_preset().docks,
            center,
            zoomed: None,
            focus: FocusTarget::Pane(PaneId(0)),
            preset: default_preset_name(),
            previous_preset: None,
            stashed: None,
            next_pane: 0,
            active_workspace: None,
            parked: BTreeMap::new(),
            dock_memory: BTreeMap::new(),
        };
        state.normalize();
        if let Some(first) = state.center.first_pane() {
            state.focus = FocusTarget::Pane(first.id);
        }
        state
    }

    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.center.pane(id)
    }

    pub fn panes(&self) -> Vec<&Pane> {
        self.center.panes()
    }

    pub fn first_pane_id(&self) -> PaneId {
        self.center
            .first_pane()
            .map(|p| p.id)
            .unwrap_or(PaneId(self.next_pane))
    }

    /// The pane that receives keyboard commands: the focused pane, or the first pane when a dock
    /// has focus.
    pub fn active_pane_id(&self) -> PaneId {
        match self.focus {
            FocusTarget::Pane(id) if self.center.pane(id).is_some() => id,
            _ => self.first_pane_id(),
        }
    }

    pub fn active_pane(&self) -> Option<&Pane> {
        self.center.pane(self.active_pane_id())
    }

    /// Item shown in the focused pane, or the active panel of the focused dock.
    pub fn active_item(&self) -> Option<ItemRef> {
        match self.focus {
            FocusTarget::Dock(pos) => {
                let dock = self.docks.get(pos);
                dock.active
                    .clone()
                    .map(|panel| ItemRef::Panel { panel })
                    .or_else(|| self.active_pane().and_then(|p| p.active_item().cloned()))
            }
            FocusTarget::Pane(_) => self.active_pane().and_then(|p| p.active_item().cloned()),
        }
    }

    /// Item shown in the focused (else the first) center pane, whatever has focus: what the
    /// window title names. A sidebar page never becomes the title of the window.
    pub fn active_center_item(&self) -> Option<ItemRef> {
        self.active_pane().and_then(|p| p.active_item().cloned())
    }

    /// Every open item exactly once: dock panels (left, right, bottom) then center items in tree
    /// order, then what a center-override preset (the overview) keeps stashed out of sight
    /// (panels already shown are not repeated). Used to assert conservation across operations.
    pub fn all_items(&self) -> Vec<ItemRef> {
        let mut items = self.visible_items();
        if let Some(saved) = &self.stashed {
            let mut shown: HashSet<PanelId> =
                items.iter().filter_map(|i| i.as_panel().cloned()).collect();
            let hidden = DockPosition::ALL
                .into_iter()
                .flat_map(|pos| saved.docks.get(pos).panels.iter().cloned())
                .map(|panel| ItemRef::Panel { panel })
                .chain(
                    saved
                        .center
                        .panes()
                        .into_iter()
                        .flat_map(|p| p.items.clone()),
                );
            for item in hidden {
                match item.as_panel() {
                    Some(panel) if !shown.insert(panel.clone()) => {}
                    _ => items.push(item),
                }
            }
        }
        items
    }

    /// The items currently on screen (docks and center), without the stash.
    pub fn visible_items(&self) -> Vec<ItemRef> {
        let mut items: Vec<ItemRef> = DockPosition::ALL
            .into_iter()
            .flat_map(|pos| self.docks.get(pos).panels.iter().cloned())
            .map(|panel| ItemRef::Panel { panel })
            .collect();
        for pane in self.center.panes() {
            items.extend(pane.items.iter().cloned());
        }
        items
    }

    pub(crate) fn alloc_pane_id(&mut self) -> PaneId {
        let id = PaneId(self.next_pane.max(self.max_pane_id() + 1));
        self.next_pane = id.0 + 1;
        id
    }

    fn max_pane_id(&self) -> u64 {
        self.center
            .panes()
            .iter()
            .map(|p| p.id.0)
            .max()
            .unwrap_or(0)
    }

    /// Restore every invariant. Cheap and idempotent; mutators call it, and it sanitizes states
    /// loaded from disk.
    pub fn normalize(&mut self) {
        self.next_pane = self.next_pane.max(self.max_pane_id() + 1);

        // Unique pane ids.
        let mut next = self.next_pane;
        let mut seen = HashSet::new();
        for pane in self.center.panes_mut() {
            if !seen.insert(pane.id) {
                pane.id = PaneId(next);
                next += 1;
                seen.insert(pane.id);
            }
        }
        self.next_pane = next;

        // Each panel is placed at most once; docks win over the center.
        let mut placed: HashSet<PanelId> = HashSet::new();
        for pos in DockPosition::ALL {
            let dock = self.docks.get_mut(pos);
            dock.panels.retain(|p| placed.insert(p.clone()));
            let (min, max) = pos.size_range();
            if !dock.size.is_finite() {
                dock.size = 240.0;
            }
            dock.size = dock.size.clamp(min, max);
            if pos == DockPosition::Bottom {
                dock.collapsed = false;
            }
            if dock
                .active
                .as_ref()
                .is_none_or(|a| !dock.panels.contains(a))
            {
                dock.active = dock.panels.first().cloned();
            }
        }
        for pane in self.center.panes_mut() {
            pane.items.retain(|item| match item.as_panel() {
                Some(panel) => placed.insert(panel.clone()),
                None => true,
            });
        }

        let fallback = match self.focus {
            FocusTarget::Pane(id) if self.center.pane(id).is_some() => id,
            _ => self.first_pane_id(),
        };
        self.center.normalize(fallback);

        if let Some(saved) = self.stashed.take() {
            let mut inner = saved.into_state(self.next_pane);
            inner.normalize();
            self.next_pane = self.next_pane.max(inner.next_pane);
            self.stashed = Some(Box::new(SavedLayout::of(&inner)));
        }
        self.normalize_parked();

        if let Some(zoomed) = self.zoomed
            && self.center.pane(zoomed).is_none()
        {
            self.zoomed = None;
        }
        if let FocusTarget::Pane(id) = self.focus
            && self.center.pane(id).is_none()
        {
            self.focus = FocusTarget::Pane(self.first_pane_id());
        }
    }
}
