# Claude Code (anthropics/claude-code) — UX/UI pain points

Scope: CLI/TUI, Desktop "Code" tab, VS Code/JetBrains surfaces only where they touch the same UX (permissions, diffs, sessions, keys, notifications). Pure model-quality, billing, and auth complaints excluded.

Snapshot: GitHub data read 2026-10-20 (UTC). Reaction count = `reactions.total_count` (REST); C = comment count. Quotes are short and taken from issue bodies/titles.

## How it was searched (reproducible)

- `gh label list -R anthropics/claude-code` to find UX labels: `area:tui`, `area:permissions`, `area:ui`, `area:a11y`, `area:desktop`, `area:agent-view`, `area:ide`, `keybindings`, `user-experience`, `terminal`.
- `gh api --paginate "repos/anthropics/claude-code/issues?state=all&labels=<L>&per_page=100"` per label; sorted locally by `reactions.total_count` (non-PR only). Counts: area:tui 10,594; area:permissions 3,046; area:desktop 6,315; area:ide 3,439; area:ui 1,561; area:agent-view 1,166; area:a11y 447; keybindings 279; user-experience 248.
- `gh search issues --sort reactions` queries (`permission prompt`, `diff review`, `vim`, `keybinding`, `notification`, `flicker`, `scroll`, `fullscreen`, `terminal UI`, `approval`, `confusing`, `focus`). Search API was rate-limited (shared token) and returned mostly the same high-comment issues; used as a cross-check only. `gh search` JSON omits reactions, so counts come from REST.
- Per-issue bodies via `gh api repos/anthropics/claude-code/issues/<n>`; closure via `state_reason`.
- Caveat: many 2026 issues are closed `completed` in Aug 2026 in bulk; closure does not imply the UX was fixed. Where I could not verify a fix, I say so.

---

## Top pain points

### 1. The viewport jumps under the user (auto-scroll to top / back to bottom)
- Problem: While output streams, the terminal scrolls by itself, so reading or typing is interrupted.
- #3648 "Terminal Scrolling Uncontrollably During Claude Code Interaction" — R836, C337, closed `completed` 2025-12-17. Quote: "the claude code terminal starts scrolling superfast by itself and often I can't stop it unless I kill the terminal tab".
- #36582 "Terminal keeps scrolling to top when conversation gets long" — R135, C39, open (2026-03-20). Quote: "the terminal constantly scrolls back to the top automatically".
- #34845 "Terminal randomly scrolls to top and auto-scrolls to bottom during output" — R48, C19, open.
- #1486 "View intermittently jumps to top … when reviewing code edits while scrolled up" — R96, C33, closed.
- Principle violated: user control and predictability; the system must not move the user's viewport.
- Hutan should: follow the tail only while the user is at the bottom; the moment they scroll up, freeze the viewport and show a "N new lines · jump to latest" affordance. Never move the scroll position on a streaming update.

### 2. Fullscreen/alternate-screen TUI removed native scrollback
- Problem: After v2.1.89 the TUI took over the alternate screen, so terminal scrollback, Cmd+F, and mouse-wheel history disappeared.
- #42670 "[Critical UX Regression] No way to access conversation history — alternate screen buffer kills all scrollback (v2.1.89+)" — R53, C16, closed `completed` 2026-05-05. Quote: "completely destroys native terminal scrollback. You cannot scroll up to see messages from even 10 messages ago."
- #41814 "Messages disappear from terminal display during conversation after v2.1.89 update" — R130, C62, closed `completed` 2026-04-15.
- #28077 "Allow scrolling back to view full conversation history in CLI TUI" — R80, C45, closed `completed` 2026-02-24.
- Principle violated: the user's existing affordances (scrollback, find, copy) should not be removed; reversibility.
- Hutan should: keep a real, searchable transcript that the user can scroll, search and export without leaving the session; if an alt-screen is used, it must have full in-app search and an export/"open transcript" action.

