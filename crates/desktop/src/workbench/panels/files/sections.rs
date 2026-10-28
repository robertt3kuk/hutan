//! The sections above the Files tree (EX-5): **Open Editors** — the files open in tabs, in tab
//! order, with an unsaved dot and a close button — and **Changed by agents** — the files an
//! agent's turn changed that are not accepted yet (the E-10 ledger), newest first. Each is a
//! header (a chevron, the title, the count) that folds its rows; a section with no rows is not
//! there. The tree is the third section, below.
//!
//! What the sections list lives in the workbench (its tabs, its buffers, the ledger), so the
//! workbench pushes it into an [`ExplorerFeed`] the panel observes; the panel keeps only the
//! rows below its root. Clicks are actions the workbench handles ([`OpenChosenFile`],
//! [`CloseFileTabs`]); the panel never touches a tab.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, App, ClickEvent, Context, InteractiveElement, IntoElement, MouseButton,
    ParentElement, ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Window, div,
    prelude::*, px,
};
use hutan_core::SessionId;

use super::FilesPanel;
use crate::icons::{Icon, icon};
use crate::settings::{FilesPanelSettings, OpenEditorsSection};
use crate::theme::{DensityExt as _, Theme, Tokens};
use crate::workbench::Region;
use crate::workbench::actions::{ChosenFile, CloseFileTabs, OpenChosenFile};
use crate::workbench::files::FilePlace;
use crate::workbench::panels::common::age_label;
use crate::workbench::panels::files_tree::file_icon;
use crate::workbench::regions::Regions;
use crate::workbench::render::probe;
use crate::workbench::style::{TextTooltip, UiStyle};
use crate::workbench::ui::scroll::ScrollAreaExt as _;

/// Rows *Changed by agents* lists; the rest is "N more".
pub const MAX_AGENT_ROWS: usize = 50;
/// *Open Editors* shows this many rows and scrolls inside itself beyond them (and scrolls the
/// active file into view), so thirty tabs never push the tree off the panel.
pub const MAX_OPEN_ROWS: usize = 6;
/// The sections never take more than this share of the window's height (they scroll inside).
const MAX_SHARE: f32 = 0.4;

/// A section above the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    OpenEditors,
    ChangedByAgents,
}

impl Section {
    fn title(self) -> &'static str {
        match self {
            Self::OpenEditors => "OPEN EDITORS",
            Self::ChangedByAgents => "CHANGED BY AGENTS",
        }
    }

    /// The name the region and the saved state use.
    pub fn id(self) -> &'static str {
        match self {
            Self::OpenEditors => "open-editors",
            Self::ChangedByAgents => "changed-by-agents",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        [Self::OpenEditors, Self::ChangedByAgents]
            .into_iter()
            .find(|s| s.id() == id)
    }
}

/// A file open in a tab of the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenEditor {
    /// Canonical, as the tab and the tree name it.
    pub path: PathBuf,
    /// The buffer has unsaved changes.
    pub dirty: bool,
    /// The tab is the preview tab (its title is in italics).
    pub preview: bool,
}

/// A file an agent changed and nobody accepted yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFile {
    pub path: PathBuf,
    pub session: SessionId,
    /// Who: the harness (`Codex`), else the session's title.
    pub who: String,
    /// Places the agent changed in the file as it is now (never 0).
    pub hunks: usize,
    /// The line of the first one, zero-based.
    pub line: u32,
    pub updated_ms: u64,
}

impl AgentFile {
    /// `Codex · 3 hunks`.
    pub fn summary(&self) -> String {
        format!(
            "{} · {} {}",
            self.who,
            self.hunks,
            if self.hunks == 1 { "hunk" } else { "hunks" }
        )
    }

