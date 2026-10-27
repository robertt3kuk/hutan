//! The `instructions` panel (AGENT-INSTRUCTIONS): the instruction files the agents read, in
//! two groups — *This project* (the project's own files, opened in a file tab by a click) and
//! *Your user files* (`~/.codex`, `~/.claude`, `~/.omp/agent`: read-only here, *Show in Finder*
//! for editing) — each with the agents that read it as small harness glyphs and, under its
//! name, the same in words. *Share one file with every agent…* sits above the list while an
//! `AGENTS.md` is not read by all three; with no instruction file in the project the panel
//! offers *Create AGENTS.md*.
//!
//! - **Live.** The panel reads the project through the daemon when it is made, when the project
//!   or the connection changes, after every `fs.changed` of its folders, and with the header's
//!   Refresh. One read at a time: a change during a read asks for one more afterwards.
//! - **No colour-only cue.** A glyph is a shape per harness; a file an agent reads on demand
//!   or not at all (another file wins) is dimmer, and the words under the name say which.
//! - **Keyboard.** ↑↓ move between files, Return opens a project file or shows a user file in
//!   Finder.

use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Task,
    Window, div, prelude::*, px,
};
use hutan_client::{ConnectionState, Notice};
use tokio::sync::broadcast::error::RecvError;

use super::common::panel_toolbar;
use crate::icons::{self, Icon};
use crate::instructions::{Discovery, Harness, InstructionFile, Reads, Scope, discover};
use crate::settings::InstructionsPanelSettings;
use crate::theme::DensityExt as _;
use crate::workbench::Region;
use crate::workbench::actions::{
    ChosenFile, CreateAgentsMd, OpenChosenFile, RevealInstructionFile, ShareInstructions,
};
use crate::workbench::files::FilePlace;
use crate::workbench::instructions::gather;
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::{TextTooltip, UiStyle};
use crate::workbench::ui;

/// Least height of a file's row (the words under its name wrap).
const ROW_HEIGHT: f32 = 40.0;

/// What the panel has read.
#[derive(Debug, Clone)]
pub enum Instructions {
    /// No project, or no connection: nothing was asked.
    Idle,
    Loading,
    Ready(Discovery),
    Failed(String),
}

pub struct InstructionsPanel {
    env: PanelEnv,
    focus: FocusHandle,
    root: Option<PathBuf>,
    state: Instructions,
    /// The read in flight, and whether the panel was asked again meanwhile.
    task: Option<Task<()>>,
    again: bool,
    /// How many reads have ended (tests wait on it).
    reads: u64,
    listener: Option<Task<()>>,
    watched: Vec<PathBuf>,
    /// The connection and root the panel last asked for.
    asked: Option<(Option<PathBuf>, ConnectionState)>,
    /// Row index among [`Self::files`].
    selected: Option<usize>,
}

impl std::fmt::Debug for InstructionsPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstructionsPanel")
            .field("root", &self.root)
            .field("reads", &self.reads)
            .finish_non_exhaustive()
    }
}

