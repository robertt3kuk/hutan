//! The cards of a transcript: messages, reasoning, tool calls (commands with their output, file
//! changes with diffs, other tools), approvals, plans, and the turn footer.

use std::rc::Rc;

use super::context::{meta_text, split_context};
use super::markdown_view::{Ids, Look, blocks, code_block};
use super::view::ChatView;
use super::widgets::{
    ButtonKind, always_note, approval_group, approval_option, approval_outcome, decision_mark,
    duration, pill, scope_tooltip, tail_lines, tokens,
};
use crate::workbench::harnesses::harness_name;
use crate::workbench::render::probe;
use crate::workbench::ui::motion;
use crate::workbench::ui::{Button, StyledExt as _, approval_button_kind};
use crate::workbench::view::Region;
use gpui::{
    AnyElement, App, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px, relative,
};
use hutan_chat_view::diff::{self, DiffLine, FileDiff, LineKind, MAX_LINES};
use hutan_chat_view::markdown::{self, Block as MdBlock};
use hutan_chat_view::{
    Approval, ApprovalState, Body, Entry, Footer, Message, Plan, Reasoning, Tool, Transcript,
    TurnOutcome, TurnUsage,
};
use hutan_core::ApprovalId;
use hutan_core::agent::{FileChangeKind, NoticeLevel, PlanStepStatus, ToolKind, ToolStatus};

/// The default open/closed part of a card (a command's output, a tool's arguments, reasoning).
pub(super) const PART_BODY: u32 = 0;
/// A file's diff in a file change card: `PART_DIFF + index`.
pub(super) const PART_DIFF: u32 = 100;
/// The approvals this window saw waiting and when each was answered: an answered card keeps its
/// buttons folding away for the length of a collapse (`approval_card`), and no card that was
/// not seen waiting does (a transcript loaded with its approvals answered).
#[derive(Debug, Default)]
pub(super) struct ApprovalsSeen {
    waiting: std::collections::HashSet<ApprovalId>,
    answered: std::collections::HashMap<ApprovalId, std::time::Instant>,
}

/// Lines of a command's output shown while it is collapsed.
const TAIL_LINES: usize = 8;
/// Most lines of a command's output drawn even when expanded.
const MAX_OUTPUT_LINES: usize = 2000;

/// The state color of a tool that ended badly: `danger` for a failure or a non-zero exit,
/// `warning` for a declined one; `None` while it runs and once it ended well or was stopped,
/// when its header's words stay `muted`.
fn status_tone(look: &Look, tool: &Tool) -> Option<gpui::Hsla> {
    let theme = &look.theme;
    match (tool.status, tool.interrupted) {
        (_, true) | (None, _) => None,
        (Some(ToolStatus::Completed), _) => match tool.exit_code {
            Some(code) if code != 0 => Some(theme.danger),
            _ => None,
        },
        (Some(ToolStatus::Failed), _) => Some(theme.danger),
        (Some(ToolStatus::Declined), _) => Some(theme.warning),
    }
}

fn status_text(tool: &Tool) -> String {
    let mut text = match (tool.status, tool.interrupted) {
        (_, true) => "stopped".to_owned(),
        (None, _) => "running…".to_owned(),
        (Some(ToolStatus::Completed), _) => match tool.exit_code {
            Some(code) => format!("exit {code}"),
            None => "done".to_owned(),
        },
        (Some(ToolStatus::Failed), _) => match tool.exit_code {
            Some(code) => format!("failed · exit {code}"),
            None => "failed".to_owned(),
        },
        (Some(ToolStatus::Declined), _) => "declined".to_owned(),
    };
    if let Some(ms) = tool.duration_ms.filter(|ms| *ms > 0) {
        text.push_str(&format!(" · {}", duration(ms)));
    }
    text
}

/// The right end of a tool card's header: what the tool did, as muted words (`running…`,
/// `done · 2 m 53 s`, `exit 0 · 4 s`), or a state label in the state's color when it ended
/// badly. Never a pill, never the accent.
fn tool_status(tool: &Tool, look: &Look) -> AnyElement {
    let (theme, tokens) = (&look.theme, &look.tokens);
    match status_tone(look, tool) {
        Some(tone) => crate::workbench::ui::state_label(
            crate::workbench::ui::state_glyph(tone, theme),
            status_text(tool),
            tone,
            false,
            theme,
            tokens,
        )
        .into_any_element(),
        None => div()
            .flex_none()
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.muted)
            .child(SharedString::from(status_text(tool)))
            .into_any_element(),
    }
}

/// Space above the first card of a transcript, under the header (CL-POLISH-4).
pub(crate) const TRANSCRIPT_TOP_INSET: f32 = 24.0;

impl ChatView {
    fn blocks_of(&mut self, entry: &Entry, text: &str) -> Rc<Vec<MdBlock>> {
        if let Some((rev, parsed)) = self.parsed.get(&entry.id)
            && *rev == entry.rev
        {
            return parsed.clone();
        }
        self.markdown_parsed += 1;
        let parsed = Rc::new(markdown::parse(text));
        self.parsed.insert(entry.id, (entry.rev, parsed.clone()));
        parsed
    }