### 3. Mouse wheel repurposed to arrow keys
- Problem: In v2.1.150 the wheel stopped scrolling the conversation and sent arrow keys, which cycle prompt history.
- #65833 "v2.1.150: scroll wheel no longer scrolls conversation — sends arrow keys instead" — R124, C48, open (2026-06-06). Quote from the in-app hint: "Scroll wheel is sending arrow keys · use PgUp/PgDn to scroll".
- #70539 "Add scroll-only mouse mode to disable clicks while preserving wheel scrolling" — R92, C15, closed. #70672 (same ask, click-selection) — R56, C22, closed.
- Principle violated: consistency; a gesture should keep its meaning across updates.
- Hutan should: wheel/trackpad always scrolls the content pane. Any mode that changes that must be explicit and visible, not a side effect of an update. Offer separate toggles for "mouse selects" vs "mouse scrolls".

### 4. Mouse clicks silently act as approvals
- Problem: Clickable Yes/No in prompts meant a click on empty terminal space could approve or cancel.
- #70622 "[Feature Request] Add option to disable clickable Yes/No prompts in terminal" — R85, C23, closed `completed` 2026-08-17. Quote: "I am clicking on black space in my terminal window and constantly accidentally cancelling things, or more dangerously approving things."
- #70685 "Mouse clicks in interactive prompts auto-select options unintentionally" — R43, C11, closed.
- Principle violated: error prevention; consequential actions need deliberate input.
- Hutan should: approvals require an explicit keypress or a click on a visible button with a deliberate target, never a click on a list row or blank space. Add a setting to disable mouse-activated approvals.

### 5. Permission prompt defaults to "Always allow"
- Problem: In the Desktop app the primary button became "Always allow", with "Allow once" behind a chevron, so Enter grants permanent permission.
- #102136 "[BUG] Permission prompt defaults to 'Always allow' instead of 'Allow once' - dangerous UX regression" — R17, C10, open (2026-10-14). Quote: "Users reflexively pressing Enter silently grant permanent permissions they never intended to give."
- #55854 "Permission prompts: never use a number for 'No'; treat stray digits as 'Yes' fallback" — R10, C11, open. Quote from title: stray digits treated as Yes.
- #20093 "Add setting to enable bypassPermissions as a selectable runtime mode" — R18, C4, open.
- Principle violated: safe defaults; recognition over recall of what Enter does.
- Hutan should: the default-focused button is always "Allow once" (or "Deny"); "Always" is secondary and labelled with its exact scope. Digits and stray keys never approve.

### 6. Prompts name the wrong thing, and "always allow" doesn't match what runs
- Problem: Compound commands (`cd … && git commit`) prompt on `cd`; wildcard rules don't match compound commands; rules in settings.json aren't honored.
- #28240 "[BUG] Permission prompt incorrectly triggers on cd instead of the actual command in compound bash statements" — R209, C49, open (2026-02-24). Quote: "the permission approval prompt incorrectly identifies cd as the action requiring approval".
- #30519 "Permissions matching is fundamentally broken — 30+ open issues, no staff engagement" — R81, C31, open. Quote: "Wildcards don't match compound commands"; the author notes community is writing PreToolUse hooks to replace it.
- #6850 "[BUG] settings.local.json allow not working - keeps asking" — R46, C42, open. #13340 "global/local settings.json allow permissions are not respected" — R51, C26, open. #10906 "Built-in Plan agent ignores parent settings.json permissions" — R48, C20, open.
- Principle violated: the system's status must match the user's mental model; "always" must mean what the user thinks.
- Hutan should: show the exact command(s) being approved, split compound commands into the segments that will actually be checked, and preview the rule that "Always allow" will save. A rule that is saved must demonstrably apply on the next run; surface a "why was I asked?" line.

### 7. Mode indicators lie or fail silently; mode switching breaks
- Problem: Bypass/auto modes cannot be entered, revert after the first prompt, or are "unavailable" with little explanation.
- #61415 "Desktop: Bypass Permissions mode can't be enabled on macOS — reverts to Accept Edits, 'Permission mode couldn't be changed'" — R31, C65, open (2026-05-22).
- #36168 "Bypass/dangerously skip permissions now broken in all Claude Code versions newer than v2.1.77" — R65, C55, open.
- #33587 "auto mode temporarily unavailable - cannot enter auto mode via Shift+Tab" — R62, C36, closed.
- #97854 "Auto mode: server-side safety classifier intermittently returns no verdict, blocking Bash" — R42, C31, open (2026-09-28). #63819 "Auto mode classifier … repeatedly unavailable — blocks Bash/Write/Edit" — R30, C20, open.
- Principle violated: visibility of system status; error recovery with a stated reason and a next step.
- Hutan should: a mode chip that always reflects the real effective mode; if a mode can't be entered, say why and what to do; when a classifier is down, fall back to asking, not to silent blocking.

