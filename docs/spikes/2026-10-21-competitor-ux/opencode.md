# OpenCode (TUI + desktop/web) — UX pain points

Snapshot: 2026-10-21. Reaction counts are total GitHub reactions (`R`) and comments (`C`) from REST `issues/N` on 2026-10-21. They move; re-check before quoting. Quotes are verbatim from issue bodies or maintainer comments as read on that date. Where I did not read the body, no quote is given.

## Scope and repo verification

- `sst/opencode` redirects to **anomalyco/opencode** (verified with `gh repo view`; active, pushed 2026-10-20). All links below use that repo.
- `opencode-ai/opencode` is a different project, **archived** on 2025-09-18. Excluded.
- **Context:** OpenCode 2.0 shipped in Oct 2026 ("OpenCode 2.0 is now out and development is focused on V2", https://anoma.ly/notes/opencode-2-0). Maintainers then closed many v1 UX issues in bulk with the marker `<!-- v1-issue-triage-2026-10 -->`, e.g. https://github.com/anomalyco/opencode/issues/36936 and https://github.com/anomalyco/opencode/issues/17076 (both closed 2026-10-11 by thdxr). Those closures mean "upgrade to v2", not "fixed in v1". Where a maintainer says v2 has the fix, I say so for that item.

## How I searched (reproducible)

- Keyword searches: `gh search issues --repo anomalyco/opencode --sort reactions --limit 25–50 '<q>'`, for `ux`, `ui`, `confusing`, `keyboard`, `focus`, `layout`, `panel`, `vim`, `clipboard`, `scroll`, `sidebar`, `permission`, `approval`, `tab`, `mouse`, `accessibility`, `modal`, `flicker`, `subagent`, `desktop`, `theme`, `notification`, `diff`, `review`, `onboarding`, `settings`, `slow`, `lag`, `paste`.
- Label searches: `gh search issues --repo anomalyco/opencode --label web`, `--label tui`, `--label 2.0`, sorted by reactions. Repo labels from `gh label list -R anomalyco/opencode` include `tui`, `opentui`, `web`, `2.0`, `windows`, `core`, `perf`, `discussion`, `design-feedback`.
- Search API returned HTTP 403 (secondary rate limit) during the run. Affected queries were re-run after waiting. Search is sorted by reactions, so the list skews to popular items. Recent low-reaction bugs are underrepresented.
- Reaction/comment counts: `gh api repos/anomalyco/opencode/issues/N`. Maintainer replies: `gh api repos/anomalyco/opencode/issues/N/comments`, filtered by `author_association` MEMBER/OWNER/COLLABORATOR.
- "open" = still open on 2026-10-21. Dates are created → closed.

## Top pain points

### 1. Forced layout migration with no way back (desktop + web)
- **Problem:** Upgrading moves users to the new "V2 / tabs-on-top" layout. The legacy layout, with a persistent left sidebar of projects and sessions, is removed. In some builds there is no in-app way back.
- **Evidence:**
  - https://github.com/anomalyco/opencode/issues/37012 — "[FEATURE] : keep legacy layout option", R 82 / C 53, closed 2026-10-01. Maintainer neriousy: "you have the ability to toggle between vertical & horizontal tabs."
  - https://github.com/anomalyco/opencode/issues/48882 — "Restore the legacy UI with the persistent left sidebar as an option", R 36 / C 27, **open**.
  - https://github.com/anomalyco/opencode/issues/48837 — "[UI feedback] Forced V2 interface destroys productivity for multi-project/multi-agent work", R 19 / C 8, closed 2026-10-20.
  - https://github.com/anomalyco/opencode/issues/37546 — "Web: no way to revert the new layout, and the new layout is missing workspaces/worktrees", R 28 / C 9, closed 2026-10-19. Maintainer: "In v2 the web UI has a single layout."
  - https://github.com/anomalyco/opencode/issues/48958 — "New layout makes the ui unusable", R 13 / C 12. Quote: "keeping basic features working is not a feature imo."
  - https://github.com/anomalyco/opencode/issues/48953 — "Why do we have to use the new layout? The new layout sucks for me.", R 14 / C 8, closed 2026-09-14.
- **Quote:** "the option to switch back to the old GUI is completely gone" (#48837); "no way to revert the new layout" (#37546 title).
- **Principle violated:** user control and freedom (escape hatch); no forced changes to a working workflow; consistency with the user's mental model.
- **Hutan should:** ship any new layout behind a visible, persisted toggle that is not removed in the same release. Never migrate existing users silently. Keep a persistent project/session sidebar as a first-class option, not a fallback.

### 2. Horizontal session tabs don't scale; sessions aren't scannable
- **Problem:** With many sessions, tab titles truncate or disappear. Users cannot see more than about five sessions at once.
- **Evidence:**
  - https://github.com/anomalyco/opencode/issues/36936 — "Desktop: wtf is the new tab layout, tab titles dont fit anymore on screen", R 31 / C 20. Quote: "I can't see any sessions titles in new layout." Closed 2026-10-11 under the v1 triage.
  - https://github.com/anomalyco/opencode/issues/36942 — "[FEATURE]: Vertical tabs", R 50 / C 23, closed 2026-10-01. Quote: "make it really difficult to see more than 5 session titles at once."
  - https://github.com/anomalyco/opencode/issues/37273 — "Allow the project and session sidebar to be used alongside session tabs", R 21 / C 4, closed 2026-10-20.
  - https://github.com/anomalyco/opencode/issues/16349 — "Add option to swap left/right panel layout", R 24 / C 8, **open**.
- **Principle violated:** recognition over recall; visibility of system status; information density.
- **Hutan should:** default to a vertical, scrollable session list with per-session state (running, waiting, needs approval, done). Offer horizontal tabs as an option, not the only mode.

### 3. Mode and agent controls disappear in the new layout
- **Problem:** The Plan/Build agent switch vanished from the chat UI. In one report the desktop webview intercepted the Tab key, so the mode could not be changed by keyboard.
- **Evidence:**
  - https://github.com/anomalyco/opencode/issues/37070 — "Bug: Plan/Build mode toggle missing from chat UI after latest update", R 24 / C 17, closed 2026-09-16.
  - https://github.com/anomalyco/opencode/issues/36997 — "[Bug] Desktop App v1.18.1 new layout hides agent (Plan/Build) switching UI", R 6 / C 13, closed 2026-10-11. Reporter says the Tab key "is also intercepted by the desktop webview"; the workaround is a settings flag, `newLayoutDesigns: false`.
  - https://github.com/anomalyco/opencode/issues/30360 — "Agent picker (build/plan) missing in v2 layout", R 10 / C 5, closed 2026-07-05.
  - https://github.com/anomalyco/opencode/issues/41742 — "Desktop app: support keyboard shortcut to switch agents (build/plan)", R 2 / C 3, closed 2026-10-19.
- **Principle violated:** visibility of system status (current mode); keyboard parity; focus conflicts.
- **Hutan should:** show the active agent and mode as a persistent, labeled control in the composer. Make the agent-switch shortcut global and documented. Never let a webview or focus handler take Tab.

### 4. Copy/selection model is broken and not configurable
- **Problem:** Copy to clipboard fails, or shows "copied" while the clipboard is empty. Mouse selection auto-copies (copy-on-select). Terminal selection and app selection conflict. Mouse capture cannot be turned off.
- **Evidence:**
  - https://github.com/anomalyco/opencode/issues/4283 — "Copy To Clipboard is not working", R 135 / C 152. Maintainer kommander (2025-12-03): "osc52 seems broken the way it is implemented currently and it shouldn't do both by default."
  - https://github.com/anomalyco/opencode/issues/13984 — "can not copy and paste in opencode CLI", R 43 / C 66. Quote: "right top show copied to clipboard，but when I ctrl+v，nothing to paste." Closed 2026-10-01 as a duplicate of #4283.
  - https://github.com/anomalyco/opencode/issues/10490 — "[Feature Request] Add config option to disable copy-on-select", R 36 / C 19, closed 2026-10-01. Maintainer neriousy: "You can switch between manual / auto in tui settings."
  - https://github.com/anomalyco/opencode/issues/4751 — "[FEATURE]: Add config option to disable copy-on-select", R 21 / C 26, closed 2025-12-02.
  - https://github.com/anomalyco/opencode/issues/5489 — "remove Copy with dragging", R 13 / C 18, closed 2026-05-22.
  - https://github.com/anomalyco/opencode/issues/6824 — "add a settings to disable mouse", R 23 / C 10. https://github.com/anomalyco/opencode/issues/7926 — "Add option to disable mouse capture in TUI", R 23 / C 11, closed 2026-08-04.
- **Principle violated:** predictable affordances; error prevention; user control; honest feedback (a "copied" message with an empty clipboard is a false status).
- **Hutan should:** make copy an explicit action (button, context menu, ⌘C in the transcript) and confirm only what reached the clipboard. Keep copy-on-select off by default. Put mouse capture and selection behavior in one visible setting, and verify clipboard writes.

### 5. Ctrl+C quits the app
- **Problem:** Ctrl+C is the universal copy key on Windows and Linux. OpenCode exits on it, and users lose work by reflex.
- **Evidence:**
  - https://github.com/anomalyco/opencode/issues/7957 — "[UX] Ctrl+C should not exit OpenCode - conflicts with universal copy shortcut", R 62 / C 19, closed 2026-10-01 as a duplicate of #2999. Maintainer neriousy: "In OpenCode v2 you can already change this by remapping or disabling the `app.exit` keybind (set it to `"none"`)."
  - https://github.com/anomalyco/opencode/issues/2999 — "Provide means to disable Ctrl-C", R 31 / C 39, **open**. Quote: "I can live with this not doing what its supposed to do (Copy to clipboard)…"
- **Principle violated:** error prevention; consistency with platform conventions.
- **Hutan should:** on macOS, use ⌘C for copy and never bind ⌘C or Ctrl+C to quit. Confirm before quitting a running session. Make the interrupt key documented and remappable.

### 6. Newline and submit keys depend on the terminal
- **Problem:** Shift+Enter inserts a newline in some terminals and sends in others. The fix is terminal-specific, and users don't know which keys to use.
- **Evidence:**
  - https://github.com/anomalyco/opencode/issues/1505 — "shift+enter keybinding not working", R 104 / C 128, closed 2025-12-15. Maintainer rekram1-node (2025-08-16): "some terminals/multiplexers don't pass this through right, you can use ctrl+j in those cases."
  - https://github.com/anomalyco/opencode/issues/9836 — "Allow to Shift + enter to edit in next line without enter", R 80 / C 28, closed 2026-10-01.
- **Principle violated:** discoverability; consistency; reliance on terminal capability.
- **Hutan should:** own the composer. Enter sends, Shift+Enter inserts a newline, and the hint is visible in the composer. Don't depend on the terminal passing modifiers.

### 7. Input editing and in-session find are missing
- **Problem:** Users want editor-grade input (vim motions, Home/End, Ctrl+R history) and search inside the transcript. A maintainer has said vim modes tend to end up as partial implementations.
- **Evidence:**
  - https://github.com/anomalyco/opencode/issues/1764 — "[FEATURE]: vim motions in input box", R 198 / C 35, closed 2026-09-14. Maintainer thdxr (2025-08-12): "vim modes tend to be kind of shitty feeling because they end up as partial implementations."
  - https://github.com/anomalyco/opencode/issues/4714 — "TUI - Search for and find string in session buffer", R 74 / C 39, **open**. Maintainer rekram1-node: "this is cool… ill have david (our designer) take a look."
  - https://github.com/anomalyco/opencode/issues/5062 — "Ctrl+R to search and navigate prompt history", R 33 / C 17, closed 2026-08-27. Maintainer: "This is a good idea."
  - https://github.com/anomalyco/opencode/issues/2649 — "Standard keyboard cursor navigation (Home/End, ctrl+left/right)", R 12 / C 11, closed 2025-12-11.
- **Principle violated:** consistency with standard editing; user control; efficiency for expert users.
- **Hutan should:** ship the planned vim mode as a complete, documented subset with a visible mode indicator. Limit it to what is documented. Add Ctrl+R history search and ⌘F find in the transcript before other features.

### 8. Auto-scroll fights reading; scroll stops working
- **Problem:** The transcript jumps to the bottom while streaming. After a manual scroll up, auto-scroll may stop for good. Scrolling does not move the output pane in some terminals.
- **Evidence:**
  - https://github.com/anomalyco/opencode/issues/7659 — "Don't Automatically Scroll Chat Window", R 28 / C 13, closed 2026-05-22. Quote: "It's very frustating to never be able to comfortably and fully read what the age…"
  - https://github.com/anomalyco/opencode/issues/7648 — "Setting to prevent TUI scrolling when new message are streamed-in", R 28 / C 13, closed 2026-07-16.
  - https://github.com/anomalyco/opencode/issues/29992 — "Auto-scroll stops working after manually scrolling and returning to bottom", R 17 / C 13, closed 2026-06-06.
  - https://github.com/anomalyco/opencode/issues/6209 — "Cannot scroll on opencode when using iterm", R 23 / C 23. Quote: "it scrolls the input box but not the output of the previous command." Maintainer could not reproduce on first pass.
  - https://github.com/anomalyco/opencode/issues/8449 — "Scroll not working", R 18 / C 7, closed 2026-08-11. https://github.com/anomalyco/opencode/issues/6257 — "Is there an easier way to scroll?", R 15 / C 12, closed 2026-06-23.
- **Principle violated:** user control of the viewport; visibility of system status.
- **Hutan should:** keep sticky-bottom only while the user is at the bottom. Show a "new output ↓ / jump to latest" control instead of moving the view. Test scrolling in each terminal and in the GUI.

### 9. Pasted text collapsed into a placeholder you can't inspect
- **Problem:** Long pastes become `[Pasted ~N lines]`. Users can't see or edit what will be sent. The placeholder also breaks speech-to-text input.
- **Evidence:**
  - https://github.com/anomalyco/opencode/issues/8501 — "Allow to expand the pasted text (e.g. `[Pasted ~1 lines]`)", R 328 / C 37, closed 2026-10-01. Maintainer rekram1-node (2026-04-29): "it feels janky to me."
  - https://github.com/anomalyco/opencode/issues/1386 — "Text paste placeholders break speech-to-text input (accessibility)", R 15 / C 6, closed 2025-09-11.
  - https://github.com/anomalyco/opencode/issues/15421 — "Pasted multi-line text from voice dictation is collapsed", R 2 / C 2.
- **Principle violated:** visibility of system status; hidden state; accessibility.
- **Hutan should:** show pasted content inline, with expand/collapse and a line count. Never send content the user cannot see.

### 10. Approvals: prompts hang, don't persist, and show only the first file
- **Problem:** Nested subagent permission prompts never render, so the session hangs. "Allow always" doesn't survive a restart. Multi-file `apply_patch` approvals show only the first file's diff. Users ask for an ask-before-edit mode and a clear YOLO mode.
- **Evidence:**
  - https://github.com/anomalyco/opencode/issues/13715 — "Permission asks from nested subagent sessions silently hang", R 32 / C 17, closed 2026-10-01. Maintainer neriousy: "Nested requests now appear in the TUI instead of hanging silently." (v2)
  - https://github.com/anomalyco/opencode/issues/20066 — "Make Allow always permission option persist across sessions", R 33 / C 11, closed 2026-09-29. Quote: "the permission is lost and the user is prompted again."
  - https://github.com/anomalyco/opencode/issues/17076 — "CLI/TUI multi-file apply_patch approval only shows first file diff", R 29 / C 10, closed 2026-10-11 (v1 triage).
  - https://github.com/anomalyco/opencode/issues/935 — "Feature Request: Add 'ask before editing files' mode", R 77 / C 23, closed 2025-07-31.
  - https://github.com/anomalyco/opencode/issues/8463 — "[FEATURE]: Add `--dangerously-skip-permissions` (aka YOLO mode)", R 94 / C 31, closed 2026-08-04. https://github.com/anomalyco/opencode/issues/1813 — yolo / non-yolo mode, R 45 / C 26, closed 2026-04-08.
  - https://github.com/anomalyco/opencode/issues/2242 — "Is there a way to sandbox the agent?", R 87 / C 100, **open**.
  - https://github.com/anomalyco/opencode/issues/37564 — "'Auto mode' LLM classifier auto-approval for permissions", R 43 / C 10, **open**.
- **Principle violated:** visibility of system status; user control; error prevention; reviewability before apply.
- **Hutan should:** run one approval inbox across all sessions, with each pending request on the session that owns it. Review every file in a patch before apply. "Allow always" should be scoped, visible, persisted, and revocable. Keep ask / auto / yolo modes explicit and labeled.

### 11. Diff review gaps, and a crash when reopening a session
- **Problem:** No session-level diff viewer. Users review by file or in an external editor. Reopening an old browser session crashes.
- **Evidence:**
  - https://github.com/anomalyco/opencode/issues/5102 — "Session or Per-chat File Diff Viewer", R 18 / C 7, closed 2026-07-03.
  - https://github.com/anomalyco/opencode/issues/9370 — "Integrated Manual File Editing via VS Code native diff editor", R 32 / C 4, closed 2026-07-06.
  - https://github.com/anomalyco/opencode/issues/8003 — "VS Code Integration for Reviewing OpenCode Code Changes (Diff Preview)", C 17. Reactions not fetched.
  - https://github.com/anomalyco/opencode/issues/19217 — "TypeError: e.diffs.map is not a function when reopening old browser session", R 69 / C 57, closed 2026-03-26.
- **Principle violated:** visibility of system status; error prevention; recovery.
- **Hutan should:** make "what changed in this session" a first-class, persistent view, per session and per turn. Test reopening old sessions as a release gate.

### 12. Context, cost and sub-agent status are hard to see
- **Problem:** Users can't see context-window use, token use, or cost during a session, or what sub-agents are doing.
- **Evidence:**
  - https://github.com/anomalyco/opencode/issues/6152 — "Session context usage (similar to /context in Claude)", R 154 / C 24, closed 2026-10-20.
  - https://github.com/anomalyco/opencode/issues/13003 — "Display token usage information in the TUI", R 54 / C 15, closed 2026-10-01. Maintainer neriousy: "The sidebar Context panel shows tokens, the percentage of the context window used, and cost."
  - https://github.com/anomalyco/opencode/issues/4925 — "Display total cost for session", R 30 / C 12, closed 2026-04-15.
  - https://github.com/anomalyco/opencode/issues/12463 — "Feature: Sub-agent status sidebar panel", R 23 / C 4, closed 2026-06-26.
  - https://github.com/anomalyco/opencode/issues/1990 — "Add User Controls for Context Management", R 45 / C 19, closed 2026-06-02.
- **Principle violated:** visibility of system status; user control.
- **Hutan should:** keep a context/cost meter and a running-agents list visible during work, not behind a command.

### 13. Archived and older sessions are hard to find or restore
- **Problem:** Archived sessions have no list or restore path on desktop. The TUI `/sessions` picker shows only the last 30 days, while the CLI shows full history.
- **Evidence:**
  - https://github.com/anomalyco/opencode/issues/6680 — "[FEATURE]: view archived sessions on desktop", R 57 / C 43, **open**. Maintainer adamdotdevin (2026-01-08): "Apologies… we'll resolve this very soon."
  - https://github.com/anomalyco/opencode/issues/24153 — "Add unarchive/restore for archived sessions", R 12 / C 11, closed 2026-10-01.
  - https://github.com/anomalyco/opencode/issues/16733 — "TUI /sessions only show sessions from last 30 days", R 4 / C 7, closed 2026-08-21. Quote: "makes it look like older sessions are gone."
  - https://github.com/anomalyco/opencode/issues/44589 — "Automatically group desktop session tabs by project", R 8 / C 3, **open**.
- **Principle violated:** recoverability; visibility of data; error prevention.
- **Hutan should:** make archive reversible from the same list, with a visible "Archived" filter. A picker should never truncate history without saying so.

### 14. In-flight messages: queue, unqueue and steer are unclear
- **Problem:** Users can't unqueue a message. The difference between queue, steer and break is unclear while the agent runs.
- **Evidence:**
  - https://github.com/anomalyco/opencode/issues/4821 — "[FEATURE]: Add ability to unqueue messages", R 126 / C 30, closed 2026-10-01. Quote: "Right now If I queue message there is no way to 'unqueue' it."
  - https://github.com/anomalyco/opencode/issues/5408 — "[FEATURE]: Delayed queue feature", R 42 / C 22, closed 2026-10-01.
  - https://github.com/anomalyco/opencode/issues/32157 — "Configurable mid-run prompt delivery: queue vs steer, with compaction-aware…", R 84 / C 10, closed 2026-10-01. Maintainer neriousy (v2): "Enter steers: the message is delivered at the next step boundary of the current run."
  - https://github.com/anomalyco/opencode/issues/19205 — "[FEATURE]: Support Interactive Steering", R 32 / C 3, closed 2026-07-10.
- **Principle violated:** user control and freedom; visibility of system state; error prevention.
- **Hutan should:** show queued messages as editable chips above the composer, each with unqueue and edit. Label send modes in place ("Send after current step" / "Interrupt and send") so the user knows what a key will do before pressing it.

### 15. Error messages mislead, and failures are silent
- **Problem:** Errors give no clear next step. The free-tier error tells a user already inside OpenCode to "download" OpenCode. Retry countdowns escalate with no skip. An aborted stream is recorded as a clean stop.
- **Evidence:**
  - https://github.com/anomalyco/opencode/issues/56324 — "Free models fail inside official OpenCode TUI with 'free tier can only be used from within OpenCode'", R 31 / C 69, closed 2026-10-20. Quote: "Error: OpenCode's free tier can only be used from within OpenCode. Download it at https://opencode.ai".
  - https://github.com/anomalyco/opencode/issues/49433 — same error text, R 19 / C 62, closed 2026-10-11 (v1 triage).
  - https://github.com/anomalyco/opencode/issues/50093 — "Free usage exceeded and there's long retry timers keep escalating across different free models", R 6 / C 10, closed 2026-10-11 (v1 triage).
  - https://github.com/anomalyco/opencode/issues/15988 — "Add 'Retry Now' button to skip rate limit retry countdown", R 28 / C 20, **open**.
  - https://github.com/anomalyco/opencode/issues/37852 — "Aborted provider stream recorded as clean stop", R 57 / C 20, closed 2026-10-11.
- **Principle violated:** help users recognize, diagnose and recover from errors (Nielsen #9); visibility of system status.
- **Hutan should:** every error names the cause, the next action, and offers a retry. Don't escalate countdowns without a skip. Mark aborted or partial turns as such in the transcript.

### 16. Felt latency, freezes and high CPU in the UI
- **Problem:** Users report slowdowns, freezes in the "working" state, and high CPU, which makes several concurrent sessions unusable.
- **Evidence:**
  - https://github.com/anomalyco/opencode/issues/30086 — "High CPU usage in newer versions of OpenCode", R 37 / C 55, closed 2026-10-01 as a duplicate of #19466. Quote: "my PC has problems with 3" (sessions, down from over 10).
  - https://github.com/anomalyco/opencode/issues/29129 — "OpenAI stream intermittently freezes in working state with high CPU", R 49 / C 50, closed 2026-05-27.
  - https://github.com/anomalyco/opencode/issues/42264 — "TUI: text disappears during LLM streaming, main thread stuck in timerfd busy-loop", R 4 / C 8.
  - https://github.com/anomalyco/opencode/issues/11112 — "always stuck at 'Preparing write...'", R 52 / C 83, closed 2026-10-01.
- **Principle violated:** responsiveness and feedback; visibility of system status.
- **Hutan should:** budget frame time and CPU per session as a release metric. Show a "working / stalled" indicator with time since the last token, so a stall doesn't look like work.

## Also observed (not ranked)
- Theme/readability: #16470 "Code output unreadable in light mode" (R 27 / C 28); #3688 "System Theme no longer works after v1.0.0" (R 20 / C 38); #9697 light/dark auto-switch (R 21 / C 5); #3845 "Markdown tables display as plain text instead of formatted tables" (R 54 / C 11).
- Accessibility: #8565 "Accessibility mode for screen reader users" (R 3 / C 10); #33137 "Screen readers cannot access streaming assistant content" (R 2 / C 4); #16252 "Keybinds should work regardless of active input method (IME)" (R 2 / C 5). Low reactions; these are the only a11y items I found.
- v2 still in flux: #28686 "Desktop V2 UI hides prompt controls and status popover" (R 7 / C 3, closed 2026-07-08); #42421 "[Feature]: add todo support for v2" (R 8 / C 32, **open**); #36423 "v2: subagent: no cancellation support for background subagents" (R 8 / C 11, **open**); #52184 "changelog: v2 releases have no published release notes" (R 33 / C 14, closed 2026-10-20).
- Onboarding/config: #4208 "No opencode.json file" (R 6 / C 19); #6156 config location (R 5 / C 9); #32988 "add interactive /settings command for editing opencode.json" (C 3, **open**).

## The 3 most important lessons for Hutan

1. **Never remove or auto-migrate a working layout or mode.** OpenCode's biggest UX problem is one forced UI change (#37012, #48882, #48837, #37546, #36936). Ship new layouts behind a persisted toggle, keep the project/session sidebar as a first-class option, and keep agent and mode controls visible in every layout.
2. **Make in-flight state visible and controllable.** Attention across many sessions, approvals (nested, persisted, per-file), queued vs steered messages, and context and cost were all heavily reacted (#13715, #20066, #17076, #4821, #32157, #6152). Give each a glanceable status and a reversible action.
3. **Don't fight terminal and OS conventions for keys and clipboard.** Ctrl+C that quits (#7957, #2999), Shift+Enter that depends on the terminal (#1505, #9836), a copy that reports success with nothing copied (#4283, #13984), and a partial vim mode (#1764) are the most-reacted input problems. Use ⌘C for copy, make Enter and Shift+Enter explicit in the composer, verify clipboard writes, and ship a vim mode only as complete as what it claims.