    /// One list item: the card of entry `index` in the conversation column.
    pub(super) fn render_entry(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.entries_built += 1;
        let look = Look::of(cx);
        let model = self.model.clone();
        let transcript = model.read(cx).transcript();
        let Some(entry) = transcript.entries().get(index) else {
            return div().into_any_element();
        };
        let mut ids = Ids::new(format!("e{}", entry.id)).probing(self.regions.clone(), index);
        let tokens = look.tokens;
        let card = match &entry.body {
            Body::User(message) => self.user_card(index, entry, message, &look, &mut ids, cx),
            Body::Assistant(message) => self.assistant_card(entry, message, &look, &mut ids),
            Body::Reasoning(reasoning) => {
                self.reasoning_card(index, entry, reasoning, &look, &mut ids, cx)
            }
            Body::Tool(tool) => self.tool_card(index, entry, tool, transcript, &look, &mut ids, cx),
            Body::Approval(id) => match transcript.approval(*id) {
                Some(approval) => self.approval_card(approval, true, &look, cx),
                None => div().into_any_element(),
            },
            Body::Question(id) => match transcript.question(*id) {
                Some(question) => self.question_card(question, &look, cx),
                None => div().into_any_element(),
            },
            Body::Plan(plan) => plan_card(plan, &look),
            Body::TurnDiff(turn_diff) => self.turn_diff_card(
                index,
                entry,
                &turn_diff.patch,
                transcript,
                &look,
                &mut ids,
                cx,
            ),
            Body::Footer(footer) => {
                let card = footer_card(footer, &look);
                let (recovery, failed) = match &footer.outcome {
                    TurnOutcome::Failed { message, .. } => (
                        self.provider_error_action(message, &look, cx),
                        Some((message.clone(), entry.ts_ms)),
                    ),
                    _ => (None, None),
                };
                let turn = entry.turn;
                let retry = failed
                    .and_then(|(message, at)| self.footer_retry(index, &message, at, &look, cx));
                // The files the turn changed, with Rewind… (CK-5).
                let files = self.checkpoint_footer(index, turn, &look, cx);
                match (card, files) {
                    // A turn that simply completed says so on its cards and in the status line;
                    // its footer is drawn only for what nothing else says (usage, a failure,
                    // the files it changed). Without a line the entry is its padding alone, the
                    // gap between two turns (a list item with no height would hide the top of
                    // the list from its scroll handler).
                    (None, None) => div().into_any_element(),
                    (card, files) => div()
                        .children(card)
                        .children(recovery)
                        .children(retry)
                        .children(files)
                        .into_any_element(),
                }
            }
            Body::Notice { level, message } => notice_card(*level, message, &look),
        };
        div()
            .id(("chat-entry", index))
            .relative()
            .w_full()
            .px(px(tokens.spacing * 3.0))
            .pb(px(tokens.spacing * 1.25))
            // The first card has the column's top inset, so it does not start at the header's
            // hairline.
            .pt(px(if index == 0 {
                TRANSCRIPT_TOP_INSET
            } else {
                tokens.spacing * 1.25
            }))
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(crate::theme::reading_width()))
                    .child(card),
            )
            .child(probe(&self.regions, Region::ChatEntry(index)))
            .into_any_element()
    }

    // ---- messages --------------------------------------------------------------------------

    /// The user's message. What the user typed is the message; the context the daemon appended
    /// (`docs/assistant.md`) is shown as one chip per attachment — kind, label, characters kept
    /// and whether the budget cut it — with the text itself folded away.
    fn user_card(
        &mut self,
        index: usize,
        entry: &Entry,
        message: &Message,
        look: &Look,
        ids: &mut Ids,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let (typed, context) = split_context(&message.text);
        let parsed = self.blocks_of(entry, typed);
        let meta = message
            .item
            .map(|item| self.model.read(cx).attachments_of(item).to_vec())
            .unwrap_or_default();
        let open = self.open(entry.id, PART_BODY, false);
        let id = entry.id;
        let hover = theme.surface_hover;
        let chips = div()
            .flex()
            .flex_wrap()
            .gap(px(tokens.spacing))
            .children(meta.iter().map(|meta| {
                if meta.truncated {
                    crate::workbench::ui::state_label(
                        crate::icons::Icon::StateWarning,
                        meta_text(meta),
                        theme.warning,
                        false,
                        theme,
                        tokens,
                    )
                    .font_family(look.mono.clone())
                } else {
                    pill(meta_text(meta), look).font_family(look.mono.clone())
                }
            }));
        // No bubble for the agent; the user's message sits in a neutral bubble on the right: a
        // card (`surface`, the card radius), at most 70 % of the column. The accent is not spent
        // on the user's own words.
        let bubble = div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 1.5))
            .max_w(relative(0.7))
            .px(px(tokens.spacing * 3.5))
            .py(px(tokens.spacing * 2.5))
            .card(theme, tokens)
            .text_color(theme.fg)
            .child(blocks(&parsed, &look.sans(), ids))
            .when(!meta.is_empty(), |s| s.child(chips))
            .when_some(context, |s, context| {
                s.child(
                    div()
                        .id(ids.next("context"))
                        .relative()
                        .flex()
                        .items_center()
                        .gap(px(tokens.spacing * 1.5))
                        .cursor_pointer()
                        .rounded(px(tokens.radius - 2.0))
                        .text_color(theme.muted)
                        .hover(move |s| s.bg(hover))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle(index, id, PART_BODY, cx);
                        }))
                        .child(probe(&self.regions, Region::ChatPart(index, PART_BODY)))
                        .child(crate::icons::glyph(if open {
                            crate::icons::Icon::ActionExpand
                        } else {
                            crate::icons::Icon::ActionDisclose
                        }))
                        .child(if open {
                            "Attached context"
                        } else {
                            "Show attached context"
                        }),
                )
                .when(open, |s| s.child(code_block(context, None, look, ids)))
            });
        div()
            .flex()
            .justify_end()
            .w_full()
            .child(bubble)
            .into_any_element()
    }

    fn assistant_card(
        &mut self,
        entry: &Entry,
        message: &Message,
        look: &Look,
        ids: &mut Ids,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let parsed = self.blocks_of(entry, &message.text);
        let commentary = message.phase.as_deref() == Some("commentary");
        div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .text_color(if commentary { theme.muted } else { theme.fg })
            .child(blocks(&parsed, look, ids))
            .when(!message.done, |s| {
                s.child(div().text_color(theme.accent).child("▍"))
            })
            .into_any_element()
    }

    fn reasoning_card(
        &mut self,
        index: usize,
        entry: &Entry,
        reasoning: &Reasoning,
        look: &Look,
        ids: &mut Ids,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let open = self.open(entry.id, PART_BODY, false);
        let id = entry.id;
        let label = if reasoning.done {
            "Thought"
        } else {
            "Thinking…"
        };
        let preview = reasoning
            .summary
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .trim_matches('*')
            .to_owned();
        let summary = (open && !reasoning.summary.is_empty())
            .then(|| self.blocks_of(entry, &reasoning.summary));
        let hover = theme.surface_hover;
        div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .text_color(theme.muted)
            .child(
                div()
                    .id(ids.next("reasoning"))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 1.5))
                    .cursor_pointer()
                    .rounded(px(tokens.radius - 2.0))
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle(index, id, PART_BODY, cx);
                    }))
                    .child(probe(&self.regions, Region::ChatPart(index, PART_BODY)))
                    .child(crate::icons::glyph(if open {
                        crate::icons::Icon::ActionExpand
                    } else {
                        crate::icons::Icon::ActionDisclose
                    }))
                    .child(SharedString::from(label))
                    .when(!open && !preview.is_empty(), |s| {
                        s.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(tokens.secondary_font_size))
                                .child(SharedString::from(preview.clone())),
                        )
                    }),
            )
            .children(summary.map(|parsed| {
                div()
                    .pl(px(tokens.spacing * 4.0))
                    .child(blocks(&parsed, look, ids))
            }))
            .when(open && !reasoning.content.is_empty(), |s| {
                s.child(
                    div()
                        .pl(px(tokens.spacing * 4.0))
                        .text_size(px(tokens.secondary_font_size))
                        .child(SharedString::from(reasoning.content.clone())),
                )
            })
            .into_any_element()
    }

    // ---- tool calls ------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn tool_card(
        &mut self,
        index: usize,
        entry: &Entry,
        tool: &Tool,
        transcript: &Transcript,
        look: &Look,
        ids: &mut Ids,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let pending = tool.approvals.iter().any(|id| {
            transcript
                .approval(*id)
                .is_some_and(|a| a.state == ApprovalState::Pending)
        });
        let icon = match tool.kind {
            ToolKind::Command => crate::icons::Icon::SessionTerminal,
            ToolKind::FileChange => crate::icons::Icon::ActionRename,
            ToolKind::WebSearch => crate::icons::Icon::ActionSearch,
            ToolKind::Mcp | ToolKind::Dynamic | ToolKind::Other => {
                crate::icons::Icon::ActionSettings
            }
        };
        let id = entry.id;
        let hover = theme.surface_hover;
        let header_title = match tool.kind {
            ToolKind::Command => tool.command().to_owned(),
            _ if tool.title.is_empty() => tool.name.clone(),
            _ => tool.title.clone(),
        };
        let body = match tool.kind {
            ToolKind::Command => self.command_body(index, entry, tool, look, ids, cx),
            ToolKind::FileChange => {
                self.file_change_body(index, entry, tool, pending, look, ids, cx)
            }
            _ => self.other_body(index, entry, tool, look, ids, cx),
        };
        let approvals: Vec<AnyElement> = tool
            .approvals
            .iter()
            .filter_map(|approval| transcript.approval(*approval))
            .map(|approval| self.approval_card(approval, false, look, cx))
            .collect();
        // A fill without a border; a request waiting on it says so in its own group, under the
        // card's only hairline, and in the status line, not as a colored edge.
        div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 1.5))
            .p(px(tokens.spacing * 2.5))
            .card(theme, tokens)
            .child(
                div()
                    .id(ids.next("tool-header"))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .rounded(px(tokens.radius - 2.0))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle(index, id, PART_BODY, cx);
                    }))
                    .child(
                        div()
                            .flex_none()
                            .w(px(tokens.font_size))
                            .text_color(theme.muted)
                            .font_family(look.mono.clone())
                            .child(crate::icons::glyph(icon)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(look.mono.clone())
                            .child(SharedString::from(header_title)),
                    )
                    .child(tool_status(tool, look))
                    .child(probe(&self.regions, Region::ChatPart(index, PART_BODY))),
            )
            .child(body)
            .children(approvals)
            .into_any_element()
    }

    fn command_body(
        &mut self,
        index: usize,
        entry: &Entry,
        tool: &Tool,
        look: &Look,
        ids: &mut Ids,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let open = self.open(entry.id, PART_BODY, false);
        let id = entry.id;
        let output = tool.output.trim_end_matches('\n');
        let total = output.lines().count();
        let (shown, hidden) = if open {
            tail_lines(output, MAX_OUTPUT_LINES)
        } else {
            tail_lines(output, TAIL_LINES)
        };
        div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .children(tool.cwd().map(|cwd| {
                div()
                    .font_family(look.mono.clone())
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .truncate()
                    .child(SharedString::from(format!("in {cwd}")))
            }))
            .when(!output.is_empty(), |s| {
                s.child(
                    div()
                        .flex()
                        .flex_col()
                        .rounded(px(tokens.radius))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.bg)
                        .px(px(tokens.spacing * 2.0))
                        .py(px(tokens.spacing * 1.5))
                        .font_family(look.mono.clone())
                        .text_size(px(tokens.font_size - 0.5))
                        .when(hidden > 0, |s| {
                            s.child(
                                Button::new(
                                    ids.next("output-more"),
                                    SharedString::from(format!(
                                        "… {hidden} earlier line{} (click to {})",
                                        if hidden == 1 { "" } else { "s" },
                                        if open { "collapse" } else { "show" }
                                    )),
                                    theme,
                                    tokens,
                                )
                                .link()
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.toggle(index, id, PART_BODY, cx);
                                    },
                                )),
                            )
                        })
                        .child(div().child(SharedString::from(shown.to_owned())))
                        .when(tool.truncated, |s| {
                            s.child(
                                div()
                                    .text_color(theme.muted)
                                    .child("… the output was cut by the daemon"),
                            )
                        }),
                )
            })
            .when(open && hidden == 0 && total > TAIL_LINES, |s| {
                s.child(
                    Button::new(ids.next("output-less"), "collapse", theme, tokens)
                        .link()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle(index, id, PART_BODY, cx);
                        })),
                )
            })
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn file_change_body(
        &mut self,
        index: usize,
        entry: &Entry,
        tool: &Tool,
        pending: bool,
        look: &Look,
        ids: &mut Ids,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let id = entry.id;
        let hover = theme.surface_hover;
        let cwd = self.session_cwd(cx);
        let mut rows: Vec<AnyElement> = Vec::new();
        for (file, change) in tool.changes.iter().enumerate() {
            let diff = diff::from_change(change);
            // A request that waits for an answer shows what it would change.
            let part = PART_DIFF + u32::try_from(file).unwrap_or(0);
            let open = self.open(id, part, pending);
            let label = match change.kind {
                FileChangeKind::Add => "added",
                FileChangeKind::Delete => "deleted",
                FileChangeKind::Update => "edited",
            };
            let name = match &diff.moved_from {
                Some(from) => format!("{from} → {}", diff.path),
                None => diff.path.clone(),
            };
            // "Open": the file in a tab at its first change (E-10); a deleted file has none.
            let target = (change.kind != FileChangeKind::Delete)
                .then(|| open_target(change, cwd.as_deref(), diff.first_changed_line()))
                .flatten();
            rows.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(tokens.spacing))
                            .child(
                                div()
                                    .id(ids.next("file"))
                                    .relative()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap(px(tokens.spacing * 2.0))
                                    .rounded(px(tokens.radius - 2.0))
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(hover))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.toggle(index, id, part, cx);
                                    }))
                                    .child(div().flex_none().text_color(theme.muted).child(
                                        crate::icons::glyph(if open {
                                            crate::icons::Icon::ActionExpand
                                        } else {
                                            crate::icons::Icon::ActionDisclose
                                        }),
                                    ))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .font_family(look.mono.clone())
                                            .child(SharedString::from(name)),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_color(theme.muted)
                                            .text_size(px(tokens.secondary_font_size))
                                            .child(label),
                                    )
                                    .child(counts(&diff, look))
                                    .child(probe(&self.regions, Region::ChatPart(index, part))),
                            )
                            .children(target.map(|target| {
                                let tip = SharedString::from(match target.line {
                                    Some(line) => format!(
                                        "Open {} at line {line} (its first change)",
                                        target.path
                                    ),
                                    None => format!("Open {}", target.path),
                                });
                                Button::new(ids.next("open"), "Open", theme, tokens)
                                    .ghost()
                                    .compact()
                                    .tooltip(tip)
                                    .on_click(move |_, window, cx| {
                                        cx.stop_propagation();
                                        window.dispatch_action(
                                            Box::new(crate::workbench::actions::OpenFile(
                                                target.clone(),
                                            )),
                                            cx,
                                        );
                                    })
                                    .child(probe(&self.regions, Region::ChatOpen(index, part)))
                            })),
                    )
                    .when(open, |s| s.child(diff_view(&diff, look, ids)))
                    .into_any_element(),
            );
        }
        if rows.is_empty() {
            rows.push(
                div()
                    .text_color(theme.muted)
                    .child("No file contents were reported for this change.")
                    .into_any_element(),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 1.5))
            .children(rows)
            .into_any_element()
    }

    fn other_body(
        &mut self,
        index: usize,
        entry: &Entry,
        tool: &Tool,
        look: &Look,
        ids: &mut Ids,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let open = self.open(entry.id, PART_BODY, false);
        let id = entry.id;
        let hover = theme.surface_hover;
        let args: Option<String> = tool
            .input
            .as_ref()
            .and_then(|input| serde_json::to_string_pretty(input).ok());
        let output = tool.output.trim_end_matches('\n');
        let (shown, hidden) = if open {
            tail_lines(output, MAX_OUTPUT_LINES)
        } else {
            tail_lines(output, TAIL_LINES / 2)
        };
        div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .child(
                div()
                    .id(ids.next("args"))
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 1.5))
                    .text_color(theme.muted)
                    .cursor_pointer()
                    .rounded(px(tokens.radius - 2.0))
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle(index, id, PART_BODY, cx);
                    }))
                    .child(crate::icons::glyph(if open {
                        crate::icons::Icon::ActionExpand
                    } else {
                        crate::icons::Icon::ActionDisclose
                    }))
                    .child(SharedString::from(format!("{} · arguments", tool.name))),
            )
            .when_some(args.filter(|_| open), |s, args| {
                s.child(code_block(&args, Some("json"), look, ids))
            })
            .children(tool.updates.last().map(|update| {
                div()
                    .text_color(theme.muted)
                    .child(SharedString::from(update.clone()))
            }))
            .when(!output.is_empty(), |s| {
                s.child(
                    div()
                        .font_family(look.mono.clone())
                        .text_size(px(tokens.font_size - 0.5))
                        .when(hidden > 0, |s| {
                            s.child(
                                div()
                                    .text_color(theme.muted)
                                    .child(SharedString::from(format!("… {hidden} earlier lines"))),
                            )
                        })
                        .child(SharedString::from(shown.to_owned())),
                )
            })
            .into_any_element()
    }

    // ---- the turn's changes ----------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn turn_diff_card(
        &mut self,
        index: usize,
        entry: &Entry,
        patch: &str,
        transcript: &Transcript,
        look: &Look,
        ids: &mut Ids,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        // File change cards of the same turn already show these files.
        let covered = transcript.entries().iter().any(|other| {
            other.turn == entry.turn
                && matches!(&other.body, Body::Tool(t) if t.kind == ToolKind::FileChange && !t.changes.is_empty())
        });
        if covered {
            return div().into_any_element();
        }
        let files = diff::split_patch(patch);
        let (added, removed) = files
            .iter()
            .fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
        let open = self.open(entry.id, PART_BODY, false);
        let id = entry.id;
        let hover = theme.surface_hover;
        div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .p(px(tokens.spacing * 2.5))
            .card(theme, tokens)
            .child(
                div()
                    .id(ids.next("turn-diff"))
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .cursor_pointer()
                    .rounded(px(tokens.radius - 2.0))
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle(index, id, PART_BODY, cx);
                    }))
                    .child(crate::icons::glyph(if open {
                        crate::icons::Icon::ActionExpand
                    } else {
                        crate::icons::Icon::ActionDisclose
                    }))
                    .child(SharedString::from(format!(
                        "Changes this turn · {} file{}",
                        files.len(),
                        if files.len() == 1 { "" } else { "s" }
                    )))
                    .child(
                        div()
                            .text_color(theme.success)
                            .child(SharedString::from(format!("+{added}"))),
                    )
                    .child(
                        div()
                            .text_color(theme.danger)
                            .child(SharedString::from(format!("−{removed}"))),
                    ),
            )
            .when(open, |s| {
                s.children(files.iter().map(|file| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing))
                        .child(
                            div()
                                .font_family(look.mono.clone())
                                .child(SharedString::from(file.path.clone())),
                        )
                        .child(diff_view(
                            file,
                            look,
                            &mut Ids::new(format!("td{}", entry.id)),
                        ))
                }))
            })
            .into_any_element()
    }

    // ---- approvals -------------------------------------------------------------------------

    /// What a resolved card says: what was decided and where the answer came from ("Allowed
    /// once · from CLI"); nothing while the request is open or when it expired.
    fn resolution_line(&self, approval: &Approval, cx: &App) -> Option<String> {
        if !matches!(approval.state, ApprovalState::Selected { .. }) {
            return None;
        }
        let here = self.sessions.read(cx).answered_here(approval.id).is_some();
        Some(format!(
            "{} · {}",
            approval.decision_text(),
            approval.answered_from(here)
        ))
    }

    /// The line a resolved approval's card shows (`None` while it is open or when it expired).
    pub fn resolution_text(&self, id: ApprovalId, cx: &App) -> Option<String> {
        let model = self.model.read(cx);
        self.resolution_line(model.transcript().approval(id)?, cx)
    }

    /// The harness of this chat by name (`OMP`), for the sentences about standing rules.
    fn harness(&self, cx: &App) -> Option<String> {
        self.sessions
            .read(cx)
            .session(self.session)
            .and_then(|info| info.chat.as_ref())
            .map(|chat| harness_name(&chat.agent))
    }

    /// The tooltip of each standing ("Always") option of `approval`, in order: its key, its
    /// label and what it does beyond this request (what the card printed under its buttons
    /// before CL-CHAT).
    fn scope_lines(&self, approval: &Approval, cx: &App) -> Vec<String> {
        let agent = self.harness(cx);
        approval
            .options
            .iter()
            .enumerate()
            .filter_map(|(number, option)| {
                scope_tooltip(number, option, &approval.title, agent.as_deref())
            })
            .collect()
    }

    /// The tooltips of the standing options of request `id` (`None` when it is not in this
    /// transcript).
    pub fn scope_text(&self, id: ApprovalId, cx: &App) -> Option<Vec<String>> {
        let model = self.model.read(cx);
        Some(self.scope_lines(model.transcript().approval(id)?, cx))
    }

    /// The one sentence under the buttons of request `id` while it waits (`None` when it offers
    /// no standing option or is not in this transcript).
    pub fn always_note_text(&self, id: ApprovalId, cx: &App) -> Option<String> {
        let model = self.model.read(cx);
        let approval = model.transcript().approval(id)?;
        always_note(&approval.options, &approval.title)
    }

    /// The ids of the options of request `id` that its card draws as primary buttons.
    pub fn primary_options(&self, id: ApprovalId, cx: &App) -> Option<Vec<String>> {
        let model = self.model.read(cx);
        let approval = model.transcript().approval(id)?;
        Some(
            approval
                .options
                .iter()
                .filter(|o| approval_button_kind(o.kind) == ButtonKind::Primary)
                .map(|o| o.id.clone())
                .collect(),
        )
    }

    /// A permission request. Waiting, it is the approval group: inside its tool's card
    /// (`standalone` false) under the card's one hairline, indented 16 px; on its own as a card
    /// that also names the tool. Answered or expired, it collapses to one line
    /// (`✓ Allowed once · from this window`), with the tool named when there is no card around.
    pub(super) fn approval_card(
        &self,
        approval: &Approval,
        standalone: bool,
        look: &Look,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let outcome = match &approval.state {
            ApprovalState::Pending => None,
            ApprovalState::Selected { .. } => Some((
                decision_mark(approval.chosen().map(|o| o.kind), look),
                self.resolution_line(approval, cx).unwrap_or_default(),
            )),
            ApprovalState::Expired => Some((
                decision_mark(None, look),
                "No answer: the turn ended or the agent withdrew the request".to_owned(),
            )),
        };
        let now = cx.background_executor().now();
        let key = SharedString::from(format!("approval-group-{}", approval.id));
        let leaving = {
            let mut seen = self.approvals_seen.borrow_mut();
            if outcome.is_none() {
                seen.waiting.insert(approval.id);
                false
            } else {
                if seen.waiting.remove(&approval.id) {
                    seen.answered.insert(approval.id, now);
                }
                let exit = motion::Kind::Collapse.exit_duration(cx.reduce_motion());
                seen.answered
                    .retain(|_, at| now.saturating_duration_since(*at) < exit);
                seen.answered.contains_key(&approval.id)
            }
        };
        let ordinal = self.pending.iter().position(|id| *id == approval.id);
        let approval_id: ApprovalId = approval.id;
        let agent = self.harness(cx);
        // The group of buttons; the one that is still on screen while the answered card folds
        // into its line has no handlers and no probes.
        let build_group = |live: bool| -> AnyElement {
            let buttons: Vec<AnyElement> = approval
                .options
                .iter()
                .enumerate()
                .map(|(number, option)| {
                    let option_id = option.id.clone();
                    let region = ordinal.map_or(usize::MAX, |ordinal| ordinal * 8 + number);
                    let button = approval_option(
                        SharedString::from(format!("approve-{}-{number}", approval.id)),
                        number,
                        option,
                        scope_tooltip(number, option, &approval.title, agent.as_deref()),
                        look,
                    );
                    if live {
                        button
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.answer(approval_id, option_id.clone(), cx);
                            }))
                            .child(probe(&self.regions, Region::ChatOption(region)))
                            .into_any_element()
                    } else {
                        button.into_any_element()
                    }
                })
                .collect();
            let group = approval_group(
                approval.kind,
                standalone.then_some(approval.title.as_str()),
                approval.detail.as_deref(),
                buttons,
                always_note(&approval.options, &approval.title),
                look,
            )
            .relative();
            if standalone {
                group
                    .p(px(tokens.spacing * 3.0))
                    .card(theme, tokens)
                    .into_any_element()
            } else {
                group
                    .pt(px(tokens.spacing * 3.0))
                    .pl(px(16.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .into_any_element()
            }
        };
        if let Some((mark, line)) = outcome {
            let line = if standalone {
                format!(
                    "{line} · {}",
                    approval.title.lines().next().unwrap_or_default()
                )
            } else {
                line
            };
            let line = approval_outcome(mark, line, look).into_any_element();
            // The buttons fold away (a clipped height, fading) above the line they became.
            return if leaving {
                div()
                    .child(motion::expand(key, false, build_group(false)))
                    .child(line)
                    .into_any_element()
            } else {
                line
            };
        }
        // Waiting: drawn through the same fold, open, so that answering it can fold it.
        motion::expand(key, true, build_group(true)).into_any_element()
    }
}

