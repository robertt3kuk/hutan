# Warp terminal: UX/UI pain points from user reports

Source: `warpdotdev/Warp` issues (GitHub also serves it as `warpdotdev/warp`). Reaction (R) and comment (C) counts are totals from GraphQL; `state/reason` is GitHub's stateReason. Quotes are short excerpts from the issue body.

## How this was searched (reproducible)
- REST `gh search issues` hit the search rate limit (403). Switched to GraphQL `search(type: ISSUE)` with `repo:warpdotdev/Warp is:issue <q> sort:reactions-+1-desc`, which returns reaction/comment totals and labels in one call.
- Queries: `label:area:agent`, `label:accessibility`, `label:area:code-review`, `label:area:terminal-input`, `label:area:window-tabs-panes`, `label:area:settings-keybindings`, `label:area:onboarding`, `label:area:ui-framework label:bug`, `label:os:mac label:area:agent`, `in:title notification|scroll|focus|block|"tab bar"|"input cursor"`, `in:title keybinding OR shortcut label:bug`, `in:title "Warp 2" OR redesign`, `vim`, `keyboard`, `lag|freeze|flicker`.
- Bodies read with `gh issue view N --json body,reactionGroups,comments`.
- Coverage: the repo is large and mostly feature requests (2021–2023 backlog); 2025–2026 UX bugs are fewer and lower-reaction. Thin areas are noted below.

## Top pain points

