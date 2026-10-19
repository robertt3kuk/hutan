//! Per-workspace layouts (M12): every workspace remembers its own docks, pane tree, zoom,
//! focus and preset. The live fields of [`WorkbenchState`] always describe the shown workspace;
//! the others sit in [`WorkbenchState::parked`] until the user comes back to them.
//!
//! Switching never touches sessions: the model only swaps which layout is live, and the shell
//! attaches the terminals the live layout shows (the others detach, see `terminals.rs`).

use super::{
    FocusTarget, ItemRef, Pane, PaneId, PaneTree, SavedLayout, WorkbenchState, default_preset_name,
};
use serde::{Deserialize, Serialize};

/// A parked layout: a [`SavedLayout`] plus the overview stash it had when it was parked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceLayout {
    #[serde(flatten)]
    pub layout: SavedLayout,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stashed: Option<Box<SavedLayout>>,
}

impl WorkspaceLayout {
    fn of(state: &WorkbenchState) -> Self {
        Self {
            layout: SavedLayout::of(state),
            stashed: state.stashed.clone(),
        }
    }

    fn into_state(self, next_pane: u64) -> WorkbenchState {
        let mut state = self.layout.into_state(next_pane);
        state.stashed = self.stashed;
        state
    }

    /// Every terminal or chat session the layout shows, including the stash.
    pub fn sessions(&self) -> Vec<String> {
        let stash = self.stashed.iter().flat_map(|s| s.center.panes());
        self.layout
            .center
            .panes()
            .into_iter()
            .chain(stash)
            .flat_map(|pane| pane.items.iter())
            .filter_map(|item| match item {
                ItemRef::Terminal { session } | ItemRef::Chat { session } => Some(session.clone()),
                _ => None,
            })
            .collect()
    }
}

impl WorkbenchState {
    /// The workspace whose layout is live.
    pub fn workspace(&self) -> Option<&str> {
        self.active_workspace.as_deref()
    }

    /// Declares that the live layout belongs to `id` (a layout saved before workspaces existed,
    /// or a first start). Does nothing when a workspace is already shown.
    pub fn adopt_workspace(&mut self, id: &str) {
        if self.active_workspace.is_none() {
            self.active_workspace = Some(id.to_owned());
            self.parked.remove(id);
        }
    }

    /// Shows the layout of workspace `id`: the live layout is parked under the workspace it
    /// belonged to, and the one parked for `id` (or, for a workspace seen for the first time, a
    /// fresh one that keeps the dock arrangement) becomes live. `false` when `id` is already
    /// shown. A layout that belongs to no workspace yet becomes the one of `id` (nothing is
    /// lost before the desktop knows what the daemon has).
    pub fn switch_workspace(&mut self, id: &str) -> bool {
        match self.active_workspace.as_deref() {
            Some(current) if current == id => return false,
            None => {
                self.adopt_workspace(id);
                return true;
            }
            Some(_) => {}
        }
        let outgoing = self.active_workspace.take().map(|previous| {
            let layout = WorkspaceLayout::of(self);
            (previous, layout)
        });
        let mut parked = std::mem::take(&mut self.parked);
        let mut next = match parked.remove(id) {
            Some(layout) => layout.into_state(self.next_pane),
            None => self.fresh_layout(),
        };
        parked.extend(outgoing);
        next.next_pane = next.next_pane.max(self.next_pane);
        next.active_workspace = Some(id.to_owned());
        next.parked = parked;
        next.normalize();
        *self = next;
        true
    }

    /// A single welcome pane under the dock arrangement and preset the user has now (the one
    /// before the overview when the overview is up).
    fn fresh_layout(&mut self) -> WorkbenchState {
        let (docks, preset, previous_preset) = match &self.stashed {
            Some(saved) => (
                saved.docks.clone(),
                saved.preset.clone(),
                saved.previous_preset.clone(),
            ),
            None => (
                self.docks.clone(),
                self.preset.clone(),
                self.previous_preset.clone(),
            ),
        };
        let pane = self.alloc_pane_id();
        let mut state = WorkbenchState {
            docks,
            center: PaneTree::single(Pane::new(pane, vec![ItemRef::placeholder("Welcome")])),
            zoomed: None,
            focus: FocusTarget::Pane(pane),
            preset,
            previous_preset,
            stashed: None,
            next_pane: self.next_pane,
            active_workspace: None,
            parked: Default::default(),
            dock_memory: self.dock_memory.clone(),
        };
        if state.preset.is_empty() {
            state.preset = default_preset_name();
        }
        state
    }

    /// Forgets the layout parked for `id` (the workspace is gone).
    pub fn forget_workspace(&mut self, id: &str) -> bool {
        self.parked.remove(id).is_some()
    }