    /// `Codex changed this · 3 hunks · 2m ago`: the mark's tooltip.
    pub fn tooltip(&self) -> String {
        let ago = match age_label(self.updated_ms).as_str() {
            "now" => "just now".to_owned(),
            short => format!("{short} ago"),
        };
        format!(
            "{} changed this · {} {} · {ago}",
            self.who,
            self.hunks,
            if self.hunks == 1 { "hunk" } else { "hunks" }
        )
    }
}

/// What the workbench tells the Files panel for its sections: the files of its tabs (tab order,
/// each file once), which one the focused tab shows, and the ledger's files newest first. The
/// workbench updates it when a tab, a buffer's unsaved state or the ledger changes; the panel
/// observes it.
#[derive(Debug, Default)]
pub struct ExplorerFeed {
    open: Vec<OpenEditor>,
    active: Option<PathBuf>,
    agents: Vec<AgentFile>,
}

impl ExplorerFeed {
    pub fn open(&self) -> &[OpenEditor] {
        &self.open
    }

    /// The file the focused tab shows.
    pub fn active(&self) -> Option<&Path> {
        self.active.as_deref()
    }

    pub fn agents(&self) -> &[AgentFile] {
        &self.agents
    }

    /// The files of the tabs and the active one; the panel redraws when they changed.
    pub fn set_open(
        &mut self,
        open: Vec<OpenEditor>,
        active: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if self.open != open || self.active != active {
            self.open = open;
            self.active = active;
            cx.notify();
        }
    }

    /// The ledger's files, newest first.
    pub fn set_agents(&mut self, agents: Vec<AgentFile>, cx: &mut Context<Self>) {
        if self.agents != agents {
            self.agents = agents;
            cx.notify();
        }
    }
}

/// The agent marks of the tree's rows: the files an agent changed, and the folders above them.
#[derive(Debug, Default)]
pub(super) struct AgentMarks {
    files: HashMap<PathBuf, AgentFile>,
    dirs: HashSet<PathBuf>,
}

impl AgentMarks {
    /// The marks of the files below one of `roots`; a root has a row only in a workspace's tree
    /// (`root_rows`), where it holds what is below it too.
    pub(super) fn of(agents: &[AgentFile], roots: &[PathBuf], root_rows: bool) -> Self {
        let mut marks = Self::default();
        for file in agents {
            let Some(root) = roots
                .iter()
                .filter(|r| file.path.starts_with(r))
                .max_by_key(|r| r.components().count())
            else {
                continue;
            };
            let mut dir = file.path.parent();
            while let Some(parent) =
                dir.filter(|d| d.starts_with(root) && (root_rows || *d != root.as_path()))
            {
                marks.dirs.insert(parent.to_owned());
                dir = parent.parent();
            }
            marks.files.insert(file.path.clone(), file.clone());
        }
        marks
    }

    pub(super) fn file(&self, path: &Path) -> Option<&AgentFile> {
        self.files.get(path)
    }

    pub(super) fn holds(&self, dir: &Path) -> bool {
        self.dirs.contains(dir)
    }
}

/// The part of `path` under `root`, its folder only (`crates/desktop`), `None` at the root.
fn folder_under(root: &Path, path: &Path) -> Option<String> {
    let parent = path.parent()?.strip_prefix(root).ok()?;
    (!parent.as_os_str().is_empty()).then(|| parent.display().to_string())
}

fn file_name(path: &Path) -> SharedString {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .into()
}

impl FilesPanel {
    /// The files of the window's tabs below a root, tab order.
    fn open_below_root(&self, cx: &App) -> Vec<OpenEditor> {
        let roots = self.roots();
        self.env
            .files_feed
            .read(cx)
            .open()
            .iter()
            .filter(|e| roots.iter().any(|root| e.path.starts_with(root)))
            .cloned()
            .collect()
    }