### 1. Typing a command silently switches the input into Agent mode
- Issues: https://github.com/warpdotdev/warp/issues/5552 (R24, C18, open, bug, reopened); https://github.com/warpdotdev/warp/issues/2227 (R118, C14, closed COMPLETED 2026-08-20)
- Quote (#5552): "the UX doesn't give me a clear way to get out of it and i just get stuck in this loop"; (#2227): "typing `#` in the terminal opens the AI Command Search"
- Principle violated: mode clarity, user control / reversibility, don't trigger AI on ordinary input.
- Hutan should: never change input mode based on typed text; show the current mode (shell vs agent) in the input itself; make switching one key in both directions; keep AI triggers opt-in and configurable.

### 2. Agent-first redesign removed the raw-shell default; users can't get back
- Issues: https://github.com/warpdotdev/warp/issues/8691 (R10, C8, closed DUPLICATE 2026-04-03); https://github.com/warpdotdev/warp/issues/8629 (R10, C13, open); https://github.com/warpdotdev/warp/issues/11277 (R1, C5, closed COMPLETED 2026-07-22, low-reaction but clear)
- Quote (#8691): "The old UI and way of using agents was intuitive… Please give me a way to switch back"; (#8629): "Persistent 'new /agent conversation' that just gets in the way", "opening a new tab now takes 5-10s to get a usable prompt, where previously it took 0.5s"; (#11277): "User could not find a UI setting to change the startup mode of new sessions" and had to edit `settings.toml`.
- Principle violated: don't break an established mental model; user choice of defaults; discoverability of settings.
- Hutan should: keep a plain shell as the default prompt; expose "default mode for new sessions" as a visible setting with search; never add persistent agent banners to every new tab; treat tab-open latency as a UX regression with a budget.

### 3. Input loses focus after a command finishes; keystrokes vanish
- Issues: https://github.com/warpdotdev/warp/issues/3729 (R13, C10, open since 2023-10, ready-to-implement); https://github.com/warpdotdev/warp/issues/12830 (R5, C9, open, macOS, 2026-06-19)
- Quote (#3729): "all subsequent key presses have no effect until one of the window sections is manually selected again"; (#12830): "the terminal input loses focus after every command that gets run."
- Principle violated: predictable focus and system-status visibility (the user cannot tell where input goes).
- Hutan should: after a command completes, return focus to the prompt deterministically; show a clear focus indicator on the active pane; add a regression test for focus after command exit/error/success.

### 4. Active tab is hard to see; a "tab indicator" setting has no effect
- Issues: https://github.com/warpdotdev/warp/issues/6629 (R45, C48, closed NOT_PLANNED 2026-05-12, accessibility, os:mac)
- Quote: "Compared to the previous version, it feels less minimal and harder to distinguish the active tab. The 'Show tab indicators' setting also appears to have no effect."
- Principle violated: visibility of system status; settings must visibly do what they say; the users' complaint was closed as NOT_PLANNED while still at 45 reactions.
- Hutan should: make the active tab/pane high-contrast by default and check it against WCAG-style contrast; every toggle needs a visible before/after; don't close high-reaction visual regressions without a fix or an explicit alternative.

### 5. Chrome clutter: too many tab-bar buttons, pane headers, and faint dividers
- Issues: https://github.com/warpdotdev/warp/issues/3216 (R30, C8, open); https://github.com/warpdotdev/warp/issues/4158 (R52, C5, open, "Hide Pane Headers"); https://github.com/warpdotdev/warp/issues/6317 (R14, C3, open, accessibility, "hard to tell where one pane ends and another begins")
- Quote (#3216): "It would be more appropriate to let users decide whether to display these buttons on the titlebar"; (#4158): "Pane headers take up space on the screen that is not useful for me."
- Principle violated: aesthetic and minimalist design; user control over density; contrast for boundaries.
- Hutan should: ship density controls (toggle pane headers, toggle tab-bar buttons, pane-divider color/width); default to a minimal chrome and let power users add back controls.

### 6. Hard to tell which of many agent/CLI sessions needs attention
- Issues: https://github.com/warpdotdev/warp/issues/8851 (R13, C6, open, "Badge notification on tabs when keyboard input is needed"); https://github.com/warpdotdev/warp/issues/12329 (R4, C9, open, SSH + tmux); https://github.com/warpdotdev/warp/issues/10715 (R7, C18, closed COMPLETED 2026-05-18, notifications stopped after a Claude Code update)
- Quote (#8851): "it can take a few clicks to figure out which Warp tab had the claude code instance that triggered the notification"; (#12329): "hard to identify which tab needs attention when several remote agent sessions are running."
- Principle violated: visibility of system status and attention routing across multiple sessions; reliability of the notification pipeline.
- Hutan should: give each session/tab a persistent "waiting for input / running / done" state with an unread badge; route OS notifications to the exact session; add tests against the CLI-agent event protocols (including over SSH/tmux) since they silently regress.

### 7. Notifications interrupt and cover output
- Issues: https://github.com/warpdotdev/warp/issues/8112 (R14, C7, open, os:mac); https://github.com/warpdotdev/warp/issues/8065 (R10, C5, open, "Nagging about out of credits"); https://github.com/warpdotdev/warp/issues/3548 (R3, C6, closed COMPLETED 2026-07-01)
- Quote (#8112): "a lot of notifications saying 'Blocked .... is waiting for a password', … each time I run a command that last more than ~3 seconds"; (#8065): "Warp is constantly nagging me to buy credits, obscuring output in the terminal"; "I'd be OK with just once per day as a reminder."; (#3548): "Notification messages cover user content when they appear."
- Principle violated: interruption cost, don't obscure content, avoid false positives.
- Hutan should: rate-limit and dedupe notifications; never overlay the output region (use a non-covering status strip); keep password/long-command detection tuned to avoid false positives; make each notification class individually toggleable.

### 8. Keybindings ignored, conflicts unexplained, and layout-dependent shortcuts break
- Issues: https://github.com/warpdotdev/warp/issues/537 (R287, C73, open since 2021-12, "Support bindkeys", warp:tracking); https://github.com/warpdotdev/warp/issues/4851 (R24, C1, open, "Keyboard Shortcuts are a Mess"); https://github.com/warpdotdev/warp/issues/341 (R144, C75, open, non-US input sources)
- Quote (#537): "Warp just ignores the user's bindkeys and they don't do anything."; (#4851): "The settings panel for shortcuts indicates there's a conflict but doesn't say or show what its conflicting with"; (#341): "`cmd+p` does nothing while Ukrainian input source selected".
- Principle violated: user control and consistency with existing muscle memory; error prevention and recognition over recall.
- Hutan should: honor a user keymap file (or explicitly document overrides); show the conflicting binding inline in the editor; bind by physical key position so non-Latin layouts still trigger shortcuts.

### 9. Vim mode exists but cannot be remapped to the user's own config
- Issues: https://github.com/warpdotdev/warp/issues/159 (R308, C119, closed COMPLETED 2023-10-05; the request was "Use neovim to drive vim keybindings"); https://github.com/warpdotdev/warp/issues/4161 (R18, C2, open)
- Quote (#4161): "Vim mode is great... unless you don't use the default vim bindings because you're not on a qwerty keyboard."
- Principle violated: user control; consistency with the user's own tooling. (Closure of #159 as COMPLETED plus the linked implementation post #3735 suggests vim mode shipped [INFERENCE].)
- Hutan should: make the planned vim mode read the user's remaps and layout; don't make it a fixed default-only mode.

### 10. Scroll position is yanked during output or agent streaming
- Issues: https://github.com/warpdotdev/warp/issues/8089 (R9, C14, closed COMPLETED 2026-02-03); https://github.com/warpdotdev/warp/issues/6590 (R7, C4, closed COMPLETED 2025-09-26); https://github.com/warpdotdev/warp/issues/1355 (R39, C40, closed, title-only read: "Unable to pause scroll")
- Quote (#8089): "Every keystroke causes the terminal to scroll up to the top of the conversation history and then back down"; (#6590): "scrolling through it will spike CPU usage and lag."
- Principle violated: user control of viewport; responsiveness/jank.
- Hutan should: never move the viewport the user has scrolled away from; add "pause follow" and a "jump to latest" affordance; keep scrollback scrolling within a frame budget.

### 11. Block chrome cannot be turned off; "plain terminal" mode is missing
- Issues: https://github.com/warpdotdev/warp/issues/3189 (R27, C46, closed NOT_PLANNED 2023-06-13, "Please let me disable blocks (the big red squares)"); https://github.com/warpdotdev/warp/issues/3109 (R62, C45, reopened, "Option to disable block highlighting"); https://github.com/warpdotdev/warp/issues/4894 (R8, C2, open, "Improve UI, reduce clutter")
- Quote (#4894): "Warp's UI adds a lot of clutter to the terminal output making it hard to see where the boundaries between commands and their output is"
- Principle violated: user control; aesthetic minimalism; recognition of boundaries.
- Hutan should: offer a plain-terminal rendering mode and a per-session block style toggle; keep command boundaries visible with lightweight dividers, not heavy outlines.

### 12. No keyboard-only copy/search over past output
- Issues: https://github.com/warpdotdev/warp/issues/3436 (R53, C12, open, "Find, select & copy text within blocks with keyboard only (similar to vi or tmux copy-mode)"); https://github.com/warpdotdev/warp/issues/2165 (R53, C19, closed, rectangular selection in a block)
- Quote (#3436): "Entering copy-mode manually (`prefix` + `[`), navigating using keyboard, marking wanted text and yanking it to clipboard" is what users do in iTerm2 + tmux and cannot do in Warp.
- Principle violated: keyboard accessibility and efficiency; flexibility for expert users.
- Hutan should: ship a keyboard copy/search mode over blocks and scrollback (vi-style motions), reachable from the keyboard alone.

### 13. Warp's completion popup replaces the shell's own TAB completion; suggestions disagree with menus
- Issues: https://github.com/warpdotdev/warp/issues/1811 (R372, C96, open, reopened); https://github.com/warpdotdev/warp/issues/3513 (R9, C7, open, "Autosuggestion and completions should match")
- Quote (#1811): "When I press TAB I get Warp's dialog pop-up to scroll through the options… I want the shell to handle my auto completion"; (#3513): "the autosuggestion feature often shows different suggestions than the completions menu."
- Principle violated: consistency with existing mental model; predictability.
- Hutan should: offer a "shell-native completion" mode that defers Tab to the shell; ensure the ghost suggestion and the menu's first item are the same thing.

### 14. Login wall and paywalled BYOK
- Issues: https://github.com/warpdotdev/warp/issues/900 (R506, C199, closed COMPLETED 2024-11-22, "Login required without a reason + Trial without sign in + Offline mode"); https://github.com/warpdotdev/warp/issues/9288 (R150, C25, closed COMPLETED 2026-05-22, "Make BYOK available without requiring a Pro plan"); https://github.com/warpdotdev/warp/issues/2788 (R653, C179, closed, own LLM API key)
- Quote (#900): "I'm immediately presented with a login form that I cannot dismiss."; (#9288): "BYOK still requires a Pro plan."
- Principle violated: user freedom and control; onboarding should not gate the core tool; don't paywall the user's own keys.
- Hutan should: let the terminal run fully without an account (offline mode is first-class); keep BYOK and local models free; explain what is collected and why before any sign-in prompt.

### 15. Diff/review panel lacks staging, commit, and wrap
- Issues: https://github.com/warpdotdev/warp/issues/8542 (R44, C9, open, full Git staging/commit in the Changes panel); https://github.com/warpdotdev/warp/issues/9017 (R22, C9, open, word wrap in diff and Markdown source); https://github.com/warpdotdev/warp/issues/9129 (R5, C3, open, Collapse/Expand All)
- Quote (#8542): "Warp's Changes feature currently allows users to: View file-level diffs…" but cannot stage/unstage/commit; (#9017): "long lines extend beyond the viewport and require horizontal scrolling."
- Principle violated: task completion within one tool; feature parity with where users already work.
- Hutan should: make review a complete loop (stage, unstage, commit) and keep diffs wrap-able; collapse-all for large diffs.

## Also noted (lower reaction or thinner evidence)
- UI font size cannot be raised; "Warp 2 unusable": https://github.com/warpdotdev/warp/issues/6894 (R10, C6, open, accessibility). Related: independent vertical-tab font/row scale for low vision, https://github.com/warpdotdev/warp/issues/12743 (R1, C4).
- Horizontal scrolling vs line wrap, no toggle: https://github.com/warpdotdev/warp/issues/4008 (R180, C23, open): a per-pane wrap toggle was requested.
- Group vertical tabs by repo, and workspace-style tabs holding several sessions: https://github.com/warpdotdev/warp/issues/9875 (R15, C23, open); https://github.com/warpdotdev/warp/issues/9382 (R18, C14, open).
- IME and RTL: https://github.com/warpdotdev/warp/issues/320 (R53, C17, open, marked text); https://github.com/warpdotdev/warp/issues/10355 (R16, C21, open, RTL).
- Agent output vs user input look alike: https://github.com/warpdotdev/warp/issues/10771 (R1, C2, open).
- Agent output copy was missing: https://github.com/warpdotdev/warp/issues/5815 (R9, C10, closed COMPLETED 2025-07-29). Shipped fix is evidence the pattern existed.
- Sluggish new tabs and CPU/memory: https://github.com/warpdotdev/warp/issues/8629 (see #2); https://github.com/warpdotdev/warp/issues/2611 (R71, C127, open, high memory).

## Thin areas
- Agent approval/permission UX: searched `in:title approval OR permission OR confirm OR allowlist`. Results were macOS permission prompts or unrelated confirmations (e.g. #197, #2679, R16/R10), with no agent tool-approval issue above R5. This area is under-evidenced in the repo.
- Agent code review: `in:title "review" label:area:agent` shows open, low-reaction items: #11562 "Cannot submit code review comment because all terminals are busy" (R0, C1); #9575 "PR-style review for AI agent plans before execution" (R0, C5); #9051 "Multiline code review comments" (R7, C4).
- Most 2025–2026 UX bugs in this repo have 0–5 reactions; the high-reaction items are mostly 2021–2024 feature requests.

## The 3 most important lessons for Hutan
1. Mode and focus must never change silently. Don't switch input mode (shell ↔ agent) on typed text (#5552, #2227), and always return focus to the prompt after a command completes (#3729, #12830). Show the current mode and focus in the input itself.
2. Attention is the product in multi-session work. Give every tab a persistent waiting/running/done state with an unread badge, route notifications to the exact session, rate-limit them, and never let them cover output (#8851, #12329, #10715, #8112, #8065).
3. Default to a minimal, user-controlled terminal. Offer a plain-terminal mode, toggles for block chrome, pane headers, and line wrap, user keymaps that are honored, and no login or plan wall before the terminal works (#3189, #3109, #4894, #537, #4008, #900, #9288).
