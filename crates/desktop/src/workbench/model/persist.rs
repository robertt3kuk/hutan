//! Versioned JSON (de)serialization of [`WorkbenchState`].

use super::{
    DockPosition, Docks, ItemRef, LayoutPreset, ModelError, PaneTree, PanelId, WorkbenchState,
};

/// The saved layout's version. 2 is the Orca-style relayout (OL-2): Projects, Tasks and the
/// Agents page as pages of the left sidebar, History / Changes / Requests / Agents / Resources
/// in the right sidebar's strip, no Assistant in any dock. A version 1 file is migrated once
/// when it is read (see [`migrate_v1`]); the next save writes version 2.
pub const STATE_VERSION: u32 = 2;

/// Result of reading a saved state.
#[derive(Debug)]
pub struct LoadOutcome {
    pub state: WorkbenchState,
    /// Set when the saved data was unusable and the default layout was substituted.
    pub warning: Option<String>,
}

impl WorkbenchState {
    /// `{ "version": 2, "docks": …, "center": …, … }`, pretty-printed.
    pub fn to_json(&self) -> Result<String, ModelError> {
        let mut value =
            serde_json::to_value(self).map_err(|e| ModelError::Invalid(e.to_string()))?;
        if let Some(map) = value.as_object_mut() {
            map.insert("version".to_owned(), STATE_VERSION.into());
        }
        serde_json::to_string_pretty(&value).map_err(|e| ModelError::Invalid(e.to_string()))
    }

    /// Strict parse: wrong version or malformed data is an error. The result is normalized, so
    /// hand-edited files with broken ratios or empty panes still load into a valid state.
    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|e| ModelError::Invalid(e.to_string()))?;
        let version = match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(v @ (1 | 2)) => v,
            Some(v) => return Err(ModelError::Invalid(format!("unsupported version {v}"))),
            None => return Err(ModelError::Invalid("missing version".to_owned())),
        };
        let mut state: WorkbenchState =
            serde_json::from_value(value).map_err(|e| ModelError::Invalid(e.to_string()))?;
        if version == 1 {
            migrate_v1(&mut state);
        }
        state.normalize();
        Ok(state)
    }

    /// Like [`Self::from_json`] but falls back to the default layout with a warning.
    pub fn from_json_or_default(json: &str) -> LoadOutcome {
        match Self::from_json(json) {
            Ok(state) => LoadOutcome {
                state,
                warning: None,
            },
            Err(err) => LoadOutcome {
                state: WorkbenchState::default(),
                warning: Some(format!("saved layout ignored: {err}")),
            },
        }
    }
}

/// The one-time move of a version 1 layout to the Orca-style arrangement, applied to the live
/// layout, the overview stash and every parked workspace layout. The center (tabs, splits, zoom,
/// focus), the preset name and every size the user changed stay as they were.
///
/// - History moves to the front of the right dock and becomes its active tab (a History tab in
///   the center or the bottom dock stays where the user put it).
/// - The Assistant leaves every dock and the center (it is a left page that opens on demand).
/// - Tasks moves from the right dock to the left sidebar, behind Projects.
/// - Changes joins the right dock after History; Resources moves there from a hidden bottom dock.
/// - Projects is the left dock's active page.
/// - A dock at the old default size (240 / 300 px) gets the new one (280 / 350 px).
pub fn migrate_v1(state: &mut WorkbenchState) {
    migrate_layout_v1(&mut state.docks, &mut state.center);
    if let Some(saved) = state.stashed.as_deref_mut() {
        migrate_layout_v1(&mut saved.docks, &mut saved.center);
    }
    for parked in state.parked.values_mut() {
        migrate_layout_v1(&mut parked.layout.docks, &mut parked.layout.center);
        if let Some(saved) = parked.stashed.as_deref_mut() {
            migrate_layout_v1(&mut saved.docks, &mut saved.center);
        }
    }
}

fn migrate_layout_v1(docks: &mut Docks, center: &mut PaneTree) {
    let placed = |docks: &Docks, center: &PaneTree, panel: &PanelId| {
        docks.find(panel).is_some()
            || center
                .panes()
                .iter()
                .any(|pane| pane.items.iter().any(|i| i.as_panel() == Some(panel)))
    };
    let history = PanelId::new("history");
    let changes = PanelId::new("changes");
    let tasks = PanelId::new("tasks");
    let resources = PanelId::new("resources");
    let sessions = PanelId::new("sessions");

    // The Assistant is a left page now; a layout that still shows it loses the tab, not the
    // conversation (the daemon keeps it).
    for pos in DockPosition::ALL {
        docks
            .get_mut(pos)
            .panels
            .retain(|p| p.as_str() != "assistant");
    }
    for pane in center.panes_mut() {
        pane.items
            .retain(|i| !matches!(i, ItemRef::Panel { panel } if panel.as_str() == "assistant"));
        pane.active = pane.active.min(pane.items.len().saturating_sub(1));
    }

    // History → the right dock's first tab.
    let history_in_dock = matches!(
        docks.find(&history),
        Some(DockPosition::Left | DockPosition::Right)
    );
    if history_in_dock {
        for pos in [DockPosition::Left, DockPosition::Right] {
            docks.get_mut(pos).panels.retain(|p| *p != history);
        }
        docks.right.panels.insert(0, history.clone());
        docks.right.active = Some(history.clone());
    }

    // Tasks → the left sidebar, behind Projects.
    if docks.right.panels.contains(&tasks) {
        docks.right.panels.retain(|p| *p != tasks);
        let at = docks
            .left
            .panels
            .iter()
            .position(|p| *p == sessions)
            .map_or(0, |i| i + 1);
        docks.left.panels.insert(at, tasks.clone());
    }

    // Changes joins the right dock, after History when History is there.
    if !placed(docks, center, &changes) {
        let at = docks
            .right
            .panels
            .iter()
            .position(|p| *p == history)
            .map_or(0, |i| i + 1);
        docks
            .right
            .panels
            .insert(at.min(docks.right.panels.len()), changes);
    }

    // Resources → the right dock's strip unless the user opened the bottom dock for it.
    if !docks.bottom.visible && docks.bottom.panels.contains(&resources) {
        docks.bottom.panels.retain(|p| *p != resources);
        docks.right.panels.push(resources);
    }

    if docks.left.panels.contains(&sessions) {
        docks.left.active = Some(sessions);
    }
    // Sizes the user never changed follow the new defaults.
    if (docks.left.size - 240.0).abs() < f32::EPSILON {
        docks.left.size = LayoutPreset::LEFT_SIZE;
    }
    if (docks.right.size - 300.0).abs() < f32::EPSILON {
        docks.right.size = LayoutPreset::RIGHT_SIZE;
    }
}
