# OpenAI Codex (openai/codex): UX/UI pain points for Hutan

Scope: CLI/TUI, Codex desktop app (macOS/Windows/Linux), VS Code extension. Backend, rate-limit and crash issues are included only where users describe them as UX pain.

Snapshot: 2026-10-21. Counts are `reactions.total_count` / `comments` on each issue. Where a `+1` count is needed it is noted.

## Method (reproducible)

- Labels: `gh label list -R openai/codex`. UX-related labels used: `TUI`, `app`, `keyboard`, `diffs`, `code-review`, `worktrees`, `session`, `composer`, `terminal`, `accessibility`, `UI Polish`, `Sidebar`, `Confusing interaction`, `Broken flow`, `Slow interaction`, `Confusing copy`, `Papercuts 2026`, `Papercuts: Type - Confusing`, `Papercuts: Type - Broken flow`.
- Sorting: `gh search issues` does not return reaction counts, and REST search hit its 30/min quota. The reaction-sorted search used GraphQL `search(type: ISSUE)` with `sort:reactions-+1-desc`, and `reactions { totalCount }` and `comments { totalCount }` for each result. Example query text: `repo:openai/codex is:issue label:TUI ...`. Keyword queries included: `ux`, `confusing`, `keyboard`, `focus`, `layout`, `panel`, `vim`, `notification`, `diff`, `review`, `undo`, `rewind`, `approval prompt`, `Awaiting approval`, `copy text select mouse`, `queued follow-up`, `Starting your task`, `status line`, `theme colors`, `screen reader`, `worktree`, `branch selection`.
- Bodies and maintainer replies were read with `gh api repos/openai/codex/issues/N` and `.../comments`. Staff replies came from `etraut-openai`, `alexi-openai`, `ambrosino-oai`, `shijie-oai`, `nicky-oai`, `alexr-oai`, `fcoury-oai`, and `montella1507`.
- Limits: the reaction-sorted search covers highly reacted issues. The sample under-represents accessibility (very little reported) and keyboard/vim discussion that is inside long threads.

---

## Pain points

### 1. No reversible undo for agent edits (`/undo`, `/rewind`, Esc-Esc)
- **Problem:** Users cannot take back what the agent changed in files. Rewind covers the conversation, not the code.
- **Links:**
  - #9203 "Please make '/undo' back": +538 / 96 comments, open, 2026-01-14. Maintainer (etraut-openai, 2026-01-16): the earlier experimental version "was causing many issues even for users who did not use the feature. If we bring it back, it will need to be redesigned."
  - #11626 "/rewind checkpoint restore that reverts both chat context and Codex-applied code edits": +229 / 49, open, 2026-02-12.
  - #37421 Esc-Esc backtrack cannot find selected prompt: +44 / 10, closed, 2026-08-07.