### 8. You cannot queue the next message while the agent works
- Problem: The only way to add a thought mid-task is to interrupt, which can derail work.
- #50246 "Feature Request: Message queue mode — queue messages instead of interrupting active tasks" — R254, C77, closed `completed` 2026-08-17. Quote: "When Claude is actively working on a task, the only option is to interrupt it."
- #33323 "[FEATURE] Task queue for queuing multiple prompts/tasks" — R64, C28, open.
- #96608 "Desktop app (Code tab): let Enter queue and Cmd+Enter send now" — R8, C2, open (2026-09-24).
- Principle violated: user control; don't make the user choose between losing work and waiting.
- Hutan should: composing while an agent runs is always allowed; new input queues visibly (with an edit/remove control) and a separate explicit action interrupts.

### 9. Submit key ambiguity; Enter and IME
- Problem: Enter sometimes sends and sometimes inserts a newline depending on state; users keep asking for configurable submit keys; IME confirmation sends the prompt.
- #89673 "Desktop app: option to make Enter insert a newline and Ctrl+Enter submit" — R67, C11, open (2026-08-26). Plus #95125 (R40, closed), #74655 "Opt-in, state-independent submit key … consolidates the Enter-to-send cluster" (R24, closed), #92774 (R10, open).
- #2054 "Insert a new line with Enter key instead of sending a message" — R196, C37, closed. #5064 "Ctrl+Enter for newlines conflicts with standard app conventions" — R58, C33, closed.
- #8405 "Pressing Enter to confirm IME (Japanese) conversion unintentionally sends the prompt" — R95, C6, closed. #1547 "IME input causes performance issues and duplicate conversion candidates" — R261, C31, open.
- Principle violated: consistency and error prevention; respect input-method composition.
- Hutan should: ship a configurable submit key with one default, never submit during IME composition, and show the active key in the composer hint.

### 10. Keys steal editing shortcuts; left arrow destroys work
- Problem: Default bindings shadow readline/macOS text editing; a stray arrow key navigates away or clears a conversation.
- #49317 "v2.1.111: Ctrl+W and Ctrl+U default bindings changed, shadowing readline delete-word and delete-to-line-start" — R8, C5, closed.
- #38807 "Ctrl+W, Shift+Tab, Alt+V stopped working in Claude Code on Windows (regression)" — R10, C5, closed. #66291 "[VSCode] macOS Ctrl+F and Ctrl+P no longer work in chat input" — R10, C9, open.
- #64657 "Left arrow key clears entire conversation with no confirmation and no undo" — R21, C6, closed `completed` 2026-08-19. Quote: "cleared the entire active conversation with no confirmation prompt and no undo path."
- #75899 "Left arrow accidentally navigates to agents screen (not rebindable)" — R21, C22, closed.
- #9177 "[FEATURE] Add user-configurable keyboard shortcut actions" — R54, C22, closed.
- Principle violated: don't override the text-editing conventions users already rely on; reversibility.
- Hutan should: never bind plain arrows or Ctrl+letters that the editor uses; make every binding rebindable from one settings screen; destructive actions need confirmation or undo.

### 11. Destructive actions are one keystroke away
- Problem: Rewind applies "Restore code and conversation" on Enter with no preview; compact/clear run without confirmation.
- #64615 "[BUG] /rewind (Esc Esc) silently reverts/loses code — destructive 'Restore code and conversation' is the default with no confirmation" — R3, C13, closed `completed` 2026-08-17. Quote: it is "the first/default option, and applies it immediately on Enter with no confirmation step and no preview".
- #72609 "Add a confirmation prompt before /compact and /clear" — R4, C4, closed. #75064 "Context/compact button triggers compact on single click with no confirmation or undo" — R3, C2, open.
- #8763 "API Error: 400 … Run /rewind to recover the conversation" — R277, C270, closed: recovery depended on the same rewind surface.
- Principle violated: error prevention; undo and preview for destructive operations.
- Hutan should: any "restore" shows a file-level diff of what will change and a non-default confirm; default focus is cancel. Checkpoints should be undoable.

