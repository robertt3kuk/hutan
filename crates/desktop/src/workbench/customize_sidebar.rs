//! The left sidebar's sections, hidden and reordered where they are (WIDGET-CONFIG item 4): the
//! nav rows (Agents, Mawas, Memory, Tasks) and the projects list. They live in the `sidebar`
//! settings (`nav_rows`, `show_projects`); the right-click menu of the sidebar's nav, its
//! compact rail and the Projects header's ⋯ change them, with an Undo like every other change.

use gpui::{Context, Pixels, Point};

use super::model::MEMORY_PAGE;
use super::view::{MenuEntry, Workbench};
use crate::settings::SidebarSettings;

/// What the sections menu calls a nav row.
fn row_title(row: &str) -> &str {
    match row {
        "agents" => "Agents",
        "assistant" => "Mawas",
        "memory" => "Memory",
        "tasks" => "Tasks",
        other => other,
    }
}

/// The section a nav entry (by its key) belongs to: Memory's key is its panel id.
pub(crate) fn section_of(key: &str) -> &str {
    if key == MEMORY_PAGE { "memory" } else { key }
}

fn rows_value(rows: &[String]) -> toml::Value {
    toml::Value::Array(rows.iter().cloned().map(toml::Value::String).collect())
}

impl Workbench {
    /// The sections of the left sidebar as a checklist: each nav row and the projects list
    /// (checked while shown), *Move … up* for the shown rows below the first, and the reset.
    /// A choice keeps the list open at `position`.
    pub(crate) fn sidebar_sections_entries(&self, position: Point<Pixels>) -> Vec<MenuEntry> {
        let settings = self.settings.get::<SidebarSettings>();
        let mut entries = vec![Self::disabled_entry("Sidebar sections")];
        for row in SidebarSettings::NAV_ROWS {
            let shown = settings.nav_rows.iter().any(|r| r == row);
            let mut entry = Self::entry(row_title(row), move |this, window, cx| {
                this.toggle_sidebar_row(row, cx);
                let entries = this.left_checklist(position);
                this.open_menu(position, entries, window, cx);
            });
            entry.checked = shown;
            entries.push(entry);
        }
        let mut list = Self::entry("Projects list", move |this, window, cx| {
            this.toggle_projects_list(cx);
            let entries = this.left_checklist(position);
            this.open_menu(position, entries, window, cx);
        });
        list.checked = settings.show_projects;
        entries.push(list);
        for (index, row) in settings
            .nav_rows
            .iter()
            .enumerate()
            .filter(|(index, _)| *index > 0)
        {
            let row = row.clone();
            entries.push(Self::entry(
                format!("Move {} up", row_title(&row)),
                move |this, window, cx| {
                    this.move_sidebar_row_up(&row, cx);
                    let entries = this.left_checklist(position);
                    this.open_menu(position, entries, window, cx);
                },
            ));
            let _ = index;
        }
        entries
    }

    /// The left sidebar's checklist: its pages, then its sections.
    pub(crate) fn left_checklist(&self, position: Point<Pixels>) -> Vec<MenuEntry> {
        let mut entries = self.dock_checklist(super::model::DockPosition::Left, position);
        // The reset stays last.
        let reset = entries.pop();
        entries.extend(self.sidebar_sections_entries(position));
        entries.extend(reset);
        entries
    }

    /// Hides `row` while it shows, shows it at its default place otherwise.
    pub fn toggle_sidebar_row(&mut self, row: &str, cx: &mut Context<Self>) {
        let rows = self.settings.get::<SidebarSettings>().nav_rows.clone();
        let shown = rows.iter().any(|r| r == row);
        let mut next = rows.clone();
        if shown {
            next.retain(|r| r != row);
        } else {
            // Keep the default order among the rows that show.
            let rank = |r: &str| SidebarSettings::NAV_ROWS.iter().position(|d| *d == r);
            let at = next
                .iter()
                .position(|r| rank(r) > rank(row))
                .unwrap_or(next.len());
            next.insert(at, row.to_owned());
        }
        let verb = if shown { "Hid" } else { "Showed" };
        self.customize_keys(
            format!("{verb} {} in the sidebar", row_title(row)),
            vec![("sidebar.nav_rows".to_owned(), Some(rows_value(&next)))],
            cx,
        );
    }

    /// Moves the shown row `row` one place up.
    pub fn move_sidebar_row_up(&mut self, row: &str, cx: &mut Context<Self>) {
        let mut rows = self.settings.get::<SidebarSettings>().nav_rows.clone();
        let Some(at) = rows.iter().position(|r| r == row).filter(|at| *at > 0) else {
            return;
        };
        rows.swap(at - 1, at);
        self.customize_keys(
            format!("Moved {} up in the sidebar", row_title(row)),
            vec![("sidebar.nav_rows".to_owned(), Some(rows_value(&rows)))],
            cx,
        );
    }

    /// Hides or shows the projects list under the nav.
    pub fn toggle_projects_list(&mut self, cx: &mut Context<Self>) {
        let shown = self.settings.get::<SidebarSettings>().show_projects;
        let verb = if shown { "Hid" } else { "Showed" };
        self.customize_keys(
            format!("{verb} the projects list"),
            vec![(
                "sidebar.show_projects".to_owned(),
                Some(toml::Value::Boolean(!shown)),
            )],
            cx,
        );
    }

    /// The nav's rows as the sidebar settings say: the rows left out are hidden (unless their
    /// page is the one showing), the others in the order of the list, then whatever else the
    /// left dock holds.
    pub(crate) fn arrange_nav(
        &self,
        entries: Vec<super::sidebars::NavEntry>,
    ) -> Vec<super::sidebars::NavEntry> {
        let rows = self.settings.get::<SidebarSettings>().nav_rows.clone();
        let governed = |key: &str| SidebarSettings::NAV_ROWS.contains(&section_of(key));
        let (mut mine, others): (Vec<_>, Vec<_>) =
            entries.into_iter().partition(|e| governed(&e.key));
        mine.retain(|e| e.selected || rows.iter().any(|r| r == section_of(&e.key)));
        let place = |e: &super::sidebars::NavEntry| {
            rows.iter()
                .position(|r| r == section_of(&e.key))
                .unwrap_or(rows.len())
        };
        mine.sort_by_key(place);
        mine.extend(others);
        mine
    }
}
