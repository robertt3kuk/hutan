//! Versioned JSON (de)serialization of [`WorkbenchState`].

use super::{
    AGENTS_PAGE, DockPosition, Docks, ItemRef, LayoutPreset, MEMORY_PAGE, ModelError, PaneTree,
    PanelId, WorkbenchState,
};

/// The saved layout's version. 2 is the Orca-style relayout (OL-2): Projects, Tasks and the
/// Agents page as pages of the left sidebar, History / Changes / Requests / Agents / Resources
/// in the right sidebar's strip, no Assistant in any dock. 3 (OL-4) turns Agents from a left page
/// into the dashboard drawer: the `overview` panel leaves the left dock. 4 (MW-1) turns the
/// Assistant from a left page into Mawas Home, a tab of the center: the `assistant` panel leaves
/// every dock. 5 (DK-1) gives the side docks a rail: a dock now has a `collapsed` flag next to
/// `visible` (visible + collapsed is the 44 pt left icon rail or the right activity rail alone,
/// hidden keeps the mode it comes back in), and the side sizes have 220–500 / 240–480 limits. 6
/// (AG-1) replaces the `overview` panel (the old board of terminal
/// previews) with the Agents page, `agents-dashboard`: every reference to `overview` in a layout
/// (a center tab, a dock's panel, the Agents overview stash, a parked workspace) now names the
/// page, and the `overview` preset keeps its name. A version 1 file is migrated once when it is
/// read (see [`migrate_v1`], then [`migrate_v2`], then [`migrate_v3`]), a version 2 file by
/// [`migrate_v2`] and [`migrate_v3`], a version 3 file by [`migrate_v3`]; a version 4 file needs
/// no rewrite (`visible` already is Expanded/Hidden, `collapsed` defaults to off, the sizes are
/// clamped like every loaded layout); every file older than 6 goes through [`migrate_v5`]. 7
/// (MEM-SIDEBAR) makes Mawas's Memory a body of the left sidebar: a `mawas-memory` tab of the
/// center moves into the left dock ([`migrate_v6`]). 8 (SB-ATTACH) puts Files in the right
/// dock: a layout whose left dock is the built-in one loses `files` there and gets it at the
/// front of the right dock ([`migrate_v7`]); a Files panel the user placed stays. 9
/// (FX-CHANGES-VIEW) gives Files to every layout that has it nowhere: a layout saved before Files
/// existed (the first release's left dock was Projects and Tasks) never named it, and a saved
/// layout wins over the presets, so Files was missing for good; such a layout gets `files` at the
/// front of the right dock ([`migrate_v8`]) and one note, once. A Files panel hidden in a
/// version 9 layout stays hidden. 10 (CONTEXT-PANEL) adds the Context panel to the rail: a layout
/// whose right dock lists `changes` and has `context` nowhere gets it right after Changes
/// ([`migrate_v9`]); the user's order and a Context panel they placed or closed stay. The next
/// save writes version 10.
pub const STATE_VERSION: u32 = 10;

/// What loading a saved layout changed that the user is told once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutNote {
    /// The layout had no Files panel anywhere: it is on the right rail now ([`migrate_v8`]).
    FilesRestored,
}

/// Result of reading a saved state.
#[derive(Debug)]
pub struct LoadOutcome {
    pub state: WorkbenchState,
    /// Set when the saved data was unusable and the default layout was substituted.
    pub warning: Option<String>,
    /// What loading changed that the user is told once.
    pub notes: Vec<LayoutNote>,
}

