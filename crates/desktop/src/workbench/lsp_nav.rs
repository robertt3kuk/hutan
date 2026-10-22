//! What the workbench does with a file tab's language server (slice E-12a): the hover
//! popover's text, go to definition across files with a jumplist (*Go Back*, `⌃O`), opening a
//! file at an LSP position (definitions, the Problems panel), the Problems panel, and what the
//! `editor.diagnostics` status item shows. The documents, diagnostics and servers themselves
//! are `lsp.rs`'s.

use std::path::PathBuf;

use gpui::{AppContext as _, Context, Entity, Window};
use hutan_client::ClientError;
use hutan_code_view::CodeView;
use hutan_protocol::fs::FsKind;
use hutan_protocol::lsp::{LspLocations, LspPosition};

use super::file_view::FileView;
use super::files::{FileOpen, FilePlace, error_text};
use super::lsp::{Jump, LspStore, LspStoreEvent, position_of};
use super::registry::{EditorDiagnostics, EditorServer};
use super::view::Workbench;

impl Workbench {
    /// The window's language servers (tests).
    pub fn lsp(&self) -> &Entity<LspStore> {
        &self.lsp
    }

    pub(crate) fn on_lsp_event(&mut self, event: &LspStoreEvent, cx: &mut Context<Self>) {
        match event {
            // The status item counts the focused file's diagnostics.
            LspStoreEvent::Changed => cx.notify(),
            LspStoreEvent::Notice(text) => self.set_notice(text.clone(), cx),
        }
    }

    /// What `editor.diagnostics` shows for the file of `path`: its counts once its document
    /// has a server, and that server's state.
    pub(crate) fn editor_diagnostics(&self, path: &str, cx: &gpui::App) -> EditorDiagnostics {
        let lsp = self.lsp.read(cx);
        let server = lsp.server_of(path).map(|s| EditorServer {
            name: s.name,
            state: s.state,
            reason: s.reason,
        });
        let counts = self
            .buffers
            .get(path)
            .filter(|_| lsp.is_open(path))
            .map(|b| b.read(cx).diagnostic_counts());
        EditorDiagnostics { counts, server }
    }

    /// The hover popover of `view` opened at byte `offset`: the server's text goes into it, or
    /// nothing when the file has no server.
    pub(crate) fn lsp_hover(
        &mut self,
        view: &Entity<FileView>,
        offset: usize,
        cx: &mut Context<Self>,
    ) {
        let code = view.read(cx).code().clone();
        let (path, position) = {
            let buffer = view.read(cx).buffer().read(cx);
            (
                buffer.path().display().to_string(),
                position_of(buffer.text(), offset),
            )
        };
        let Some(call) = self
            .lsp
            .update(cx, |lsp, cx| lsp.hover(&path, position, cx))
        else {
            code.update(cx, |code, cx| code.set_hover_text(offset, None, cx));
            return;
        };
        cx.spawn(async move |_, cx| {
            let text = match call.await {
                Ok(hover) => hover.contents,
                Err(error) => {
                    tracing::debug!(%error, "lsp.hover failed");
                    None
                }
            };
            code.update(cx, |code, cx| {
                code.set_hover_text(offset, text.as_deref(), cx)
            });
        })
        .detach();
    }

