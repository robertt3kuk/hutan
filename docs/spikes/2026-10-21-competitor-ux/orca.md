# Orca (stablyai/orca): UX/UI pain points for Hutan

Scope: UX/UI problems users report in `stablyai/orca`. Backend-only crashes are excluded unless they surface as UX pain.

## Method (reproducible)
- Repo: `stablyai/orca` (about 99.5k stars). Snapshot taken from GitHub GraphQL: **8,552 issues**, all created in 2026, fetched with `gh api graphql --paginate`. Reactions and comments come from that snapshot.
- Filtering: a keyword regex on issue titles, `\b(ux|ui|confus|keyboard|shortcut|focus|layout|panel|sidebar|vim|notif|attention|diff|review|setting|theme|scroll|lag|slow|freeze|permission|prompt|status|indicator|paste|copy|workspace|switch|sort|...)\b`, matched 3,567 issues. These were ranked by `2 × reactions + comments`. The top ~100 were read through REST bodies (`gh api repos/stablyai/orca/issues/N`). For the highest-comment threads, comments were read with `gh api .../comments`.
- Labels: there is no UX or design label. The labels in use are `bug`, `enhancement`, `terminal`, `settings`, `agent-workflow`, `agent-management`, `browser`, `git`, `file-editor`, `orchestration`, `ssh`, `mobile`, and `os:*`.
- `gh search issues` was rate-limited (30/min; it returned 403 once). It also has no reaction field, so GraphQL was used instead.
- Discussions (112) were not searched.
- **Caveat on counts:** many reports are Discord-sourced. A bot mirrors them as issues with the body `via Discord`, and their reaction count is usually 0. Comment count is therefore the better demand signal here. Maximum reactions in the repo is 78 (#1099). Only a few UX requests reach 20+ reactions.

Format: `R` = reactions (thumbs included), `C` = comments. Open/closed is the state at snapshot time.

---

## Pain points

### 1. Typing and UI lag grows with the number of terminals
- **Problem:** after running for a while, the UI and typing slow down. On Windows the whole UI freezes with about 5 terminals.
- **Issues:** [#6795](https://github.com/stablyai/orca/issues/6795) (C55, closed 2026-08-02, "Orca suddenly VERY slow!"); [#11363](https://github.com/stablyai/orca/issues/11363) (R0 C12, open, Windows freeze with ~5 terminals); [#5096](https://github.com/stablyai/orca/issues/5096) (R4 C4, open, "Terminal Cursor and Orca slows down heavily").
- **Quote:** "The UI is super slow to respond!" (#6795). "cannot click, cannot type… only restart recovers" (#11363).
- **Principle violated:** system response and feedback (perceived latency). The reporter cannot see why it slows down.
- **Lesson:** Hutan should set a latency budget for input and render under N concurrent sessions. It should show per-session CPU in the UI, so users can find and stop the hungry terminal.
- Note: #6795 was closed by a maintainer with "Closing for now! Should be resolved" (nwparker, 2026-08-02). The thread shows no fix. [INFERENCE] This is a closure without a root cause.

### 2. Mouse-wheel scrolling in terminals is jittery or changes meaning
- **Problem:** scroll in a TUI stutters. After a session restore, the wheel sends arrow keys instead of scrolling.
- **Issues:** [#6144](https://github.com/stablyai/orca/issues/6144) (R1 C36, closed as a duplicate of #4932; "Laggy clause code terminal scroll"); [#16137](https://github.com/stablyai/orca/issues/16137) (R5 C3, open, "Wheel scrolling stutters in full-screen TUI apps… on macOS"); [#11123](https://github.com/stablyai/orca/issues/11123) (R2 C9, open, "mouse wheel no longer scrolls opencode sessions history" after restore).
- **Quote:** "Each wheel tick goes to the app…" (#16137, "The same app scrolls smoothly in Ghostty on the same machine"). "claude code scroll is very laggy" (#6144).
- **Principle violated:** consistency with the platform and with a reference terminal. Users compare to Ghostty and Terminal.app.
- **Lesson:** Hutan should keep wheel semantics identical to the reference terminal, in both normal and mouse-reporting modes. Restored sessions must restore the mode too.

### 3. Background events steal keyboard focus or type into the user's terminal
- **Problem:** focus moves away from where the user is typing. Agent browser commands can type into the user's terminal. Windows console windows take focus.
- **Issues:** [#19697](https://github.com/stablyai/orca/issues/19697) (R1 C9, closed, SSH tabs "keeps force-switching focus back to the tab whose agent session is running"); [#20595](https://github.com/stablyai/orca/issues/20595) (R2 C10, closed, "Agent browser commands steal the user's keyboard focus or type into their terminal"); [#14828](https://github.com/stablyai/orca/issues/14828) (R0 C11, closed, Grok hooks on Windows "flash a visible console and steal keyboard focus on every tool call"); [#20492](https://github.com/stablyai/orca/issues/20492) (R0 C12, closed, "console window flashes + steals keyboard focus").
- **Principle violated:** user control and freedom. Focus should move only when the user acts.
- **Lesson:** Hutan should never move focus or inject keystrokes from agent or background events. Agent-driven actions should be visible in a separate pane, not typed into the focused one.

### 4. Agent messages land in the user's draft, and some submit it
- **Problem:** orchestration messages are pushed into a pane while the user is typing. The text lands in the middle of the draft, and an automatic Enter can submit it. A pending prompt can also be answered by mistake.
- **Issues:** [#11242](https://github.com/stablyai/orca/issues/11242) (R1 C9, open, "push-on-idle delivery still clobbers in-progress user input, and auto-submits it"); [#21745](https://github.com/stablyai/orca/issues/21745) (R1 C6, open, "mail pointer answers a Claude AskUserQuestion, the delayed Enter selects the highlighted" option).
- **Quote:** "The injected text landed on top of my half-typed input (my draft was lost / mangled)" (#11242). "This has been making me want to uninstall orca… agents running autonomously can end up assuming a user made decisions" (#21745 comment, RafaelVidaurre, 2026-10-19).
- **Principle violated:** error prevention; user control over decisions. Automatic submission is the high-severity part.
- **Lesson:** Hutan should queue agent-to-user and agent-to-agent messages until the target input is idle and empty. It must never auto-submit or answer a question on the user's behalf. Pending messages should be visible in the pane.

### 5. Notification noise: a notification fires on every tool step
- **Problem:** users get "turn ended" notifications after tool calls, and in orchestrations every worker raises one. Users turn notifications off entirely.
- **Issues:** [#4375](https://github.com/stablyai/orca/issues/4375) (R7 C8, open, "Non turn-end notification spam"); [#15148](https://github.com/stablyai/orca/issues/15148) (R4 C7, open, "Setting to keep dispatched orchestration workers from raising notifications"); [#13600](https://github.com/stablyai/orca/issues/13600) (R9 C9, closed, "Codex 'Approve for me' can still trigger false attention notifications").
- **Quote:** "Getting notifications on non-terminal turn ends, causing too much noise and having to disable to feature" (#4375). "Every worker that finishes its turn raises an 'Agent Task Complete' notification" (#15148).
- **Principle violated:** attention management. Notifications should be scarce and actionable.
- **Lesson:** Hutan should tier notifications: needs input, then final turn done, then nothing. Workers in an orchestration should roll up into one coordinator notification by default.

### 6. Agents that need input do not notify, and there is no keyboard route to them
- **Problem:** the reverse of #5. An agent that pauses for approval raises no notification. Even when one fires, there is no keyboard way to reach the agent.
- **Issues:** [#14426](https://github.com/stablyai/orca/issues/14426) (R3 C3, closed, "No Notification When Agent Requires User Input"); [#12577](https://github.com/stablyai/orca/issues/12577) (R10 C4, open, "Keyboard shortcut to jump to the next agent that needs input").
- **Quote:** "Clicking a native notification is currently the only direct way to reach an agent that needs input" (#12577). "Currently, Orca only sends a notification when a task is completed. However, when an agent pauses and requires the user…" (#14426).
- **Principle violated:** visibility of system status, plus keyboard accessibility of the main task.
- **Lesson:** Hutan should expose a "next session that needs me" command and a count badge. Users should not need to find a notification to reach a blocked agent.

### 7. Status labels are wrong, stuck, or unexplained
- **Problem:** sidebar state does not match the real agent state. Some pills have no explanation. Recent-sort ignores agent activity.
- **Issues:** [#22017](https://github.com/stablyai/orca/issues/22017) (R2 C5, closed, "OMP agent status remains stuck at 'working'… after task completes"); [#22371](https://github.com/stablyai/orca/issues/22371) (R0 C16, closed, "incorrectly categorizes opencode2's subagent menu as a 'question'"); [#10633](https://github.com/stablyai/orca/issues/10633) (R1 C8, closed, "'Needs attention' skill pill has no explanation"); [#10813](https://github.com/stablyai/orca/issues/10813) (R0 C9, closed, "Left nav stays on 'Needs attention' after successful skill checks"); [#21765](https://github.com/stablyai/orca/issues/21765) (R0 C8, open, "Recent sorting should count agent activity").
- **Quote:** "the sidebar status indicator remains stuck at 'working' (yellow spinner) indefinitely" (#22017). "same visual state is confusing when the agent is no longer actively working" (#979, closed).
- **Principle violated:** visibility of system status; match between system and the real world; recognition over recall.
- **Lesson:** Hutan should derive status from explicit signals and show the reason on hover (e.g., "waiting for approval: Bash rm"). A state needs a timeout or a recheck path, so it cannot stay stuck.

### 8. Projects, folders, and worktrees are a confusing hierarchy
- **Problem:** the "+" buttons create a worktree every time. Child worktrees are grouped confusingly. Users cannot find how to create a folder. Switching a branch leads into the worktree flow.
- **Issues:** [#2654](https://github.com/stablyai/orca/issues/2654) (R16 C6, open, "creating workspaces decoupled from new worktrees"); [#11007](https://github.com/stablyai/orca/issues/11007) (R2 C15, closed, "Child worktree grouping in sidebar"); [#22906](https://github.com/stablyai/orca/issues/22906) (R0 C12, open, "How to create new folders from left sidebar?"); [#12984](https://github.com/stablyai/orca/issues/12984) (R7 C3, open, "switch an existing project branch without creating a worktree"); [#17855](https://github.com/stablyai/orca/issues/17855) (R0 C9, open, "Sidebar drops the first project's header").
- **Quotes:** "Both '+' buttons in the sidebar… open the same composer, and every path through that composer creates a new git worktree" (#2654). "I'm a bit confused as to what is going on with child worktrees" (#11007). "How to create new folders from left sidebar?" (#22906).
- **Principle violated:** consistency and discoverability; the user's mental model (project vs workspace vs worktree).
- **Lesson:** Hutan should name the three concepts and make each action explicit ("New worktree", "New folder", "Switch branch"). Each entry point should do one thing. A folder-kind project should not look like a broken worktree project.

### 9. The layout is fixed, and the most-requested layouts are missing
- **Problem:** the sidebars cannot be swapped. Explorer and Source Control are fixed on the right. Users cannot open a second window on another monitor. Theming stops at the left sidebar.
- **Issues:** [#6074](https://github.com/stablyai/orca/issues/6074) (R38 C15, open, "Multiple windows"); [#8377](https://github.com/stablyai/orca/issues/8377) (R18 C10, open, "Multi-worktree parallel view"); [#1355](https://github.com/stablyai/orca/issues/1355) (R3 C7, closed 2026-09-27, "swap left and right sidebar sides"); [#10153](https://github.com/stablyai/orca/issues/10153) (R5 C4, open, "Explorer and Source Control on the left"); [#10864](https://github.com/stablyai/orca/issues/10864) (R10 C9, open, "Match terminal only colors the left sidebar").
- **Quotes:** "not having the ability to have multiple windows… one when multiple displays, another when single" (#6074). "Every editor Orca sits next to puts files and source control on the left" (#10153). "the window looks two-toned" (#10864).
- **Principle violated:** flexibility and user control; aesthetic consistency.
- **Lesson:** Hutan should allow panel placement (left/right swap), multi-window and multi-monitor work, and a parallel multi-worktree view. A theme should cover the whole chrome, not just one region.

### 10. Removing a view toggle breaks escape hatches; mode parity lags
- **Problem:** the maintainers removed the terminal⇄chat toggle in 1.4.193. Users had used it to check on frozen agents. The native Chat UI also lacks mode parity: it can't run Claude in auto mode, and permission mode is global.
- **Issues:** [#17883](https://github.com/stablyai/orca/issues/17883) (R3 C12, closed 2026-09-05, "Restore the per-tab Chat UI / terminal view toggle removed after 1.4.192"); [#25559](https://github.com/stablyai/orca/issues/25559) (R13 C1, open, "Chat UI can only run Claude in bypass or Manual, never in auto mode"); [#12576](https://github.com/stablyai/orca/issues/12576) (R5 C0, open, "Per-session permission mode selector in the Chat UI composer"); [#3099](https://github.com/stablyai/orca/issues/3099) (R28 C12, open, "Chat-based UI").
- **Quotes:** "Terminal view is the ground truth. When there is something wrong in the Chat UI, I can always quickly go back to Terminal view" (QixingQstar, #17883, 2026-09-03). "you can't see what's going on unless you switch to terminal view" (MichealKennedy, #17883). Maintainer: "The problem isn't the button but the burden of maintaining this feature itself" (#17883, 2026-09-04). The toggle returned in #18532, per the thread.
- **Principle violated:** user control and freedom (always keep the raw view); consistency across modes.
- **Lesson:** Hutan should keep the raw terminal as the ground truth in any structured or chat view. Switching should stay one click and never disappear in a release.

### 11. Permission prompts: binary modes and prompt storms
- **Problem:** the choice is only "fully manual" or "no guardrails". On macOS, OS permission dialogs (1Password, "Access data from other apps", Local Network) repeat after each update or in loops.
- **Issues:** [#13081](https://github.com/stablyai/orca/issues/13081) (R22 C1, closed 2026-10-20, "Support the intermediate permission modes… Auto mode and Approve for me"); [#12534](https://github.com/stablyai/orca/issues/12534) (R0 C16, closed, "1Password permission prompt storm in Orca 1.4.167"); [#12287](https://github.com/stablyai/orca/issues/12287) (R0 C9, closed, "Sporadic infinite permission loops 'Access data from other apps' on macOS"); [#21988](https://github.com/stablyai/orca/issues/21988) (R3 C6, open, "Local Network access breaks after every Orca update on macOS 27").
- **Quote:** "run everything in manual mode and answer an approval prompt for every single action, or turn all guardrails off" (#13081). "I'm getting an extreme number of permission dialogs whenever active Orca worktrees invoke…" (#12534).
- **Principle violated:** error prevention; the approval cost is too high, so users switch guardrails off. Also "Recognition over recall" for OS prompts.
- **Lesson:** Hutan should offer a middle approval tier (ask only for risky actions) and rate-limit or batch OS permission requests. It should explain which permission a request is for and what to do.

### 12. Accounts: which account a session uses is hidden, and switching loses state
- **Problem:** switching accounts drops chat history. A running session keeps the old account. Two panes in one worktree can run on two accounts. Multiple Claude subscriptions cannot be configured side by side.
- **Issues:** [#11557](https://github.com/stablyai/orca/issues/11557) (R23 C10, open, "Support multiple Claude Code subscriptions"); [#10757](https://github.com/stablyai/orca/issues/10757) (R0 C10, closed, "CHAT HISTORY LOST AFTER ACCOUNT SWITCH"); [#19900](https://github.com/stablyai/orca/issues/19900) (R0 C9, open, "Claude account switch does not reach a running session; two panes… run on two accounts").
- **Quote:** "there only seems to be room for a single Claude Code configuration" (#11557). "Switching the managed Claude account has no effect on a Claude Code session that is already running" (#19900).
- **Principle violated:** visibility of system status; user control over which identity an action uses.
- **Lesson:** Hutan should show the account on each session header. Switching should say which sessions are affected and whether they restart. History must survive a switch.

### 13. Remote and mobile: error floods, stuck states, and lost notifications
- **Problem:** a dropped remote connection floods the UI with timeout messages. Mobile sticks on "Opening workspace". Pairing fails with an unclear code. Mobile push is missing while the app is backgrounded. Image paste on a remote host does not work.
- **Issues:** [#6364](https://github.com/stablyai/orca/issues/6364) (R0 C75, closed, "Pasting images into an agent CLI not working using Remote Host", the most-commented issue in this set); [#11392](https://github.com/stablyai/orca/issues/11392) (R0 C14, closed, "Remote environment disconnection floods the UI with repeated timeout messages"); [#28626](https://github.com/stablyai/orca/issues/28626) (R0 C9, closed, "Mobile stuck on 'Opening workspace'"); [#11714](https://github.com/stablyai/orca/issues/11714) (R10 C18, open, "Mobile pairing via Relay fails: relay_control_not_active"); [#14116](https://github.com/stablyai/orca/issues/14116) (R4 C5, closed, "notifications not delivered while app is backgrounded").
- **Quote:** "Remote environment disconnection floods the UI with repeated timeout messages" (#11392). "Couldn't create a Relay pairing code. Retry, or use LAN…" (#11714, quoted error).
- **Principle violated:** error recovery and feedback; a single state banner is better than many messages.
- **Lesson:** Hutan should show one connection-state banner with a retry, not a per-action error stream. Remote paste should have a visible fallback (for example, upload to the remote), and mobile should say when it is waiting.

### 14. Keyboard conventions: vim, rebinding, macOS copy/paste, and IME
- **Problem:** the built-in editor is not modal. Shortcuts cannot be rebound. Cmd+C and Cmd+V do not follow macOS conventions in some views. A Korean IME inserts a period.
- **Issues:** [#9699](https://github.com/stablyai/orca/issues/9699) (R15 C7, open, "Configurable editor keybinding presets (Vim, Emacs…)"); [#26921](https://github.com/stablyai/orca/issues/26921) (R4 C1, closed, "Vim Mode in Editor"); [#1493](https://github.com/stablyai/orca/issues/1493) (R1 C7, closed, "Customize keyboard shortcuts"); [#23208](https://github.com/stablyai/orca/issues/23208) (R2 C7, closed, "Cmd+C in native chat does not reliably copy selected text"); [#23530](https://github.com/stablyai/orca/issues/23530) (R3 C8, closed, "Cmd+C does not copy Codex terminal selection"); [#26739](https://github.com/stablyai/orca/issues/26739) (R2 C8, closed, "Cmd+V with an image inserts nothing"); [#11504](https://github.com/stablyai/orca/issues/11504) (R0 C9, closed, "macOS automatic period substitution injects '. ' with a CJK input source").
- **Quotes:** "Since the editor isn't modal (no Vim mode), it breaks muscle memory and slows down these quick edits" (#9699). "Pressing Cmd+C makes the Orca Edit menu react, but the selected text is not available in the clipboard" (#23208).
- **Principle violated:** consistency with platform conventions; user control; flexibility.
- **Lesson:** Hutan should make Cmd+C and Cmd+V follow macOS rules in every surface, including terminal and chat, and test IME input. Ship a vim preset and a rebinding UI from the start. Vim is the most-requested editor feature in this set.

### 15. Editor intelligence and theming are thin
- **Problem:** the editor has no LSP. Hover shows `any` for most symbols, and cross-file definition does not work. Theming cannot be set separately for the editor.
- **Issues:** [#14872](https://github.com/stablyai/orca/issues/14872) (R15 C5, open, "Editor lacks language intelligence (LSP)"); [#3035](https://github.com/stablyai/orca/issues/3035) (R29 C6, open, "Support LSP and codebase search"); [#6308](https://github.com/stablyai/orca/issues/6308) (R24 C6, open, "Configurable Monaco editor theme in Settings"); [#18512](https://github.com/stablyai/orca/issues/18512) (R9 C0, open, "Code editor themes (separately from terminal themes)").
- **Quote:** "hover shows `any` for most symbols, go-to-definition cannot leave the file" (#14872).
- **Principle violated:** match with user expectations (the editor is expected to behave like VS Code or Zed); user control of theme.
- **Lesson:** Hutan should either ship language features or be explicit about the editor's limits. Editor theme should be a separate token set from terminal theme.

---

## Not in the top 15 (noted, thin)
- Review and diff: #4540 (mark files as viewed, R5 C3), #9512 (submit PR reviews, R15 C1), #334 (per-chunk staging, R4 C7). Demand exists, but the threads are short.
- Mobile diff viewer error: #14315 (R3 C5).
- Settings and presets: #2284 (custom agent presets, R17 C20), #4382 (settings sync).

## The 3 most important lessons
1. **Protect focus and the user's draft.** No background event (agent message, browser command, SSH tab switch, OS dialog) may move focus, type, or submit input. Sources: #11242, #21745, #20595, #19697, #14828.
2. **Make attention tiered, explained, and reachable from the keyboard.** Needs input beats turn-done, which beats nothing. Workers roll up into one notification. Every status shows its reason. A shortcut jumps to the next blocked session. Sources: #4375, #15148, #14426, #12577, #22017, #10633.
3. **Keep the terminal fast with many sessions, and never remove the escape hatch.** Show resource use per session, keep one-click access to the raw terminal, and do not drop a view toggle without a replacement. Sources: #6795, #11363, #17883.

## Limits
- Reaction counts are low across the repo. Most evidence is comment-heavy bug threads, and many are Discord-mirrored, so comments may include duplicates and bot text.
- Several closures do not show a root-cause fix (for example, #6795 "Should be resolved"; #6144 closed as a duplicate of #4932). Treat them as unverified [INFERENCE].
- Discussions (112) were not searched. Product docs were not used as pain-point evidence.