### 12. Diff review is missing or noisy
- Problem: No in-panel accept/reject review for agent edits; inline diffs flood the transcript; the Read/Edit summary hides which file.
- #33932 "[FEATURE] VS Code Extension: Diff review UI similar to GitHub Copilot Edits Review" — R289, C41, open (2026-03-13). Quote: "there is no built-in UI to review, accept, or reject those changes" — the user must leave the panel for Source Control.
- #8660 "[BUG] Edit preview/diff not showing in VSCode extension UI when confirming changes" — R94, C55, open.
- #37951 "Option to hide inline diffs for Edit/Write tool output" — R124, C34, open. Quote: "there's no way to hide the diffs that appear inline during file edits."
- #61280 "Desktop app: setting to auto-expand Edit/Write diffs by default" — R33, C12, open.
- #21151 "[FEATURE] No indication of WHICH file for READ tool" — R186, C133, open (2026-01-27). Quote: "No indication of WHICH file"; "'Read 3 files' tells me nothing".
- Principle violated: visibility of system state; recognition; user control over density.
- Hutan should: a review tray listing changed files with per-hunk accept/reject, available while the agent continues; collapsed tool rows always show the target path; inline diff density is a setting.

### 13. Focus stealing and notifications
- Problem: Dialogs and notifications take focus while the user is typing; notifications cannot be disabled; idle-prompt hooks have a fixed timeout.
- #87053 "[FEATURE] Non-intrusive prompts — stop stealing focus from active typing (52 related reports, consolidated)" — R5, C5, open (2026-08-16). Quote: "A dialog, popup, or panel steals keyboard focus while the user is actively typing — text is lost, misdirected, or a wrong option fires."
- #12046 "[FEATURE] Option to disable notifications" — R41, C13, closed `completed` 2026-08-17. Quote: "Claude Code sends me notifications via my Terminal app which show up as MacOS system notifications."
- #13922 "Configurable timeout for idle_prompt notification hook" — R42, C8, open. The hook has a hardcoded 60-second timeout.
- #1288 "Feature: Sound notifications" — R44, C26, closed `completed` 2025-08-22.
- Principle violated: interruption should be proportional to need; user control of attention.
- Hutan should: notifications only for "needs you" states (approval, question, done) with per-state toggles; never steal focus while the composer has keyboard focus; queue non-urgent prompts into the attention inbox.

### 14. A question that times out answers itself
- Problem: AskUserQuestion returned after 60 s with "proceed using your best judgment", so an away user's question was answered by the model.
- #73125 "[BUG] AskUserQuestion: 'No response after 60s — continued without an answer'" — R414, C144, closed `completed` 2026-07-04. The quoted harness text: "Proceed using your best judgment based on the context so far".
- Principle violated: user control; never act on behalf of an unanswered consequential question silently.
- Hutan should: questions wait indefinitely by default and remain pending in the attention list; any timeout is opt-in and visibly labelled "assumed" in the transcript.

### 15. Redraw flicker and high-rate repaints
- Problem: Status/spinner updates repaint the whole buffer; multiplexers receive thousands of scroll events per second.
- #769 "[BUG] In-progress Call causes Screen Flickering" — R336, C307, open (2025-04-12). Quote: "The entire terminal buffer redraws with each update of the status indicator"; the author flags it as an accessibility issue for flash-sensitive users.
- #1913 "Terminal Flickering" — R321, C187, open (2025-06-10).
- #9935 "Excessive scroll events causing UI jitter in terminal multiplexers" — R53, C18, open. Quote: "4,000-6,700 scroll events per second".
- #60440 "Agent view: high-rate terminal flicker in Windows Terminal" — R13, C3, closed. #80123 "Agent view uses stale terminal width after window resize on Windows (ConPTY)" — R13, C6, open.
- Principle violated: perceived stability and latency; accessibility (photosensitivity).
- Hutan should: diff-based rendering that only touches changed cells/lines; test the spinner in tmux and Windows; cap update rate; respect reduced-motion.