    /// `from` became part of workspace `to` (a project was put into a workspace), so its layout
    /// belongs to `to` now. The layout `to` already has wins; without one, `from`'s becomes
    /// `to`'s (shown or parked as it was). `false` when `from` has no layout here.
    pub fn merge_workspace(&mut self, from: &str, to: &str) -> bool {
        if from == to {
            return false;
        }
        if self.active_workspace.as_deref() == Some(from) {
            if self.parked.contains_key(to) {
                self.switch_workspace(to);
                self.parked.remove(from);
            } else {
                self.active_workspace = Some(to.to_owned());
            }
            return true;
        }
        match self.parked.remove(from) {
            Some(layout) => {
                if self.active_workspace.as_deref() != Some(to) && !self.parked.contains_key(to) {
                    self.parked.insert(to.to_owned(), layout);
                }
                true
            }
            None => false,
        }
    }

    /// Ids of the workspaces with a parked layout.
    pub fn parked_workspaces(&self) -> impl Iterator<Item = &str> {
        self.parked.keys().map(String::as_str)
    }

    /// The default layout for the shown workspace; the other workspaces keep theirs.
    pub fn reset_layout(&mut self) {
        let mut fresh = WorkbenchState::default();
        // Pane ids stay unique across the layouts of all workspaces.
        let pane = PaneId(self.next_pane.max(self.max_pane_id() + 1));
        fresh.center = PaneTree::single(Pane::new(pane, vec![ItemRef::placeholder("Welcome")]));
        fresh.focus = FocusTarget::Pane(pane);
        fresh.next_pane = pane.0 + 1;
        fresh.active_workspace = self.active_workspace.take();
        fresh.parked = std::mem::take(&mut self.parked);
        fresh.normalize();
        *self = fresh;
    }

    /// Normalizes every parked layout like the live one and keeps pane ids unique across all of
    /// them (the shell keys terminal views by pane id).
    pub(super) fn normalize_parked(&mut self) {
        let parked = std::mem::take(&mut self.parked);
        for (id, layout) in parked {
            let mut inner = layout.into_state(self.next_pane);
            inner.normalize();
            self.next_pane = self.next_pane.max(inner.next_pane);
            self.parked.insert(id, WorkspaceLayout::of(&inner));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Axis, DockPosition, Edge, ItemRef, LayoutPreset, PaneId};
    use super::*;
    use std::collections::HashSet;

    fn terminal(session: &str) -> ItemRef {
        ItemRef::Terminal {
            session: session.to_owned(),
        }
    }

    /// A state with a split of two terminals shown under workspace `id`.
    fn split_of(id: &str, a: &str, b: &str) -> WorkbenchState {
        let mut state = WorkbenchState::default();
        state.adopt_workspace(id);
        let pane = state.active_pane_id();
        state.replace_item(&ItemRef::placeholder("Welcome"), terminal(a));
        state.split_pane(pane, Edge::Right, terminal(b)).unwrap();
        state
    }

    fn sessions_of(state: &WorkbenchState) -> Vec<String> {
        state
            .center
            .panes()
            .into_iter()
            .flat_map(|p| p.items.iter())
            .filter_map(|i| match i {
                ItemRef::Terminal { session } => Some(session.clone()),
                _ => None,
            })
            .collect()
    }

    fn all_pane_ids(state: &WorkbenchState) -> Vec<PaneId> {
        let mut ids: Vec<PaneId> = state.center.panes().iter().map(|p| p.id).collect();
        for layout in state.parked.values() {
            ids.extend(layout.layout.center.panes().iter().map(|p| p.id));
        }
        ids
    }

    #[test]
    fn adopting_names_the_layout_once() {
        let mut state = WorkbenchState::default();
        assert_eq!(state.workspace(), None);
        state.adopt_workspace("a");
        state.adopt_workspace("b");
        assert_eq!(state.workspace(), Some("a"));
    }

    #[test]
    fn a_layout_without_a_workspace_goes_to_the_first_one_shown() {
        let mut state = split_of("x", "s1", "s2");
        state.active_workspace = None;
        let before = state.center.clone();
        assert!(state.switch_workspace("home"));
        assert_eq!(state.workspace(), Some("home"));
        assert_eq!(state.center, before, "nothing is thrown away");
        assert!(state.parked.is_empty());
    }

    #[test]
    fn every_workspace_gets_its_own_tree_and_gets_it_back_exactly() {
        let mut state = split_of("a", "a1", "a2");
        let tree_a = state.center.clone();
        state.docks.get_mut(DockPosition::Bottom).visible = true;

        assert!(state.switch_workspace("b"));
        assert_eq!(state.workspace(), Some("b"));
        assert_eq!(state.center.panes().len(), 1, "a fresh pane");
        assert!(sessions_of(&state).is_empty());
        assert!(
            state.docks.bottom.visible,
            "a new workspace keeps the dock arrangement"
        );
        let pane = state.active_pane_id();
        state.replace_item(&ItemRef::placeholder("Welcome"), terminal("b1"));
        let tree_b = state.center.clone();
        assert!(state.parked.contains_key("a"));

        assert!(state.switch_workspace("a"));
        assert_eq!(state.center, tree_a);
        assert_eq!(sessions_of(&state), ["a1", "a2"]);
        assert!(state.switch_workspace("b"));
        assert_eq!(state.center, tree_b);
        assert_eq!(state.active_pane_id(), pane);
        assert!(!state.switch_workspace("b"), "already shown");
    }

    #[test]
    fn pane_ids_stay_unique_across_all_layouts() {
        let mut state = split_of("a", "a1", "a2");
        for next in ["b", "c", "a", "b", "d", "c"] {
            state.switch_workspace(next);
            let pane = state.active_pane_id();
            let _ = state.split_pane(pane, Edge::Bottom, ItemRef::placeholder(next));
            let ids = all_pane_ids(&state);
            let unique: HashSet<_> = ids.iter().copied().collect();
            assert_eq!(unique.len(), ids.len(), "{ids:?}");
        }
    }

    #[test]
    fn the_overview_travels_with_its_workspace() {
        let presets = LayoutPreset::builtin();
        let mut state = split_of("a", "a1", "a2");
        state
            .toggle_preset(LayoutPreset::OVERVIEW, &presets)
            .unwrap();
        assert_eq!(state.preset, LayoutPreset::OVERVIEW);
        state.switch_workspace("b");
        assert_eq!(state.preset, LayoutPreset::DEFAULT, "b starts without it");
        assert!(state.stashed.is_none());
        state.switch_workspace("a");
        assert_eq!(state.preset, LayoutPreset::OVERVIEW);
        state
            .toggle_preset(LayoutPreset::OVERVIEW, &presets)
            .unwrap();
        assert_eq!(sessions_of(&state), ["a1", "a2"], "the stash came back");
    }

    #[test]
    fn layouts_survive_json_and_unknown_workspaces_are_forgotten() {
        let mut state = split_of("a", "a1", "a2");
        state.switch_workspace("b");
        state.switch_workspace("c");
        let json = state.to_json().unwrap();
        let loaded = WorkbenchState::from_json(&json).unwrap();
        assert_eq!(loaded, state);
        assert_eq!(loaded.parked_workspaces().count(), 2);

        let mut loaded = loaded;
        assert!(loaded.forget_workspace("a"));
        assert!(!loaded.forget_workspace("a"));
        assert_eq!(loaded.parked_workspaces().collect::<Vec<_>>(), ["b"]);
    }

    #[test]
    fn a_layout_saved_before_workspaces_existed_still_loads() {
        let json = WorkbenchState::default().to_json().unwrap();
        assert!(!json.contains("active_workspace") && !json.contains("parked"));
        let loaded = WorkbenchState::from_json(&json).unwrap();
        assert_eq!(loaded.workspace(), None);
    }

    #[test]
    fn reset_keeps_the_workspace_and_the_others_layouts() {
        let mut state = split_of("a", "a1", "a2");
        state.switch_workspace("b");
        state.replace_item(&ItemRef::placeholder("Welcome"), terminal("b1"));
        state.reset_layout();
        assert_eq!(state.workspace(), Some("b"));
        assert!(sessions_of(&state).is_empty());
        assert!(state.parked.contains_key("a"));
        let ids = all_pane_ids(&state);
        let unique: HashSet<_> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "{ids:?}");
        state.switch_workspace("a");
        assert_eq!(sessions_of(&state), ["a1", "a2"]);
    }

