# Zed (zed-industries/zed) — UX/UI pain points

Scope: UX/UI only (agent panel, git panel/diff, panels/docks, windows, keyboard focus, vim/helix, latency felt in UI, settings, approvals). Backend-only crashes excluded.

## Method (reproducible)
- `gh search issues --sort reactions` hit the shared GitHub search quota (30/min) mid-run, so it was replaced by REST sweeps (core quota):
  `gh api --paginate "repos/zed-industries/zed/issues?labels=<label>&state=all&since=2025-01-01T00:00:00Z&per_page=100"`, PRs filtered (`.pull_request==null`), reactions from `.reactions.total_count`, deduped and sorted by reactions. Labels swept: `area:ai`, `area:ai/agent thread`, `area:ai/acp`, `area:ai/multiagent sidebar`, `area:ai/terminal threads`, `area:ai/agent thread/{tools,checkpoints}`, `area:ui/panel`, `area:ui/dock`, `area:ui/menus`, `area:ui/tabs`, `area:controls/keybinds`, `area:controls/mouse`, `area:parity/{vim,helix,vscode,jetbrains}`, `area:integrations/git{,/panel,/split view}`, `area:workspace`, `area:navigation`, `area:settings`, `area:discoverability`, `area:accessibility`, `area:ui/themes`, `area:performance`, `area:multi-cursor`, `area:ui/minimap`, `area:controls/ime`, and ~20 more. ~8k unique issues scanned; then keyword filter (ux/confus/focus/keyboard/panel/diff/…).
