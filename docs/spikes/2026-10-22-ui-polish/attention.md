# Spike: "needs your attention" states in agent tools

Date: 2026-10-22. Read-only. Counts from `gh` at fetch time.

## Method

- Read in full: Claude Code desktop and agent-view docs, Codex notifications, pets and config docs, Warp agent notifications, Superset agent status, Orca notifications, Sculptor terminal-agent spec, GitHub notifications, Linear Inbox and Triage.
- Thin: Cursor (release notes and a forum thread), Conductor (no status UI described), Slack (help pages returned 404; search snippets only, secondary).
- Issue counts: `gh issue view N -R <repo> --json reactionGroups,comments`.
- Search: `gh search issues "notification" --repo anthropics/claude-code --match title --limit 25`; same for `--repo openai/codex`; `gh search issues "attention" --repo openai/codex --match title`; `gh search issues "notification spam" --match title`.
- Prior Hutan notes read: `docs/spikes/2026-10-21-competitor-ux/orca.md`, `warp.md`, `2026-10-15-notifications.md`, `2026-10-15-agent-monitoring.md`.

## Label words by tool

| Tool | Waiting on you | Finished, to review | Failed | Source |
|---|---|---|---|---|
| Claude Code agent view | Needs input (yellow) | Completed (green) | Failed (red) | [agent-view](https://code.claude.com/docs/en/agent-view.md) |
| Claude Code desktop | red dot | none when idle | not stated | [desktop](https://code.claude.com/docs/en/desktop) |
| Codex pets | Needs input | Ready | Blocked | [pets](https://learn.chatgpt.com/docs/pets.md) |
| Codex CLI title | ✋ Action Required | not stated | not stated | [#18162](https://github.com/openai/codex/issues/18162) |
| Warp | Request | Complete | Error | [agent notifications](https://docs.warp.dev/agents/capabilities/agent-notifications/) |
| Cursor | Needs Attention group | finished unread, same group | Error group | [forum](https://forum.cursor.com/t/agents-approval-window/170388) |
| Superset | needs input | finished | not documented | [agent status](https://docs.superset.sh/agent-status) |
| Orca | none | unread mark only, no alert | no unread mark | [notifications](https://www.onorca.dev/docs/notifications) |
| Sculptor | waiting dot | neutral (terminal agents) | not stated | [spec](https://github.com/imbue-ai/sculptor/blob/main/agent_docs/terminal-agents/spec.md) |
| Hutan today | "Needs you" / "Waiting for approval" | completed-unread inside "Needs you" | unread failure inside "Needs you" | `presentation.rs`, `attention.rs` |

Most tools keep finished work in a soft tier (Codex "Ready", Claude "Ready for review", Warp "Complete" with no alert). Cursor is the exception. Its staff describe "Needs Attention" as agents "blocked on you (approvals, questions, pending plans) plus finished work you have not looked at yet". Hutan follows the Cursor pattern. That mixing is what the complaints below target.

## Sound and notification behavior

- **Claude Code**: an OS notification fires when a session finishes while the user isn't viewing it.
- **Codex**: turn-complete alerts are never, background-only, or always (`tui.notification_condition`: `unfocused` or `always`). Permission and question alerts have separate toggles.
- **Orca**: a successful chat turn sets unread only, with no notification or sound. Failed turns set no unread mark. The Dock badge clears when the workspace is visited.
- **Warp**: at most two toasts; the oldest is replaced. Toasts auto-dismiss after "a few seconds" (exact value not documented). Hover pauses. Child-agent conversations are excluded from toasts and the mailbox "so the mailbox doesn't get cluttered with per-child status churn".
- **Superset**: some agents emit no waiting signal, so they only get completion alerts.

## User complaints (counts at fetch time)

Missing blocking signal:
- openai/codex #30821, VS Code does not request attention when approval is required. **9 👍, 5 comments**, open, 2026-07-01.
- openai/codex #42433, inline questions do not trigger pet or desktop notifications. **6 👍, 3 comments**, open, 2026-09-03.
- anthropics/claude-code #101316, asks for a better "alert/waiting" notification or flashing icon. 0 comments, open, 2026-10-11.
- anthropics/claude-code #96510, users add emoji title prefixes (⏳ waiting on me, 👥 waiting on someone else, ✅ done) as a workaround. 0 comments, open, 2026-09-23.
- anthropics/claude-code #95593, desktop completion alerts silent for chat only. **6 comments**, open, 2026-09-19.

Wrong or conflated blocking signal:
- anthropics/claude-code #89531, asks for distinct state colors. **2 👍, 2 comments**, open, updated 2026-10-21. Quote: "waiting on agents must not count as waiting on me". A commenter reports yellow is set on every turn end, including turns that only wait for subagents.
- getpaseo/paseo #5546, Codex Auto-review sends "needs input" alerts while Codex continues. **0 👍, 3 comments**, open, 2026-09-28. The maintainer's trace shows the Codex `PermissionRequest` hook fires for every auto-reviewed approval, and Codex has no hook that fires only when a person must answer.
- 1devstudio/moonlet PR #185 (closed, 2026-10-15): "With several agents running, moonlet alerted for every finished turn." Fix: one "done" per session per 90 s burst. "Needs you" and "failed" always alert. The author notes it has not been checked in the running app.

Noise and repeats:
- anthropics/claude-code #101789, "spams my notifications with irrelevant information". 0 comments, open, 2026-10-13.
- openai/codex #53727, interim turns still trigger unread and taskbar attention. 0 comments, open, 2026-10-13.
- openai/codex #42705, Linux windows each send a turn-complete alert, so one response shows 2–3 copies. 0 comments, open, 2026-09-04.
- openai/codex #28013, separate sounds for attention and completion. **2 👍, 2 comments**, open, 2026-06-13.
- stablyai/orca #4375, non-turn-end notification spam. **8 comments**, open, 2026-06-01 (prior spike; comment counts are the better signal because the Discord mirror inflates reports).


Demand is real but small. The highest reaction count here is 9, and most reports have 0–2. Two patterns repeat across tools: a conflated "needs you", and finished or subagent work leaking into it. Secondary: Junctionpanel, "If a notification fires too often, people start muting it." https://junctionpanel.dev/blog/what-ai-coding-agent-notifications-should-actually-tell-you/

## Recommendations for Hutan

Proposed numbers; the basis is in parentheses.

1. **"Needs you" counts only blocking items**: approval, question, pending plan. Finished-unread moves to a separate "Ready for review" group. (Codex pets, Warp, Cursor staff note, Claude #89531.) This changes `attention.rs` (~line 75) and `presentation.rs` (`Group::NeedsYou`). Migrate all callers.
2. **Turn end alone never sets "Needs you".** Only an explicit approval or question event does. Subagent and background waits never count. (Claude #89531, moonlet #185, Warp child exclusion.)
3. **Badge**: at most 2 digits (`99+`), blocking items only. One `state.warning` orange mark per screen (spike budget ≤5). (Clean-aesthetic spike.)
4. **Sound**: none for finished-unread. Sound only for needs-you and failed. (Orca rule; Codex #28013.)
5. **OS notifications**: only when the window is unfocused or the session isn't visible (Claude; Codex `unfocused`). Needs-you and failed always alert. Finished alerts at most once per session per 90 s burst (moonlet #185; the window is the author's choice, not measured).
6. **Toasts**: at most 2 visible, oldest replaced, auto-dismiss about 5 s, hover pauses. (Warp; 5 s is [INFERENCE].)
7. **One notification per turn per session**, across windows. (Codex #42705.)
8. **Clear on view**: opening a session marks its finished and failed items read and clears its badge. (Warp, Orca.)
9. **Failure** gets its own error group and word, not a place inside "Needs you". (Cursor, Warp, Codex "Blocked".)

## Naming options for "Needs you"

Clean-aesthetic rule: state = glyph plus word. The group name appears on every surface, so pick one.

**A. Keep "Needs you"; row words say what.** Rows: "Needs approval", "Needs answer", "Failed", "Ready for review".
- Pros: no rename of an existing group. Matches Claude's and Codex's "Needs input" without the technical word. Row words stay precise. Short.
- Cons: "you" can read as "finished items need action". Mitigated by recommendation 1.

**B. "Waiting on you".**
- Pros: plain English; close to GitHub's "review requested" and Claude's "awaiting input".
- Cons: Claude #89531's complaint is that waiting on agents must not count as waiting on you. The phrase invites that confusion [INFERENCE].

**C. "Input needed", rows "Approval" and "Question".**
- Pros: matches Codex "Needs input" and Warp "Request" category. Row word names the kind.
- Cons: "Input" reads technical. Warp's "Request" also covers idle prompts, so the word is ambiguous.

Recommendation: **A**. It's the smallest change, and recommendation 1 does the real fix.

## Gaps

- **Conductor**: docs describe review, checks, merge, and archive. No status labels, dots, or notifications. Missing, not padded.
- **Slack Activity**: from snippets only. Bold names mark unread; a numbered badge appears only for mentions and replies. https://slack.com/help/articles/201355156-Configure-your-Slack-notifications (secondary).
- **Superset**: the status page names "running", "needs input", and "finished" but gives no colors.
- **Sculptor**: busy spinner and waiting dot, no label words. Terminal agents without signals show neutral status.

## Sources

- Claude Code: https://code.claude.com/docs/en/desktop · https://code.claude.com/docs/en/agent-view.md · https://code.claude.com/docs/en/changelog
- anthropics/claude-code issues #89531, #96510, #102824, #74875, #101789, #101316, #95593
- Codex: https://learn.chatgpt.com/docs/notifications · https://learn.chatgpt.com/docs/pets.md · https://learn.chatgpt.com/docs/config-file/config-reference
- openai/codex issues #30821, #42433, #28013, #49367, #53727, #42705, #18162 (fix in PR #18372)
- Warp: https://docs.warp.dev/agents/capabilities/agent-notifications/
- Superset: https://docs.superset.sh/agent-status
- Sculptor: https://github.com/imbue-ai/sculptor/blob/main/agent_docs/terminal-agents/spec.md
- Orca: https://www.onorca.dev/docs/notifications · stablyai/orca #4375
- Cursor: https://forum.cursor.com/t/agents-approval-window/170388 · https://forum.cursor.com/t/glass-ui-session-bar-shows-working-dot-matrix-instead-of-orange-awaiting-confirmation-dot-when-a-subagent-needs-an-allow-confirm/164687
- Paseo #5546: https://github.com/getpaseo/paseo/issues/5546
- moonlet PR #185: https://github.com/1devstudio/moonlet/pull/185
- Junctionpanel (secondary): https://junctionpanel.dev/blog/what-ai-coding-agent-notifications-should-actually-tell-you/
- Hutan prior: `docs/spikes/2026-10-21-competitor-ux/orca.md`, `warp.md`, `docs/spikes/2026-10-15-notifications.md`, `docs/spikes/2026-10-15-agent-monitoring.md`
