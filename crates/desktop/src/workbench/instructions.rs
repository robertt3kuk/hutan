//! Instruction files in the workbench (AGENT-INSTRUCTIONS, `docs/agents.md` § Instruction
//! files): what the daemon says is in the project ([`gather`]), *Create AGENTS.md*, *Share one
//! file with every agent…* (a confirm sheet that shows what it writes, the writes through
//! `fs.write`, a ten-second Undo toast), and the toast itself.
//!
//! Hutan writes only project files, only after the sheet (or the explicit *Create AGENTS.md*),
//! never through a symbolic link, and never the user's own files in `~/.codex`, `~/.claude` or
//! `~/.omp`. A file that changed after the sheet opened is refused (`fs.write` with the version
//! the plan read); a failed write puts back what was already written.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, AppContext as _, Context, InteractiveElement, IntoElement, ParentElement,
    SharedString, Styled, Window, div, px,
};
use hutan_client::{ClientError, Connection};
use hutan_protocol::fs::{FileVersion, FsFind, FsKind, FsList, FsWriteBegin};

use super::actions::{ChosenFile, OpenChosenFile};
use super::files::{FilePlace, error_text};
use super::ui::motion;
use super::ui::{Button, StyledExt as _};
use super::view::{Region, Workbench};
use crate::instructions::{
    Change, Existing, Facts, HarnessConfig, HarnessHomes, PlannedWrite, ProjectFile, STARTER,
    ShareInput, SharePlan, UserFile, imports_of, plan_share, preview, project_names,
    user_candidates,
};
use crate::theme::DensityExt as _;

/// How long the Undo toast shows.
pub const TOAST_TIME: Duration = Duration::from_secs(10);
/// Files the panel looks at below the project's folder.
const NESTED_MAX: usize = 100;
/// The file size above which an `@import` scan does not read the file.
const SCAN_MAX_BYTES: u64 = 256 * 1024;
/// Lines of each file the sheet shows.
const PREVIEW_LINES: usize = 10;

/// "Shared one file with every agent · Undo".
#[derive(Debug, Clone)]
pub struct InstructionsToast {
    pub text: SharedString,
    rev: u64,
}

/// One write that went through: enough to put it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// Relative to the project.
    pub rel: String,
    /// The text the file had; `None` for a file the write created.
    pub before: Option<String>,
    /// The text written.
    pub written: String,
    /// The version of the file right after.
    pub version: FileVersion,
}

/// What the toast's Undo puts back.
#[derive(Debug, Clone)]
pub(crate) struct InstructionsUndo {
    root: PathBuf,
    applied: Vec<Applied>,
}

/// The toast and what it undoes.
#[derive(Debug, Default)]
pub(crate) struct InstructionsState {
    pub toast: Option<InstructionsToast>,
    pub undo: Option<InstructionsUndo>,
    rev: u64,
}

/// What the plan read, held by the confirm sheet.
#[derive(Debug, Clone)]
struct Pending {
    root: PathBuf,
    writes: Vec<PlannedWrite>,
    /// The version of each file the plan changes, as it read it.
    versions: HashMap<String, FileVersion>,
}

// ---- reading the project ------------------------------------------------------------------