// ---- pieces without state -------------------------------------------------------------------

/// Where a file row's *Open* goes: the changed file (its new name after a move), relative
/// paths taken from the chat's folder, at `line`.
pub(crate) fn open_target(
    change: &hutan_core::agent::FileChange,
    cwd: Option<&std::path::Path>,
    line: Option<u32>,
) -> Option<crate::workbench::actions::FileTarget> {
    let named = std::path::Path::new(change.move_path.as_deref().unwrap_or(&change.path));
    let path = if named.is_absolute() {
        named.to_owned()
    } else {
        cwd?.join(named)
    };
    Some(crate::workbench::actions::FileTarget {
        path: path.display().to_string(),
        line,
        column: None,
    })
}

impl ChatView {
    /// The folder the chat works in.
    fn session_cwd(&self, cx: &App) -> Option<std::path::PathBuf> {
        self.sessions
            .read(cx)
            .session(self.session)
            .and_then(|s| s.cwd.clone())
    }
}

fn counts(diff: &FileDiff, look: &Look) -> AnyElement {
    counts_of(diff.added, diff.removed, look)
}

/// `+3 −1` in the diff colors.
pub(crate) fn counts_of(added: usize, removed: usize, look: &Look) -> AnyElement {
    let (theme, tokens) = (&look.theme, &look.tokens);
    div()
        .flex()
        .flex_none()
        .gap(px(tokens.spacing))
        .text_size(px(tokens.secondary_font_size))
        .child(
            div()
                .text_color(theme.diff_added)
                .child(SharedString::from(format!("+{added}"))),
        )
        .child(
            div()
                .text_color(theme.diff_removed)
                .child(SharedString::from(format!("−{removed}"))),
        )
        .into_any_element()
}