- Per-issue verification via `gh api repos/zed-industries/zed/issues/N` (reaction breakdown, state, labels, body quote).
- Web: `web_search` for "Zed agent panel UX complaints" and "Zed vim mode issues".
- Reaction counts = total reactions (👍 shown where relevant). "c" = comment count. Dates are created dates.
- Caveats: many highest-reacted Zed issues are feature requests from 2022–2024 (e.g. #6722 drag tab to new window, 327👍, open; #4663 recent files palette, 155👍, open). Included only where they illustrate a current UX gap. Notifications and onboarding: searches returned no 2025–26 UX-focused issues (only #43803 "Notifications don't work on Linux", 8👍 — Linux-only, thin). Onboarding was not searched successfully (quota). Two items have 0 reactions (#57899, #54879) and are included only as focus-ambiguity examples, flagged as weak signal.

## Top pain points

### 1. Staged and unstaged changes interleaved in one diff
- Problem: git panel and diff show combined staged + unstaged hunks in one buffer; users can't see what will be committed.
- #26560 "Staged and Unstaged diffs" — 897 reactions (👍564, ❤178, 🚀155), 119 comments, closed completed 2026-07-15. https://github.com/zed-industries/zed/issues/26560
- Quote: "the git panel and diff view only show the combined staged and unstaged diffs, by interleaving them together in the buffer."
- Principle violated: visibility of system state; separation of concerns in review.
- Hutan should: keep staged and unstaged views distinct (or one clearly labelled toggle) everywhere a diff is shown; the commit set must be unambiguous before commit.

### 2. Git panel is a flat list; no tree view and no bulk selection
- #35803 "Tree view for Git Panel" — 151 reactions (👍129, 👀14, 🚀8), 25 comments, closed completed 2025-12-10. https://github.com/zed-industries/zed/issues/35803
- Quote: "Support for a nested tree-view in the Git Panel."
- #26430 "Multi-select files in the git panel" — 63👍, 24 comments, closed 2025-03-11. https://github.com/zed-industries/zed/issues/26430
- Quote: "it should be possible to select files … and then check or uncheck them as a group"
- #26097 "Files in nested directories are hard to distinguish" — 31 reactions, closed. https://github.com/zed-industries/zed/issues/26097
- Principle violated: efficiency of repeated actions; recognition over recall for file hierarchy.
- Hutan should: tree/flat toggle, shift/cmd multi-select, stage/unstage by selection, keyboard equivalents.

### 3. Git panel shows misleading state for staged-then-undone files
- #36703 "The Git panel & Uncommitted Changes view is confusing for files with staged (but since undone) changes." — 30👍, 9 comments, open (updated 2026-07-26). https://github.com/zed-industries/zed/issues/36703
- Quote: "Zed is deeply confusing in how it surfaces that fact to users of the Git Pane"
- Principle violated: state consistency; clear feedback about what an action did.
- Hutan should: make the index/worktree state of each file explicit in one place; reconcile stage/undo transitions immediately.

### 4. Git status and branch display go stale
- #49799 "Git panel always shows Project Name (no branch)" — 13👍, 63 comments, closed. https://github.com/zed-industries/zed/issues/49799 — Quote: "It incorrectly shows (no branch) even though branch exists"
- #60102 "Git status markers don't update immediately when performing git actions from within a terminal" — 26 reactions, 28 comments, open (2026-06-29). https://github.com/zed-industries/zed/issues/60102 (title only verified)
- #59459 "Git Panel doesn't refresh after click the commit button" — 14👍, 7 comments, 2026-06-17. https://github.com/zed-industries/zed/issues/59459
- Principle violated: visibility of system status (stale state is worse than none).
- Hutan should: watch git state (index, HEAD, refs) and refresh after any action, including ones run in the integrated terminal.

### 5. External file changes not reflected in the tree
- #38109 "Zed out of sync with changes made outside of editor" — 178👍 (all 👍), 210 comments, closed completed 2026-03-19. https://github.com/zed-industries/zed/issues/38109
- Quote: "Adding, removing, or renaming files and folders outside of Zed will not automatically update the project panel."
- #29598 "Manual refresh on unsupported filesystems (nfs, fuse, exfat)" — 39 reactions, open. https://github.com/zed-industries/zed/issues/29598
- Principle violated: visibility of system state; user trust in the navigator.
- Hutan should: file watching with a visible fallback (manual refresh affordance) when watching is unavailable.

### 6. Keyboard focus and shortcut routing are ambiguous
- #34865 "ctrl-w with pane::CloseActiveItem … closes the project panel instead of the active pane" — 48 reactions (👍44, 👀4), 40 comments, closed completed 2026-09-21. https://github.com/zed-industries/zed/issues/34865
- Quote: "closes the Project Panel (left dock) instead of the active editor pane after selecting a file."
- #26018 "Zed grab all shortcuts even if terminal is focussed" — 17👍, 17 comments, open (2026-09-11 update). https://github.com/zed-industries/zed/issues/26018
- Quote: "I can't do some shortcuts as they are grabbed by the zed interface" (nano in terminal)
- #38258 "Keyboards shortcuts does not work until mouse clicked inside Zed" — 12👍, 21 comments, closed. https://github.com/zed-industries/zed/issues/38258
- Quote: "After startup, keyboard shortcut does nothing. I first need to mouse click inside Zed"
- Principle violated: predictable focus; user control/escape hatches.
- Hutan should: one documented focus model (which context owns Ctrl-W, Ctrl-keys) with terminal pass-through by default and an explicit override; shortcuts must work on first keypress after launch.

### 7. Agent panel focus after "add selection to thread" is misleading (weak signal)
- #57899 "Agent Panel cursor blinks after adding code selection to thread, but focus stays in editor (misleading UX)" — 0 reactions, 2 comments, closed, labelled state:needs triage. https://github.com/zed-industries/zed/issues/57899
- #54879 "Command-Enter in agent panel focuses editor instead of sending message immediately" — 0 reactions, 5 comments, closed (stale). https://github.com/zed-industries/zed/issues/54879
- Principle violated: feedback matches focus (the cursor implies focus that isn't there).
- Hutan should: the focused element must be the one showing the caret; Enter/Cmd-Enter send behaviour must match what the input promises.

### 8. Window and project opening is unpredictable
- #53528 "Opening a new directory/project does not open a new window" — 54👍, 43 comments, open, "community champion" label (updated 2026-10-09). https://github.com/zed-industries/zed/issues/53528
- Quote: "Zed opens the directory on the currently open window instead of creating a new one"
- #49724 "'Merge All Windows' only shows one tab instead of merging both windows" — 23👍, 22 comments, open. https://github.com/zed-industries/zed/issues/49724
- #35773 "Can't use multiple windows" — 16 reactions, 3 comments, closed. https://github.com/zed-industries/zed/issues/35773
- Principle violated: user control over context; consistency with OS conventions.
- Hutan should: explicit "open here / open in new window" choice on CLI and in-app, and macOS-standard window/tab merging.

### 9. Panel and chrome clutter; no way to hide unused surfaces
- #7121 "Allow disabling unwanted panels completely" — 165 reactions (👍155, 🚀5, ❤4), 29 comments, open since 2024-01-31 (updated 2025-11-21). https://github.com/zed-industries/zed/issues/7121
- Quote: "Allow to disable _Collab Panel_, _Assistant Panel_, _Chat Panel_, _Inline Assist_"
- #22869 "Setting to always show menu bar" — 86 reactions (👍74, ❤8, 👀4), 22 comments, closed completed 2025-07-09. https://github.com/zed-industries/zed/issues/22869
- Quote: "Currently, we need to clic on the menu button to show the title menu"
- #35040 "Setting to always show menu bar but it opens without having to click on the menus" — 9 reactions, closed. https://github.com/zed-industries/zed/issues/35040
- Principle violated: user control and freedom; minimalist design should be configurable.
- Hutan should: per-surface visibility toggles (agent, collab, menu bar, status bar), persisted, with discoverable reset.

### 10. Tool approvals are ambiguous and can be over-broad
- #30313 "AI: 'Always allow' tool button should only apply to the current tool, not all tools" — 62👍, 10 comments, closed (2026-03-06 update). https://github.com/zed-industries/zed/issues/30313
- Quote: "A button that looks like it's meant to always allow a specific tool puts `"always_allow_tool_actions": true` in your config, allowing every tool to be used without confirmation."
- #56594 "Agent tool approval UI does not display command" — 11👍, 13 comments, closed. https://github.com/zed-industries/zed/issues/56594
- Quote: "It's hard to know if I can approve or deny a tool use because I can't see what the actual command is"
- #37343 "AGENT CAN RUN rm -rf $HOME/ WITHOUT ANY WARNING" — 48 reactions, 10 comments, closed. https://github.com/zed-industries/zed/issues/37343
- #39392 "'Bypass Permissions' setting reverts automatically" — 8 reactions, 10 comments. https://github.com/zed-industries/zed/issues/39392
- Principle violated: error prevention; match between approval label and effect; visibility of the action being approved.
- Hutan should: approval prompts show the exact command/diff, scope labels say exactly what they allow, and a persistent visible indicator shows when auto-approve is on.

### 11. Rate limits and provider errors surface as dead ends
- #31531 "Wait/retry when agent hits rate limits (don't just error)" — 55 reactions (👍29, ❤11, 🚀7, 🎉4), 10 comments, closed, labelled stale. https://github.com/zed-industries/zed/issues/31531
- Quote: "When a user encounters a recoverable rate-limit error … instead of erroring, wait."
- #30994 "Agent Panel: Error 'Too Many Requests' when use GitHub Copilot's model" — 45 reactions, 26 comments, closed. https://github.com/zed-industries/zed/issues/30994
- Empty-state and failed-thread states also appear in #42521 (see item 12).
- Principle violated: help users recover from errors; status visibility during waits.
- Hutan should: show retry-after countdown in-thread, auto-resume or offer one-click retry, never leave an empty/failed thread.

### 12. Agent thread history is missing, duplicated, or cross-workspace
- #37074 "Support history with external ACP agents" — 260 reactions (all 👍), 82 comments, closed (updated 2026-02-25). https://github.com/zed-industries/zed/issues/37074
- Quote: "none of the threads I've created have been saved to the history list of threads"
- #37481 "Claude code threads don't appear in the history" — 37👍, 18 comments, closed. https://github.com/zed-industries/zed/issues/37481 — Quote: "I can't find my claude code threads in the history"
- #42521 "AI: You don't have any past threads yet" — 22 reactions (👍19), 13 comments, closed. https://github.com/zed-industries/zed/issues/42521 — Quote: "it always say no past threads even I did have mulitple"
- #50854 "Agent thread history shows threads from all workspaces and fails to open threads created in a different directory" — 15👍, 8 comments, closed. https://github.com/zed-industries/zed/issues/50854
- #58676 "Search across thread history" — 19 reactions, 3 comments, open (2026-06-05). https://github.com/zed-industries/zed/issues/58676
- Principle violated: user control over their work history; consistency of mental model (scope of "history").
- Hutan should: one history model scoped by workspace with an explicit "all workspaces" switch, works identically across harnesses (OMP, Claude Code, Codex), and is searchable.

### 13. Agent edits: review/diff trust is weak
- #42074 "AI always rewrites entire file instead of doing individual line changes" — 34 reactions (👍33), 17 comments, open. https://github.com/zed-industries/zed/issues/42074 — Quote: "the AI will delete the entire file and stream the entire contents back"
- #35029 "Diff View completely green after a single line agent change" — 19👍, 7 comments, closed (stale). https://github.com/zed-industries/zed/issues/35029 — Quote: "sometimes work, and sometimes don't, i.e. instead of showing exactly which lines have been modified"
- #49856 "Agent review no longer shows in singleton editors" — 16 reactions, 16 comments, closed. https://github.com/zed-industries/zed/issues/49856 — Quote: "Observe the file changes, but no review UI appears"
- #28676 "Checkpoint restore is irreversibly destructive" — 17 reactions (👍16, 😕1), 13 comments, open (updated 2026-07-10). https://github.com/zed-industries/zed/issues/28676 — Quote: "checkpoints are the only way to walk a thread back to a previous state"
- #54545 "No way to revert multi-agent layout" — 37👍, 18 comments, closed (state needs info). https://github.com/zed-industries/zed/issues/54545 — Quote: "3. No way to revert it back"
- Principle violated: user control and freedom; undo; visibility of what the agent changed.
- Hutan should: diff review that is always accurate per-change, undo that is non-destructive (undo of a restore), and reversible layout changes.

### 14. Agent context affordances are hidden or lost
- #37087 "Missing mouse-based affordance to add context, including the currently active tab" — 23👍, 22 comments, closed. https://github.com/zed-industries/zed/issues/37087 — Quote: "The old method made it easy to add active file as context; the new method is neither comfortable to use nor works at all"
- #31351 "Missing ability to add terminal and diagnostic output into assistant panel" — 31👍, 9 comments, closed. https://github.com/zed-industries/zed/issues/31351 — Quote: "the `/diagnostic` command was incredibly valuable"
- #30081 "Agent Panel: Support vim mode in the agent panel" — 35 reactions, 11 comments, closed. https://github.com/zed-industries/zed/issues/30081
- Principle violated: discoverability; consistency with the editor's own input modes.
- Hutan should: visible, mouse-discoverable "add context" for current file/selection/terminal/diagnostics, and vim-compatible prompt editing if the editor has vim.

### 15. Modal editing: vim and helix collide; which-key lies
- #42662 "Make modal editing modes (Vim/Helix) mutually exclusive" — 15👍, 9 comments, open. https://github.com/zed-industries/zed/issues/42662 — Quote: "Enabling Vim and Helix modes simultaneously shouldn't be possible"
- #47467 "which key menu should not show vim keybindings when helix mode is on" — 12👍, 12 comments, open. https://github.com/zed-industries/zed/issues/47467 — Quote: "I would expect to not see vim keybindings"
- #38151 "helix match mode is incomplete" — 54 reactions (👍52), 18 comments, open. https://github.com/zed-industries/zed/issues/38151
- Principle violated: consistency; recognition in the help surface; user expectations of a mode.
- Hutan should: one active modal scheme at a time, and which-key/help filtered by the active scheme. Ship vim mode knowing it will be compared to Zed's behaviour.

### 16. Felt latency: scrolling, cursor, agent UI
- #33182 "Scrolling performance is very laggy on macOS 26 with external Magic Trackpad" — 35 reactions (👍33, 🚀2), 40 comments, closed. https://github.com/zed-industries/zed/issues/33182 — Quote: "it is impossible to use the IDE on the macOS beta."
- #35122 "Noticeably slow cursor movement on macOS" — 21 reactions, 6 comments, closed. https://github.com/zed-industries/zed/issues/35122
- #43338 "Zed + claude code slow and laggy and freezes" — 22 reactions (👍16, 👀6), 15 comments, closed. https://github.com/zed-industries/zed/issues/43338 — Quote: "Sessions will take ~30 seconds to launch initially, then the sessions lag and get stuck frequently."
- #53102 "All agent actions extremely slow" — 8 reactions, 45 comments, open (needs info). https://github.com/zed-industries/zed/issues/53102 — Quote: "all actions are extremely slow"
- Principle violated: response time feedback; perceived performance.
- Hutan should: budget input/scroll/caret paths on macOS (trackpad) and stream agent output without freezing the UI; show progress for slow launches.

### 17. Settings: sync missing, deprecated-setting warnings, symlinks ignored
- #59978 "Settings Sync" — 125 reactions (👍93, ❤32), 14 comments, open (created 2026-06-26; from discussion #6569 originally posted 2023). https://github.com/zed-industries/zed/issues/59978
- #24967 "Zed complains about (but does not show) deprecated settings" — 14 reactions, 11 comments, closed. https://github.com/zed-industries/zed/issues/24967
- #44786 "Settings keeps saying I'm using deprecated settings when I add an MCP server" — 12 reactions, 10 comments, closed. https://github.com/zed-industries/zed/issues/44786
- #48729 "A Zed config in a symlinked directory is not applied when I change it" — 29 reactions, 21 comments, closed. https://github.com/zed-industries/zed/issues/48729
- Principle violated: error messages that point at what's wrong; trustworthy config feedback.
- Hutan should: settings that show which file they came from and whether they're applied, warnings pointing at the exact key, and cross-machine sync.

### 18. Terminal-thread and project-switch continuity
- #55771 "Terminal processes dying when switching between projects" — 13 reactions, 15 comments, open. https://github.com/zed-industries/zed/issues/55771 — Quote: "Notice that the previously running processes no longer exists."
- #40790 "AI: current chat will stop if I switch to another chat" — 8 reactions, 5 comments, closed. https://github.com/zed-industries/zed/issues/40790
- Principle violated: user control of long-running work; state persistence across context switches.
- Hutan should: multi-session/multi-agent switching must never silently kill a running process or agent turn.

## Top 3 lessons for Hutan
1. Make diff and git state explicit and separable: staged vs unstaged, tree vs flat, bulk select, always-fresh status (#26560 897👍/119c; #35803 151👍; #26430 63👍; #36703 30👍; #60102 26👍/28c).
2. Keep displayed state true and focus obvious: external FS and git changes, which pane owns keys, which input has the caret, agent thread history scoped and searchable (#38109 178👍/210c; #34865 48👍/40c; #37074 260👍/82c; #50854).
3. Give agent actions honest approvals and reversibility: approvals show the exact command, "always allow" is scoped, auto-approve is visible, checkpoint restore and layout changes are undoable, rate limits retry instead of dead-ending (#30313 62👍; #56594; #37343 48; #28676 17/13c; #54545 37👍; #31531 55).
