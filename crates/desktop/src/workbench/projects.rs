//! Managing user workspaces and projects from the sidebar (M18-B): making a workspace (from
//! nothing, with folders, or from the projects selected), putting a project into a workspace or
//! out of it (menus and drag), renaming inline, pinning, deleting a workspace while its projects
//! stay, opening a folder into a workspace, and the menus that offer all of it. Every change is
//! a request of protocol 0.19 (`workspace.create`, `workspace.assign`, `workspace.delete`, …); the
//! daemon owns the state, the desktop re-reads it from the events.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::{Context, Window};
use hutan_core::WorkspaceId;
use hutan_protocol::messages::{WorkspaceInfo, WorkspaceKind};

use super::modal::{Modal, ModalChoice, PromptSpec};
use super::panels::SessionsPanel;
use super::view::{MenuEntry, Region, Workbench};
use super::workspaces::close_blocker;
use crate::sessions::workspaces as order;
use crate::settings::{WorkspaceSort, WorkspacesSettings};

impl Workbench {
    // ---- the sidebar panel --------------------------------------------------------------------

    /// The projects selected in the sidebar (⌘-click, ⇧-click), in display order.
    pub fn selected_projects(&self, cx: &gpui::App) -> Vec<WorkspaceId> {
        self.panel::<SessionsPanel>("sessions")
            .map(|panel| panel.read(cx).selected_projects())
            .unwrap_or_default()
    }