/// One line of a unified diff: the old and the new line number, a sign and the text; additions
/// and removals tinted green and red, hunk headers in the accent.
pub(crate) fn diff_row(line: &DiffLine, look: &Look) -> gpui::Div {
    let (theme, tokens) = (&look.theme, &look.tokens);
    let number = |n: Option<u32>| {
        div()
            .flex_none()
            .w(px(tokens.font_size * 3.2))
            .text_right()
            .pr(px(tokens.spacing))
            .text_color(theme.muted)
            .child(SharedString::from(
                n.map_or(String::new(), |n| n.to_string()),
            ))
    };
    let (sign, tint, text_color) = match line.kind {
        LineKind::Add => ("+", Some(theme.diff_added_bg), theme.fg),
        LineKind::Remove => ("−", Some(theme.diff_removed_bg), theme.fg),
        LineKind::Context => (" ", None, theme.fg),
        LineKind::Hunk => ("", Some(theme.diff_hunk_bg), theme.diff_hunk_fg),
        LineKind::Note => ("", None, theme.muted),
    };
    let shown = if matches!(line.kind, LineKind::Hunk) {
        line.text.clone()
    } else {
        line.text.replace('\t', "    ")
    };
    div()
        .flex()
        .min_w_full()
        .when_some(tint, |s, tint| s.bg(tint))
        .text_color(text_color)
        .child(number(line.old))
        .child(number(line.new))
        .child(div().flex_none().w(px(tokens.font_size)).child(sign))
        .child(div().flex_none().child(SharedString::from(shown)))
}