    /// The files *Open Editors* lists now: those of the tabs below the root, when
    /// `panels.files.open_editors` allows (`auto`: two or more).
    pub fn open_editors(&self, cx: &App) -> Vec<OpenEditor> {
        let open = self.open_below_root(cx);
        let wanted = match self.env.settings.get::<FilesPanelSettings>().open_editors {
            OpenEditorsSection::Auto => open.len() >= 2,
            OpenEditorsSection::Always => !open.is_empty(),
            OpenEditorsSection::Never => false,
        };
        if wanted { open } else { Vec::new() }
    }

    /// The files of the ledger below a root, newest first (every one, not only the rows the
    /// section draws).
    pub fn changed_by_agents(&self, cx: &App) -> Vec<AgentFile> {
        let roots = self.roots();
        self.env
            .files_feed
            .read(cx)
            .agents()
            .iter()
            .filter(|a| roots.iter().any(|root| a.path.starts_with(root)))
            .cloned()
            .collect()
    }

    /// The agent marks of the rows (a lookup per row, built once a frame).
    pub(super) fn agent_marks(&self, cx: &App) -> AgentMarks {
        let root_rows = self.tree.as_ref().is_some_and(|t| t.is_grouped());
        AgentMarks::of(
            self.env.files_feed.read(cx).agents(),
            self.roots(),
            root_rows,
        )
    }

    /// Where an open file's folder is, for the *Open Editors* row: the folder below its root;
    /// in a workspace's tree the root's name leads (`alpha/src`).
    fn folder_label(&self, path: &Path) -> Option<String> {
        let tree = self.tree.as_ref()?;
        let root = tree.root_of(path)?;
        let inside = folder_under(root, path);
        if !tree.is_grouped() {
            return inside;
        }
        let name = root
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        Some(match inside {
            Some(inside) => format!("{name}/{inside}"),
            None => name,
        })
    }

    /// The ledger's entry the tree marks the file at `path` with (◆), if any.
    pub fn agent_mark_of(&self, path: &Path, cx: &App) -> Option<AgentFile> {
        self.agent_marks(cx).file(path).cloned()
    }

    /// Whether the tree marks the folder at `dir` as holding files an agent changed.
    pub fn agent_mark_in(&self, dir: &Path, cx: &App) -> bool {
        self.agent_marks(cx).holds(dir)
    }

    /// Whether the rows of `section` are folded into its header.
    pub fn section_folded(&self, section: Section) -> bool {
        self.sections.folded.contains(&section)
    }

    /// Folds or unfolds `section`; the root remembers it (across restarts too).
    pub fn toggle_section(&mut self, section: Section, cx: &mut Context<Self>) {
        if !self.sections.folded.remove(&section) {
            self.sections.folded.insert(section);
        }
        if let Some(root) = self.root().map(Path::to_owned) {
            let mut folded: Vec<String> = self
                .sections
                .folded
                .iter()
                .map(|s| s.id().to_owned())
                .collect();
            folded.sort();
            self.env.files_memory.remember_sections(&root, folded, cx);
        }
        cx.notify();
    }

    /// Takes in which sections the root's memory has folded (a new root, or a root come back).
    pub(super) fn load_folded_sections(&mut self) {
        self.sections.folded = match self.root() {
            Some(root) => self
                .env
                .files_memory
                .folded_of(root)
                .iter()
                .filter_map(|id| Section::from_id(id))
                .collect(),
            None => HashSet::new(),
        };
    }

