# Superset (superset-sh/superset) — UX/UI pain points

Scope: UX/UI reports only. Snapshot taken 2026-10-20 (UTC).
Repo verified: https://github.com/superset-sh/superset — "agentic IDE to orchestrate 100+ coding agents in parallel", ~15.4k stars, ~1,515 non-PR issues.

## Method (reproducible)
- `gh search issues --repo superset-sh/superset --sort reactions ...` worked for the first queries, then hit the search API rate limit (quota 0). Not retried.
- Instead: `gh api --paginate 'repos/superset-sh/superset/issues?state=all&per_page=100'`, saved to `/tmp/superset-ux/issues.jsonl` (1,515 non-PR issues), then filtered locally with a UX keyword regex (ux/ui/confus/keyboard/shortcut/focus/layout/panel/sidebar/vim/notif/diff/review/onboard/setting/theme/scroll/lag/tab/approval/permission/…) and sorted by `reactions.total_count`.
- Bodies and comments read with `gh api repos/superset-sh/superset/issues/N[/comments]` (GET only).
- Labels: repo has no UX labels (only bug 508, enhancement 206, Feature 72, Improvement 31, Chore 29, help wanted 19, Security 6). Maintainers reference triage labels in closing comments (`keyboard-shortcuts`, `workspace-creation`, `performance`, `auth`, `update-issues`) that are not in the label list.
- Reaction counts are low across the board (max 34 on #1513; most UX items 1–23). Comment counts are included as a second signal. Counts are as of 2026-10-20.
- Caveat: many relevant threads were closed by a July 2026 "stale-issue sweep" (see item 15), so closed ≠ resolved.

## Top pain points

### 1. Workspace-creation lost its name/branch control in the v2 redesign
- Problem: users can no longer name the workspace or branch at creation, or pick an existing branch; the redesigned modal hid these, and a maintainer says an A/B arm did this.
- Links: #7168 (14 reactions / 4 comments, open, 2026-09-04); #6398 (9 reactions / 9 comments, open); #7318 (10 / 7, open); #2575 (14 / 6, closed as dup); #2589 (3 / 8, closed); #3305 (3 / 3, closed).
- Quote (#7168, callumbooth): "I got very confused when I couldn't name the workspace manually until I learned that in the old version you can."
- Quote (#6398, saddlepaddle): the redesign "has done really well so far (~28% lift)… I'd like to let this one bake a bit more before we add back in the naming / branch renaming".
- Principle violated: discoverability and user control; hiding a capability behind an experiment without an alternative path. Also the one-field "name = branch" mental model is what users had.
- Hutan should: keep "name" and "branch" as explicit, first-class fields (or infer the branch from a valid typed name, as #6398 proposes) and always offer "open existing branch / worktree" next to "create new". Never ship an A/B arm that removes an existing workflow without a fallback.

### 2. The create surface mixes three jobs (new / import / open) and puts an agent prompt in the critical path
- Problem: one modal serves "from scratch", "from an existing branch", and "from an existing worktree"; the prompt dominates; the worktree picker is hard to find.
- Links: #2589 (3 / 8, closed; comment by mathieubellon, shmuelhizmi); #7365 (7 / 3, open, 2026-09-09): "clicking on the '+' icon… now it just opens the chat and there's no way to create a new branch"; #5263 (1 / 1, open): Enter vs Cmd+Enter to submit the prompt, "the opposite of most chat-style input".
- Quote (#2589, mathieubellon): "the 'create new workspace' flow is kind of confusing with the agent prompt in the critical path… the useful UI (select worktree is kind of non obvious to find)".
- Quote (#2589, shmuelhizmi): "maybe create and import should be different flows?"
- Principle violated: single-purpose flows; consistent primary action per button ("+" should mean the same thing everywhere); Enter-to-submit convention.
- Hutan should: split "New workspace", "Open existing branch/worktree", and "Start agent" into separate, labeled entry points; make "+" always create a workspace; default Enter to submit in prompt fields with Shift+Enter for newline.

### 3. Sidebar state silently disappears or reappears
- Problem: after refresh/restart, workspaces and projects vanish from the sidebar; deleted workspaces come back.
- Links: #2731 (7 / 20, closed; the most-discussed UX bug; users report "press META+R to refresh" as the fix); #4165 (4 / 5, closed); #5106 (1 / 1, closed: "Workspaces missing from repo side bar but in the Workspaces pane"); #5076 (3 / 3, open: "Deleted workspaces reappear in the sidebar"); #5226 (2 / 0).
- Quote (#2731): "When it reloaded, I lost all of my workspaces/projects. Normally, I see everything in the left sidebar."
- Principle violated: visibility of system state and trust in persistence; "refresh" as the only recovery is a sign of broken reconciliation.
- Hutan should: treat the sidebar as the single source of truth with reconciliation on launch; show a visible "syncing / N hidden / out of sync" state rather than dropping items; make refresh unnecessary.

### 4. Destructive actions are unreliable and leave the user guessing
- Problem: deleting a workspace leaves worktrees on disk (one user: ~26 GB), or errors while the worktree is present.
- Links: #2863 (10 / 2, closed: "8 orphaned worktrees accumulated to ~26 GB"); #4555 (5 / 4, closed: "Workspace is no longer available on this host" while the folder exists); #5076 (see 3); #5226 (2 / 0, open: delete-sync aborts and deleted workspaces resurrect).
- Principle violated: error prevention and clear system status before destructive commits; users can't tell what "delete" removes (DB row vs. disk).
- Hutan should: have delete show exactly what will be removed (branch, worktree folder, session) with a preview, and report partial failure explicitly with a retry, never a silent leftover.

### 5. Input lag and frozen renderer make the app feel unusable at scale
- Problem: typing lags, first keystroke takes 15–20 s on a new terminal, UI freezes, and users restart the app to keep working.
- Links: #2861 (18 / 5, closed as dup; "Performance regression: periodic input lag"); #4198 (2 / 8, open: git subprocesses across open workspaces; a user reports it "still reproduces on 1.9.7"); #3061 (3 / 7, closed: 15–20 s to first key); #4412 (4 / 3, closed: "5GB of RAM and 33% of CPU, even when idle" with 12 workspaces); #5521 (1 / 0, open: "Renderer frozen ≥11s when Changes panel mounts"); #3235 (2 / 7: EDR agent CPU).
- Quote (#2861, loeck): "I currently have 17 workspaces, and I have to restart Superset roughly every 30 minutes to keep it usable."
- Quote (#2861, Hainesy): "this lagginess is killing their enthusiasm for it."
- Principle violated: responsiveness (input must never be blocked by background polling); perceived performance.
- Hutan should: keep input on its own path; batch and back off git/status polling per workspace; never block the renderer on mount of a panel; set a latency budget for keystrokes and check it in CI.

### 6. Terminal text corrupts ("gibberish") and only resize fixes it
- Problem: glyphs turn into symbols across terminal and agent chat until the window resizes; CJK and mixed-script text corrupt.
- Links: #4601 (9 / 13, closed: "Gibberish text and symbols"); #3572 (1 / 12, closed: garbled text with 7–8 Claude Code tabs); #4639 (7 / 12, closed: CJK glyph corruption); #3570 (3 / 8, closed: ASCII mixed with Chinese corrupted); #4914 (6 / 2, closed: chat text garbled until changing the Appearance font); #4839 (2 / 0, closed: UTF-8 double-encoded).
- Quote (#4601, komly): "Same problem multiple times a day. Please fix. Only restart helps."
- Quote (#4601, pgt2195): "I just resize the windows… and it fixes it."
- Maintainer position: "it's unfortunately an xterm issue, same reports in vscode as well" (Kitenite, 2026-05-17); later "I believe this was fixed" (AviPeltz, 2026-07-10). [INFERENCE] Users still report recurrence, so the fix is not confirmed by the thread.
- Principle violated: rendering integrity; "recover by resizing" is not a recovery path users can rely on.
- Hutan should: own a forced repaint/resync on focus, visibility change and font change; ship a CJK/mixed-script test fixture; have a visible "redraw" command; do not stop at "upstream bug".

### 7. Keyboard shortcuts are swallowed, stolen, or conflict with the terminal and macOS
- Problem: Ctrl/Cmd shortcuts stop working after updates; app shortcuts override keys that agent TUIs need; macOS text-editing keys don't work in terminal input; layout-specific keys break.
- Links: #3370 (16 / 8, closed: "Mac: Control+<Key> shortcuts are broken in 1.5.0"); #3435 (8 / 4, closed); #3385 (2 / 1, closed: shortcuts from TUIs "just ignored"); #1359 (6 / 9, closed: Alt+P outputs π in Claude Code); #2970 (2 / 4, closed: Shift+Enter intercepted, breaks Kitty keyboard protocol); #2470 (2 / 3); #4924 (4 / 1, closed: Cmd+Backspace detaches Claude Code's agent view); #2222 (4 / 2, closed: "Terminals inherit keybindings from the 1970s"); #2221 (2 / 1, closed: Cmd+A selects all scrollback); #8137 (2 / 4: Ctrl+C with mouse selection).
- Quote (#3370, transcendr): "The application has become unusable." Another user (rube-de): "my layout has V on W, using cmd + V it will still trigger the cmd+w shortcut."
- Principle violated: consistency with platform conventions; predictable ownership of keys between app and embedded TUI; user remapping.
- Hutan should: publish a single shortcut-ownership table (which keys go to the app vs. the terminal); map Cmd+Backspace/Cmd+A/Cmd+C/Cmd+V to macOS text semantics in input fields; expose a keymap editor that respects keyboard layout; add an escape hatch that passes a key through to the PTY.

### 8. Keyboard navigation does not scale across workspaces and panes
- Problem: Cmd+1–9 caps at 9; sidebar scanning with 10+ workspaces; pane moves are sequential only; the focused pane is hard to see; vim motions unsupported.
- Links: #3077 (2 / 3, closed: "Cmd+K command palette… Cmd+1-9 caps out at 9 workspaces"); #3271 (1 / 1, closed: directional pane navigation); #3446 (3 / 1, closed: Cmd+Option+arrows stopped working in 1.5.3); #1920 (1 / 1, closed: "hard to tell at a glance which pane currently has focus"); #1763 (3 / 3, closed: maximize pane, "tmux PREFIX+z"); #1375 (8 / 1, closed: "impossible to navigate superset with vim motions"); #4204 (1 / 2, closed: optional Vim mode for editors); #5094 (1 / 2, open: "Equalize Pane Splits doesn't work").
- Quote (#1375): "it's impossible to navigate superset with vim motions, which is a big bummer."
- Principle violated: keyboard-first efficiency for power users; visible focus indication (WCAG 2.4.7-style focus visibility).
- Hutan should: ship a command palette and workspace switcher with fuzzy search from day one; directional pane focus; a strong, always-visible focus ring; a maximize/zoom toggle; a vim-motion mode in the embedded editor.

### 9. Agent attention and notifications are noisy, missing, or in the wrong place
- Problem: no native notification when agents need input; too many notifications for sub-agents or every turn; status dots disappear; "idle" vs "review" are conflated.
- Links: #1461 (2 / 9, closed: "Notifications and task status not working on macOS 26.2"); #3429 (3 / 1, closed: no dock badge or bounce, only in-app sound); #3652 (5 / 5, closed: opencode permission prompts get no notification); #5259 (3 / 2, open: Cursor Agent "Task complete" fires every model turn); #1785 (1 / 2, open: notification spam from Agent Teams sub-agents); #5061 (1 / 1, closed: green "done" circle not showing); #3689 (4 / 2, closed: Stop hook sets 'idle' instead of 'review', so the green completion indicator never renders); #4305 (1 / 1, closed: in-progress dot lost in the v2 sidebar); #3931 (2 / 2, closed: status dots missing in v2 hook env).
- Quote (#3429): "Superset's notification system currently only plays an in-app sound; it does not fire native macOS notifications."
- Quote (#5259): "every model turn triggers a macOS 'Task complete' notification with sound — even while the agent is clearly still working."
- Principle violated: attention management; notify on state transitions that need a human (needs input, permission, done), not on every event; consistent status vocabulary.
- Hutan should: define four states (working / needs input / ready for review / idle) and map each agent harness to them with tests; send native notifications and a dock badge only for "needs input" and "ready for review"; dedupe and throttle sub-agent events; show which workspace needs attention from anywhere.

### 10. Closing an agent tab or workspace can kill a live session with no warning or undo
- Problem: accidental Ctrl+W/close kills mid-task agents; no reopen; no confirmation.
- Links: #2124 (1 / 1, closed: "I keep accidentally closing tabs with claude code mid-task"); #3240 (2 / 2, closed: confirm before closing a tab with a live session); #3992 (2 / 2, closed: exit confirmation and reopen closed tabs); #3496 (10 / 5, open: sessions lost on quit/restart; users must hunt session IDs and run `claude --resume <id>`).
- Quote (#3496): "all open terminal tabs running agent sessions… are lost. The user has to manually find each session ID."
- Principle violated: error prevention and undo; recoverability.
- Hutan should: confirm before closing a tab with a running agent (state shown in the dialog); keep a "reopen closed tab" stack; persist session IDs and offer one-click resume on launch.

### 11. Diff/Changes review is stale, hard to scan, and not tied to agent feedback
- Problem: diffs don't refresh after follow-up edits or external changes; the v2 changes viewer makes paths hard to read; review feedback can't be anchored to lines before a PR.
- Links: #2753 (4 / 2, closed: "Diff view does not auto-refresh when files change on disk"); #5001 (1 / 0, open: stale change sets after follow-ups); #7349 (1 / 1, open: Changes pane displays stale diffs after commit + edits); #4368 (1 / 0, closed: "Bring back the v1 changes viewing experience" — paths "much harder to read", users open the changes in Cursor instead); #935 (13 / 2, closed: all-diff commenting, the single most-reacted review request); #3244 (6 / 3, closed: inline annotations on local diffs, "no way to leave feedback anchored to specific lines without first creating a PR"); #5415 and #5529 (1 / 0 each, open: comment composer is a plain textarea, no @file or slash commands).
- Quote (#4368): "the changes viewer makes paths and filenames much harder to read… it is more practical to open the changes in Cursor instead."
- Principle violated: visibility of system status (stale state presented as current); reviewing what the agent did is a core loop and must be trustworthy.
- Hutan should: subscribe to the file watcher and show "updated" / "stale" explicitly; group by directory with full path on hover and a readable filename-first layout; allow line-anchored comments on local diffs that become agent instructions, with the same rich input as the chat.

### 12. Layout: panes and agent views don't use the space they're given
- Problem: new agent panes take half the width; coding-assistant views don't fill the screen until a resize; chat and preview can't sit side by side; window clutter.
- Links: #3413 (3 / 2, closed: "Agentic sessions are using half of available space"); #3482 (2 / 2, closed: Coding Assistant doesn't fill width until resize); #5316 (1 / 1, closed: "Split Tabs View… see the chat/terminal window next to the browser window"); #3530 (12 / 3, open: "Embedded IDE tabs for Git Worktrees (to reduce window clutter!)"); #1602 (2 / 3, open: option to skip worktree creation).
- Quote (#3530): the request is to reduce the number of windows by embedding IDE tabs per worktree (12 reactions, open since 2026-04-17).
- Principle violated: layout should reflow on creation and resize without user action; fewer context switches (windows) for multi-agent work.
- Hutan should: re-layout on pane creation and on resize without a manual kick; side-by-side chat/preview as a first-class split; an embedded per-worktree editor/browser option to avoid a window per task.

### 13. Scroll behavior fights the user
- Problem: output updates yank the viewport while the user is reading history; wheel sensitivity feels coarse next to iTerm/Ghostty.
- Links: #2498 (7 / 4, closed: "being jumped around the console output"); #3057 (3 / 2, closed: scrolling up causes viewport to jump to the very top); #5364 (3 / 2, open: "Choppy scroll: low wheel sensitivity (and coarse ~3-line per-notch step) vs iTerm/Ghostty/Warp").
- Quote (#2498): "I get jumped further up the console output - disorienting and frustrating if I was in the middle of reading something."
- Principle violated: user control of the viewport; predictable scrolling.
- Hutan should: sticky-scroll only when the user is at the bottom; show a "jump to latest" pill when new output arrives; match platform wheel acceleration.

### 14. Onboarding gates and version opt-in are unclear; a paid integration did not work
- Problem: sign-in is required even for local use; the v2 switch is undocumented in the changelog; a paid Linear integration showed "not connected".
- Links: #2685 (23 / 7, closed as dup of #5591: "Make login non-required"); #4894 (11 / 1, open: "Drop the sign-in requirement; make it work offline, always, by default"); #3598 (4 / 4, closed: "not clear how to opt into v2" — answer: Settings → Experimental, flip v2 on); #1338 (2 / 13, closed: Linear integration "not connected" while connected; "shows 'not connected' despite being connected"); #4146 (2 / 1, closed: no way to set up a v2 project on a headless host).
- Quotes: #2685 (issue body): "I wonder why it requires a login. I don't think it's necessary." #1338 (florian-trehaut-hillcode): "being asked to pay for the Linear integration, paying for it, and then discovering it doesn't work is a poor user experience."
- Principle violated: progressive disclosure and trust at first run; explicit migration paths; paid features verified before purchase.
- Hutan should: make local use work without an account (offline by default); document the version/experiment switch in-app next to the change; show what an integration does and its connection status before anyone pays.

### 15. v1 → v2 churn removed working controls; closures feel like dismissal
- Problem: v2 dropped a set of v1 affordances (Run button, changes grouped view, worktree location, commit history, fetch/pull, removing v1 projects); a July 2026 "stale-issue sweep" closed many UX reports as duplicates of trackers.
- Links: #4283 (2 / 2, closed: "Bring back the one-click Run button"); #4696 (5 / 6, closed: worktree location in v2); #4841 (1 / 2, open: fetch & pull); #5101 (cited in parity tracker); #3689 (4 / 2, closed: "v1 parity" umbrella list); closures of #2685 → #5591, #3370 → #5598, #2861 → #5602, #2589 → #5609, #2575 → #5610 (all "stale-issue sweep… no activity since 2026-06-01").
- Quote (#3370, Kitenite, 2026-07-11): "Closing as a duplicate of #5598, the consolidated tracker for stale `keyboard-shortcuts` issues from the July 2026 stale-issue sweep (no activity since 2026-06-01)."
- Principle violated: continuity across redesigns (don't remove affordances without an equivalent); user feedback loop (closing reports on inactivity reads as dismissal when the bug still reproduces).
- Hutan should: run a v1 parity checklist before retiring any UI; keep a visible "Run" and worktree-location setting; never auto-close user-reported UX issues; reopen-by-comment should be obvious.

## Three most important lessons for Hutan
1. **Never hide a core path behind an experiment or redesign.** Naming, picking an existing branch, and "+ new workspace" were the most-discussed UX failures (#7168, #6398, #2575, #2589, #7365); keep explicit create/import/open entry points.
2. **Make state trustworthy and visible.** Sidebar items vanish or return (#2731, #5076), diffs go stale (#2753, #5001, #7349), status dots and notifications are missing or noisy (#3689, #3429, #5259); show reconciled state and notify only on needs-input or ready-for-review.
3. **Protect the input path and keyboard model.** Lag and renderer freezes (#2861, #4198, #5521) and swallowed or conflicting shortcuts (#3370, #2970, #1359, #2222) drove the strongest frustration; ship a shortcut-ownership table, a keymap editor, and a keystroke latency budget.
