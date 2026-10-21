//! The cards of a transcript: messages, reasoning, tool calls (commands with their output, file
//! changes with diffs, other tools), approvals, plans, and the turn footer.

use std::rc::Rc;

use super::context::{meta_text, split_context};
use super::markdown_view::{Ids, Look, blocks, code_block};
use super::view::ChatView;
use super::widgets::{ButtonKind, button, duration, pill, tail_lines, tokens};
use crate::workbench::harnesses::harness_name;
use crate::workbench::render::probe;
use crate::workbench::ui::{StyledExt as _, approval_button_kind};
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
use hutan_core::agent::{
    FileChangeKind, NoticeLevel, PermissionKind, PermissionOptionKind, PlanStepStatus, ToolKind,
    ToolStatus,
};

/// The default open/closed part of a card (a command's output, a tool's arguments, reasoning).
pub(super) const PART_BODY: u32 = 0;
/// A file's diff in a file change card: `PART_DIFF + index`.
pub(super) const PART_DIFF: u32 = 100;
/// Lines of a command's output shown while it is collapsed.
const TAIL_LINES: usize = 8;
/// Most lines of a command's output drawn even when expanded.
const MAX_OUTPUT_LINES: usize = 2000;

fn status_color(look: &Look, tool: &Tool) -> gpui::Hsla {
    let theme = &look.theme;
    match (tool.status, tool.interrupted) {
        (_, true) => theme.muted,
        (None, _) => theme.accent,
        (Some(ToolStatus::Completed), _) => match tool.exit_code {
            Some(code) if code != 0 => theme.danger,
            _ => theme.success,
        },
        (Some(ToolStatus::Failed), _) => theme.danger,
        (Some(ToolStatus::Declined), _) => theme.warning,
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

fn approval_heading(kind: PermissionKind) -> &'static str {
    match kind {
        PermissionKind::Command => "Run this command?",
        PermissionKind::FileChange => "Apply these changes?",
        PermissionKind::Permissions => "Grant extra permissions?",
        PermissionKind::Control => "Let this agent control Hutan?",
        PermissionKind::Other => "Allow this?",
    }
}

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
                let retry = failed
                    .and_then(|(message, at)| self.footer_retry(index, &message, at, &look, cx));
                div()
                    .child(card)
                    .children(recovery)
                    .children(retry)
                    .into_any_element()
            }
            Body::Notice { level, message } => notice_card(*level, message, &look),
        };
        div()
            .id(("chat-entry", index))
            .relative()
            .w_full()
            .px(px(tokens.spacing * 3.0))
            .py(px(tokens.spacing * 1.25))
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
                let color = if meta.truncated {
                    theme.warning
                } else {
                    theme.accent
                };
                pill(meta_text(meta), color, look).font_family(look.mono.clone())
            }));
        // No bubble for the agent; the user's message sits in an `accent_soft` bubble on the right.
        let bubble = div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 1.5))
            .max_w(relative(0.85))
            .px(px(tokens.spacing * 3.5))
            .py(px(tokens.spacing * 2.5))
            .rounded(px(tokens.card_radius))
            .bg(theme.accent_soft)
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
        let color = status_color(look, tool);
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
        div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 1.5))
            .p(px(tokens.spacing * 2.5))
            .card(theme, tokens)
            .border_color(if pending { theme.warning } else { theme.border })
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
                    .child(div().flex_none().size(px(7.0)).rounded_full().bg(color))
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
                    .child(pill(status_text(tool), color, look))
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
        let hover = theme.surface_hover;
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
                                div()
                                    .id(ids.next("output-more"))
                                    .text_color(theme.muted)
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(hover))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.toggle(index, id, PART_BODY, cx);
                                    }))
                                    .child(SharedString::from(format!(
                                        "… {hidden} earlier line{} (click to {})",
                                        if hidden == 1 { "" } else { "s" },
                                        if open { "collapse" } else { "show" }
                                    ))),
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
                    div()
                        .id(ids.next("output-less"))
                        .text_color(theme.muted)
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle(index, id, PART_BODY, cx);
                        }))
                        .child("collapse"),
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
            rows.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing))
                    .child(
                        div()
                            .id(ids.next("file"))
                            .relative()
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

    /// One line per standing ("Always") option of `approval`, naming the option and what it
    /// does beyond this request; what the card shows under its buttons.
    fn scope_lines(&self, approval: &Approval, cx: &App) -> Vec<String> {
        let agent = self
            .sessions
            .read(cx)
            .session(self.session)
            .and_then(|info| info.chat.as_ref())
            .map(|chat| harness_name(&chat.agent));
        approval
            .options
            .iter()
            .enumerate()
            .filter_map(|(number, option)| {
                scope_sentence(option, approval, agent.as_deref())
                    .map(|sentence| format!("{} {}: {sentence}", number + 1, option.label))
            })
            .collect()
    }

    /// The scope lines of request `id` as its card draws them (`None` when it is not in this
    /// transcript).
    pub fn scope_text(&self, id: ApprovalId, cx: &App) -> Option<Vec<String>> {
        let model = self.model.read(cx);
        Some(self.scope_lines(model.transcript().approval(id)?, cx))
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

    pub(super) fn approval_card(
        &self,
        approval: &Approval,
        show_title: bool,
        look: &Look,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let pending = approval.state == ApprovalState::Pending;
        let ordinal = self.pending.iter().position(|id| *id == approval.id);
        let status: Option<(String, gpui::Hsla)> = match &approval.state {
            ApprovalState::Pending => None,
            ApprovalState::Selected { .. } => {
                let option = approval.chosen();
                let tone = match option.map(|o| o.kind) {
                    Some(PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways) => {
                        theme.success
                    }
                    Some(PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways) => {
                        theme.danger
                    }
                    _ => theme.muted,
                };
                self.resolution_line(approval, cx).map(|line| (line, tone))
            }
            ApprovalState::Expired => Some((
                "No answer: the turn ended or the agent withdrew the request".to_owned(),
                theme.muted,
            )),
        };
        let approval_id: ApprovalId = approval.id;
        // What an "Always" option reaches is said before it can be clicked.
        let scopes = if pending {
            self.scope_lines(approval, cx)
        } else {
            Vec::new()
        };
        let buttons: Vec<AnyElement> = if pending {
            approval
                .options
                .iter()
                .enumerate()
                .map(|(number, option)| {
                    let option_id = option.id.clone();
                    let region = ordinal.map_or(usize::MAX, |ordinal| ordinal * 8 + number);
                    button(
                        SharedString::from(format!("approve-{}-{number}", approval.id)),
                        format!("{} {}", number + 1, option.label),
                        approval_button_kind(option.kind),
                        true,
                        look,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.answer(approval_id, option_id.clone(), cx);
                    }))
                    .child(probe(&self.regions, Region::ChatOption(region)))
                    .into_any_element()
                })
                .collect()
        } else {
            Vec::new()
        };
        div()
            .relative()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 3.0))
            .card(theme, tokens)
            .border_color(if pending { theme.warning } else { theme.border })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 1.5))
                    .child(
                        div()
                            .text_color(if pending { theme.warning } else { theme.muted })
                            .child(crate::icons::glyph(if pending {
                                crate::icons::Icon::StateUnread
                            } else {
                                crate::icons::Icon::StateIdle
                            })),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(approval_heading(approval.kind)),
                    ),
            )
            .when(show_title, |s| {
                s.child(
                    div()
                        .font_family(look.mono.clone())
                        .child(SharedString::from(approval.title.clone())),
                )
            })
            .children(approval.detail.as_ref().map(|detail| {
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(detail.clone()))
            }))
            .when(pending, |s| {
                s.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(tokens.spacing * 2.0))
                        .children(buttons),
                )
            })
            .children(scopes.into_iter().map(|line| {
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(line))
            }))
            .children(
                status.map(|(text, color)| div().text_color(color).child(SharedString::from(text))),
            )
            .into_any_element()
    }
}