---

## Also observed (shorter, same themes)

- Copy/paste mangled: #18170 "Copy/paste from terminal includes unwanted indentation and trailing spaces" — R300, C138, open; #62699 "Text cannot be copied … using Ctrl+Shift+C or right-click" — R77, C48, open; #15199 "CLI output formatting artifacts break copy/paste" — R109, C28, open. Lesson: offer "copy raw/markdown" per message and code block.
- Pasted text collapsed: #3412 "Allow viewing and editing content of 'pasted text' blocks before submission" — R313, C87, closed `completed` 2026-08-17 (dictation users); #23134 "Option to disable paste text collapse" — R179, C55, open. Lesson: never hide user input; expand inline.
- Vim: #32469 "Support cursor shape changes for vim mode" — R56, C5, open; #26231 caret stays block in INSERT — R9, C4, closed; #2588 "Incorrect Enter Key Behavior in VIM Insert Mode" — R22, C8, open; #66430 cursor renders outside input box in vim mode — R15, C9, open; #69181 Escape in INSERT clears prompt on agent screen — R6, C5, open; #76684 redo in NORMAL mode — R7, C5, open. Lesson: vim mode must keep Enter, Escape and cursor shape consistent with the mode.
- Agents/multi-session: #66202 "dismiss an agent session" (stuck on "Ready for review"/"Needs input") — R23, C11, closed; #58243 "sort by most recently updated" — R25, C9, open; #66402 "/model and /effort mutate global settings.json — breaks agents/fleet view" — R14, C16, open; #2112 "session-name" (sessions identifiable only by ID/timestamp) — R182, C44, closed `completed`; #60848 ambiguous "Don't ask me again" on resume — R16, C8, open.
- Desktop sidebar regressions: #102088 "'Group by' option removed from Code tab sidebar" — R45, C63, closed `completed` 2026-10-19; #78857 "sidebar project filter removed — docs still document it" — R32, C22, open; #78775 time-range filter only with Group by State — R43, C19, open; #89698 worktree checkbox "sticky and invisible" — R3, C1, open; #102150 Routines section "forced hidden … no UI toggle" — R24, C19, open.
- Theme and copy noise: #2990 "Automatic light/dark theme selection?" — R242, C50, closed; #11813 system theme support — R134, C20, closed; #4034 "Option to disable humorous verb choice" — R161, C56, closed; #24968 turn-duration verbs not customizable (accessibility) — R72, C22, open; #79305 desktop custom themes — R63, C20, open; #2254 "Disable the welcome banner" — R151, C47, open; #6814 "Turn off annoying progress messages?" — R218, C55, closed.
- Nagging warnings: #97679 weekly-limit banner cannot be dismissed persistently ("there's no setting or env var to turn it off") — R8, C9, open; #100278 warning every 2 minutes — R9, C4, open.
- Accessibility: #11002 "--screen-reader mode for NVDA and JAWS" — R38, C67, closed `completed` 2026-07-21; #36765 "Use of native terminal's cursor" — R79, C37, closed; #39900 same in menus — R49, C6, closed.

---

## The 3 most important lessons for Hutan

1. **Never move the viewport or focus under the user.** Follow the tail only at the bottom, freeze on scroll-up with a "jump to latest" control, and make prompts non-focus-stealing while typing (#3648, #36582, #87053, #769).
2. **Make consequential actions safe by default.** Default focus is "Allow once"/"Cancel"; "Always" shows its exact scope; no click-to-approve on blank space; rewind/compact/clear confirm with a preview; questions never auto-answer on timeout (#102136, #28240, #70622, #64615, #73125).
3. **Own the terminal basics and don't shadow them.** Keep a searchable scrollback, make the wheel scroll, copy clean text, leave readline/Ctrl+letter/arrow keys alone, and make the submit key configurable with IME-safe Enter (#42670, #65833, #18170, #89673, #49317).