    /// The sections above the tree; `None` when none has a row.
    pub(super) fn render_sections(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let open = self.open_editors(cx);
        let agents = self.changed_by_agents(cx);
        if open.is_empty() && agents.is_empty() {
            return None;
        }
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let active = self.env.files_feed.read(cx).active().map(Path::to_owned);
        self.root()?;
        let regions = self.env.regions.clone();
        let mut blocks: Vec<AnyElement> = Vec::new();
        if !open.is_empty() {
            let folded = self.section_folded(Section::OpenEditors);
            blocks.push(self.section_header(
                Section::OpenEditors,
                open.len(),
                folded,
                &theme,
                &tokens,
                cx,
            ));
            if !folded {
                let rows: Vec<AnyElement> = open
                    .iter()
                    .enumerate()
                    .map(|(index, editor)| {
                        open_row(
                            index,
                            editor,
                            active.as_deref() == Some(editor.path.as_path()),
                            self.folder_label(&editor.path),
                            &regions,
                            &theme,
                            &tokens,
                            cx,
                        )
                    })
                    .collect();
                // The row of the focused tab is in view whenever it changes.
                if self.sections.open_active != active {
                    self.sections.open_active.clone_from(&active);
                    if let Some(at) = open
                        .iter()
                        .position(|e| active.as_deref() == Some(e.path.as_path()))
                    {
                        self.sections.open_scroll.scroll_to_item(at);
                    }
                }
                blocks.push(
                    div()
                        .id("files-open-rows")
                        .flex_none()
                        .flex()
                        .flex_col()
                        .max_h(px(MAX_OPEN_ROWS as f32 * (tokens.sidebar_row - 4.0)))
                        .scroll_y_contained(&self.sections.open_scroll)
                        .children(rows)
                        .child(probe(&regions, Region::FilesSection("open-rows", 0)))
                        .into_any_element(),
                );
            }
        }
        if !agents.is_empty() {
            let folded = self.section_folded(Section::ChangedByAgents);
            blocks.push(self.section_header(
                Section::ChangedByAgents,
                agents.len(),
                folded,
                &theme,
                &tokens,
                cx,
            ));
            if !folded {
                for (index, file) in agents.iter().take(MAX_AGENT_ROWS).enumerate() {
                    blocks.push(agent_row(index, file, &regions, &theme, &tokens, cx));
                }
                if agents.len() > MAX_AGENT_ROWS {
                    blocks.push(
                        div()
                            .px(px(tokens.spacing * 4.0))
                            .h(px(tokens.sidebar_row - 4.0))
                            .flex()
                            .items_center()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.faint)
                            .child(SharedString::from(format!(
                                "{} more",
                                agents.len() - MAX_AGENT_ROWS
                            )))
                            .into_any_element(),
                    );
                }
            }
        }
        let max = f32::from(window.viewport_size().height) * MAX_SHARE;
        Some(
            div()
                .id("files-sections")
                .flex_none()
                .flex()
                .flex_col()
                .max_h(px(max))
                .pb(px(tokens.spacing))
                .border_b_1()
                .border_color(theme.border)
                .scroll_y_contained(&self.sections.scroll)
                .children(blocks)
                .into_any_element(),
        )
    }

    fn section_header(
        &self,
        section: Section,
        count: usize,
        folded: bool,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chevron = if folded {
            Icon::ActionDisclose
        } else {
            Icon::ActionExpand
        };
        let index: usize = match section {
            Section::OpenEditors => 0,
            Section::ChangedByAgents => 1,
        };
        let hover = theme.sidebar_hover;
        div()
            .id(("files-section", index))
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.0))
            .h(px(22.0))
            .px(px(tokens.spacing * 3.0))
            .cursor_pointer()
            .text_size(px(tokens.section_font_size))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme.muted)
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.toggle_section(section, cx);
            }))
            .child(icon(chevron, 12.0, theme.muted))
            .child(section.title())
            .child(
                div()
                    .text_color(theme.faint)
                    .child(SharedString::from(count.to_string())),
            )
            .child(probe(
                &self.env.regions,
                Region::FilesSection(section.id(), usize::MAX),
            ))
            .into_any_element()
    }
}

/// The scroll state of the sections block and which sections are folded.
#[derive(Default)]
pub(super) struct Sections {
    pub(super) folded: HashSet<Section>,
    pub(super) scroll: ScrollHandle,
    /// *Open Editors* scrolls inside itself past [`MAX_OPEN_ROWS`] rows.
    pub(super) open_scroll: ScrollHandle,
    /// The file the section last scrolled to.
    pub(super) open_active: Option<PathBuf>,
}