// ---- pieces without state -------------------------------------------------------------------

/// The sentence that says what a standing ("Always") option does beyond this request, naming
/// the tool the request is about (its title, first line, at most [`SCOPE_TITLE`] characters)
/// and the harness whose rule it is. `None` for options that only answer this request.
fn scope_sentence(
    option: &hutan_core::agent::PermissionOption,
    approval: &Approval,
    agent: Option<&str>,
) -> Option<String> {
    let verb = match option.kind {
        PermissionOptionKind::AllowAlways => "stops asking about",
        PermissionOptionKind::RejectAlways => "refuses, without asking,",
        _ => return None,
    };
    let agent = agent.unwrap_or("the agent");
    let first = approval.title.lines().next().unwrap_or_default();
    let mut tool: String = first.chars().take(SCOPE_TITLE).collect();
    if first.chars().count() > SCOPE_TITLE {
        tool.push('…');
    }
    Some(format!(
        "beyond this one request, {agent} {verb} what its rule for “{tool}” covers. Hutan cannot show or revoke that rule."
    ))
}

/// Longest tool name a scope sentence quotes.
const SCOPE_TITLE: usize = 80;

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

/// A unified diff: line numbers, a sign and the text; additions and removals tinted
/// green and red.
fn diff_view(diff: &FileDiff, look: &Look, ids: &mut Ids) -> AnyElement {
    let (theme, tokens) = (&look.theme, &look.tokens);
    let shown = diff.lines.len().min(MAX_LINES);
    let hidden = diff.lines.len() - shown;
    div()
        .id(ids.next("diff"))
        .overflow_x_scroll()
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
    if let (Some(used), Some(window)) = (usage.conversation_tokens, usage.context_window) {
        parts.push(format!("context {} / {}", tokens(used), tokens(window)));
    }
    parts.join(" · ")
}

