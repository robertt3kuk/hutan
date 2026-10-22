//! Layout presets and applying them without losing open items.

use super::{
    AGENTS_PAGE, Dock, DockPosition, Docks, FocusTarget, ItemRef, ModelError, Pane, PaneTree,
    PanelId, SavedLayout, WorkbenchState,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// What a preset does to the center.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CenterLayout {
    /// Leave the pane tree as it is.
    Keep,
    /// Replace the tree with one pane showing these items first.
    Items(Vec<ItemRef>),
}

/// A named arrangement of docks and (optionally) the center.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutPreset {
    pub name: String,
    pub docks: Docks,
    pub center: CenterLayout,
    /// Zoom the active pane after applying (the `focus` preset).
    pub zoom_active: bool,
}

impl LayoutPreset {
    pub const DEFAULT: &'static str = "default";
    pub const FOCUS: &'static str = "focus";
    pub const OVERVIEW: &'static str = "overview";

    /// Default width of the left sidebar and of the right sidebar (Orca's 280 / 350).
    pub const LEFT_SIZE: f32 = 280.0;
    pub const RIGHT_SIZE: f32 = 350.0;

    /// The left sidebar: Projects is its default page, Tasks one of its nav pages. The
    /// Assistant is never listed here: it is a left page that opens on demand.
    fn left(visible: bool, panels: &[&str]) -> Dock {
        Dock::new(panels, Self::LEFT_SIZE, visible)
    }

    /// The right side's activity rail (CL-CHROME, the clean default; SB-ATTACH): Files first
    /// (Orca puts the explorer on the right), then Changes (the default tab), Agents, History as
    /// icons; Requests (the inspector) and Resources listed in its `⋯` menu, Requests coming
    /// onto the rail with its count while a request waits.
    fn right(visible: bool) -> Dock {
        let mut dock = Dock::new(
            &[
                "files",
                "changes",
                "agents",
                "history",
                "inspector",
                "resources",
            ],
            Self::RIGHT_SIZE,
            visible,
        )
        .with_more(&["inspector", "resources"]);
        dock.active = Some(PanelId::new("changes"));
        dock
    }

    fn docks(left: Dock, right: Dock) -> Docks {
        Docks {
            left,
            right,
            bottom: Dock::new(&[], 200.0, false),
        }
    }

    /// The pages of the left sidebar in every built-in preset: Projects (shown) and Tasks.
    const LEFT_PAGES: [&'static str; 2] = ["sessions", "tasks"];

    /// `sessions` (Projects) and `tasks` as pages of the left sidebar (Agents is a
    /// page of the center, not of the sidebar); the right rail of [`Self::right`]; the bottom
    /// dock hidden and empty; center untouched.
    pub fn default_preset() -> Self {
        Self {
            name: Self::DEFAULT.to_owned(),
            docks: Self::docks(Self::left(true, &Self::LEFT_PAGES), Self::right(true)),
            center: CenterLayout::Keep,
            zoom_active: false,
        }
    }

    /// All docks hidden, the active pane zoomed.
    pub fn focus_preset() -> Self {
        Self {
            name: Self::FOCUS.to_owned(),
            docks: Self::docks(Self::left(false, &Self::LEFT_PAGES), Self::right(false)),
            center: CenterLayout::Keep,
            zoom_active: true,
        }
    }

    /// The Agents overview: the Agents page as the only center item, side docks visible. The
    /// preset keeps the name `overview` (the setting, the key and saved layouts name it).
    pub fn overview_preset() -> Self {
        Self {
            name: Self::OVERVIEW.to_owned(),
            docks: Self::docks(Self::left(true, &Self::LEFT_PAGES), Self::right(true)),
            center: CenterLayout::Items(vec![ItemRef::panel(AGENTS_PAGE)]),
            zoom_active: false,
        }
    }

    pub fn builtin() -> Vec<LayoutPreset> {
        vec![
            Self::default_preset(),
            Self::focus_preset(),
            Self::overview_preset(),
        ]
    }

    /// Panels this preset places (docks and center override).
    fn placed_panels(&self) -> HashSet<PanelId> {
        let mut placed: HashSet<PanelId> = DockPosition::ALL
            .into_iter()
            .flat_map(|pos| self.docks.get(pos).panels.iter().cloned())
            .collect();
        if let CenterLayout::Items(items) = &self.center {
            placed.extend(items.iter().filter_map(|i| i.as_panel().cloned()));
        }
        placed
    }
}

impl WorkbenchState {
    /// Apply `preset`, keeping every open item.
    ///
    /// Docks are replaced by the preset's. Panels the preset places are moved to those places;
    /// items it does not place (terminals, chats, placeholders, panels that were open elsewhere)
    /// are kept in the first pane. A [`CenterLayout::Items`] override (the overview) stashes the
    /// whole layout and shows only its own items; the stash comes back on leaving it.
    pub fn apply_preset(&mut self, preset: &LayoutPreset) {
        self.apply_preset_from(preset, &LayoutPreset::overview_preset().placed_panels());
    }

