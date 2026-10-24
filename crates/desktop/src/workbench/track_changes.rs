//! *Track changes in this folder…*: puts the Home folder (where Mawas works, and where a
//! terminal at Home starts) under version control, so Changes, checkpoints (*Rewind…* on
//! Mawas's turns) and Finish work there like in any repository.
//!
//! Offered by the Changes panel at Home (while its listing is empty) and Mawas Home's ⋯ menu
//! (Global scope), both through [`actions::TrackHomeChanges`]. A confirm sheet says what
//! happens; the daemon does it (`project.git_init`, user-only) and refuses what must not be
//! tracked, which the notice reports as the daemon words it.

use gpui::{Context, Window};

use super::panels::ChangesPanel;
use super::view::Workbench;
use super::workspaces::tilde;

/// What the confirm sheet tells the user before anything happens.
fn explanation(folder: &str) -> String {
    format!(
        "Hutan will run git init in {folder}, add a .gitignore with Hutan's own scratch \
         files, and commit what is there now as the first commit, with your git identity.\n\n\
         After that, Changes shows what Mawas (or you) edit there, Mawas's turns can be \
         rewound, and Finish commits. Nothing leaves this machine: there is no remote."
    )
}

impl Workbench {
    /// *Track changes in this folder…*: the confirm sheet, then `project.git_init` of the Home
    /// folder. Says why when it is not on offer.
    pub fn track_home_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let folder = {
            let store = self.sessions.read(cx);
            if !store.can_track_home() {
                let why = if store.home_is_repository() {
                    "The Home folder already tracks its changes"
                } else if !store.connection().supports_project_git_init() {
                    "This daemon cannot track changes in a folder (update hutand)"
                } else {
                    "The Home folder is not known yet"
                };
                self.set_notice(why, cx);
                return;
            }
            store.home_folder()
        };
        let Some(folder) = folder else {
            return;
        };
        self.open_confirm(
            "Track changes in this folder?",
            explanation(&tilde(&folder)),
            "Track Changes",
            |this, window, cx| this.start_tracking_home(window, cx),
            window,
            cx,
        );
    }

    /// Asks the daemon to put the Home folder under version control and refreshes what shows
    /// checkouts afterwards.
    fn start_tracking_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = self.sessions.update(cx, |store, cx| store.track_home(cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(done) => {
                    this.sessions
                        .update(cx, |store, cx| store.refresh_checkouts(cx));
                    if let Some(panel) = this.panel::<ChangesPanel>("changes") {
                        panel.update(cx, |panel, cx| panel.refresh(cx));
                    }
                    this.set_notice(
                        format!(
                            "Tracking changes in {} · {} files in the first commit",
                            tilde(&done.root),
                            done.files
                        ),
                        cx,
                    );
                }
                Err(err) => {
                    this.set_notice(format!("Cannot track changes: {err}"), cx);
                }
            })
            .ok();
        })
        .detach();
    }
}