    fn clear_project_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = self.panel::<SessionsPanel>("sessions") {
            panel.update(cx, |panel, cx| panel.clear_selection(cx));
        }
    }

    // ---- pin, rename, folders ------------------------------------------------------------------

    pub fn pin_workspace(
        &mut self,
        workspace: WorkspaceId,
        pinned: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .sessions
            .update(cx, |store, cx| store.pin_workspace(workspace, pinned, cx));
        self.report_unit(if pinned { "pin" } else { "unpin" }, task, window, cx);
    }

    /// Renames a workspace or project: inline in the sidebar when its row is on screen, else
    /// with a prompt.
    pub fn rename_workspace(
        &mut self,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = self.workspace_info(workspace, cx) else {
            return;
        };
        if self
            .region_bounds(Region::WorkspaceRow(workspace))
            .is_some()
            && let Some(panel) = self.panel::<SessionsPanel>("sessions")
        {
            panel.update(cx, |panel, cx| panel.begin_rename(workspace, window, cx));
            return;
        }
        let title = match info.kind {
            WorkspaceKind::Workspace => "Rename workspace",
            WorkspaceKind::Project => "Rename project",
            WorkspaceKind::Home => "Rename Home",
        };
        self.open_prompt(
            PromptSpec {
                title,
                message: "New name:",
                initial: &info.name,
                confirm_label: "Rename",
            },
            move |this, name, window, cx| this.rename_workspace_to(workspace, name, window, cx),
            window,
            cx,
        );
        self.select_modal_input(cx);
    }

    /// `workspace.rename`: the answer to the inline edit or the prompt.
    pub fn rename_workspace_to(
        &mut self,
        workspace: WorkspaceId,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The inline editor is gone: the workbench takes the keyboard back.
        self.focus_root(window, cx);
        let name = name.trim().to_owned();
        if name.is_empty() {
            return;
        }
        let task = self
            .sessions
            .update(cx, |store, cx| store.rename_workspace(workspace, name, cx));
        self.report_unit("rename", task, window, cx);
    }

    /// Asks for a folder with the system's panel and adds its project to `workspace`.
    pub fn add_workspace_folder(
        &mut self,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.choose_folder(
            "Add",
            move |this, path, window, cx| this.add_folder_at(workspace, path, window, cx),
            window,
            cx,
        );
    }

    /// `workspace.add_root`: the project of `path` (made when new) joins `workspace`.
    pub fn add_folder_at(
        &mut self,
        workspace: WorkspaceId,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self.sessions.update(cx, |store, cx| {
            store.add_workspace_root(workspace, path, cx)
        });
        self.report_unit("add the folder", task, window, cx);
    }

    /// Closes a project without live sessions after asking; it stays in the recent projects.
    /// A workspace is deleted instead (its projects stay), and Home cannot be closed.
    pub fn close_workspace(
        &mut self,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = self.workspace_info(workspace, cx) else {
            return;
        };
        if info.kind == WorkspaceKind::Workspace {
            self.delete_workspace(workspace, window, cx);
            return;
        }
        if let Some(reason) = close_blocker(&info) {
            self.set_notice(format!("Cannot close “{}”: {reason}", info.name), cx);
            return;
        }
        self.open_confirm(
            "Close project?",
            format!(
                "“{}” leaves the sidebar and its ended sessions are removed. It stays in the recent projects (⌘O).",
                info.name
            ),
            "Close",
            move |this, window, cx| {
                let task = this
                    .sessions
                    .update(cx, |store, cx| store.close_workspace(workspace, cx));
                this.report_unit("close the project", task, window, cx);
            },
            window,
            cx,
        );
    }

    pub fn reveal_workspace(&mut self, workspace: WorkspaceId, cx: &mut Context<Self>) {
        match self
            .workspace_info(workspace, cx)
            .and_then(|w| super::workspaces::project_root_of(&w))
        {
            Some(root) => (self.daemon.reveal.clone())(&root, cx),
            None => self.set_notice("This workspace has no folder", cx),
        }
    }

    // ---- workspaces: create, delete -------------------------------------------------------------

    /// The "New Workspace…" dialog: a name, then either the workspace as it is (it holds
    /// `projects`) or — when `projects` is empty — "Choose Folders…" to put the projects of some
    /// folders into it. `show` switches the window to the new workspace.
    pub fn new_workspace_dialog(
        &mut self,
        projects: Vec<WorkspaceId>,
        show: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let for_projects = projects.clone();
        let choice = projects.is_empty().then(|| ModalChoice {
            label: "Choose Folders…".into(),
            run: Rc::new(move |this, result, window, cx| {
                let name = result.input.unwrap_or_default().trim().to_owned();
                this.choose_folders(
                    "Add",
                    move |this, paths, window, cx| {
                        this.create_workspace(name.clone(), paths, Vec::new(), true, window, cx);
                    },
                    window,
                    cx,
                );
            }),
        });
        let message = if projects.is_empty() {
            "Name of the workspace. Folders can be added now or later (+ → Open Folder into Workspace)."
        } else {
            "Name of the workspace for the selected projects."
        };
        self.open_modal(
            Modal {
                title: "New workspace".into(),
                message: message.into(),
                confirm_label: "Create".into(),
                input: Some(String::new()),
                secure_input: false,
                choice,
                checkbox: None,
                on_confirm: Rc::new(move |this, result, window, cx| {
                    let name = result.input.unwrap_or_default().trim().to_owned();
                    this.create_workspace(name, Vec::new(), for_projects.clone(), show, window, cx);
                }),
            },
            window,
            cx,
        );
    }

    /// `workspace.create`: a workspace named `name` with the projects of `paths` and the existing
    /// `projects`; `show` switches the window to it.
    pub fn create_workspace(
        &mut self,
        name: String,
        paths: Vec<PathBuf>,
        projects: Vec<WorkspaceId>,
        show: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self.sessions.update(cx, |store, cx| {
            store.create_workspace(name, paths, projects, cx)
        });
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(created) => {
                this.update_in(cx, |this, window, cx| {
                    this.clear_project_selection(cx);
                    if show {
                        // Its `workspace.updated` may still be on its way.
                        this.pending_workspace = Some(created.id);
                        this.switch_workspace(created.id, window, cx);
                    }
                })
                .ok();
            }
            Err(err) => {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Cannot create the workspace: {err}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// "New Workspace from Selection…": a name for the workspace that takes `projects`.
    pub fn new_workspace_from(
        &mut self,
        projects: Vec<WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if projects.is_empty() {
            self.set_notice("Select projects first (⌘-click or ⇧-click)", cx);
            return;
        }
        self.new_workspace_dialog(projects, false, window, cx);
    }

    /// Deletes a workspace after asking; its projects and their sessions stay, ungrouped.
    pub fn delete_workspace(
        &mut self,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = self.workspace_info(workspace, cx) else {
            return;
        };
        if info.kind != WorkspaceKind::Workspace {
            self.set_notice(format!("“{}” is not a workspace", info.name), cx);
            return;
        }
        let projects = self
            .sessions
            .read(cx)
            .state()
            .projects_in(workspace)
            .count();
        let kept = match projects {
            0 => "It holds no projects.".to_owned(),
            1 => "Its project and the project's sessions are kept; it moves to “No workspace”."
                .to_owned(),
            n => format!(
                "Its {n} projects and their sessions are kept; they move to “No workspace”."
            ),
        };
        self.open_confirm(
            "Delete workspace?",
            format!("“{}” is removed. {kept}", info.name),
            "Delete Workspace",
            move |this, window, cx| {
                let task = this
                    .sessions
                    .update(cx, |store, cx| store.delete_workspace(workspace, cx));
                this.report_unit("delete the workspace", task, window, cx);
            },
            window,
            cx,
        );
    }

    // ---- projects: assign, open into a workspace --------------------------------------------------

    /// `workspace.assign`: `project` into `workspace`, or out of its workspace when `None`.
    pub fn assign_project(
        &mut self,
        project: WorkspaceId,
        workspace: Option<WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .sessions
            .update(cx, |store, cx| store.assign_project(project, workspace, cx));
        self.report_unit(
            if workspace.is_some() {
                "move the project"
            } else {
                "remove the project from its workspace"
            },
            task,
            window,
            cx,
        );
    }

    /// [`Self::assign_project`] for every one of `projects`, then forgets the selection.
    pub fn assign_projects(
        &mut self,
        projects: &[WorkspaceId],
        workspace: Option<WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for project in projects {
            self.assign_project(*project, workspace, window, cx);
        }
        self.clear_project_selection(cx);
    }

    /// A project row was dropped on `target`: a workspace (the project joins it), a project (it
    /// takes its place when both are in the same workspace — or in none — and the order is
    /// manual, else it joins the workspace of that project, or leaves its own), or the "No
    /// workspace" area (`None`: it leaves its workspace).
    pub fn drop_project(
        &mut self,
        dragged: WorkspaceId,
        target: Option<WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (project, onto) = {
            let store = self.sessions.read(cx);
            (
                store.workspace(dragged).cloned(),
                target.and_then(|id| store.workspace(id).cloned()),
            )
        };
        let Some(project) = project.filter(|p| p.kind == WorkspaceKind::Project) else {
            return;
        };
        match (target, onto) {
            // The "No workspace" area.
            (None, _) => {
                if project.parent_id.is_some() {
                    self.assign_project(dragged, None, window, cx);
                }
            }
            (Some(_), None) => {}
            (Some(workspace), Some(onto)) => match onto.kind {
                WorkspaceKind::Workspace => {
                    if project.parent_id != Some(workspace) {
                        self.assign_project(dragged, Some(workspace), window, cx);
                    }
                }
                WorkspaceKind::Project => {
                    if onto.parent_id == project.parent_id {
                        self.drop_workspace(dragged, workspace, window, cx);
                    } else {
                        self.assign_project(dragged, onto.parent_id, window, cx);
                    }
                }
                WorkspaceKind::Home => {}
            },
        }
    }

    /// A workspace row was dropped onto another one: reorder (manual order only).
    pub fn drop_workspace(
        &mut self,
        dragged: WorkspaceId,
        target: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.get::<WorkspacesSettings>().sort != WorkspaceSort::Manual {
            return;
        }
        let store = self.sessions.read(cx);
        let same_level = match (store.workspace(dragged), store.workspace(target)) {
            (Some(a), Some(b)) => {
                a.kind == b.kind && a.kind != WorkspaceKind::Home && a.parent_id == b.parent_id
            }
            _ => false,
        };
        if !same_level {
            return;
        }
        let Some(before) = order::reorder(store.workspaces(), dragged, target) else {
            return;
        };
        let task = self
            .sessions
            .update(cx, |store, cx| store.move_workspace(dragged, before, cx));
        self.report_unit("move", task, window, cx);
    }

    /// "Open Folder into Workspace": the system folder panel, then the folder's project joins
    /// `workspace` and is shown.
    pub fn open_folder_into_workspace(
        &mut self,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.choose_folder(
            "Open",
            move |this, path, window, cx| this.open_project_into(path, workspace, window, cx),
            window,
            cx,
        );
    }

    /// `workspace.open` for `path`, then `workspace.assign` into `workspace`, then shows it.
    pub fn open_project_into(
        &mut self,
        path: PathBuf,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let opening = self
            .sessions
            .update(cx, |store, cx| store.open_workspace(path, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let opened = opening
                    .await
                    .map_err(|err| format!("open the folder: {err}"))?;
                let project = opened.workspace;
                if project.kind != WorkspaceKind::Project {
                    return Err(format!("put {} into a workspace", project.name));
                }
                let assigning = this
                    .update(cx, |this, cx| {
                        this.sessions.update(cx, |store, cx| {
                            store.assign_project(project.id, Some(workspace), cx)
                        })
                    })
                    .map_err(|err| err.to_string())?;
                assigning
                    .await
                    .map_err(|err| format!("put the folder into the workspace: {err}"))?;
                Ok(project.id)
            }
            .await;
            match result {
                Ok(id) => {
                    this.update_in(cx, |this, window, cx| {
                        this.pending_workspace = Some(id);
                        this.switch_workspace(id, window, cx);
                    })
                    .ok();
                }
                Err(what) => {
                    this.update(cx, |this, cx| {
                        this.set_notice(format!("Cannot {what}"), cx);
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    // ---- menus ------------------------------------------------------------------------------------

    /// The user workspaces, in the sidebar's order.
    fn user_workspaces(&self, cx: &gpui::App) -> Vec<WorkspaceInfo> {
        let store = self.sessions.read(cx);
        let settings = self.settings.get::<WorkspacesSettings>();
        order::sort(
            store.workspaces(),
            settings.sort,
            &store.requests_by_workspace(),
        )
        .into_iter()
        .filter(|w| w.kind == WorkspaceKind::Workspace)
        .cloned()
        .collect()
    }

    /// A workspace or Home row's context menu: Pin / Unpin, Rename, Add Folder, Delete
    /// Workspace, Reveal in Finder.
    pub(crate) fn workspace_menu(&self, workspace: WorkspaceId, cx: &gpui::App) -> Vec<MenuEntry> {
        let Some(info) = self.workspace_info(workspace, cx) else {
            return Vec::new();
        };
        let pinned = info.pinned;
        let mut entries = vec![
            Self::entry(
                if pinned { "Unpin" } else { "Pin" },
                move |this, window, cx| this.pin_workspace(workspace, !pinned, window, cx),
            ),
            Self::entry("Rename…", move |this, window, cx| {
                this.rename_workspace(workspace, window, cx);
            }),
        ];
        match info.kind {
            WorkspaceKind::Workspace => {
                entries.push(Self::entry("Add Folder…", move |this, window, cx| {
                    this.add_workspace_folder(workspace, window, cx);
                }));
                entries.push(Self::entry(
                    "Delete Workspace…",
                    move |this, window, cx| this.delete_workspace(workspace, window, cx),
                ));
            }
            WorkspaceKind::Home => {
                entries.push(Self::disabled_entry("Add Folder… (Home has one folder)"));
            }
            WorkspaceKind::Project => {}
        }
        if info.kind != WorkspaceKind::Workspace {
            entries.push(Self::entry("Reveal in Finder", move |this, _, cx| {
                this.reveal_workspace(workspace, cx);
            }));
        } else if info.roots.is_empty() {
            entries.push(Self::disabled_entry("Reveal in Finder (no folders yet)"));
        } else {
            entries.push(Self::entry("Reveal in Finder", move |this, _, cx| {
                this.reveal_workspace(workspace, cx);
            }));
        }
        entries
    }

    /// A project row's context menu. With several projects selected (the clicked one among them)
    /// it offers what applies to all of them.
    pub(crate) fn project_menu(
        &self,
        project: WorkspaceId,
        selection: &[WorkspaceId],
        position: gpui::Point<gpui::Pixels>,
        cx: &gpui::App,
    ) -> Vec<MenuEntry> {
        let Some(info) = self.workspace_info(project, cx) else {
            return Vec::new();
        };
        if selection.len() > 1 && selection.contains(&project) {
            let (new_from, move_to, out_of) =
                (selection.to_vec(), selection.to_vec(), selection.to_vec());
            return vec![
                Self::entry(
                    "New Workspace from Selection…",
                    move |this, window, cx| {
                        this.new_workspace_from(new_from.clone(), window, cx);
                    },
                ),
                Self::submenu_entry("Move Selection to Workspace", move |this, window, cx| {
                    let entries = this.move_menu(&move_to, cx);
                    this.open_menu(position, entries, window, cx);
                }),
                Self::entry(
                    "Remove Selection from Workspace",
                    move |this, window, cx| {
                        this.assign_projects(&out_of, None, window, cx);
                    },
                ),
            ];
        }
        let pinned = info.pinned;
        let mut entries = vec![
            Self::entry("Rename…", move |this, window, cx| {
                this.rename_workspace(project, window, cx);
            }),
            Self::entry(
                if pinned { "Unpin" } else { "Pin" },
                move |this, window, cx| this.pin_workspace(project, !pinned, window, cx),
            ),
            Self::submenu_entry("Move to Workspace", move |this, window, cx| {
                let entries = this.move_menu(&[project], cx);
                this.open_menu(position, entries, window, cx);
            }),
        ];
        match info.parent_id {
            Some(_) => entries.push(Self::entry(
                "Remove from Workspace",
                move |this, window, cx| this.assign_project(project, None, window, cx),
            )),
            None => entries.push(Self::disabled_entry(
                "Remove from Workspace (in no workspace)",
            )),
        }
        entries.push(Self::open_file_entry());
        entries.push(Self::entry("Reveal in Finder", move |this, _, cx| {
            this.reveal_workspace(project, cx);
        }));
        match close_blocker(&info) {
            Some(reason) => entries.push(Self::disabled_entry(format!("Close ({reason})"))),
            None => entries.push(Self::entry("Close", move |this, window, cx| {
                this.close_workspace(project, window, cx);
            })),
        }
        entries
    }

    /// The "Move to Workspace" submenu: every workspace (the current one checked), and a new one.
    fn move_menu(&self, projects: &[WorkspaceId], cx: &gpui::App) -> Vec<MenuEntry> {
        let store = self.sessions.read(cx);
        let current: Vec<Option<WorkspaceId>> = projects
            .iter()
            .map(|id| store.workspace(*id).and_then(|p| p.parent_id))
            .collect();
        let mut entries: Vec<MenuEntry> = self
            .user_workspaces(cx)
            .into_iter()
            .map(|workspace| {
                let id = workspace.id;
                let targets = projects.to_vec();
                let mut entry = Self::entry(workspace.name.clone(), move |this, window, cx| {
                    this.assign_projects(&targets, Some(id), window, cx);
                });
                entry.checked = current.iter().all(|parent| *parent == Some(id));
                entry
            })
            .collect();
        let targets = projects.to_vec();
        entries.push(Self::entry("New Workspace…", move |this, window, cx| {
            this.new_workspace_dialog(targets.clone(), false, window, cx);
        }));
        entries
    }

    /// The "+" menu of the sidebar header: New Workspace…, Open Folder…, and Open Folder into
    /// Workspace.
    pub(crate) fn plus_menu(
        &self,
        position: gpui::Point<gpui::Pixels>,
        cx: &gpui::App,
    ) -> Vec<MenuEntry> {
        let workspaces = self.user_workspaces(cx);
        let mut entries = vec![
            Self::entry("New Workspace…", |this, window, cx| {
                this.new_workspace_dialog(Vec::new(), true, window, cx);
            }),
            Self::entry("Open Folder…", |this, window, cx| {
                this.open_folder_dialog(window, cx);
            }),
        ];
        if workspaces.is_empty() {
            entries.push(Self::disabled_entry(
                "Open Folder into Workspace (no workspaces yet)",
            ));
        } else {
            entries.push(Self::submenu_entry(
                "Open Folder into Workspace",
                move |this, window, cx| {
                    let entries: Vec<MenuEntry> = this
                        .user_workspaces(cx)
                        .into_iter()
                        .map(|workspace| {
                            let id = workspace.id;
                            Self::entry(workspace.name, move |this, window, cx| {
                                this.open_folder_into_workspace(id, window, cx);
                            })
                        })
                        .collect();
                    this.open_menu(position, entries, window, cx);
                },
            ));
        }
        entries
    }

    /// Opens the context menu of a container row: a workspace's or Home's, or a project's
    /// (alone).
    pub fn show_workspace_menu(
        &mut self,
        workspace: WorkspaceId,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_project = self
            .workspace_info(workspace, cx)
            .is_some_and(|info| info.kind == WorkspaceKind::Project);
        let entries = if is_project {
            self.project_menu(workspace, &[workspace], position, cx)
        } else {
            self.workspace_menu(workspace, cx)
        };
        if !entries.is_empty() {
            self.open_menu(position, entries, window, cx);
        }
    }

    /// Opens the context menu of a project row (`selection`: the projects selected with it).
    pub fn show_project_menu(
        &mut self,
        project: WorkspaceId,
        selection: &[WorkspaceId],
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = self.project_menu(project, selection, position, cx);
        if !entries.is_empty() {
            self.open_menu(position, entries, window, cx);
        }
    }

    /// Opens the "+" menu of the sidebar header.
    pub fn show_plus_menu(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = self.plus_menu(position, cx);
        self.open_menu(position, entries, window, cx);
    }

    /// What "Add Folder to Active Workspace…" does for the container on screen.
    pub(crate) fn add_folder_to_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_workspace_info(cx) {
            Some(info) if info.kind == WorkspaceKind::Workspace => {
                self.add_workspace_folder(info.id, window, cx);
            }
            Some(info) => {
                self.set_notice(
                    format!(
                        "{} is {}; add folders to a workspace (+ → New Workspace…)",
                        info.name,
                        match info.kind {
                            WorkspaceKind::Home => "Home, which has one folder (see daemon.toml)",
                            _ => "a project",
                        }
                    ),
                    cx,
                );
            }
            None => {}
        }
    }
}