- **Quote:** "I miss the /undo feature each time." (#9203). "Esc rewind only rewinds conversation/history" (#11626 title and body).
- **Principle violated:** User control and freedom (undo, reversibility). Recovery from errors.
- **Hutan should…** checkpoint every agent turn, so the file state and the conversation can be rewound together from one action. Show a preview of the files it will revert, and make the entry point discoverable (one keybinding plus a visible button), not a hidden Esc sequence.

### 2. Questions and approvals resolve themselves on a timer
- **Problem:** Plan-mode questions auto-pick the "recommended" answer after 60 seconds. Approvals time out even after the user approves late.
- **Links:**
  - #28969 "Add setting to disable the auto-resolve in 60 seconds for questions": +220 / 97, open, 2026-06-18. Staff (shijie-oai, 2026-08-13) said 147.0 "should no longer auto resolve in plan mode", but the issue is still open.
  - #31651 auto-dismiss countdown on the ask-question UI: +34 / 8, open, 2026-07-08.
  - #27458 "Codex appears to timeout while waiting for user input": +57 / 13, open, 2026-06-10.
  - #48794 cannot scroll while the "Implement this plan" selector is open: closed 2026-10-02.
- **Quote:** "Codex will automatically accept the recommended answer to a question within 60 seconds." (#28969). "Codex later responds that the operation timed out, even though I eventually approved it." (#27458)
- **Principle violated:** User control (the system must not decide for the user). Error prevention. Visibility of pending state.
- **Hutan should…** never auto-answer or auto-dismiss a question or approval. Keep waiting requests pending until answered. Any timer must be opt-in. Show a persistent "waiting for you" state with a count, so a request doesn't disappear when focus moves.

### 3. Approval UI that looks answered but does nothing
- **Problem:** The approval prompt highlights an option, but pressing it has no effect. The user has to restart and loses reasoning context.
- **Links:**
  - #10760 "macOS app: Stuck 'Awaiting approval' prompt - Cannot be approved": +76 / 59, closed 2026-02-11. Staff (etraut-openai, 2026-02-05) "It's not clear to me how to repro this." Fix shipped 2026-02-06 to 02-07.
- **Quote:** "Pressing 'Yes' or any of the other options did nothing, although the interface highlighted the chosen option." Restarting "lead to the reasoning process thus far being deleted".
- **Principle violated:** Visibility of system status. Consistency between control and effect. Error recovery.
- **Hutan should…** make approval actions idempotent and show their result. Keep the approval state after a tab switch or restart. Never discard the reasoning state on restart.

### 4. Approval fatigue: asked for every command, no progressive trust
- **Problem:** Users are asked for permission on every shell or tool call, even under "never" or "Full Access" settings. Scoped, durable rules are missing.
- **Links:**
  - #2860 Windows asks for permission on every shell command: +110 / 77, closed 2025-11-20.
  - #3085 allow-list for commands: +125 / 24, closed 2026-01-14.
  - #13476 Playwright MCP approval spam despite `approval_policy="never"`: +39 / 30, open.
  - #28988 "Full Access mode keep asking for Permission" after 26.614.11602: +33 / 27, open, 2026-06-18.
  - #11915 read-only mode approval semantics: +43 / 20, open.
  - #21975 "Auto-review approvals should support optional human fallback on deny/abort": +14 / 7, open, 2026-05-09. Quote: when auto-review denies or aborts, "the user has no first-class" way to override.
- **Quote:** "Full Access mode keep asking for file edit, mcp run, etc. permissions" (#28988).
- **Principle violated:** Error prevention and recognition over recall. Consistency of permission modes. User control.
- **Hutan should…** give approvals clear scope (once, this command, this tool, this path, this session). Batch similar requests. Show which rule matched and why the user is being asked. Offer a human fallback whenever an automated reviewer denies.

### 5. Sensitive-file boundaries are missing
- **Problem:** Users want to exclude `.env` and secrets from what the agent reads or sends, and have no clear per-project control.
- **Links:** #2847 "A way to exclude sensitive files": +472 / 92, created 2025-08-28, closed 2026-06-29. Use it as a demand signal, not a confirmed UX fix.
- **Principle violated:** Visibility and control of data boundaries (trust).
- **Hutan should…** show what the agent can read and send per project, with an explicit ignore/deny list in the UI.

### 6. Transcript density is decided for the user, then reverted
- **Problem:** Consecutive commands were collapsed into "Ran N commands" with no way to see them unless you press Ctrl+T.
- **Links:**
  - #39903 "add an option to disable 'Ran N commands' collapsing": +83 / 66, closed 2026-09-03. Staff (etraut-openai, 2026-08-31): "we've decided to revert this change ... many of you prefer to see this level of detail, and this change broke your workflows."
  - #18396 hide tool calls and output: +50 / 18, open.
  - #41522 "allow configuring or disabling the inline diff preview limit": +33 / 5, closed 2026-09-15. Default 12 rendered rows, then "Diff preview limited (ctrl + t to view transcript)".
  - #41587 inline diff preview limit (related): +17.
- **Quote:** "Users must press Ctrl+T repeatedly to see which commands Codex executed." (#39903)
- **Principle violated:** Flexibility and efficiency of use. Progressive disclosure that users can't control. Recognition over recall.
- **Hutan should…** keep executed commands visible by default. Offer density levels (collapsed, summary, full) as a user setting, and make the full log one keystroke away. Don't flip a default without a setting.

### 7. Decorative effects and random copy add noise and hide status
- **Problem:** Animations and greetings are on by default, which hides progress. Users rely on "something changing" as their signal that an agent is running.
- **Links:**
  - #44561 whimsy/astra stars on by default: +87 / 33, open, 2026-09-10. Quote: "I thought my screen was glitching."
  - #44640 "Sparkle effects make terminal update, difficult to track agent progress": +23 / 3, closed 2026-09-24. Quote: "everything appears as if it's updating all the time".
  - #16857 "High GPU usage while the app is 'thinking' due to tiny useless animation": +137 / 53, open, 2026-04-05.
  - #48913 "Add a setting to disable random session greetings": +36 / 13, closed 2026-10-02.
  - #48991 "allow me to disable inane 'welcome messages'": +32 / 7, closed 2026-09-29.
  - Workaround from staff, #44398 (2026-09-23): `tui.animations = false` for screen readers. The issue is about sparkles blocking mouse selection in kitty (+16 / 19).
- **Principle violated:** Aesthetic and minimalist design (decoration competing with content). Visibility of system status. Accessibility (motion).
- **Hutan should…** keep activity indicators meaningful and stable. Decorative motion and copy default off or easy to disable with one setting. Also honor reduced-motion.

### 8. Attention: no reliable "done" signal
- **Problem:** Users must watch the screen to know an agent has finished. Dock/taskbar badges don't clear.
- **Links:**
  - #3962 "Play a sound when Codex finishes a prompt / task": +222 / 56, open, 2025-09-20. Quote: "frustrating that I have to sit and stare at the screen".
  - #10605 macOS Dock badge shows unread count with "no way to view or dismiss notifications in the app": +11 / 11, closed 2026-02-04.
  - #35884 "Dock badge remains at 1 after completed task is opened and read": +2 / 3, open, 2026-07-29.
  - #51244 Windows: ordinary chat responses no longer get an unread dot: +1 / 6, open, 2026-10-06.
  - #48074 Windows terminal windows flash during every request after daemon install: +164 / 153, closed 2026-10-01. Staff: "We understand the problem and have a fix in place" (2026-09-25), and a second code path needed more fixes.
- **Principle violated:** Visibility of system status. Interruption and focus stealing. Consistency of unread state.
- **Hutan should…** make completion and "needs input" its main attention signal, with an optional sound and a dock/badge count that clears when the thread is read. Never open windows or steal focus to signal a state change.

### 9. Copy and text selection break in the terminal
- **Problem:** Cmd+C and mouse selection stop working in the TUI, so users can't copy agent output.
- **Links:**
  - #48125 "I CANT FUCKING COPY TEXT": +44 / 24, closed 2026-09-29.
  - #48122 cmd+c not copying on macOS CLI: +53 / 12, closed 2026-10-02.
  - #47996 Cmd+C broken in iTerm2 0.157.0: +22 / 21, closed 2026-10-02. Regression in 0.157.0.
  - #2558 Zellij scroll truncation: +135 / 66, closed.
  - #12200 clean copy for multiline and soft-wrapped output: +32 / 13, closed.
- **Quote:** "cmd + c not copying" (#48122 title). "no longer copies selected transcript text" (#47996 title).
- **Principle violated:** Consistency with platform conventions (Cmd+C). User control of content. Error prevention (regression).
- **Hutan should…** keep native selection and Cmd+C in every view, including inside full-screen overlays. Add regression tests for copy. Expose copy-as-raw and copy-as-wrapped explicitly.

### 10. Status and usage visibility removed or made ambiguous
- **Problem:** Context and quota indicators were removed, or change without explanation. Users can't tell when a thread is near limits.
- **Links:**
  - #23794 "Codex Desktop no longer shows visible context/token usage indicator": +253 / 172, closed 2026-05-29. Staff (alexi-openai, 2026-05-21): "the intention is that Codex has infinite context, and the indicator often gets in the way of leveraging automatic compaction." Another staff reply (ambrosino-oai, 2026-05-22) said they "need to fix our beta program/app and run a proper early access program."
  - #14593 "Burning tokens very fast": +308 / 630, closed. Large trust signal.
  - #28879 rate-limit cost jump that drains the 5h budget: +561 / 211, closed 2026-06-18.
  - #54677 weekly quota jumped 67 points in 38 seconds: +43 / 45, open, 2026-10-16.
  - #17827 "Customizable status line": +220 / 54, open.
- **Quote:** "This makes long coding sessions harder to manage because I cannot tell when a thread is getting close to compaction" (#23794).
- **Principle violated:** Visibility of system status. Help users understand and control costs. Consistency and standards.
- **Hutan should…** keep a context/quota meter visible and explain its numbers (what changed, why). Make the status line configurable. Don't remove a status element without a replacement.

### 11. Branch and worktree choice removed or hidden
- **Problem:** The branch selector for new chats or worktrees was removed. Users can't see or choose the base, and in some cases the worktree is deleted while a thread is active.
- **Links:**
  - #49532 "Put the Branch selection BACK in codex app": +99 / 64, closed 2026-10-07 by montella1507. Comments from 2026-10-08 through 2026-10-19 say it is still missing: "When starting a new chat in a local Git project, no branch selector is available." (fcrhu, 2026-10-19).
  - #19913 default parent folder for "Start from scratch": +42 / 12, open, 2026-04-28.
  - #12895 "This thread's worktree was removed to save disk space": +3 / 1, open.
  - #14575 stale temp worktree blocks branch switch: +1 / 1, open. Error: "Failed to switch branch: fatal: 'main' is already used by worktree at ...".
  - #13762 worktrees on `/mnt/c` in WSL: +61 / 28, open.
  - #15347 moving a workspace folder loses threads: +41 / 19, open.
- **Principle violated:** Visibility of system state (where the agent is working). Error prevention. Consistency across releases (regression).
- **Hutan should…** show branch, base, and worktree path before a task starts. Warn before deleting any worktree that has an active thread. Make worktree cleanup explicit, not automatic.

### 12. Multi-session and multi-agent management gaps
- **Problem:** Users can't see or manage many threads in one view. Thread scope and expiry are unclear.
- **Links:**
  - #22321 "Agent View for managing multiple agents": +48 / 16, closed 2026-09-09. Staff (etraut-openai): "I've addressed most of your points already in the latest prerelease builds."
  - #25319 scope VS Code chats to the current workspace: +165 / 52, open.
  - #18993 VS Code past conversation history won't open: +62 / 52, closed 2026-06-30.
  - #13018 "I have to go to ~/.codex/archived_sessions/" to delete threads: +112 / 29, closed 2026-06-07. Staff: "Open Settings, then go to 'Archived chats'".
  - #25233 `/side` chats expire opaquely: +31 / 12, open.
  - #32283 Subagents panel no longer shows model or effort: +20 / 7, open, 2026-07-10.
  - #36300 Activity view keeps stale ordering: +10 / 9, open, 2026-07-31.
  - #19902 subagent patch approval omits the diff: found via web search only, not verified via `gh`. [INFERENCE] until checked.
- **Principle violated:** Visibility of system status. Recognition over recall. Consistency (the same thread in several places).
- **Hutan should…** provide one overview of all agents and sessions with state (running, waiting for input, done), project, model, and worktree. Scope lists to the project. Make expiry and archiving explicit.

### 13. Queued messages and sends are lost or stuck
- **Problem:** Queued follow-ups disappear, can't be edited, or leave the Send button spinning.
- **Links:**
  - #45019 "App-server queued follow-up no longer exists": +65 / 26, open, 2026-09-12.
  - #44781 editing and resending a queued message triggers the same error: +54 / 48, open, 2026-09-11.
  - #26683 "Queued messages disappear or remain stuck": +36 / 17, closed 2026-10-09.
  - #40968 Windows: "Send button spins forever and prompts never submit": +29 / 56, open, 2026-08-26.
  - #49988 VS Code extension drops submitted messages: +50 / 53, closed 2026-10-04 after a fix was released.
- **Quote:** "Error submitting message App-server queued follow-up no longer exists" (#45019).
- **Principle violated:** Error prevention and recovery. Users must never lose input. Visibility of system status.
- **Hutan should…** keep the queue visible, editable, and durable. Never drop text. Show "sent / queued / failed" on each message.

### 14. Hangs and stalls with no feedback
- **Problem:** Tasks sit at "Starting your task", the app freezes or stutters, and nothing is shown.
- **Links:**
  - #48212 Linux desktop: tasks stuck on "Starting your task"; CLI works: +33 / 40, closed 2026-09-28.
  - #20214 "Codex App frequently freezes/stutters on Windows 11 Pro despite sufficient system resources": +87 / 114, open, 2026-04-29.
  - #33561 severe lag typing and switching tasks: +5, open.
  - #14342 compacting stuck: +10 / 7.
- **Principle violated:** Visibility of system status. Responsiveness (feedback under 1 second, progress for longer waits).
- **Hutan should…** show progress for every operation and a timeout with a clear retry. Keep the UI responsive while agents run.

### 15. Silent transformation of user input
- **Problem:** Long pasted prompts turn into `.txt` attachments without notice.
- **Links:** #25144 "Add an option to disable automatic conversion of long pasted prompts into .txt attachments": +88 / 56, open, 2026-05-29. Staff (nicky-oai, 2026-06-02) added a setting to convert pastes back to editable text, but very large pastes still become attachments.
- **Quote:** "When Codex silently converts them into file attachments" (#25144).
- **Principle violated:** User control. Consistency and predictability.
- **Hutan should…** never convert user input silently. Show what a paste became, and let the user turn it back into editable text.

### 16. Diff review and theme/contrast
- **Problem:** Diffs are cramped, slow, or unreadable (color-blind palettes, contrast, wrong labels). Users want review inside their editor.
- **Links:**
  - #2998 IDE-integrated diff and approval: +238 / 67, open.
  - #35058 "Codex Diff crashes in VS Code": +140 / 51, closed 2026-07-24.
  - #10492 red-green color blindness in the diff view: +17 / 11, open.
  - #39229 selected text low contrast in dark appearance: +14 / 10, open.
  - #18942 input stays dark after system theme change: +15 / 14, closed.
  - #13671 Japanese "Staged" and "Unstaged" labels swapped: +9, open.
  - #15330 high CPU on diff render: +9 / 19, closed.
  - #13809 long diffs lag: +5 / 4, closed.
- **Principle violated:** Accessibility (color is not the only cue). Consistency with the system theme. Recognition.
- **Hutan should…** ship diff review as a first-class view, with a color-blind-safe palette and a non-color marker for add/remove. Follow system light/dark immediately. Keep large diffs virtualized.

---

## The 3 most important lessons for Hutan

1. **Make agent edits reversible and checkpointed.** Undo/rewind of code has been the top-reacted UX gap for a year (#9203, #11626). Each turn should be a checkpoint that can rewind files and conversation together.
2. **Never decide for the user.** Auto-answered questions, timed-out approvals, and silently converted input (#28969, #27458, #25144) are the most reported trust failures. Pending states stay pending, timers are opt-in, and approvals are scoped, batchable, and explained.
3. **Keep the signal honest and under user control.** Attention (sound, badge), status (context/quota), and transcript density (commands, tool calls) must be visible and configurable. Decorative motion and copy must be off by default (#3962, #23794, #39903, #44561). Hiding or removing these caused the largest threads.

## Thin areas
- Accessibility: very few reactions. Screen-reader issues are #23048 (0 reactions) and #44398. Use the staff workaround `tui.animations = false` as evidence of the motion problem, but this is not a full audit.
- Keyboard/vim: #9184 (vi mode) was closed 2026-07-21 as implemented. #3049 (configurable hotkeys) was closed 2026-05-02 as shipped with `/keymap`. Current vim quality is not verified here.