    /// [`Self::apply_preset`] knowing which panels the current center-override preset placed
    /// (they are not carried over when its stash is restored).
    fn apply_preset_from(&mut self, preset: &LayoutPreset, departing_panels: &HashSet<PanelId>) {
        // Leaving a center override for a preset that keeps the center brings the stash back
        // first, so no path (keys, menu, palette, settings, restore) can lose items.
        if matches!(preset.center, CenterLayout::Keep)
            && let Some(saved) = self.stashed.take()
        {
            self.restore_saved(*saved, departing_panels);
            if self.preset == preset.name {
                return;
            }
        }
        // What the layout showing now (the one a restored stash brought back, if any) had its docks
        // at is what applying it again will bring back. The overview stashes and restores whole
        // layouts, docks included, so it is not part of that memory.
        let overrides_center = matches!(preset.center, CenterLayout::Items(_));
        if !overrides_center {
            self.remember_docks();
        }
        let switching = self.preset != preset.name;
        let placed = preset.placed_panels();
        let had_stash = self.stashed.is_some();
        // A center override (overview) remembers the layout it replaced; re-applying while one
        // is already remembered keeps the original, and any other preset forgets it.
        let stash = match &preset.center {
            CenterLayout::Items(_) => Some(
                self.stashed
                    .take()
                    .unwrap_or_else(|| Box::new(SavedLayout::of(self))),
            ),
            CenterLayout::Keep => None,
        };
        let prev_focus = self.active_pane_id();
        let old_docks = std::mem::replace(&mut self.docks, preset.docks.clone());
        // Coming back to a preset the user already arranged restores their modes and sizes; a
        // preset applied again while it is current starts over from its own.
        if switching
            && !overrides_center
            && let Some(memory) = self.dock_memory.get(&preset.name).copied()
        {
            self.docks.set_modes(&memory);
        }
        let leftover_panels: Vec<ItemRef> = DockPosition::ALL
            .into_iter()
            .flat_map(|pos| old_docks.get(pos).panels.iter().cloned())
            .filter(|p| !placed.contains(p))
            .map(|panel| ItemRef::Panel { panel })
            .collect();

        match &preset.center {
            CenterLayout::Keep => {
                for pane in self.center.panes_mut() {
                    pane.items
                        .retain(|i| i.as_panel().is_none_or(|p| !placed.contains(p)));
                    pane.active = pane.active.min(pane.items.len().saturating_sub(1));
                }
                if let Some(first) = self.center.first_pane_mut() {
                    first.items.extend(leftover_panels);
                }
            }
            CenterLayout::Items(items) => {
                let first_id = self.first_pane_id();
                let mut all: Vec<ItemRef> = items.clone();
                // Entering the override: everything else lives in the stash and is not shown.
                // Re-applying it while already in it: what was opened since stays visible.
                if had_stash {
                    all.extend(leftover_panels);
                    for pane in self.center.panes() {
                        all.extend(
                            pane.items
                                .iter()
                                .filter(|i| i.as_panel().is_none_or(|p| !placed.contains(p)))
                                .cloned(),
                        );
                    }
                }
                self.center = PaneTree::single(Pane::new(first_id, all));
            }
        }

        if self.preset != preset.name {
            self.previous_preset = Some(std::mem::replace(&mut self.preset, preset.name.clone()));
        }
        self.zoomed = None;
        self.focus = FocusTarget::Pane(prev_focus);
        self.stashed = stash;
        self.normalize();
        // `normalize` moved focus to the first pane if the focused one no longer exists.
        if preset.zoom_active {
            self.zoomed = Some(self.active_pane_id());
        }
    }