    /// ⌘-click, `gd`, F12 in `view` at byte `offset`: asks the server and goes to the first
    /// place it names, in this file or another one; *Go Back* returns.
    pub(crate) fn go_to_definition(
        &mut self,
        view: &Entity<FileView>,
        offset: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let code = view.read(cx).code().clone();
        let (path, position, caret) = {
            let buffer = view.read(cx).buffer().read(cx);
            let head = code.read(cx).selections().primary().head;
            (
                buffer.path().display().to_string(),
                position_of(buffer.text(), offset),
                position_of(buffer.text(), head),
            )
        };
        let Some(call) = self
            .lsp
            .update(cx, |lsp, cx| lsp.definition(&path, position, cx))
        else {
            self.set_notice("No language server for this file", cx);
            return;
        };
        let from = Jump {
            path,
            position: caret,
        };
        cx.spawn_in(window, async move |this, cx| {
            let answer = call.await;
            this.update_in(cx, |this, window, cx| {
                this.definition_arrived(&code, from, answer, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn definition_arrived(
        &mut self,
        code: &Entity<CodeView>,
        from: Jump,
        answer: Result<LspLocations, ClientError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let location = match answer {
            Ok(answer) => answer.locations.into_iter().next(),
            Err(error) => {
                self.set_notice(format!("Go to definition: {}", error_text(&error)), cx);
                return;
            }
        };
        let Some(location) = location else {
            self.set_notice("No definition found", cx);
            return;
        };
        let start = location.range.start;
        if location.path.display().to_string() == from.path {
            // In the same file: Vim's own jumplist takes it (its `⌃O`); without Vim, *Go Back*.
            if code.read(cx).vim_mode().is_none() {
                self.lsp.update(cx, |lsp, _| lsp.push_jump(from));
            }
            code.update(cx, |code, cx| {
                code.jump_to_utf16(start.line as usize, start.character as usize, cx);
            });
            return;
        }
        self.lsp.update(cx, |lsp, _| lsp.push_jump(from));
        self.open_at_lsp(location.path, start, true, window, cx);
    }

    /// *Go Back* (`⌃O` after a jump from another file): the place the last jump left.
    pub(crate) fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(jump) = self.lsp.update(cx, |lsp, _| lsp.pop_jump()) else {
            return;
        };
        self.open_at_lsp(PathBuf::from(jump.path), jump.position, false, window, cx);
    }

    /// Opens `path` (or focuses its tab) with the caret at `position` (zero-based line, UTF-16
    /// column). `arrived`: a jump from another file brought it, so `⌃O` there goes back.
    pub(crate) fn open_at_lsp(
        &mut self,
        path: PathBuf,
        position: LspPosition,
        arrived: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let connection = self.sessions.read(cx).connection().clone();
        let stat = cx.background_spawn(async move {
            let entry = connection.fs_stat(&path, true).await;
            (path, entry)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (path, entry) = stat.await;
            this.update_in(cx, |this, window, cx| match entry {
                Ok(entry) if entry.kind == FsKind::File => {
                    let target = FileOpen::picked(entry.path.clone());
                    this.show_file(
                        &entry.path,
                        entry.size,
                        &target,
                        FilePlace::Focused,
                        window,
                        cx,
                    );
                    let key = entry.path.display().to_string();
                    let pane = this.state.active_pane_id();
                    if let Some(view) = this.file_view_of(pane, &key) {
                        let code = view.read(cx).code().clone();
                        code.update(cx, |code, cx| {
                            code.reveal_utf16(
                                position.line as usize,
                                position.character as usize,
                                cx,
                            );
                            if arrived {
                                code.arrived_by_jump();
                            }
                        });
                    }
                }
                Ok(entry) => {
                    this.set_notice(format!("{} is not a file", entry.path.display()), cx);
                }
                Err(error) => this.set_notice(
                    format!("Cannot open {}: {}", path.display(), error_text(&error)),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }

    /// The Problems panel, shown with the keyboard in it (the status item's click, *Problems:
    /// Show Problems Panel*, ⌘⇧M).
    pub(crate) fn show_problems(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_panel("problems", cx);
        let panel = self
            .panel_view(&super::model::PanelId::new("problems"), window, cx)
            .and_then(|view| view.downcast::<super::panels::ProblemsPanel>().ok());
        let Some(panel) = panel else {
            self.set_notice("The Problems panel is not available", cx);
            return;
        };
        // The panel is drawn (and its list can take the keyboard) after this frame.
        cx.defer_in(window, move |_, window, cx| {
            panel.update(cx, |panel, cx| panel.focus_list(window, cx));
        });
    }
}