/// How the Changes view lays a diff row out: lines wrap, or run on and scroll sideways with the
/// number gutter held in place.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RowFlow {
    /// Long lines wrap inside the pane; the row is as wide as the pane.
    pub wrap: bool,
    /// How far the code is scrolled sideways (positive), so the gutter can stay where it is.
    pub scrolled: gpui::Pixels,
}

/// The width of the number gutter and sign column of a flowing diff row.
pub(crate) fn gutter_width(tokens: &crate::theme::Tokens) -> gpui::Pixels {
    px(tokens.font_size * 7.4)
}

/// A diff row of the Changes view: like [`diff_row`], but the old and new line numbers and the
/// sign are a gutter that stays at the left edge of the pane while the code scrolls sideways
/// under it, and the tint covers the whole row however wide the code is (the rows of a block are
/// stretched to its widest). `lead` replaces the numbers (a hunk header's fold chevron); `trail`
/// follows the text (a hunk header's *Open*).
pub(crate) fn diff_row_flow(
    line: &DiffLine,
    look: &Look,
    flow: RowFlow,
    lead: Option<AnyElement>,
    trail: Option<AnyElement>,
) -> gpui::Div {
    let (theme, tokens) = (&look.theme, &look.tokens);
    let number = |n: Option<u32>| {
        div()
            .flex_none()
            .w(px(tokens.font_size * 3.2))
            .text_right()
            .pr(px(tokens.spacing))
            .text_color(theme.muted)
            .child(SharedString::from(
                n.map_or(String::new(), |n| n.to_string()),
            ))
    };
    let (sign, tint, text_color) = match line.kind {
        LineKind::Add => ("+", Some(theme.diff_added_bg), theme.fg),
        LineKind::Remove => ("−", Some(theme.diff_removed_bg), theme.fg),
        LineKind::Context => (" ", None, theme.fg),
        LineKind::Hunk => ("", Some(theme.diff_hunk_bg), theme.diff_hunk_fg),
        LineKind::Note => ("", None, theme.muted),
    };
    let mut shown = if matches!(line.kind, LineKind::Hunk) {
        line.text.clone()
    } else {
        line.text.replace('\t', "    ")
    };
    if shown.is_empty() {
        // An empty line keeps its height.
        shown.push(' ');
    }
    let gutter = gutter_width(tokens);
    let numbers = match lead {
        Some(lead) => div().flex().items_center().size_full().child(lead),
        None => div()
            .flex()
            .size_full()
            .child(number(line.old))
            .child(number(line.new))
            .child(div().flex_none().w(px(tokens.font_size)).child(sign)),
    };
    div()
        .relative()
        .flex()
        .when(flow.wrap, |s| s.w_full())
        .when_some(tint, |s, tint| s.bg(tint))
        .text_color(text_color)
        .pl(gutter)
        .child(if flow.wrap {
            div()
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .child(SharedString::from(shown))
        } else {
            div().flex_none().child(SharedString::from(shown))
        })
        .children(trail)
        .child(
            // Opaque under the tint, so code scrolled beneath the gutter is hidden by it.
            div()
                .absolute()
                .top_0()
                .left(flow.scrolled)
                .h_full()
                .w(gutter)
                .bg(theme.bg)
                .child(
                    numbers
                        .when_some(tint, |s, tint| s.bg(tint))
                        .text_color(theme.muted),
                ),
        )
}