impl InstructionsPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        cx.observe(&env.sessions, |this, _, cx| this.follow(cx))
            .detach();
        let mut panel = Self {
            env,
            focus: cx.focus_handle(),
            root: None,
            state: Instructions::Idle,
            task: None,
            again: false,
            reads: 0,
            listener: None,
            watched: Vec::new(),
            asked: None,
            selected: None,
        };
        panel.follow(cx);
        panel
    }

    /// What was read, once it was.
    pub fn state(&self) -> &Instructions {
        &self.state
    }

    /// The words of the Share button: *Optional — for older Claude Code versions* while every
    /// agent already reads `AGENTS.md`.
    pub fn share_button_label(&self) -> &'static str {
        share_label(self.discovery().is_some_and(Discovery::share_is_optional))
    }

    pub fn discovery(&self) -> Option<&Discovery> {
        match &self.state {
            Instructions::Ready(discovery) => Some(discovery),
            _ => None,
        }
    }

    /// Reads that have ended, successful or not.
    pub fn reads(&self) -> u64 {
        self.reads
    }

    pub fn reading(&self) -> bool {
        self.task.is_some()
    }

    /// The files in the order the rows are drawn: the project's, then the user's.
    pub fn files(&self) -> Vec<InstructionFile> {
        let Some(discovery) = self.discovery() else {
            return Vec::new();
        };
        let mut files = discovery.project.clone();
        if self
            .env
            .settings
            .get::<InstructionsPanelSettings>()
            .show_user_files
        {
            files.extend(discovery.user.iter().cloned());
        }
        files
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    // ---- reading --------------------------------------------------------------------------

    /// Reads again when the project or the connection is not the one last asked for.
    fn follow(&mut self, cx: &mut Context<Self>) {
        let root = self.env.sessions.read(cx).files_root();
        let connection = self.env.sessions.read(cx).connection_state();
        if root != self.root {
            self.unwatch_all(cx);
            self.root = root.clone();
            self.state = Instructions::Idle;
            self.selected = None;
            if let Some(root) = &root {
                self.listen(cx);
                self.watch(root, cx);
            } else {
                self.listener = None;
            }
        }
        let wanted = Some((root, connection));
        if wanted == self.asked {
            return;
        }
        self.asked = wanted;
        // A project is read once hutand is there; the user's own files need no daemon.
        if self.root.is_none() || connection == ConnectionState::Connected {
            self.refresh(cx);
        }
    }

    /// Reads the project and the user's files again.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.task.is_some() {
            self.again = true;
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        let root = self.root.clone();
        let homes = self.env.instruction_homes.borrow().clone();
        let with_user = self
            .env
            .settings
            .get::<InstructionsPanelSettings>()
            .show_user_files;
        if root.is_none() && !with_user {
            self.state = Instructions::Idle;
            cx.notify();
            return;
        }
        if !matches!(self.state, Instructions::Ready(_)) {
            self.state = Instructions::Loading;
        }
        let work = cx.background_spawn(async move {
            match root {
                Some(root) => gather(&connection, &root, homes, with_user)
                    .await
                    .map(|facts| discover(&facts))
                    .map_err(|error| crate::workbench::files::error_text(&error)),
                None => Ok(user_only(homes, with_user)),
            }
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let answer = work.await;
            this.update(cx, |this, cx| this.read(answer, cx)).ok();
        }));
        cx.notify();
    }

    fn read(&mut self, answer: Result<Discovery, String>, cx: &mut Context<Self>) {
        self.task = None;
        self.reads += 1;
        self.state = match answer {
            Ok(discovery) => Instructions::Ready(discovery),
            Err(message) => Instructions::Failed(message),
        };
        let count = self.files().len();
        self.selected = self.selected.filter(|i| *i < count);
        if std::mem::take(&mut self.again) {
            self.refresh(cx);
        }
        cx.notify();
    }

    // ---- watching -------------------------------------------------------------------------

    /// The project folder and the two folders harnesses keep files in.
    fn watch(&mut self, root: &Path, cx: &mut Context<Self>) {
        for dir in [root.to_owned(), root.join(".claude"), root.join(".omp")] {
            self.watched.push(dir.clone());
            self.env
                .sessions
                .update(cx, |store, cx| store.watch_dir(&dir, cx));
        }
    }

    fn unwatch_all(&mut self, cx: &mut Context<Self>) {
        for dir in std::mem::take(&mut self.watched) {
            self.env
                .sessions
                .update(cx, |store, cx| store.unwatch_dir(&dir, cx));
        }
    }

    fn listen(&mut self, cx: &mut Context<Self>) {
        if self.listener.is_some() {
            return;
        }
        let mut notices = self.env.sessions.read(cx).connection().notices();
        self.listener = Some(cx.spawn(async move |this, cx| {
            loop {
                let notice = match notices.recv().await {
                    Ok(notice @ (Notice::FsChanged { .. } | Notice::FsGeneration { .. })) => notice,
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => Notice::FsChanged {
                        changes: Vec::new(),
                        overflow: true,
                    },
                    Err(RecvError::Closed) => break,
                };
                if this
                    .update(cx, |this, cx| this.on_notice(&notice, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn on_notice(&mut self, notice: &Notice, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let ours = match notice {
            Notice::FsChanged { overflow: true, .. } => true,
            Notice::FsChanged { changes, .. } => changes.iter().any(|c| {
                std::iter::once(&c.path).chain(c.from.as_ref()).any(|p| {
                    p.starts_with(&root)
                        && p.file_name()
                            .is_some_and(|n| n.to_string_lossy().ends_with(".md"))
                })
            }),
            Notice::FsGeneration { root: changed, .. } => {
                root.starts_with(changed) || changed.starts_with(&root)
            }
            _ => false,
        };
        if ours {
            self.refresh(cx);
        }
    }

    // ---- acting ---------------------------------------------------------------------------

    fn open(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = self.files().get(index).cloned() else {
            return;
        };
        self.selected = Some(index);
        let path = file.path.display().to_string();
        match file.scope {
            Scope::Project => window.dispatch_action(
                Box::new(OpenChosenFile(ChosenFile {
                    path,
                    line: None,
                    column: None,
                    place: FilePlace::Focused,
                })),
                cx,
            ),
            Scope::User => window.dispatch_action(Box::new(RevealInstructionFile(path)), cx),
        }
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let keystroke = &event.keystroke;
        let mods = &keystroke.modifiers;
        if mods.platform || mods.control || mods.alt || mods.shift {
            return false;
        }
        let count = self.files().len();
        if count == 0 {
            return false;
        }
        match keystroke.key.as_str() {
            "down" => self.selected = Some(self.selected.map_or(0, |i| (i + 1).min(count - 1))),
            "up" => self.selected = Some(self.selected.map_or(0, |i| i.saturating_sub(1))),
            "enter" => {
                if let Some(index) = self.selected {
                    self.open(index, window, cx);
                }
            }
            _ => return false,
        }
        cx.notify();
        true
    }
}

/// The user's files alone (no project is open).
fn user_only(homes: Option<crate::instructions::HarnessHomes>, with_user: bool) -> Discovery {
    let Some(homes) = homes.filter(|_| with_user) else {
        return Discovery::default();
    };
    let config = crate::instructions::HarnessConfig::load(&homes);
    let user = crate::instructions::user_candidates(&homes)
        .into_iter()
        .filter_map(|path| {
            let meta = std::fs::metadata(&path).ok().filter(|m| m.is_file())?;
            Some(crate::instructions::UserFile {
                path,
                size: meta.len(),
            })
        })
        .collect();
    discover(&crate::instructions::Facts {
        root: PathBuf::new(),
        project: Vec::new(),
        user,
        homes,
        config,
    })
}

impl Focusable for InstructionsPanel {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

fn harness_icon(harness: Harness) -> Icon {
    match harness {
        Harness::Codex => Icon::HarnessCodex,
        Harness::Claude => Icon::HarnessClaude,
        Harness::Omp => Icon::HarnessOmp,
    }
}

/// The Share button's label: while every agent reads `AGENTS.md` already, sharing is optional.
pub(crate) fn share_label(optional: bool) -> &'static str {
    if optional {
        "Optional — for older Claude Code versions"
    } else {
        "Share one file with every agent…"
    }
}

impl Render for InstructionsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.env.regions.rendered(cx.entity_id());
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let focused = self.focus.contains_focused(window, cx);
        let files = self.files();
        let projects = files.iter().filter(|f| f.scope == Scope::Project).count();

        let refresh = ui::IconButton::new(
            "instructions-refresh",
            Icon::ActionRefresh,
            "Read the instruction files again",
            &theme,
            &tokens,
        )
        .compact()
        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
        .child(probe(&self.env.regions, Region::Instructions("refresh")))
        .into_any_element();
        let header = panel_toolbar("panels.instructions", Vec::new(), vec![refresh], cx);

        let shared = self.discovery().is_some_and(Discovery::shared);
        let optional = self.discovery().is_some_and(Discovery::share_is_optional);
        let share_label = self.share_button_label();
        let share_row = (self.root.is_some() && projects > 0 && !shared).then(|| {
            div()
                .flex_none()
                .px(px(tokens.spacing * 3.0))
                .pb(px(tokens.spacing * 2.0))
                .child(
                    ui::Button::new("instructions-share", share_label, &theme, &tokens)
                        .compact()
                        .tooltip(if optional {
                            "Codex, Claude Code and OMP already read AGENTS.md; this adds an \
                             @AGENTS.md import to CLAUDE.md for Claude Code versions before 2.1.277"
                        } else {
                            "Make AGENTS.md the file Codex, Claude Code and OMP all read"
                        })
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(ShareInstructions), cx);
                        })
                        .child(probe(&self.env.regions, Region::Instructions("share"))),
                )
                .into_any_element()
        });

        let section = |title: &'static str| {
            div()
                .flex_none()
                .px(px(tokens.spacing * 3.0))
                .pt(px(tokens.spacing * 2.0))
                .pb(px(tokens.spacing))
                .text_size(px(tokens.secondary_font_size))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.muted)
                .child(title)
                .into_any_element()
        };
        let note = |text: &'static str| {
            div()
                .flex_none()
                .px(px(tokens.spacing * 3.0))
                .pb(px(tokens.spacing))
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.faint)
                .child(text)
                .into_any_element()
        };

        let mut body: Vec<AnyElement> = Vec::new();
        match &self.state {
            Instructions::Failed(message) => body.push(empty_state(
                "Cannot read the instruction files",
                message.clone(),
                &theme,
                &tokens,
            )),
            Instructions::Idle | Instructions::Loading if files.is_empty() => {
                let connected =
                    self.env.sessions.read(cx).connection_state() == ConnectionState::Connected;
                body.push(match (self.root.is_some(), connected) {
                    (false, _) => empty_state(
                        "No project",
                        "Open a project to see the instruction files the agents read in it."
                            .to_owned(),
                        &theme,
                        &tokens,
                    ),
                    (true, false) => empty_state(
                        "Not connected",
                        "The instruction files are listed once hutand is connected.".to_owned(),
                        &theme,
                        &tokens,
                    ),
                    (true, true) => empty_state("Reading…", String::new(), &theme, &tokens),
                });
            }
            _ => {
                if self.root.is_some() {
                    body.push(section("This project"));
                    if projects == 0 {
                        body.push(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(tokens.spacing * 2.0))
                                .px(px(tokens.spacing * 3.0))
                                .pb(px(tokens.spacing * 2.0))
                                .child(
                                    div()
                                        .text_color(theme.muted)
                                        .child("No instruction file in this project"),
                                )
                                .child(
                                    div()
                                        .text_size(px(tokens.secondary_font_size))
                                        .text_color(theme.faint)
                                        .child(
                                            "Codex and OMP read AGENTS.md, Claude Code reads CLAUDE.md. \
                                             One AGENTS.md in the repository can serve them all.",
                                        ),
                                )
                                .child(
                                    div().flex().child(
                                        ui::Button::new(
                                            "instructions-create",
                                            "Create AGENTS.md",
                                            &theme,
                                            &tokens,
                                        )
                                        .primary()
                                        .on_click(|_, window, cx| {
                                            window.dispatch_action(Box::new(CreateAgentsMd), cx);
                                        })
                                        .child(probe(
                                            &self.env.regions,
                                            Region::Instructions("create"),
                                        )),
                                    ),
                                )
                                .into_any_element(),
                        );
                    }
                }
                let mut index = 0;
                for (title, scope) in [("", Scope::Project), ("Your user files", Scope::User)] {
                    let group: Vec<&InstructionFile> =
                        files.iter().filter(|f| f.scope == scope).collect();
                    let show_user = self
                        .env
                        .settings
                        .get::<InstructionsPanelSettings>()
                        .show_user_files;
                    if scope == Scope::User && show_user {
                        body.push(section(title));
                        body.push(note(if group.is_empty() {
                            "None found in ~/.codex, ~/.claude or ~/.omp/agent."
                        } else {
                            "Read-only here. Edit them in Finder."
                        }));
                    }
                    for file in group {
                        body.push(self.render_row(index, file, focused, &theme, &tokens, cx));
                        index += 1;
                    }
                }
            }
        }

        div()
            .id("instructions-panel")
            .key_context("InstructionsPanel")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.key(event, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(header)
            .children(share_row)
            .child(hutan_ui::scroll_y(
                "instructions-list-scroll",
                div()
                    .id("instructions-list")
                    .flex_1()
                    .min_h_0()
                    .children(body),
            ))
    }
}