    #[test]
    fn a_split_axis_survives_parking() {
        let mut state = split_of("a", "a1", "a2");
        state.switch_workspace("b");
        state.switch_workspace("a");
        assert!(matches!(
            state.center,
            PaneTree::Split {
                axis: Axis::Horizontal,
                ..
            }
        ));
    }

    #[test]
    fn a_project_layout_follows_the_workspace_it_joins() {
        // Shown, and the workspace has no layout yet: the layout is the workspace's now.
        let mut state = split_of("p", "s1", "s2");
        let before = state.center.clone();
        assert!(state.merge_workspace("p", "w"));
        assert_eq!(state.workspace(), Some("w"));
        assert_eq!(state.center, before);
        assert!(state.parked_workspaces().next().is_none());

        // Shown, and the workspace has its own: that one is shown, the project's is dropped.
        let mut state = split_of("p", "s1", "s2");
        state.switch_workspace("w");
        state.replace_item(&ItemRef::placeholder("Welcome"), terminal("w1"));
        state.switch_workspace("p");
        assert!(state.merge_workspace("p", "w"));
        assert_eq!(state.workspace(), Some("w"));
        assert_eq!(sessions_of(&state), ["w1"]);
        assert!(state.parked_workspaces().next().is_none());

        // Parked: moves over when the workspace has none, else is forgotten.
        let mut state = split_of("p", "s1", "s2");
        state.switch_workspace("home");
        assert!(state.merge_workspace("p", "w"));
        assert_eq!(state.parked_workspaces().collect::<Vec<_>>(), ["w"]);
        assert!(!state.merge_workspace("p", "w"), "nothing left to merge");
        let mut state = split_of("p", "s1", "s2");
        state.switch_workspace("w");
        state.switch_workspace("home");
        assert!(state.merge_workspace("p", "w"));
        assert_eq!(state.parked_workspaces().collect::<Vec<_>>(), ["w"]);
    }
}