/// A unified diff: line numbers, a sign and the text; additions and removals tinted
/// green and red.
fn diff_view(diff: &FileDiff, look: &Look, ids: &mut Ids) -> AnyElement {
    let (theme, tokens) = (&look.theme, &look.tokens);
    let shown = diff.lines.len().min(MAX_LINES);
    let hidden = diff.lines.len() - shown;
    div()
        .id(ids.next("diff"))
        .overflow_x_scroll()
        .restrict_scroll_to_axis()
        .rounded(px(tokens.radius))
        .border_1()
        .border_color(theme.border)
        .bg(theme.bg)
        .font_family(look.mono.clone())
        .text_size(px(tokens.font_size - 0.5))
        .whitespace_nowrap()
        .py(px(tokens.spacing))
        .children(
            diff.lines
                .iter()
                .take(shown)
                .map(|line| diff_row(line, look)),
        )
        .when(hidden > 0, |s| {
            s.child(
                div()
                    .px(px(tokens.spacing * 2.0))
                    .text_color(theme.muted)
                    .child(SharedString::from(format!(
                        "… {hidden} more lines (the whole diff is in `hutan chat log`)"
                    ))),
            )
        })
        .when(diff.lines.is_empty(), |s| {
            s.child(
                div()
                    .px(px(tokens.spacing * 2.0))
                    .text_color(theme.muted)
                    .child("No text to show (a binary file or a mode change)."),
            )
        })
        .into_any_element()
}