impl InstructionsPanel {
    fn render_row(
        &self,
        index: usize,
        file: &InstructionFile,
        focused: bool,
        theme: &crate::theme::Theme,
        tokens: &crate::theme::Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selected == Some(index);
        let hover = theme.sidebar_hover;
        let selected_bg = theme.sidebar_accent;
        let user = file.scope == Scope::User;
        let glyphs: Vec<AnyElement> = file
            .readers
            .iter()
            .map(|reader| {
                let color = match reader.reads {
                    Reads::Always | Reads::Import { .. } | Reads::Linked { .. } => theme.fg,
                    Reads::OnDemand => theme.muted,
                    Reads::Shadowed { .. } => theme.faint,
                };
                let tip = SharedString::from(reader.describe());
                div()
                    .id(SharedString::from(format!(
                        "instructions-glyph-{index}-{}",
                        reader.harness.id()
                    )))
                    .flex_none()
                    .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
                    .child(icons::icon(harness_icon(reader.harness), 14.0, color))
                    .into_any_element()
            })
            .collect();
        let size = if file.size == 0 {
            "empty".to_owned()
        } else {
            human_size(file.size)
        };
        let row = div()
            .id(SharedString::from(format!("instructions-row-{index}")))
            .relative()
            .w_full()
            .min_h(px(ROW_HEIGHT))
            .py(px(tokens.spacing))
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .pl(px(tokens.spacing * 3.0))
            .pr(px(tokens.spacing * 2.0))
            .rounded(px(tokens.radius))
            .cursor_pointer()
            .when(selected && focused, |s| s.bg(selected_bg))
            .hover(move |s| s.bg(hover))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| this.open(index, window, cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_color(theme.fg)
                            .child(SharedString::from(file.label.clone())),
                    )
                    .child(
                        div()
                            .whitespace_normal()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child(SharedString::from(format!("{} · {size}", file.summary()))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(tokens.spacing))
                    .children(glyphs),
            )
            .when(user, |s| {
                s.child(
                    div()
                        .flex_none()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.faint)
                        .child("Show in Finder"),
                )
            })
            .child(probe(&self.env.regions, Region::InstructionRow(index)));
        div()
            .w_full()
            .px(px(tokens.spacing * 2.0))
            .child(row)
            .into_any_element()
    }
}

fn human_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    }
}

fn empty_state(
    title: &'static str,
    detail: String,
    theme: &crate::theme::Theme,
    tokens: &crate::theme::Tokens,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(tokens.spacing))
        .px(px(tokens.spacing * 3.0))
        .py(px(tokens.spacing * 2.0))
        .text_size(px(tokens.font_size))
        .child(div().text_color(theme.muted).child(title))
        .when(!detail.is_empty(), |s| {
            s.child(
                div()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.faint)
                    .child(SharedString::from(detail)),
            )
        })
        .into_any_element()
}
