//! The attach form of an assistant chat (M16-B): the Attach menu and the `@shell`, `@omp` /
//! `@agent`, `@file` and `@search` mentions both end in the same form, which turns a choice into
//! a [`ContextChip`] of the composer.
//!
//! The form only *describes* a source: a terminal and a line count, a block, a transcript id, a
//! path with a line range, a query. It reads nothing itself (the file in particular is never
//! opened here); the daemon resolves every chip when the message is sent. While the form is a
//! menu it leaves the keyboard to the composer; a form with a field takes the typing.

use crate::workbench::ui::Button;
use crate::workbench::ui::IconButton;
use gpui::{
    AnyElement, App, Context, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Task, div, prelude::*, px,
};
use hutan_client::ClientError;
use hutan_core::TerminalId;
use hutan_protocol::ErrorCode;
use hutan_protocol::blocks::{Block, BlockState};
use hutan_protocol::history::{HistoryHit, HistorySearch, HistorySource};

use super::context::{
    ContextChip, DEFAULT_HITS, DEFAULT_TURNS, Mention, MentionKind, clamp_hits, clamp_turns,
    find_mention, parse_file_arg, parse_lines,
};
use super::input::TextInput;
use super::markdown_view::Look;
use super::view::ChatView;
use super::vim::{Outcome, Vim};
use super::widgets::ButtonKind;
use crate::workbench::panels::edit_text;
use crate::workbench::panels::{HISTORY_DEBOUNCE, hit_title, source_label};
use crate::workbench::render::probe;
use crate::workbench::text_field::{EditableText, FieldMut, FieldRef, FieldState};
use crate::workbench::ui::StyledExt as _;
use crate::workbench::ui::motion::{self, PresentExt as _};
use crate::workbench::view::Region;
use crate::workbench::vim_ui::{self, VimConfig};

/// Blocks listed under a terminal (the newest ones).
const BLOCKS_SHOWN: u32 = 12;
/// Indexed transcripts listed for a query.
const INDEXED_SHOWN: u32 = 8;

/// Which step of attaching the user is at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stage {
    /// The Attach menu: the kinds to choose from.
    Menu,
    /// One kind's form.
    Form(MentionKind),
}

/// Something read from the daemon for the form.
#[derive(Debug)]
pub(super) enum Loaded<T> {
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

/// The open Attach menu or form of a chat view.
pub(super) struct Attaching {
    pub(super) stage: Stage,
    /// The field of the form: lines of recent output, a filter or query, a path, a query.
    pub(super) arg: TextInput,
    /// Vim's modal state over `arg` (`editor.vim_mode`).
    pub(super) vim: Vim,
    pub(super) turns: u32,
    pub(super) hits: u32,
    /// The terminal whose blocks are listed.
    pub(super) expanded: Option<TerminalId>,
    pub(super) blocks: Loaded<Vec<Block>>,
    pub(super) indexed: Loaded<Vec<HistoryHit>>,
    /// Why the last Attach did nothing (an out-of-bounds number, a bad range).
    pub(super) error: Option<String>,
    blocks_task: Option<Task<()>>,
    search_task: Option<Task<()>>,
}

impl Attaching {
    fn new(stage: Stage) -> Self {
        Self {
            stage,
            vim: Vim::new(),
            arg: TextInput::new(),
            turns: DEFAULT_TURNS,
            hits: DEFAULT_HITS,
            expanded: None,
            blocks: Loaded::Idle,
            indexed: Loaded::Idle,
            error: None,
            blocks_task: None,
            search_task: None,
        }
    }
}

/// A terminal the form offers.
struct TerminalRow {
    terminal: TerminalId,
    title: String,
    workspace: String,
}

/// A conversation the form offers.
struct TranscriptRow {
    session_id: String,
    /// What the chip calls it.
    source: String,
    title: String,
    detail: String,
}

fn mention_of(kind: MentionKind) -> &'static str {
    match kind {
        MentionKind::Shell => "Shell output",
        MentionKind::Omp => "OMP transcript",
        MentionKind::Agent => "Agent transcript",
        MentionKind::File => "File",
        MentionKind::Search => "History search",
    }
}

impl ChatView {
    // ---- state -----------------------------------------------------------------------------

    /// Whether this chat takes structured context: an assistant does.
    pub fn supports_context(&self, cx: &App) -> bool {
        self.sessions
            .read(cx)
            .session(self.session)
            .and_then(|info| info.chat.as_ref())
            .is_some_and(|chat| chat.is_assistant())
    }