#[allow(clippy::too_many_arguments)] // one row of one list; a struct would only rename them
fn open_row(
    index: usize,
    editor: &OpenEditor,
    is_active: bool,
    folder: Option<String>,
    regions: &Regions,
    theme: &Theme,
    tokens: &Tokens,
    cx: &mut Context<FilesPanel>,
) -> AnyElement {
    let path = editor.path.display().to_string();
    let name = file_name(&editor.path);
    let hover = theme.sidebar_hover;
    let (open_path, close_path, middle_path) = (path.clone(), path.clone(), path.clone());
    let tip: SharedString = path.clone().into();
    div()
        .w_full()
        .px(px(tokens.spacing))
        .child(
            div()
                .id(("files-open-editor", index))
                .relative()
                .flex()
                .items_center()
                .gap(px(4.0))
                .h(px(tokens.sidebar_row - 4.0))
                .pl(px(tokens.spacing * 2.0 + 12.0))
                .pr(px(tokens.spacing * 2.0))
                .w_full()
                .rounded(px(tokens.radius))
                .cursor_pointer()
                .text_size(px(tokens.font_size))
                .text_color(theme.fg)
                .when(is_active, |s| {
                    s.bg(theme.sidebar_accent)
                        .font_weight(gpui::FontWeight::MEDIUM)
                })
                .when(editor.preview, |s| s.italic())
                .when(!is_active, move |s| s.hover(move |s| s.bg(hover)))
                .tooltip(move |_, cx| cx.new(|_| TextTooltip::new(tip.clone())).into())
                .on_click(cx.listener(move |_, event: &ClickEvent, window, cx| {
                    // ⌘-click closes (the tab asks first when it has unsaved changes).
                    let action: Box<dyn gpui::Action> = if event.modifiers().platform {
                        Box::new(CloseFileTabs(open_path.clone()))
                    } else {
                        Box::new(OpenChosenFile(ChosenFile {
                            path: open_path.clone(),
                            line: None,
                            column: None,
                            place: FilePlace::Focused,
                        }))
                    };
                    window.dispatch_action(action, cx);
                }))
                .on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(move |_, _, window, cx| {
                        window.dispatch_action(Box::new(CloseFileTabs(middle_path.clone())), cx);
                    }),
                )
                .child(
                    div()
                        .flex_none()
                        .child(icon(file_icon(&name), 14.0, theme.fg)),
                )
                .child(div().flex_none().max_w_3_4().truncate().child(name))
                .children(folder.map(|folder| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child(SharedString::from(folder))
                }))
                .child(div().flex_1())
                .when(editor.dirty, |s| {
                    s.child(
                        div()
                            .id(("files-open-dirty", index))
                            .flex_none()
                            .size(px(7.0))
                            .rounded_full()
                            .bg(theme.fg)
                            .tooltip(|_, cx| {
                                cx.new(|_| TextTooltip::new("Unsaved changes".into()))
                                    .into()
                            }),
                    )
                })
                .child(
                    div()
                        .id(("files-open-close", index))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(tokens.font_size + 4.0))
                        .rounded(px(tokens.radius))
                        .text_color(theme.muted)
                        .hover(move |s| s.bg(hover).text_color(theme.fg))
                        .tooltip(|_, cx| cx.new(|_| TextTooltip::new("Close".into())).into())
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            window.dispatch_action(Box::new(CloseFileTabs(close_path.clone())), cx);
                        }))
                        .child(icon(Icon::ActionClose, 12.0, theme.muted))
                        .child(probe(regions, Region::FilesSection("open-close", index))),
                )
                .child(probe(regions, Region::FilesSection("open-editors", index))),
        )
        .into_any_element()
}