impl WorkbenchState {
    /// `{ "version": 10, "docks": …, "center": …, … }`, pretty-printed.
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
        Self::from_json_noted(json).map(|(state, _)| state)
    }

    /// [`Self::from_json`], with what the migrations changed that the user is told once.
    pub fn from_json_noted(json: &str) -> Result<(Self, Vec<LayoutNote>), ModelError> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|e| ModelError::Invalid(e.to_string()))?;
        let version = match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(v @ (1..=10)) => v,
            Some(v) => return Err(ModelError::Invalid(format!("unsupported version {v}"))),
            None => return Err(ModelError::Invalid("missing version".to_owned())),
        };
        let mut state: WorkbenchState =
            serde_json::from_value(value).map_err(|e| ModelError::Invalid(e.to_string()))?;
        if version == 1 {
            migrate_v1(&mut state);
        }
        if version <= 2 {
            migrate_v2(&mut state);
        }
        if version <= 3 {
            migrate_v3(&mut state);
        }
        if version <= 5 {
            migrate_v5(&mut state);
        }
        if version <= 6 {
            migrate_v6(&mut state);
        }
        if version <= 7 {
            migrate_v7(&mut state);
        }
        let mut notes = Vec::new();
        if version <= 8 && migrate_v8(&mut state) {
            notes.push(LayoutNote::FilesRestored);
        }
        if version <= 9 {
            migrate_v9(&mut state);
        }
        state.normalize();
        Ok((state, notes))
    }

    /// Like [`Self::from_json`] but falls back to the default layout with a warning.
    pub fn from_json_or_default(json: &str) -> LoadOutcome {
        match Self::from_json_noted(json) {
            Ok((state, notes)) => LoadOutcome {
                state,
                warning: None,
                notes,
            },
            Err(err) => LoadOutcome {
                state: WorkbenchState::default(),
                warning: Some(format!("saved layout ignored: {err}")),
                notes: Vec::new(),
            },
        }
    }
}

/// The one-time addition of the Context panel to a layout older than version 10 (the live layout,
/// the overview stash and every parked workspace layout): a right dock that lists `changes` gets
/// `context` right after it, unless the layout has Context somewhere already. Nothing moves or
/// goes; a layout whose right dock has no Changes panel (the user took it out) is left alone.
/// Logged once per read, only when something was added.
pub fn migrate_v9(state: &mut WorkbenchState) {
    let mut added = usize::from(ensure_context(&mut state.docks, &state.center));
    if let Some(saved) = state.stashed.as_deref_mut() {
        added += usize::from(ensure_context(&mut saved.docks, &saved.center));
    }
    for parked in state.parked.values_mut() {
        added += usize::from(ensure_context(
            &mut parked.layout.docks,
            &parked.layout.center,
        ));
        if let Some(saved) = parked.stashed.as_deref_mut() {
            added += usize::from(ensure_context(&mut saved.docks, &saved.center));
        }
    }
    if added > 0 {
        tracing::info!(
            layouts = added,
            "saved layout: the Context panel joined the right rail after Changes"
        );
    }
}

/// Puts `context` after `changes` in the right dock unless the layout has it somewhere; whether
/// it did.
fn ensure_context(docks: &mut Docks, center: &PaneTree) -> bool {
    let context = PanelId::new("context");
    let in_center = center
        .panes()
        .iter()
        .any(|pane| pane.items.iter().any(|i| i.as_panel() == Some(&context)));
    if docks.find(&context).is_some() || in_center {
        return false;
    }
    let changes = PanelId::new("changes");
    let Some(at) = docks.right.panels.iter().position(|p| *p == changes) else {
        return false;
    };
    docks.right.panels.insert(at + 1, context);
    true
}

/// The one-time move of a layout older than version 7 to Memory in the sidebar: a `mawas-memory`
/// tab of the center (the live layout, the Agents overview stash, every parked workspace layout)
/// is removed and the page joins the left dock instead, which shows it when the tab was the one
/// on screen in its pane. Logged once per read, only when something moved.
pub fn migrate_v6(state: &mut WorkbenchState) {
    let mut moved = usize::from(memory_to_sidebar(&mut state.docks, &mut state.center));
    if let Some(saved) = state.stashed.as_deref_mut() {
        moved += usize::from(memory_to_sidebar(&mut saved.docks, &mut saved.center));
    }
    for parked in state.parked.values_mut() {
        moved += usize::from(memory_to_sidebar(
            &mut parked.layout.docks,
            &mut parked.layout.center,
        ));
        if let Some(saved) = parked.stashed.as_deref_mut() {
            moved += usize::from(memory_to_sidebar(&mut saved.docks, &mut saved.center));
        }
    }
    if moved > 0 {
        tracing::info!(
            layouts = moved,
            "saved layout: the Memory tab of the center moved into the left sidebar"
        );
    }
}