fn footer_card(footer: &Footer, look: &Look) -> AnyElement {
    let (theme, tokens) = (&look.theme, &look.tokens);
    let (mark, color, text) = match &footer.outcome {
        TurnOutcome::Completed => (
            crate::icons::Icon::StateCompleted,
            theme.success,
            "done".to_owned(),
        ),
        TurnOutcome::Failed { message, .. } => (
            crate::icons::Icon::StateFailed,
            theme.danger,
            format!("failed: {message}"),
        ),
        TurnOutcome::Cancelled => (
            crate::icons::Icon::StateInterrupted,
            theme.muted,
            "interrupted".to_owned(),
        ),
    };
    let mut line = vec![text];
    if let Some(ms) = footer.duration_ms {
        line.push(duration(ms));
    }
    if let Some(usage) = footer
        .usage
        .as_ref()
        .map(usage_text)
        .filter(|u| !u.is_empty())
    {
        line.push(usage);
    }
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
                .child(SharedString::from(line.join(" · "))),
        )
        .into_any_element()
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
    use hutan_core::agent::PermissionOption;

    fn option(kind: PermissionOptionKind, label: &str) -> PermissionOption {
        PermissionOption {
            id: label.to_owned(),
            label: label.to_owned(),
            kind,
            provider: None,
        }
    }

    fn approval(title: &str) -> Approval {
        Approval {
            id: ApprovalId::new(),
            turn: None,
            kind: PermissionKind::Command,
            title: title.to_owned(),
            detail: None,
            options: Vec::new(),
            state: ApprovalState::Pending,
            decided_by: None,
        }
    }

    #[test]
    fn only_the_choices_about_this_request_are_primary() {
        use PermissionOptionKind::*;
        let primary: Vec<bool> = [
            AllowOnce,
            AllowAlways,
            RejectOnce,
            RejectAlways,
            Cancel,
            Other,
        ]
        .map(|kind| approval_button_kind(kind) == ButtonKind::Primary)
        .to_vec();
        assert_eq!(primary, [true, false, true, false, false, false]);
    }

    #[test]
    fn standing_options_say_whose_rule_they_set_and_for_what() {
        let request = approval("cargo test --workspace\nsecond line");
        let always = option(PermissionOptionKind::AllowAlways, "Always allow `cargo`");
        let sentence = scope_sentence(&always, &request, Some("Claude Code")).unwrap();
        assert!(sentence.starts_with(
            "beyond this one request, Claude Code stops asking about what its rule for “cargo test --workspace” covers."
        ));
        let never = option(PermissionOptionKind::RejectAlways, "Never");
        assert!(
            scope_sentence(&never, &request, None)
                .unwrap()
                .starts_with("beyond this one request, the agent refuses, without asking, what")
        );
        for kind in [
            PermissionOptionKind::AllowOnce,
            PermissionOptionKind::RejectOnce,
            PermissionOptionKind::Cancel,
            PermissionOptionKind::Other,
        ] {
            assert_eq!(scope_sentence(&option(kind, "x"), &request, None), None);
        }
        // A long tool name is cut, not left to flood the card.
        let long = approval(&"x".repeat(200));
        let sentence = scope_sentence(&always, &long, None).unwrap();
        assert!(sentence.contains(&format!("“{}…”", "x".repeat(SCOPE_TITLE))));
    }
}