    /// Put `saved` back exactly. Panels the departing preset placed that are no longer open were
    /// closed while it was current and stay closed; items opened since (other than panels the
    /// departing preset placed itself) join the first pane.
    fn restore_saved(&mut self, saved: SavedLayout, departing_panels: &HashSet<PanelId>) {
        let live: HashSet<PanelId> = self
            .all_items()
            .into_iter()
            .filter_map(|i| i.as_panel().cloned())
            .collect();
        let keep_panel = |p: &PanelId| live.contains(p) || !departing_panels.contains(p);
        let mut restored = saved.into_state(self.next_pane);
        for pos in DockPosition::ALL {
            restored.docks.get_mut(pos).panels.retain(&keep_panel);
        }
        for pane in restored.center.panes_mut() {
            pane.items
                .retain(|item| item.as_panel().is_none_or(&keep_panel));
        }
        let fresh: Vec<ItemRef> = self
            .all_items()
            .into_iter()
            .filter(|item| {
                item.as_panel()
                    .is_none_or(|p| !departing_panels.contains(p))
            })
            .collect();
        // `normalize` below prunes panes the filtering emptied; give them a home first.
        if let Some(first) = restored.center.first_pane_mut() {
            first.items.extend(fresh);
        }
        restored.active_workspace = self.active_workspace.take();
        restored.parked = std::mem::take(&mut self.parked);
        restored.normalize();
        *self = restored;
    }

    /// Records the modes and expanded sizes of the docks as the layout of the current preset's.
    fn remember_docks(&mut self) {
        self.dock_memory
            .insert(self.preset.clone(), self.docks.modes());
    }

    /// The layout predates the clean default (CL-CHROME): its right dock lists nothing in the
    /// rail's `⋯` menu and its docks hold other panels than `preset`'s. Such a layout is kept as
    /// it is; *Layout: Use New Default* is offered for it once.
    pub fn predates(&self, preset: &LayoutPreset) -> bool {
        let arrangement = |docks: &Docks| {
            DockPosition::ALL.map(|pos| {
                let dock = docks.get(pos);
                (dock.panels.clone(), dock.more.clone())
            })
        };
        self.docks.right.more.is_empty() && arrangement(&self.docks) != arrangement(&preset.docks)
    }

    /// *Layout: Use New Default*: the panels of `preset`'s docks, keeping the center and the
    /// open, collapsed or hidden mode and the size the user left each dock at.
    pub fn use_new_default(&mut self, preset: &LayoutPreset) {
        let modes = self.docks.modes();
        self.apply_preset(preset);
        self.docks.set_modes(&modes);
        self.normalize();
    }

    /// Switch to the preset called `name` by any path (menu, palette, keys, settings, restore).
    ///
    /// Applying a preset that overrides the center (the overview) stashes the current layout and
    /// makes the override the only center item, active. Applying a preset that keeps the center
    /// while such an override is current first restores the stashed layout exactly, then (unless
    /// the restored layout already is that preset) applies the preset on top.
    pub fn switch_preset(
        &mut self,
        name: &str,
        presets: &[LayoutPreset],
    ) -> Result<(), ModelError> {
        let find = |n: &str| presets.iter().find(|p| p.name == n);
        let target = find(name).ok_or_else(|| ModelError::UnknownPreset(name.to_owned()))?;
        let departing = find(&self.preset)
            .cloned()
            .unwrap_or_else(LayoutPreset::overview_preset)
            .placed_panels();
        self.apply_preset_from(target, &departing);
        Ok(())
    }

    /// Apply the preset called `name`, or if it is already the current preset, go back to the
    /// previous one (or `default`). Leaving a preset that put panels in the center removes the
    /// ones the new preset does not place (the overview board only lives while selected).
    pub fn toggle_preset(
        &mut self,
        name: &str,
        presets: &[LayoutPreset],
    ) -> Result<(), ModelError> {
        let find = |n: &str| presets.iter().find(|p| p.name == n);
        if self.preset != name {
            let target = find(name).ok_or_else(|| ModelError::UnknownPreset(name.to_owned()))?;
            self.apply_preset(target);
            return Ok(());
        }
        let departing = find(name).ok_or_else(|| ModelError::UnknownPreset(name.to_owned()))?;
        if let Some(saved) = self.stashed.take() {
            self.restore_saved(*saved, &departing.placed_panels());
            return Ok(());
        }
        let back_name = self
            .previous_preset
            .clone()
            .filter(|p| p != name)
            .unwrap_or_else(|| LayoutPreset::DEFAULT.to_owned());
        let back = find(&back_name).ok_or(ModelError::UnknownPreset(back_name))?;
        self.apply_preset(back);
        if let CenterLayout::Items(items) = &departing.center {
            for panel in items.iter().filter_map(ItemRef::as_panel) {
                if matches!(self.find_panel(panel), Some(super::ItemSource::Pane { .. })) {
                    self.close_panel(panel)?;
                }
            }
        }
        Ok(())
    }
}