fn plan_card(plan: &Plan, look: &Look) -> AnyElement {
    let (theme, tokens) = (&look.theme, &look.tokens);
    div()
        .flex()
        .flex_col()
        .gap(px(tokens.spacing))
        .p(px(tokens.spacing * 2.5))
        .card(theme, tokens)
        .child(
            div()
                .text_size(px(tokens.section_font_size))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted)
                .child("Plan"),
        )
        .children(plan.explanation.as_ref().map(|text| {
            div()
                .text_color(theme.muted)
                .child(SharedString::from(text.clone()))
        }))
        .children(plan.steps.iter().map(|step| {
            let (mark, color) = match step.status {
                PlanStepStatus::Pending => (crate::icons::Icon::StateTodo, theme.muted),
                PlanStepStatus::InProgress => (crate::icons::Icon::StateWorking, theme.accent),
                PlanStepStatus::Completed => (crate::icons::Icon::StateCompleted, theme.success),
            };
            div()
                .flex()
                .gap(px(tokens.spacing * 2.0))
                .child(
                    div()
                        .flex_none()
                        .text_color(color)
                        .child(crate::icons::glyph(mark)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .when(step.status == PlanStepStatus::Completed, |s| {
                            s.text_color(theme.muted).line_through()
                        })
                        .child(SharedString::from(step.text.clone())),
                )
        }))
        .children(
            plan.text
                .as_ref()
                .map(|text| div().child(SharedString::from(text.clone()))),
        )
        .into_any_element()
}

