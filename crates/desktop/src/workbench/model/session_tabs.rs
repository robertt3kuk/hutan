//! One session, one tab: merging the duplicate tabs a saved layout may hold.
//!
//! The layout stores a tab per session as [`ItemRef::Terminal`] or [`ItemRef::Chat`], decided
//! when the tab was opened, and Mawas Home is a tab of its own (the panel `assistant`) that shows
//! a Mawas conversation. Older versions opened the same Mawas conversation both ways, and a tab
//! opened before the daemon had said what the session is became a terminal tab of what turned
//! out to be a chat. Once the daemon has said what every session is, [`WorkbenchState::merge_session_tabs`]
//! puts the layout right: a Mawas conversation is shown by Home only, a session has the tab kind
//! that matches it, and a pane never holds two tabs of one session.
//!
//! Tabs of one session in different panes are kept: those are splits the user made on purpose.

use std::collections::HashSet;

use super::{ItemRef, PaneTree, PanelId, WorkbenchState};

/// What shows a session, as the daemon's session list says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTab {
    /// A terminal tab.
    Terminal,
    /// A chat tab (an agent conversation).
    Chat,
    /// A Mawas conversation: Mawas Home shows it, it has no tab of its own.
    Mawas,
}

/// What [`WorkbenchState::merge_session_tabs`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergedTabs {
    /// The layout changed.
    pub changed: bool,
    /// Sessions of the shown layout whose own tab went to Mawas Home, the first one first.
    pub mawas: Vec<String>,
    /// Home had no tab in the shown layout and took the place of the first of those.
    pub home_added: bool,
}

impl WorkbenchState {
    /// Merges duplicate session tabs in the shown layout and in the overview's stash (see the
    /// module documentation). `kind_of` answers what a session is; `None` (a session the daemon
    /// does not know) leaves its tabs alone.
    pub fn merge_session_tabs(
        &mut self,
        home: &PanelId,
        kind_of: &dyn Fn(&str) -> Option<SessionTab>,
    ) -> MergedTabs {
        let mut report = MergedTabs::default();
        let home_open = self.docks.find(home).is_some();
        let live = merge_center(&mut self.center, home, home_open, kind_of);
        report.changed = live.changed;
        report.mawas = live.mawas;
        report.home_added = live.home_added;
        if let Some(saved) = self.stashed.as_mut() {
            let home_open = saved.docks.find(home).is_some();
            report.changed |= merge_center(&mut saved.center, home, home_open, kind_of).changed;
        }
        if report.changed {
            self.normalize();
        }
        report
    }
}

fn merge_center(
    center: &mut PaneTree,
    home: &PanelId,
    home_in_dock: bool,
    kind_of: &dyn Fn(&str) -> Option<SessionTab>,
) -> MergedTabs {
    let mut report = MergedTabs::default();
    let mut home_open = home_in_dock
        || center
            .panes()
            .iter()
            .any(|pane| pane.items.iter().any(|item| item.as_panel() == Some(home)));
    for pane in center.panes_mut() {
        let before = pane.items.clone();
        let active = pane.active;
        let mut kept: Vec<ItemRef> = Vec::with_capacity(before.len());
        let mut seen: HashSet<String> = HashSet::new();
        // Where the active tab went: its own slot, its twin's, or (when it was folded into Home)
        // the next tab that survives.
        let mut active_at: Option<usize> = None;
        let mut active_dropped = false;
        for (index, item) in before.iter().cloned().enumerate() {
            let session = match &item {
                ItemRef::Terminal { session } | ItemRef::Chat { session } => Some(session.clone()),
                _ => None,
            };
            let slot = match session {
                None => Some(item),
                Some(session) => match kind_of(&session) {
                    Some(SessionTab::Mawas) => {
                        report.mawas.push(session);
                        if home_open {
                            None
                        } else {
                            home_open = true;
                            report.home_added = true;
                            Some(ItemRef::Panel {
                                panel: home.clone(),
                            })
                        }
                    }
                    Some(SessionTab::Chat) => Some(ItemRef::Chat { session }),
                    Some(SessionTab::Terminal) => Some(ItemRef::Terminal { session }),
                    None => Some(item),
                },
            };
            let Some(slot) = slot else {
                active_dropped |= index == active;
                continue;
            };
            if let ItemRef::Terminal { session } | ItemRef::Chat { session } = &slot
                && !seen.insert(session.clone())
            {
                // The second tab of a session in this pane: the first one stays.
                if index == active {
                    active_at = kept.iter().position(|other| {
                        matches!(other, ItemRef::Terminal { session: s } | ItemRef::Chat { session: s } if s == session)
                    });
                }
                continue;
            }
            if index == active || (active_dropped && active_at.is_none()) {
                active_at = Some(kept.len());
            }
            kept.push(slot);
        }
        if kept != before {
            report.changed = true;
            pane.active = active_at.unwrap_or_else(|| kept.len().saturating_sub(1));
            pane.items = kept;
        }
    }
    report
}