/// The one-time move of a layout older than version 8 to Files in the right dock (SB-ATTACH:
/// Orca puts the explorer on the right): a layout whose left dock is exactly the built-in
/// `sessions`, `tasks`, `files` (a Memory page beside them does not count; the live layout, the overview stash and every parked workspace
/// layout) gets `files` at the front of its right dock instead. A Files panel anywhere else, or
/// in a left dock the user arranged, is where the user put it and stays. Logged once per read,
/// only when something moved.
pub fn migrate_v7(state: &mut WorkbenchState) {
    let mut moved = usize::from(files_to_right(&mut state.docks));
    if let Some(saved) = state.stashed.as_deref_mut() {
        moved += usize::from(files_to_right(&mut saved.docks));
    }
    for parked in state.parked.values_mut() {
        moved += usize::from(files_to_right(&mut parked.layout.docks));
        if let Some(saved) = parked.stashed.as_deref_mut() {
            moved += usize::from(files_to_right(&mut saved.docks));
        }
    }
    if moved > 0 {
        tracing::info!(
            layouts = moved,
            "saved layout: Files moved from the left sidebar to the right dock"
        );
    }
}

/// The one-time repair of a layout older than version 9 that has no Files panel anywhere (the
/// live layout, the overview stash and every parked workspace layout): `files` joins the front
/// of its right dock. A layout that has Files in any dock or as a center tab is left as it is.
/// Returns whether the live layout got it (that is what the note is about). Logged once per read,
/// only when something moved.
pub fn migrate_v8(state: &mut WorkbenchState) -> bool {
    let live = ensure_files(&mut state.docks, &state.center);
    let mut added = usize::from(live);
    if let Some(saved) = state.stashed.as_deref_mut() {
        added += usize::from(ensure_files(&mut saved.docks, &saved.center));
    }
    for parked in state.parked.values_mut() {
        added += usize::from(ensure_files(
            &mut parked.layout.docks,
            &parked.layout.center,
        ));
        if let Some(saved) = parked.stashed.as_deref_mut() {
            added += usize::from(ensure_files(&mut saved.docks, &saved.center));
        }
    }
    if added > 0 {
        tracing::info!(
            layouts = added,
            "saved layout: Files was in no dock; it is on the right rail now"
        );
    }
    live
}

/// Puts `files` at the front of the right dock unless the layout has it somewhere; whether it did.
fn ensure_files(docks: &mut Docks, center: &PaneTree) -> bool {
    let files = PanelId::new("files");
    let in_center = center
        .panes()
        .iter()
        .any(|pane| pane.items.iter().any(|i| i.as_panel() == Some(&files)));
    if docks.find(&files).is_some() || in_center {
        return false;
    }
    docks.right.panels.insert(0, files);
    true
}

/// Moves the center's Memory tabs into the left dock; whether there was one.
fn memory_to_sidebar(docks: &mut Docks, center: &mut PaneTree) -> bool {
    let page = PanelId::new(MEMORY_PAGE);
    let mut found = false;
    let mut shown = false;
    for pane in center.panes_mut() {
        while let Some(index) = pane.items.iter().position(|i| i.as_panel() == Some(&page)) {
            pane.items.remove(index);
            found = true;
            shown |= index == pane.active;
            if index < pane.active {
                pane.active -= 1;
            }
            pane.active = pane.active.min(pane.items.len().saturating_sub(1));
        }
    }
    if !found {
        return false;
    }
    let left = docks.get_mut(DockPosition::Left);
    if !left.panels.contains(&page) {
        left.panels.push(page.clone());
    }
    if shown {
        left.active = Some(page);
    }
    true
}

/// Moves `files` from the built-in left dock to the front of the right one; whether it did.
fn files_to_right(docks: &mut Docks) -> bool {
    let files = PanelId::new("files");
    let builtin = ["sessions", "tasks", "files"].map(PanelId::new);
    // A Memory page that just joined the sidebar (version 7) does not make it the user's own.
    let memory = PanelId::new(MEMORY_PAGE);
    let left: Vec<PanelId> = docks
        .left
        .panels
        .iter()
        .filter(|p| **p != memory)
        .cloned()
        .collect();
    if left != builtin || docks.right.panels.contains(&files) {
        return false;
    }
    docks.left.panels.retain(|p| *p != files);
    if docks.left.active.as_ref() == Some(&files) {
        docks.left.active = docks.left.panels.first().cloned();
    }
    docks.right.panels.insert(0, files);
    true
}