    /// Whether the Attach menu or a form is open.
    pub fn attach_open(&self) -> bool {
        self.attach.is_some()
    }

    /// The form that is open (`None` for the menu or when nothing is open).
    pub fn attach_form(&self) -> Option<MentionKind> {
        match self.attach.as_ref()?.stage {
            Stage::Form(kind) => Some(kind),
            Stage::Menu => None,
        }
    }

    /// The text of the open form's field.
    pub fn attach_arg(&self) -> Option<String> {
        self.attach.as_ref().map(|a| a.arg.text().to_owned())
    }

    /// Why the last Attach did nothing, while the form is open.
    pub fn attach_error(&self) -> Option<&str> {
        self.attach.as_ref()?.error.as_deref()
    }

    /// The mention at the composer's caret, for an assistant chat.
    pub fn mention(&self, cx: &App) -> Option<Mention> {
        if !self.supports_context(cx) {
            return None;
        }
        let model = self.model.read(cx);
        let input = model.input();
        find_mention(input.text(), input.cursor())
    }

    fn terminal_title(&self, terminal: TerminalId, cx: &App) -> String {
        self.sessions
            .read(cx)
            .sessions()
            .iter()
            .find(|info| info.terminal_id == Some(terminal))
            .map_or_else(|| "terminal".to_owned(), |info| info.title.clone())
    }

    /// The live terminals of the daemon, those of this chat's workspace first.
    fn terminals(&self, cx: &App) -> Vec<TerminalRow> {
        let store = self.sessions.read(cx);
        let here = store.session(self.session).map(|info| info.workspace_id);
        let mut rows: Vec<(bool, TerminalRow)> = store
            .sessions()
            .iter()
            .filter(|info| info.state.is_live())
            .filter_map(|info| {
                let terminal = info.terminal_id?;
                Some((
                    Some(info.workspace_id) == here,
                    TerminalRow {
                        terminal,
                        title: info.title.clone(),
                        workspace: store
                            .workspace(info.workspace_id)
                            .map_or_else(String::new, |w| w.name.clone()),
                    },
                ))
            })
            .collect();
        rows.sort_by_key(|(here, _)| !*here);
        rows.into_iter().map(|(_, row)| row).collect()
    }

    /// The conversations the agent form offers: sessions that reported an agent session id, Hutan
    /// chats, and the indexed transcripts the query found.
    fn transcripts(&self, source: Option<&str>, query: &str, cx: &App) -> Vec<TranscriptRow> {
        let store = self.sessions.read(cx);
        let wanted = query.trim().to_lowercase();
        let mut rows: Vec<TranscriptRow> = Vec::new();
        for info in store.sessions() {
            if let Some(report) = &info.agent
                && let Some(id) = &report.agent_session_id
                && source.is_none_or(|s| report.source == s || report.agent == s)
                && (wanted.is_empty() || info.title.to_lowercase().contains(&wanted))
            {
                rows.push(TranscriptRow {
                    session_id: id.clone(),
                    source: format!("{} · {}", info.title, report.agent),
                    title: info.title.clone(),
                    detail: format!("{} · reported by the agent", report.agent),
                });
            } else if source.is_none()
                && info.id != self.session
                && let Some(chat) = &info.chat
                && (wanted.is_empty() || info.title.to_lowercase().contains(&wanted))
            {
                rows.push(TranscriptRow {
                    session_id: info.id.to_string(),
                    source: format!("{} · hutan chat", info.title),
                    title: info.title.clone(),
                    detail: format!("Hutan chat · {}", chat.agent),
                });
            }
        }
        if let Some(Attaching {
            indexed: Loaded::Ready(hits),
            ..
        }) = &self.attach
        {
            for hit in hits {
                if rows.iter().any(|row| row.session_id == hit.session_key) {
                    continue;
                }
                let title = hit_title(hit);
                rows.push(TranscriptRow {
                    session_id: hit.session_key.clone(),
                    source: format!("{title} · {}", source_label(hit.source)),
                    title,
                    detail: format!("{} · indexed", source_label(hit.source)),
                });
            }
        }
        rows
    }

    // ---- opening and closing ---------------------------------------------------------------

    /// Opens the Attach menu, or closes it when it is open.
    pub fn toggle_attach_menu(&mut self, cx: &mut Context<Self>) {
        if self.attach.is_some() {
            self.attach = None;
        } else {
            self.attach = Some(Attaching::new(Stage::Menu));
        }
        cx.notify();
    }