/// Asks the daemon what instruction files `root` holds and stats the user's own, building what
/// [`crate::instructions::discover`] needs. User-level files are looked at only when `homes` is
/// given.
pub(crate) async fn gather(
    connection: &Connection,
    root: &Path,
    homes: Option<HarnessHomes>,
    with_user: bool,
) -> Result<Facts, ClientError> {
    let (homes, config) = match homes {
        Some(homes) => {
            let config = HarnessConfig::load(&homes);
            (homes, config)
        }
        None => (
            HarnessHomes::under(Path::new("/nonexistent")),
            HarnessConfig::default(),
        ),
    };
    let names = project_names(&config);
    let list = |dir: PathBuf| FsList {
        dir,
        ignore: false,
        limit: None,
        picked: false,
    };
    let top = connection.fs_list(list(root.to_owned())).await?;
    let mut project: Vec<ProjectFile> = Vec::new();
    let mut folders = Vec::new();
    for entry in &top.entries {
        match entry.kind {
            FsKind::File | FsKind::Symlink if names.contains(&entry.name) => {
                project.push(ProjectFile {
                    rel: entry.name.clone(),
                    size: entry.size,
                    symlink: entry.kind == FsKind::Symlink,
                    imports: Vec::new(),
                });
            }
            FsKind::Dir if entry.name == ".claude" || entry.name == ".omp" => {
                folders.push(entry.name.clone());
            }
            _ => {}
        }
    }
    for folder in folders {
        let Ok(listing) = connection.fs_list(list(root.join(&folder))).await else {
            continue;
        };
        for entry in listing.entries {
            let wanted = match folder.as_str() {
                ".claude" => ["CLAUDE.md", "AGENTS.md"].contains(&entry.name.as_str()),
                _ => ["AGENTS.md", "RULES.md"].contains(&entry.name.as_str()),
            };
            if wanted && matches!(entry.kind, FsKind::File | FsKind::Symlink) {
                project.push(ProjectFile {
                    rel: format!("{folder}/{}", entry.name),
                    size: entry.size,
                    symlink: entry.kind == FsKind::Symlink,
                    imports: Vec::new(),
                });
            }
        }
    }
    // Folders below: the daemon's finder (what `.gitignore` leaves out is not listed).
    let mut nested: Vec<String> = Vec::new();
    for name in names.iter().filter(|n| n.as_str() != "RULES.md") {
        let found = connection
            .fs_find(FsFind {
                root: root.to_owned(),
                query: name.clone(),
                limit: Some(200),
            })
            .await;
        let Ok(found) = found else { continue };
        for m in found.matches {
            let base = m.path.rsplit('/').next().unwrap_or(&m.path);
            if base == name && m.path.contains('/') && !nested.contains(&m.path) {
                nested.push(m.path);
            }
        }
    }
    nested.sort();
    for rel in nested.into_iter().take(NESTED_MAX) {
        if project.iter().any(|f| f.rel == rel) {
            continue;
        }
        if let Ok(entry) = connection.fs_stat(&root.join(&rel), false).await
            && entry.kind == FsKind::File
        {
            project.push(ProjectFile {
                rel,
                size: entry.size,
                symlink: false,
                imports: Vec::new(),
            });
        }
    }
    // The `@` imports of the files that can pull others in: the project folder's own.
    for file in &mut project {
        let (dir, name) = file.rel.rsplit_once('/').unwrap_or(("", &file.rel));
        let own_folder = dir.is_empty() || dir == ".claude" || dir == ".omp";
        let can_import = ["AGENTS.md", "CLAUDE.md", "CLAUDE.local.md"].contains(&name);
        if !(own_folder && can_import) || file.size > SCAN_MAX_BYTES {
            continue;
        }
        if let Ok(text) = connection.fs_read_all(&root.join(&file.rel), false).await {
            file.imports = imports_of(&text.text, dir);
        }
    }
    let user = if with_user {
        user_candidates(&homes)
            .into_iter()
            .filter_map(|path| {
                let meta = std::fs::metadata(&path).ok().filter(|m| m.is_file())?;
                Some(UserFile {
                    path,
                    size: meta.len(),
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(Facts {
        root: root.to_owned(),
        project,
        user,
        homes,
        config,
    })
}

/// What `plan_share` needs, with the version of every file read.
async fn read_share_input(
    connection: &Connection,
    root: &Path,
) -> Result<(ShareInput, HashMap<String, FileVersion>), ClientError> {
    let list = |dir: PathBuf| FsList {
        dir,
        ignore: false,
        limit: None,
        picked: false,
    };
    let top = connection.fs_list(list(root.to_owned())).await?;
    let mut kinds: HashMap<String, FsKind> = top
        .entries
        .iter()
        .map(|e| (e.name.clone(), e.kind))
        .collect();
    if kinds.get(".claude") == Some(&FsKind::Dir)
        && let Ok(inside) = connection.fs_list(list(root.join(".claude"))).await
    {
        for entry in inside.entries {
            kinds.insert(format!(".claude/{}", entry.name), entry.kind);
        }
    }
    let mut versions = HashMap::new();
    let mut read = async |rel: &str| -> Result<Option<Existing>, ClientError> {
        match kinds.get(rel) {
            Some(FsKind::File) => {
                let text = connection.fs_read_all(&root.join(rel), false).await?;
                versions.insert(rel.to_owned(), text.version);
                Ok(Some(Existing {
                    text: text.text,
                    symlink: false,
                }))
            }
            Some(FsKind::Symlink) => Ok(Some(Existing {
                text: String::new(),
                symlink: true,
            })),
            _ => Ok(None),
        }
    };
    let input = ShareInput {
        agents: read("AGENTS.md").await?,
        claude: read("CLAUDE.md").await?,
        dot_claude: read(".claude/CLAUDE.md").await?,
    };
    Ok((input, versions))
}

// ---- writing --------------------------------------------------------------------------------

/// Writes `writes` in order. When one fails, the ones before it are put back and the error says
/// which file it was.
async fn apply_writes(
    connection: &Connection,
    root: &Path,
    writes: &[PlannedWrite],
    versions: &HashMap<String, FileVersion>,
) -> Result<Vec<Applied>, String> {
    let mut applied: Vec<Applied> = Vec::new();
    for write in writes {
        let (expect, create, before) = match &write.change {
            Change::Create { .. } => (None, true, None),
            Change::Replace { before, .. } => (
                versions.get(&write.rel).copied(),
                false,
                Some(before.clone()),
            ),
        };
        let request = FsWriteBegin {
            path: root.join(&write.rel),
            expect,
            create,
            size_hint: None,
            picked: false,
        };
        match connection.fs_write(request, write.after()).await {
            Ok(written) => applied.push(Applied {
                rel: write.rel.clone(),
                before,
                written: write.after().to_owned(),
                version: written.version,
            }),
            Err(error) => {
                let undone = undo_writes(connection, root, &applied).await;
                let mut message = format!("Cannot write {}: {}", write.rel, error_text(&error));
                if !undone.is_empty() {
                    message.push_str(&format!(" ({})", undone.join("; ")));
                }
                return Err(message);
            }
        }
    }
    Ok(applied)
}

/// Puts `applied` back, last first: a file Hutan wrote gets its old text again, a file Hutan
/// created is removed. A file that is not what Hutan wrote any more is left alone; the answer
/// lists those.
async fn undo_writes(connection: &Connection, root: &Path, applied: &[Applied]) -> Vec<String> {
    let mut left = Vec::new();
    for write in applied.iter().rev() {
        let path = root.join(&write.rel);
        let current = match connection.fs_read_all(&path, false).await {
            Ok(current) => current,
            Err(ClientError::Remote(e))
                if e.code == hutan_protocol::control::ErrorCode::NotFound =>
            {
                continue;
            }
            Err(error) => {
                left.push(format!("{}: {}", write.rel, error_text(&error)));
                continue;
            }
        };
        if current.text != write.written {
            left.push(format!(
                "{} changed since, so it is left as it is",
                write.rel
            ));
            continue;
        }
        match &write.before {
            Some(before) => {
                let request = FsWriteBegin {
                    path,
                    expect: Some(current.version),
                    create: false,
                    size_hint: None,
                    picked: false,
                };
                if let Err(error) = connection.fs_write(request, before).await {
                    left.push(format!("{}: {}", write.rel, error_text(&error)));
                }
            }
            None => {
                // The file service has no remove call: Hutan removes the one file it created,
                // only while it still holds exactly what Hutan wrote and lies below the project.
                let root = root.to_owned();
                let removed = std::thread::spawn(move || remove_created(&root, &path)).join();
                match removed {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => left.push(format!("{}: {error}", write.rel)),
                    Err(_) => left.push(format!("{}: the removal failed", write.rel)),
                }
            }
        }
    }
    left
}

fn remove_created(root: &Path, path: &Path) -> std::io::Result<()> {
    let root = root.canonicalize()?;
    let target = path.canonicalize()?;
    if !target.starts_with(&root) || !std::fs::symlink_metadata(&target)?.is_file() {
        return Err(std::io::Error::other("not a file of the project"));
    }
    std::fs::remove_file(target)
}

/// "AGENTS.md and CLAUDE.md".
fn names_of(writes: &[Applied]) -> String {
    let names: Vec<&str> = writes.iter().map(|w| w.rel.as_str()).collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [first, rest @ ..] => format!("{first} and {}", rest.join(", ")),
    }
}

// ---- the flows -------------------------------------------------------------------------------

impl Workbench {
    /// The folders the harnesses keep their own files in, as the panel looks at them; `None`
    /// looks at no user-level file.
    pub fn instruction_homes(&self) -> Option<HarnessHomes> {
        self.instruction_homes.borrow().clone()
    }

    /// Points the Instructions panel at other harness folders (tests, `CODEX_HOME` changes).
    pub fn set_instruction_homes(&mut self, homes: Option<HarnessHomes>, cx: &mut Context<Self>) {
        *self.instruction_homes.borrow_mut() = homes;
        self.refresh_instructions(cx);
    }

    fn refresh_instructions(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = self.panel::<super::panels::InstructionsPanel>("instructions") {
            panel.update(cx, |panel, cx| panel.refresh(cx));
        }
    }

    /// *Instructions: Show Panel*.
    pub(crate) fn show_instructions(&mut self, cx: &mut Context<Self>) {
        self.open_panel("instructions", cx);
    }

    /// Shows a file of the user's own in Finder.
    pub fn reveal_instruction_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let path = Path::new(path);
        if path.is_file() {
            (self.daemon.reveal)(path, cx);
        } else {
            self.set_notice(format!("{} no longer exists", path.display()), cx);
        }
    }

    /// *Create AGENTS.md*: a new file with a short starter, opened in a tab; the toast undoes it.
    pub fn create_agents_md(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.sessions.read(cx).files_root() else {
            self.set_notice("Open a project first", cx);
            return;
        };
        let connection = self.sessions.read(cx).connection().clone();
        let writes = vec![PlannedWrite {
            rel: "AGENTS.md".to_owned(),
            change: Change::Create {
                text: STARTER.to_owned(),
            },
            note: "new file".to_owned(),
        }];
        let work = cx.background_spawn({
            let root = root.clone();
            async move { apply_writes(&connection, &root, &writes, &HashMap::new()).await }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(applied) => {
                    this.show_instructions_toast("Created AGENTS.md", root.clone(), applied, cx);
                    window.dispatch_action(
                        Box::new(OpenChosenFile(ChosenFile {
                            path: root.join("AGENTS.md").display().to_string(),
                            line: None,
                            column: None,
                            place: FilePlace::Focused,
                        })),
                        cx,
                    );
                }
                Err(message) => this.set_notice(message, cx),
            })
            .ok();
        })
        .detach();
    }

    /// *Share one file with every agent…*: reads the project's files, plans, and opens the
    /// sheet with exactly what would be written. Nothing is written until *Share*.
    pub fn share_instructions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.sessions.read(cx).files_root() else {
            self.set_notice("Open a project first", cx);
            return;
        };
        let connection = self.sessions.read(cx).connection().clone();
        let read = cx.background_spawn({
            let root = root.clone();
            async move { read_share_input(&connection, &root).await }
        });
        cx.spawn_in(window, async move |this, cx| {
            let read = read.await;
            this.update_in(cx, |this, window, cx| match read {
                Err(error) => this.set_notice(
                    format!("Cannot read the instruction files: {}", error_text(&error)),
                    cx,
                ),
                Ok((input, versions)) => match plan_share(&input) {
                    SharePlan::Done { reason } | SharePlan::Blocked { reason } => {
                        this.set_notice(reason, cx);
                    }
                    SharePlan::Writes(writes) => {
                        this.open_share_sheet(
                            Pending {
                                root,
                                writes,
                                versions,
                            },
                            window,
                            cx,
                        );
                    }
                },
            })
            .ok();
        })
        .detach();
    }

    fn open_share_sheet(&mut self, pending: Pending, window: &mut Window, cx: &mut Context<Self>) {
        let message = preview(&pending.writes, PREVIEW_LINES);
        let pending = Arc::new(pending);
        self.open_confirm(
            "Share one file with every agent",
            message,
            "Share",
            move |this, _window, cx| this.apply_share(pending.clone(), cx),
            window,
            cx,
        );
    }

    fn apply_share(&mut self, pending: Arc<Pending>, cx: &mut Context<Self>) {
        let connection = self.sessions.read(cx).connection().clone();
        let work = cx.background_spawn({
            let pending = pending.clone();
            async move {
                apply_writes(
                    &connection,
                    &pending.root,
                    &pending.writes,
                    &pending.versions,
                )
                .await
            }
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| match result {
                Ok(applied) => {
                    let text = format!("Shared one file with every agent · {}", names_of(&applied));
                    this.show_instructions_toast(text, pending.root.clone(), applied, cx);
                }
                Err(message) => this.set_notice(message, cx),
            })
            .ok();
        })
        .detach();
    }

    fn show_instructions_toast(
        &mut self,
        text: impl Into<SharedString>,
        root: PathBuf,
        applied: Vec<Applied>,
        cx: &mut Context<Self>,
    ) {
        self.instructions.rev += 1;
        let rev = self.instructions.rev;
        self.instructions.toast = Some(InstructionsToast {
            text: text.into(),
            rev,
        });
        self.instructions.undo = Some(InstructionsUndo { root, applied });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_TIME).await;
            this.update(cx, |this, cx| {
                if this
                    .instructions
                    .toast
                    .as_ref()
                    .is_some_and(|t| t.rev == rev)
                {
                    this.instructions.toast = None;
                    this.instructions.undo = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        self.refresh_instructions(cx);
        cx.notify();
    }

    /// The toast, while it shows.
    pub fn instructions_toast(&self) -> Option<&InstructionsToast> {
        self.instructions.toast.as_ref()
    }

    /// The toast's *Undo*: the files Hutan wrote go back to what they were.
    pub fn undo_instructions(&mut self, cx: &mut Context<Self>) {
        self.instructions.toast = None;
        let Some(undo) = self.instructions.undo.take() else {
            return;
        };
        let connection = self.sessions.read(cx).connection().clone();
        let work = cx.background_spawn(async move {
            undo_writes(&connection, &undo.root, &undo.applied).await
        });
        cx.spawn(async move |this, cx| {
            let left = work.await;
            this.update(cx, |this, cx| {
                if left.is_empty() {
                    this.set_notice("Put the instruction files back", cx);
                } else {
                    this.set_notice(format!("Undo left some files: {}", left.join("; ")), cx);
                }
                this.refresh_instructions(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn render_instructions_toast(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let toast = self.instructions_toast_exit.follow(
            self.instructions.toast.as_ref(),
            motion::Kind::Toast,
            cx,
        )?;
        let (theme, tokens) = (self.theme, self.density.ui());
        let (wanted, toast) = (toast.wanted, toast.value);
        Some(
            motion::present(
                "instructions-toast-motion",
                motion::Kind::Toast,
                wanted,
                motion::Slide::Rise,
                div()
                    .id("instructions-toast")
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 3.0))
                    .toast(&theme, &tokens)
                    .undo_toast(&tokens)
                    .max_w(px(super::closing::UNDO_TOAST_MAX_WIDTH))
                    .px(px(tokens.spacing * 3.0))
                    .py(px(tokens.spacing * 2.0))
                    .child(div().min_w_0().truncate().child(toast.text.clone()))
                    .child(super::render::probe(
                        &self.regions,
                        Region::InstructionsToast("toast"),
                    ))
                    .child(
                        Button::new("instructions-undo", "Undo", &theme, &tokens)
                            .link()
                            .on_click(cx.listener(|this, _, _, cx| this.undo_instructions(cx)))
                            .child(super::render::probe(
                                &self.regions,
                                Region::InstructionsToast("undo"),
                            )),
                    ),
            )
            .into_any_element(),
        )
    }
}