/// The one-time move of a layout older than version 6 to the Agents page: the `overview` panel
/// (the board of terminal previews) is the `agents-dashboard` page now, wherever the layout had
/// it — a center tab, a dock's panel, the Agents overview stash, a parked workspace layout —
/// so no tab and no dock entry is lost. A layout that ends up with the page twice keeps the
/// first (`normalize` places a panel once).
pub fn migrate_v5(state: &mut WorkbenchState) {
    rename_overview(&mut state.docks, &mut state.center);
    if let Some(saved) = state.stashed.as_deref_mut() {
        rename_overview(&mut saved.docks, &mut saved.center);
    }
    for parked in state.parked.values_mut() {
        rename_overview(&mut parked.layout.docks, &mut parked.layout.center);
        if let Some(saved) = parked.stashed.as_deref_mut() {
            rename_overview(&mut saved.docks, &mut saved.center);
        }
    }
}

fn rename_overview(docks: &mut Docks, center: &mut PaneTree) {
    let old = PanelId::new("overview");
    let new = PanelId::new(AGENTS_PAGE);
    for pos in DockPosition::ALL {
        let dock = docks.get_mut(pos);
        for panel in dock.panels.iter_mut().filter(|p| **p == old) {
            *panel = new.clone();
        }
        if dock.active.as_ref() == Some(&old) {
            dock.active = Some(new.clone());
        }
    }
    for pane in center.panes_mut() {
        for item in &mut pane.items {
            if item.as_panel() == Some(&old) {
                *item = ItemRef::panel(AGENTS_PAGE);
            }
        }
    }
}

/// The one-time move of a version 3 layout to Mawas Home: the `assistant` panel, which was a page
/// of the left sidebar, is dropped from every dock (the live layout, the overview stash and every
/// parked workspace layout). Home is a tab of the center now and opens from the nav; a tab
/// already in the center stays.
pub fn migrate_v3(state: &mut WorkbenchState) {
    drop_assistant_page(&mut state.docks);
    if let Some(saved) = state.stashed.as_deref_mut() {
        drop_assistant_page(&mut saved.docks);
    }
    for parked in state.parked.values_mut() {
        drop_assistant_page(&mut parked.layout.docks);
        if let Some(saved) = parked.stashed.as_deref_mut() {
            drop_assistant_page(&mut saved.docks);
        }
    }
}

fn drop_assistant_page(docks: &mut Docks) {
    for pos in DockPosition::ALL {
        let dock = docks.get_mut(pos);
        dock.panels.retain(|p| p.as_str() != "assistant");
        if dock
            .active
            .as_ref()
            .is_some_and(|a| a.as_str() == "assistant")
        {
            dock.active = dock.panels.first().cloned();
        }
    }
}

/// The one-time move of a version 2 layout to the Agent Dashboard drawer: the `overview` panel,
/// which was the Agents page of the left sidebar, is dropped from the left dock (the live layout,
/// the overview stash and every parked workspace layout). The overview board itself stays a
/// preset (⌘⇧O) and is not touched where it is a center item or lives in another dock.
pub fn migrate_v2(state: &mut WorkbenchState) {
    drop_agents_page(&mut state.docks);
    if let Some(saved) = state.stashed.as_deref_mut() {
        drop_agents_page(&mut saved.docks);
    }
    for parked in state.parked.values_mut() {
        drop_agents_page(&mut parked.layout.docks);
        if let Some(saved) = parked.stashed.as_deref_mut() {
            drop_agents_page(&mut saved.docks);
        }
    }
}

fn drop_agents_page(docks: &mut Docks) {
    let left = &mut docks.left;
    left.panels.retain(|p| p.as_str() != "overview");
    if left
        .active
        .as_ref()
        .is_some_and(|a| a.as_str() == "overview")
    {
        left.active = left.panels.first().cloned();
    }
}

/// The one-time move of a version 1 layout to the Orca-style arrangement, applied to the live
/// layout, the overview stash and every parked workspace layout. The center (tabs, splits, zoom,
/// focus), the preset name and every size the user changed stay as they were.
///
/// - History moves to the front of the right dock and becomes its active tab (a History tab in
///   the center or the bottom dock stays where the user put it).
/// - The Assistant leaves every dock and the center (it was a left page that opens on demand;
///   [`migrate_v3`] later makes it a tab of the center).
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