    /// Opens the form of `kind` with `arg` already in its field.
    pub fn open_attach(&mut self, kind: MentionKind, arg: &str, cx: &mut Context<Self>) {
        if !self.supports_context(cx) {
            return;
        }
        let mut attaching = Attaching::new(Stage::Form(kind));
        attaching.arg.set(arg);
        self.attach = Some(attaching);
        if matches!(kind, MentionKind::Omp | MentionKind::Agent) {
            self.search_indexed(cx);
        }
        cx.notify();
    }

    pub fn close_attach(&mut self, cx: &mut Context<Self>) {
        if self.attach.take().is_some() {
            cx.notify();
        }
    }

    /// Replaces the form's field.
    pub fn set_attach_arg(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(attaching) = &mut self.attach {
            attaching.arg.set(text);
        }
        self.arg_changed(cx);
    }

    fn arg_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(attaching) = &mut self.attach {
            attaching.error = None;
        }
        if matches!(
            self.attach_form(),
            Some(MentionKind::Omp | MentionKind::Agent)
        ) {
            self.search_indexed(cx);
        }
        cx.notify();
    }

    /// Steps the form's turns (agent forms) or hits (search form).
    pub fn step_attach_count(&mut self, delta: i32, cx: &mut Context<Self>) {
        let form = self.attach_form();
        if let Some(attaching) = &mut self.attach {
            match form {
                Some(MentionKind::Search) => {
                    attaching.hits = clamp_hits(attaching.hits.saturating_add_signed(delta));
                }
                Some(MentionKind::Omp | MentionKind::Agent) => {
                    attaching.turns = clamp_turns(attaching.turns.saturating_add_signed(delta));
                }
                _ => {}
            }
        }
        cx.notify();
    }

    /// The turns the agent form asks for.
    pub fn attach_turns(&self) -> Option<u32> {
        self.attach.as_ref().map(|a| a.turns)
    }

    /// The hits the search form asks for.
    pub fn attach_hits(&self) -> Option<u32> {
        self.attach.as_ref().map(|a| a.hits)
    }

    // ---- choosing --------------------------------------------------------------------------

    fn attach_chip(&mut self, chip: ContextChip, cx: &mut Context<Self>) -> bool {
        let added = self
            .model
            .update(cx, |model, cx| model.attach_context(chip, cx));
        if added {
            self.attach = None;
        }
        cx.notify();
        added
    }

    fn fail(&mut self, message: String, cx: &mut Context<Self>) -> bool {
        if let Some(attaching) = &mut self.attach {
            attaching.error = Some(message);
        }
        cx.notify();
        false
    }

    /// Attaches the last lines of `terminal`'s screen and scrollback, as many as the field asks
    /// for (50 when it is empty).
    pub fn attach_recent(&mut self, terminal: TerminalId, cx: &mut Context<Self>) -> bool {
        let text = self.attach_arg().unwrap_or_default();
        match parse_lines(&text) {
            Ok(lines) => {
                let title = self.terminal_title(terminal, cx);
                self.attach_chip(ContextChip::recent(&title, terminal, lines), cx)
            }
            Err(message) => self.fail(message, cx),
        }
    }

    /// Lists (or stops listing) the newest blocks of `terminal`.
    pub fn toggle_blocks(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        let connection = self.sessions.read(cx).connection().clone();
        let Some(attaching) = &mut self.attach else {
            return;
        };
        if attaching.expanded == Some(terminal) {
            attaching.expanded = None;
            attaching.blocks = Loaded::Idle;
            attaching.blocks_task = None;
            cx.notify();
            return;
        }
        attaching.expanded = Some(terminal);
        attaching.blocks = Loaded::Loading;
        attaching.blocks_task = Some(cx.spawn(async move |this, cx| {
            let answer = connection
                .terminal_blocks(terminal, None, BLOCKS_SHOWN)
                .await;
            this.update(cx, |this, cx| {
                if let Some(attaching) = &mut this.attach
                    && attaching.expanded == Some(terminal)
                {
                    attaching.blocks = match answer {
                        Ok(result) => Loaded::Ready(result.blocks),
                        Err(error) => Loaded::Failed(error.to_string()),
                    };
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    /// Attaches the output of one block of `terminal`.
    pub fn attach_block(
        &mut self,
        terminal: TerminalId,
        block: u64,
        cx: &mut Context<Self>,
    ) -> bool {
        let title = self.terminal_title(terminal, cx);
        let known = match self.attach.as_ref().map(|a| &a.blocks) {
            Some(Loaded::Ready(blocks)) => blocks.iter().find(|b| b.id == block).cloned(),
            _ => None,
        };
        let chip = ContextChip::block(
            &title,
            terminal,
            block,
            known.as_ref().and_then(|b| b.command.as_deref()),
            known.as_ref().and_then(|b| b.exit_code),
        );
        self.attach_chip(chip, cx)
    }

    /// Attaches the last turns (the form's stepper) of the conversation `session_id`: a Hutan
    /// chat, or an agent session of the history index.
    pub fn attach_transcript(
        &mut self,
        session_id: &str,
        source: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let turns = self.attach_turns().unwrap_or(DEFAULT_TURNS);
        self.attach_chip(ContextChip::transcript(source, session_id, turns), cx)
    }

    /// The Attach button of the file and search forms: attaches what the field says.
    pub fn confirm_attach(&mut self, cx: &mut Context<Self>) -> bool {
        let text = self.attach_arg().unwrap_or_default();
        match self.attach_form() {
            Some(MentionKind::File) => match parse_file_arg(&text) {
                Ok((path, range)) => self.attach_chip(ContextChip::file(&path, range), cx),
                Err(message) => self.fail(message, cx),
            },
            Some(MentionKind::Search) => {
                let query = text.trim().trim_matches('"').trim();
                if query.is_empty() {
                    return self.fail("type the words to search for".to_owned(), cx);
                }
                let hits = self.attach_hits().unwrap_or(DEFAULT_HITS);
                self.attach_chip(ContextChip::search(query, hits), cx)
            }
            _ => false,
        }
    }

    /// Turns the mention at the caret into what it offers: a file or search chip when the
    /// mention carries its argument, else the form of its kind (with the argument in the field).
    /// The mention leaves the draft. `false` when there is no mention.
    pub fn accept_mention(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(mention) = self.mention(cx) else {
            return false;
        };
        let range = mention.range.clone();
        self.model.update(cx, |model, cx| {
            model.edit(|input| input.remove_range(range), cx)
        });
        self.open_attach(mention.kind, &mention.arg, cx);
        if matches!(mention.kind, MentionKind::File | MentionKind::Search)
            && !mention.arg.is_empty()
        {
            self.confirm_attach(cx);
        }
        true
    }

    /// Looks for indexed transcripts matching the agent form's field, after a pause.
    fn search_indexed(&mut self, cx: &mut Context<Self>) {
        let connection = self.sessions.read(cx).connection().clone();
        let form = self.attach_form();
        let Some(attaching) = &mut self.attach else {
            return;
        };
        let query = attaching.arg.text().trim().to_owned();
        if query.is_empty() {
            attaching.indexed = Loaded::Idle;
            attaching.search_task = None;
            return;
        }
        attaching.indexed = Loaded::Loading;
        let sources = if form == Some(MentionKind::Omp) {
            vec![HistorySource::Omp]
        } else {
            vec![
                HistorySource::Claude,
                HistorySource::Codex,
                HistorySource::Omp,
            ]
        };
        let request = HistorySearch {
            query: query.clone(),
            limit: INDEXED_SHOWN,
            sources,
            roles: Vec::new(),
            project: None,
            since_ms: None,
            until_ms: None,
        };
        attaching.search_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HISTORY_DEBOUNCE).await;
            let answer = cx
                .background_executor()
                .spawn(async move { connection.history_search(request).await })
                .await;
            this.update(cx, |this, cx| {
                let Some(attaching) = &mut this.attach else {
                    return;
                };
                if attaching.arg.text().trim() != query {
                    return;
                }
                attaching.indexed = match answer {
                    Ok(found) => Loaded::Ready(found.hits),
                    Err(ClientError::Remote(body)) if body.code == ErrorCode::InvalidState => {
                        Loaded::Failed(
                            "Indexing is off: turn it on in the History panel to attach indexed transcripts"
                                .to_owned(),
                        )
                    }
                    Err(error) => Loaded::Failed(error.to_string()),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    // ---- keys ------------------------------------------------------------------------------

    /// Keys while the Attach menu or a form is open. `true` when the key was used. The menu
    /// leaves typing to the composer; a form's field takes it.
    pub(super) fn on_attach_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let Some(stage) = self.attach.as_ref().map(|a| a.stage) else {
            return false;
        };
        let keystroke = &event.keystroke;
        let mods = &keystroke.modifiers;
        let key = keystroke.key.as_str();
        let plain = !mods.platform && !mods.control && !mods.alt && !mods.function;
        if plain && key == "escape" {
            // Under Vim mode the first Escape of the form's field leaves Insert (or Visual).
            let vim = VimConfig::of(&self.settings);
            if vim.enabled
                && matches!(stage, Stage::Form(_))
                && let Some(attaching) = &mut self.attach
                && vim_ui::apply(
                    &mut attaching.vim,
                    &mut attaching.arg,
                    event,
                    vim.options(true, true),
                    cx,
                ) == Outcome::Handled
            {
                cx.notify();
                return true;
            }
            self.close_attach(cx);
            return true;
        }
        let Stage::Form(_) = stage else {
            return false;
        };
        let vim = VimConfig::of(&self.settings);
        let plain_keys = !mods.platform && !mods.control && !mods.alt;
        if key == "enter" && plain_keys {
            self.confirm_attach(cx);
            return true;
        }
        if matches!(key, "up" | "down" | "tab") && plain_keys {
            return false;
        }
        let Some(attaching) = &mut self.attach else {
            return false;
        };
        // Vim's commands and the editing keys; typed text reaches the field through the
        // platform's text input (`attach_field`).
        let used = edit_text(
            &mut attaching.arg,
            &mut attaching.vim,
            vim,
            event,
            false,
            cx,
        );
        if used {
            self.arg_changed(cx);
        }
        used
    }

    /// How the form's field element reaches its text: the platform's text input and the mouse.
    fn attach_field(&self, cx: &mut Context<Self>) -> FieldRef {
        FieldRef::new(cx.weak_entity(), self.focus_handle(cx), |this, cx, f| {
            let vim_on = VimConfig::of(&this.settings).enabled;
            let Some(attaching) = &mut this.attach else {
                return;
            };
            let before = attaching.arg.text().to_owned();
            f(&mut FieldMut {
                input: &mut attaching.arg,
                vim: vim_on.then_some(&mut attaching.vim),
                multiline: false,
            });
            if attaching.arg.text() != before {
                this.arg_changed(cx);
            }
        })
    }

    // ---- rendering -------------------------------------------------------------------------

    /// What the composer shows while the caret sits in a mention: what Tab would do.
    pub(super) fn render_mention_hint(
        &self,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.attach.is_some() {
            return None;
        }
        let mention = self.mention(cx)?;
        let (theme, tokens) = (&look.theme, &look.tokens);
        let offer = match (mention.kind, mention.arg.is_empty()) {
            (MentionKind::File, false) => match parse_file_arg(&mention.arg) {
                Ok((path, range)) => format!("Attach {}", ContextChip::file(&path, range).label()),
                Err(message) => message,
            },
            (MentionKind::Search, false) => format!(
                "Attach {}",
                ContextChip::search(&mention.arg, DEFAULT_HITS).label()
            ),
            (kind, _) => format!("Choose: {}", mention_of(kind).to_lowercase()),
        };
        Some(
            div()
                .relative()
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.accent)
                        .text_size(px(tokens.secondary_font_size))
                        .child(SharedString::from(format!(
                            "@{} · {offer}",
                            mention.kind.token()
                        ))),
                )
                .child(
                    Button::new("chat-mention", "Attach ↹", &look.theme, &look.tokens)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.accept_mention(cx);
                        }))
                        .child(probe(&self.regions, Region::Attach("mention"))),
                )
                .into_any_element(),
        )
    }

    /// The Attach button of the composer's footer.
    pub(super) fn render_attach_button(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        IconButton::new(
            "chat-attach",
            crate::icons::Icon::ActionAddContext,
            "Attach",
            &look.theme,
            &look.tokens,
        )
        .kind(ButtonKind::Secondary)
        .round()
        .side(28.0)
        .toggled(self.attach.is_some())
        .on_click(cx.listener(|this, _, _, cx| this.toggle_attach_menu(cx)))
        .child(probe(&self.regions, Region::Attach("button")))
        .into_any_element()
    }

    /// One line of a list: a title, what it is, and its buttons.
    fn list_row(
        &self,
        index: usize,
        id: String,
        title: &str,
        detail: &str,
        buttons: Vec<AnyElement>,
        look: &Look,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        div()
            .id(SharedString::from(id))
            .relative()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .min_h(px(tokens.sidebar_row))
            .px(px(tokens.spacing * 2.0))
            .rounded(px(tokens.radius))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(title.to_owned())),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(260.0))
                    .truncate()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(detail.to_owned())),
            )
            .children(buttons)
            .child(probe(&self.regions, Region::AttachRow(index)))
            .into_any_element()
    }

    fn stepper(
        &self,
        what: &'static str,
        value: u32,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        div()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .child(div().text_color(theme.muted).child(what))
            .child(
                Button::new(format!("attach-{what}-dec"), "−", &look.theme, &look.tokens)
                    .on_click(cx.listener(|this, _, _, cx| this.step_attach_count(-1, cx)))
                    .child(probe(&self.regions, Region::Attach("less"))),
            )
            .child(SharedString::from(value.to_string()))
            .child(
                Button::new(format!("attach-{what}-inc"), "+", &look.theme, &look.tokens)
                    .on_click(cx.listener(|this, _, _, cx| this.step_attach_count(1, cx)))
                    .child(probe(&self.regions, Region::Attach("more"))),
            )
            .into_any_element()
    }

    /// The Attach menu or form above the composer.
    pub(super) fn render_attach(
        &mut self,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let stage = self.attach.as_ref()?.stage;
        let (theme, tokens) = (&look.theme, &look.tokens);
        let title = match stage {
            Stage::Menu => "Attach context to the next message".to_owned(),
            Stage::Form(kind) => format!("Attach · {}", mention_of(kind).to_lowercase()),
        };
        let header = div()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .child(
                div()
                    .flex_1()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(SharedString::from(title)),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child("Esc closes · nothing is read until you send"),
            )
            .child(
                IconButton::new(
                    "chat-attach-close",
                    crate::icons::Icon::ActionClose,
                    "Close",
                    theme,
                    tokens,
                )
                .compact()
                .on_click(cx.listener(|this, _, _, cx| this.close_attach(cx)))
                .child(probe(&self.regions, Region::Attach("close"))),
            );
        let body = match stage {
            Stage::Menu => self.render_attach_menu(look, cx),
            Stage::Form(kind) => self.render_attach_form(kind, look, cx),
        };
        Some(
            div()
                .id("chat-attach-form")
                .relative()
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(tokens.spacing * 1.5))
                .mx(px(tokens.spacing * 3.0))
                .mt(px(tokens.spacing * 2.0))
                .max_h(px(340.0))
                .overflow_y_scroll()
                .popover(theme, tokens)
                .p(px(tokens.spacing * 2.5))
                .child(header)
                .child(body)
                .presented(
                    "attach-motion",
                    motion::Kind::Popover,
                    true,
                    motion::Slide::None,
                )
                .into_any_element(),
        )
    }

    fn render_attach_menu(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let tokens = &look.tokens;
        let item =
            |id: &'static str, label: &'static str, kind: MentionKind, cx: &mut Context<Self>| {
                Button::new(
                    format!("chat-attach-{id}"),
                    label,
                    &look.theme,
                    &look.tokens,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.open_attach(kind, "", cx)))
                .child(probe(&self.regions, Region::Attach(id)))
            };
        let supports_context = self.supports_context(cx);
        div()
            .flex()
            .flex_wrap()
            .gap(px(tokens.spacing * 2.0))
            .child(
                Button::new(
                    "chat-attach-files",
                    "Attach file or image…",
                    &look.theme,
                    &look.tokens,
                )
                .on_click(cx.listener(|this, _, window, cx| this.pick_files(window, cx)))
                .child(probe(&self.regions, Region::Attach("files"))),
            )
            .when(supports_context, |menu| {
                menu.child(item(
                    "shell",
                    "@shell · terminal output",
                    MentionKind::Shell,
                    cx,
                ))
                .child(item("omp", "@omp · OMP transcript", MentionKind::Omp, cx))
                .child(item(
                    "agent",
                    "@agent · agent transcript",
                    MentionKind::Agent,
                    cx,
                ))
                .child(item(
                    "file",
                    "@file · workspace file",
                    MentionKind::File,
                    cx,
                ))
                .child(item(
                    "search",
                    "@search · history search",
                    MentionKind::Search,
                    cx,
                ))
            })
            .into_any_element()
    }

    fn render_attach_form(
        &mut self,
        kind: MentionKind,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let (arg, vim_state, error, expanded, blocks_state) = {
            let Some(attaching) = &self.attach else {
                return div().into_any_element();
            };
            (
                attaching.arg.clone(),
                VimConfig::of(&self.settings)
                    .enabled
                    .then(|| attaching.vim.clone()),
                attaching.error.clone(),
                attaching.expanded,
                match &attaching.blocks {
                    Loaded::Idle => (None, None),
                    Loaded::Loading => (None, Some("Loading the blocks…".to_owned())),
                    Loaded::Failed(error) => {
                        (None, Some(format!("Could not read the blocks: {error}")))
                    }
                    Loaded::Ready(blocks) => (Some(blocks.clone()), None),
                },
            )
        };
        let placeholder = match kind {
            MentionKind::Shell => "lines of recent output (default 50, at most 500)",
            MentionKind::Omp | MentionKind::Agent => {
                "filter, or words to find in indexed transcripts"
            }
            MentionKind::File => "path under the workspace, e.g. src/lib.rs:1-40",
            MentionKind::Search => "words to search the history for",
        };
        let field = div()
            .id("chat-attach-field")
            .relative()
            .flex()
            .items_center()
            .gap(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .py(px(tokens.spacing))
            .rounded(px(tokens.radius))
            .bg(theme.bg)
            .border_1()
            .border_color(theme.border)
            .child(
                div().flex_1().min_w_0().child(
                    EditableText::new(
                        "chat-attach-text",
                        FieldState::of(&arg, vim_state.as_ref()),
                        theme,
                    )
                    .placeholder(placeholder)
                    .focused(true)
                    .field(Some(self.attach_field(cx))),
                ),
            )
            .children(
                vim_state
                    .as_ref()
                    .map(|vim| vim_ui::mode_pill(vim.mode(), theme, tokens)),
            )
            .child(probe(&self.regions, Region::Attach("field")));
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut footer: Vec<AnyElement> = Vec::new();
        match kind {
            MentionKind::Shell => {
                let terminals = self.terminals(cx);
                if terminals.is_empty() {
                    rows.push(
                        div()
                            .text_color(theme.muted)
                            .child("No terminal is running.")
                            .into_any_element(),
                    );
                }
                for row in terminals {
                    let terminal = row.terminal;
                    let open = expanded == Some(terminal);
                    let buttons = vec![
                        Button::new(
                            format!("attach-recent-{terminal}"),
                            "Recent output",
                            &look.theme,
                            &look.tokens,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.attach_recent(terminal, cx);
                        }))
                        .child(probe(&self.regions, Region::Attach("recent")))
                        .into_any_element(),
                        Button::new(
                            format!("attach-blocks-{terminal}"),
                            "Blocks",
                            &look.theme,
                            &look.tokens,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_blocks(terminal, cx);
                        }))
                        .child(crate::icons::glyph(if open {
                            crate::icons::Icon::ActionCollapse
                        } else {
                            crate::icons::Icon::ActionExpand
                        }))
                        .child(probe(&self.regions, Region::Attach("blocks")))
                        .into_any_element(),
                    ];
                    let at = rows.len();
                    rows.push(self.list_row(
                        at,
                        format!("attach-terminal-{terminal}"),
                        &row.title,
                        &row.workspace,
                        buttons,
                        look,
                    ));
                    if open {
                        let (blocks, note) = &blocks_state;
                        if let Some(note) = note {
                            rows.push(
                                div()
                                    .pl(px(tokens.spacing * 4.0))
                                    .text_color(theme.muted)
                                    .child(SharedString::from(note.clone()))
                                    .into_any_element(),
                            );
                        }
                        let shown: Vec<&Block> = blocks
                            .iter()
                            .flatten()
                            .filter(|b| {
                                matches!(b.state, BlockState::Finished | BlockState::Running)
                                    && (b.command.is_some() || b.output_start.is_some())
                            })
                            .collect();
                        if blocks.is_some() && shown.is_empty() {
                            rows.push(
                                div()
                                    .pl(px(tokens.spacing * 4.0))
                                    .text_color(theme.muted)
                                    .child("No finished command yet (needs shell integration).")
                                    .into_any_element(),
                            );
                        }
                        for block in shown {
                            let id = block.id;
                            let command = block
                                .command
                                .as_deref()
                                .and_then(|c| c.lines().next())
                                .unwrap_or("(output)");
                            let detail = match (block.state, block.exit_code) {
                                (BlockState::Running, _) => "running".to_owned(),
                                (_, Some(code)) => format!("exit {code}"),
                                _ => "finished".to_owned(),
                            };
                            let at = rows.len();
                            rows.push(
                                div()
                                    .pl(px(tokens.spacing * 4.0))
                                    .child(self.list_row(
                                        at,
                                        format!("attach-block-{terminal}-{id}"),
                                        &format!("$ {command}"),
                                        &detail,
                                        vec![
                                                Button::new(
                                                    format!("attach-block-button-{terminal}-{id}"),
                                                    "Attach block",
                                                    &look.theme,
                                                    &look.tokens,
                                                )
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.attach_block(terminal, id, cx);
                                                }))
                                                .child(probe(
                                                    &self.regions,
                                                    Region::Attach("block"),
                                                ))
                                                .into_any_element(),
                                            ],
                                        look,
                                    ))
                                    .into_any_element(),
                            );
                        }
                    }
                }
            }
            MentionKind::Omp | MentionKind::Agent => {
                let source = (kind == MentionKind::Omp).then_some("omp");
                let query = arg.text().to_owned();
                let turns = self.attach_turns().unwrap_or(DEFAULT_TURNS);
                footer.push(self.stepper("turns", turns, look, cx));
                let transcripts = self.transcripts(source, &query, cx);
                if transcripts.is_empty() {
                    rows.push(
                        div()
                            .text_color(theme.muted)
                            .child(if query.trim().is_empty() {
                                "No reported session. Type words to search indexed transcripts."
                            } else {
                                "Nothing matches."
                            })
                            .into_any_element(),
                    );
                }
                let note = match self.attach.as_ref().map(|a| &a.indexed) {
                    Some(Loaded::Loading) => Some("Searching the index…".to_owned()),
                    Some(Loaded::Failed(error)) => Some(error.clone()),
                    _ => None,
                };
                if let Some(note) = note {
                    rows.push(
                        div()
                            .text_color(theme.muted)
                            .child(SharedString::from(note))
                            .into_any_element(),
                    );
                }
                for (at, row) in transcripts.into_iter().enumerate() {
                    let (id, source) = (row.session_id.clone(), row.source.clone());
                    let button_id = format!("attach-transcript-{at}");
                    let attach =
                        Button::new(button_id.clone(), "Attach", &look.theme, &look.tokens)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.attach_transcript(&id, &source, cx);
                            }))
                            .child(probe(&self.regions, Region::Attach("transcript")))
                            .into_any_element();
                    rows.push(self.list_row(
                        at,
                        format!("attach-row-{at}"),
                        &row.title,
                        &row.detail,
                        vec![attach],
                        look,
                    ));
                }
            }
            MentionKind::File => {
                let preview = match parse_file_arg(arg.text()) {
                    Ok((path, range)) if !arg.is_empty() => {
                        Some(ContextChip::file(&path, range).label())
                    }
                    _ => None,
                };
                rows.push(
                    div()
                        .text_color(theme.muted)
                        .text_size(px(tokens.secondary_font_size))
                        .child("The daemon reads the file below the workspace folders when you send; this window never opens it.")
                        .into_any_element(),
                );
                footer.push(self.confirm_button("Attach file", preview.is_some(), look, cx));
                if let Some(preview) = preview {
                    footer.push(
                        div()
                            .text_color(theme.muted)
                            .font_family(look.mono.clone())
                            .child(SharedString::from(preview))
                            .into_any_element(),
                    );
                }
            }
            MentionKind::Search => {
                let query = arg.text().trim().to_owned();
                let hits = self.attach_hits().unwrap_or(DEFAULT_HITS);
                footer.push(self.stepper("hits", hits, look, cx));
                footer.push(self.confirm_button("Attach search", !query.is_empty(), look, cx));
            }
        }
        div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 1.5))
            .child(field)
            .children(error.map(|error| {
                div()
                    .text_color(theme.danger)
                    .child(SharedString::from(error))
            }))
            .child(div().flex().flex_col().children(rows))
            .when(!footer.is_empty(), |s| {
                s.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(tokens.spacing * 3.0))
                        .children(footer),
                )
            })
            .into_any_element()
    }

    fn confirm_button(
        &self,
        label: &'static str,
        enabled: bool,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        Button::new("chat-attach-confirm", label, &look.theme, &look.tokens)
            .primary()
            .disabled(!enabled)
            .when(enabled, |b| {
                b.on_click(cx.listener(|this, _, _, cx| {
                    this.confirm_attach(cx);
                }))
            })
            .child(probe(&self.regions, Region::Attach("confirm")))
            .into_any_element()
    }
}