fn usage_text(usage: &TurnUsage) -> String {
    let mut parts = Vec::new();
    if let Some(total) = usage.total_tokens {
        parts.push(format!("{} tokens", tokens(total)));
    }
    let mut detail = Vec::new();
    if let Some(input) = usage.input_tokens {
        let cached = usage
            .cached_input_tokens
            .filter(|c| *c > 0)
            .map(|c| format!(" ({} cached)", tokens(c)))
            .unwrap_or_default();
        detail.push(format!("in {}{cached}", tokens(input)));
    }
    if let Some(output) = usage.output_tokens {
        detail.push(format!("out {}", tokens(output)));
    }
    if !detail.is_empty() {
        parts.push(detail.join(" · "));
    }
    // The context fill is the header's to say, once; it is not repeated under every answer.
    parts.join(" · ")
}

/// What a turn's footer says, or `None` when it would only repeat what is already on screen:
/// a turn that simply completed (no usage reported) ends as its cards say (`done · 2 m 53 s` on
/// the tool's header) and as the status line says (`idle`). The turn's result is drawn once.
/// A completed turn with usage figures, a failed and an interrupted one keep their line.
fn footer_line(footer: &Footer) -> Option<String> {
    let usage = footer
        .usage
        .as_ref()
        .map(usage_text)
        .filter(|u| !u.is_empty());
    let text = match &footer.outcome {
        TurnOutcome::Completed if usage.is_none() => return None,
        TurnOutcome::Completed => "done".to_owned(),
        TurnOutcome::Failed { message, .. } => format!("failed: {message}"),
        TurnOutcome::Cancelled => "interrupted".to_owned(),
    };
    let mut line = vec![text];
    if let Some(ms) = footer.duration_ms {
        line.push(duration(ms));
    }
    line.extend(usage);
    Some(line.join(" · "))
}

fn footer_card(footer: &Footer, look: &Look) -> Option<AnyElement> {
    let (theme, tokens) = (&look.theme, &look.tokens);
    let line = footer_line(footer)?;
    let (mark, color) = match &footer.outcome {
        TurnOutcome::Completed => (crate::icons::Icon::StateCompleted, theme.success),
        TurnOutcome::Failed { .. } => (crate::icons::Icon::StateFailed, theme.danger),
        TurnOutcome::Cancelled => (crate::icons::Icon::StateInterrupted, theme.muted),
    };
    Some(
        div()
            .flex()
            .gap(px(tokens.spacing * 1.5))
            .text_color(theme.muted)
            .text_size(px(tokens.secondary_font_size))
            .child(
                div()
                    .flex_none()
                    .text_color(color)
                    .child(crate::icons::glyph(mark)),
            )
            .child(
                div()
                    .min_w_0()
                    .when(matches!(footer.outcome, TurnOutcome::Failed { .. }), |s| {
                        s.text_color(theme.danger)
                    })
                    .child(SharedString::from(line)),
            )
            .into_any_element(),
    )
}

fn notice_card(level: NoticeLevel, message: &str, look: &Look) -> AnyElement {
    let (theme, tokens) = (&look.theme, &look.tokens);
    let (mark, color) = match level {
        NoticeLevel::Info => (crate::icons::Icon::StateInfo, theme.muted),
        NoticeLevel::Warning => (crate::icons::Icon::StateWarning, theme.warning),
        NoticeLevel::Error => (crate::icons::Icon::StateFailed, theme.danger),
    };
    div()
        .flex()
        .gap(px(tokens.spacing * 1.5))
        .text_color(color)
        .child(div().flex_none().child(crate::icons::glyph(mark)))
        .child(
            div()
                .min_w_0()
                .child(SharedString::from(message.to_owned())),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn footer(outcome: TurnOutcome, usage: Option<TurnUsage>) -> Footer {
        Footer {
            outcome,
            duration_ms: Some(173_000),
            usage,
        }
    }

    #[test]
    fn the_turn_result_is_drawn_once() {
        // A turn that simply completed: its cards and the status line already say it.
        assert_eq!(footer_line(&footer(TurnOutcome::Completed, None)), None);
        let usage = TurnUsage {
            total_tokens: Some(1_234),
            ..TurnUsage::default()
        };
        assert_eq!(
            footer_line(&footer(TurnOutcome::Completed, Some(usage))).as_deref(),
            Some("done · 2 m 53 s · 1.2k tokens")
        );
        // Usage without a figure to show is no usage.
        assert_eq!(
            footer_line(&footer(TurnOutcome::Completed, Some(TurnUsage::default()))),
            None
        );
        // The context fill is the header's, once: a turn's footer never repeats it.
        let with_context = TurnUsage {
            total_tokens: Some(1_234),
            conversation_tokens: Some(17_900),
            context_window: Some(100_000),
            ..TurnUsage::default()
        };
        let line = footer_line(&footer(TurnOutcome::Completed, Some(with_context)));
        assert_eq!(line.as_deref(), Some("done · 2 m 53 s · 1.2k tokens"));
        let only_context = TurnUsage {
            conversation_tokens: Some(17_900),
            context_window: Some(100_000),
            ..TurnUsage::default()
        };
        assert_eq!(
            footer_line(&footer(TurnOutcome::Completed, Some(only_context))),
            None
        );
        let failed = TurnOutcome::Failed {
            message: "rate limited".into(),
            details: None,
        };
        assert_eq!(
            footer_line(&footer(failed, None)).as_deref(),
            Some("failed: rate limited · 2 m 53 s")
        );
        assert_eq!(
            footer_line(&footer(TurnOutcome::Cancelled, None)).as_deref(),
            Some("interrupted · 2 m 53 s")
        );
    }
}