fn agent_row(
    index: usize,
    file: &AgentFile,
    regions: &Regions,
    theme: &Theme,
    tokens: &Tokens,
    cx: &mut Context<FilesPanel>,
) -> AnyElement {
    let name = file_name(&file.path);
    let hover = theme.sidebar_hover;
    let path = file.path.display().to_string();
    let line = file.line + 1;
    let tip: SharedString = file.tooltip().into();
    let summary: SharedString = file.summary().into();
    div()
        .w_full()
        .px(px(tokens.spacing))
        .child(
            div()
                .id(("files-agent-file", index))
                .relative()
                .flex()
                .items_center()
                .gap(px(4.0))
                .h(px(tokens.sidebar_row - 4.0))
                .pl(px(tokens.spacing * 2.0 + 12.0))
                .pr(px(tokens.spacing * 2.0))
                .w_full()
                .rounded(px(tokens.radius))
                .cursor_pointer()
                .text_size(px(tokens.font_size))
                .text_color(theme.fg)
                .hover(move |s| s.bg(hover))
                .tooltip(move |_, cx| cx.new(|_| TextTooltip::new(tip.clone())).into())
                .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
                    window.dispatch_action(
                        Box::new(OpenChosenFile(ChosenFile {
                            path: path.clone(),
                            line: Some(line),
                            column: None,
                            place: FilePlace::Focused,
                        })),
                        cx,
                    );
                }))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.accent)
                        .child("◆"),
                )
                .child(div().flex_none().max_w_1_2().truncate().child(name))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child(summary),
                )
                .child(probe(
                    regions,
                    Region::FilesSection("changed-by-agents", index),
                )),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, hunks: usize) -> AgentFile {
        AgentFile {
            path: PathBuf::from(path),
            session: "01K7F3B5N2Q9W8E4R6T1Y0V3KM".parse().unwrap(),
            who: "Codex".to_owned(),
            hunks,
            line: 0,
            updated_ms: 0,
        }
    }

    #[test]
    fn marks_cover_the_files_and_every_folder_below_the_root_above_them() {
        let marks = AgentMarks::of(
            &[
                file("/r/a/b/x.rs", 1),
                file("/r/y.rs", 2),
                file("/other/z.rs", 1),
            ],
            &[PathBuf::from("/r")],
            false,
        );
        assert!(marks.file(Path::new("/r/a/b/x.rs")).is_some());
        assert!(marks.file(Path::new("/r/y.rs")).is_some());
        assert!(marks.file(Path::new("/other/z.rs")).is_none());
        assert!(marks.holds(Path::new("/r/a")) && marks.holds(Path::new("/r/a/b")));
        assert!(!marks.holds(Path::new("/r")), "the root has no row");
        assert!(!marks.holds(Path::new("/other")));
    }

    #[test]
    fn a_workspaces_roots_hold_the_marks_below_them() {
        let marks = AgentMarks::of(
            &[file("/a/src/x.rs", 1), file("/b/y.rs", 1)],
            &[PathBuf::from("/a"), PathBuf::from("/b")],
            true,
        );
        assert!(marks.holds(Path::new("/a")) && marks.holds(Path::new("/b")));
        assert!(marks.holds(Path::new("/a/src")));
        assert!(marks.file(Path::new("/b/y.rs")).is_some());
    }

    #[test]
    fn the_summary_names_the_agent_and_counts_the_hunks() {
        assert_eq!(file("/r/x", 1).summary(), "Codex · 1 hunk");
        assert_eq!(file("/r/x", 3).summary(), "Codex · 3 hunks");
        assert!(
            file("/r/x", 3)
                .tooltip()
                .starts_with("Codex changed this · 3 hunks · ")
        );
    }

    #[test]
    fn a_folder_is_named_under_the_root_and_the_root_has_none() {
        let root = Path::new("/r");
        assert_eq!(
            folder_under(root, Path::new("/r/crates/desktop/a.rs")).as_deref(),
            Some("crates/desktop")
        );
        assert_eq!(folder_under(root, Path::new("/r/a.rs")), None);
    }
}
