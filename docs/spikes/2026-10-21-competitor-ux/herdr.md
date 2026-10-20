# herdr: UX/UI pain points (for Hutan)

Subject: `herdrdev/herdr` (~45k stars; "the runtime your coding agents live on"). Terminal-first multiplexer-style workbench with an agent-aware sidebar, Agents panel, spaces/tabs/panes, remote (`--remote`) attach, and notifications.

## Method (reproducible)

- **Issues/PRs corpus:** `gh api --paginate 'repos/herdrdev/herdr/issues?state=all&per_page=100'` → 3,745 items (2,621 issues, 1,124 PRs). Each item's `reactions` and `comments` were read from this REST output. Titles were keyword-filtered (ux, ui, confusing, focus, copy, prefix, notification, status, blocked, idle, sidebar, agents, theme, contrast, remote, ssh, onboarding, config, keybind, flicker, lag, paste, confirm, close, layout).
- **Bodies:** `gh api repos/herdrdev/herdr/issues/<n> --jq .body` for ~100 candidates; short quotes come from these bodies.
- **Discussions:** GraphQL `repository.discussions(orderBy: UPDATED_AT)`, first 800 of 1,682 total (8 pages). Ranked by `upvoteCount` within that set. The most-upvoted UX ideas are in the Ideas category.
- **Label check:** `gh label list -R herdrdev/herdr`. The `ux-changing` label exists but none of the fetched issues carried it. Labels `ui` (105), `input` (223), `rendering` (107), `agent-detection` (122), `remote` (162), `sessions` (113) were used as filters.
- **Caveat:** reactions are low overall (top UX issue ~18 reactions; most 0–8). Comment counts are the better engagement proxy here. Discussion upvotes are a separate signal (up to 104).
- **Caveat:** the discussion sample is the 800 most recent by update, so the ranking is partial.

Format per entry: problem · links (R = reactions, C = comments) · quote · principle violated · Hutan lesson.

---

## 1. Agent status misreports approval and question prompts (blocked shows as idle/done or working)

- Problem: the sidebar says idle/done while the agent is waiting for a permission approval, a question dialog, or a tool confirmation. The user stops watching the pane and misses the prompt.
- Links:
  - #3871 "Antigravity CLI command permission prompt is reported as idle instead of blocked" (closed, R4 C11)
  - #846 "Unfocused panes don't update status — agent status stays stale (approval shows idle) until focused" (closed, C8)
  - #4573 "Claude Code AskUserQuestion dialog reports idle when the session runs as a custom agent" (closed, R5 C10)
  - #5221 "Claude 'Teach auto mode…' dialog is reported as idle, not blocked" (open, C0)
  - #2917 "pi: permission-approval and ask_user prompts report agent as 'working', never 'blocked'" (closed, C2)
  - #4143 "Pi: extension ctx.ui dialogs are reported as working, never blocked" (open, R1 C2)
  - #3971 "ctx.ui.confirm silently auto-approves in herdr-hosted pi sessions — confirmation dialogs never render" (closed, C2)
- Quote (#3871): "Herdr reports the agent as `idle` (shown as done in the sidebar) instead of `blocked`."
- Quote (#846): "a pane that has reached an approval/permission prompt keeps showing `idle` in the sidebar … Focusing the pane immediately corrects the status."
- Principle: visibility of system status (Nielsen #1); error prevention.
- Hutan should: treat "blocked on me" as the highest-priority state and derive it per agent adapter with tests against each agent's real prompts. Re-evaluate status for background panes, not only the focused one. Show "status unverified" when detection is uncertain, rather than a confident "idle".

## 2. Dangerous input routing: text sent into a live approval dialog

- Problem: `agent prompt` typed text and Enter into a Claude Code permission dialog because the agent was reported as `working`. Enter then approved the highlighted "Yes" and the message was lost.
- Links: #5263 "agent prompt types into a Claude Code permission dialog while a PermissionRequest hook is still running" (open, C0, filed 2026-10). Related #5226 "Claude Code 2.1.296 question box not detected as blocked" (closed, C1).
- Quote (#5263): "In the dialog, Enter confirms the highlighted option ("Yes"), so the prompt approves the pending tool call and the message itself is lost."
- Principle: error prevention; user control and freedom (Nielsen #3, #5).
- Hutan should: block or queue programmatic/keyboard input to any pane whose state is blocked or unknown, and show a confirm step when the target looks like a prompt. Never let a "send message" action approve a tool call.

## 3. Attention ordering and unread control (the agent list can't surface what matters)

- Problem: the Agents panel lists by workspace order, so a blocked agent sits at the bottom. Users also cannot re-flag an agent they glanced at.
- Links:
  - #318 "feat: sort agents panel by attention priority" (closed, R9 C6): "A blocked agent at the bottom of the list stays at the bottom until scanned for."
  - #3786 "Priority agent panel places working agents below idle agents" (closed, C2)
  - Discussion #864 "[idea] 'Mark unread' action for agents" (**41 upvotes**, 13 comments): "there's no way to manually flip a green agent back to blue when I've only glanced at it"
  - Discussion #691 "add agent read-state controls and panel close menu" (21 upvotes, 8 comments)
- Principle: recognition over recall; flexibility and efficiency of use.
- Hutan should: make attention the default sort (blocked → done-unseen → working → idle). Provide a manual read/unread toggle with a context menu and keyboard shortcut. Keep priority obvious at a glance without scrolling.

## 4. Notifications that stick, duplicate, or fire for the wrong client or event

- Problem: toasts persist after the cause is gone, the same completion fires twice, every attached client plays the same sound, and clicking a system notification lands in Finder or does nothing.
- Links:
  - #1827 "Notification stays and never goes away" (closed, R5 C8): "Notification shows up and stays… Also quitting herdr and coming back."
  - #4787 "Every attached client plays the same notification sound, one afplay per client" (open, R3)
  - #5377 "One finished turn fires two completion notifications on a single client" (open)
  - #4480 "agent done toast is not clickable, and idle fires a second finished ping" (closed, C1)
  - #5066 "Herdr macOS notification click goes to finder" (closed, C2)
  - #916 "macOS notifications show up as 'Script Editor' instead of herdr" (closed, R1 C6)
  - #3916 "Pi: /new fires a false 'finished' toast + Done sound" (closed, C6)
  - Discussion #625 "Click a system notification to focus its originating pane" (22 upvotes, 6 comments)
  - Discussion-adjacent: #5354 "per-pane notification mute for managed agent panes" (closed, C1)
- Principle: consistency; user control; error recovery.
- Hutan should: one notification per agent event, deduped across windows. Clicking any notification must focus the originating pane and bring the window forward. Expose per-agent mute and an explicit "dismiss/clear" for sticky items. Only the client that owns focus should sound.

## 5. Copy feedback that lies: "Copied to clipboard" with nothing copied

- Problem: the app shows a success toast, but the system clipboard is empty or stale. This happens over OSC 52 and on WSL.
- Links:
  - #2399 "Clipboard copy silently fails when outer terminal ignores OSC 52" (closed, R8 C16): "the 'copied to clipboard' toast appears, but the content is not actually available in the system clipboard."
  - #4341 "copy shows 'Copied to clipboard' toast but writes to no clipboard backend (WSL2 + WSLg)" (open, R3 C5)
  - #4306 false-positive copied toast (closed)
  - Discussion #748 "Add a copy-on-select clipboard target option (PRIMARY selection)" (39 upvotes, 8 comments)
- Principle: visibility of system status; trust (feedback must match outcome).
- Hutan should: show success only after the clipboard write is verified, and show an actionable error otherwise. Offer clipboard targets (system, PRIMARY where relevant) as explicit settings.

## 6. Copy-on-select settings and mouse selection break each other

- Problem: turning off copy-on-select disables selection altogether, and drag-select drops while a TUI is busy.
- Links:
  - #1471 "copy_on_select = false disables text selection entirely instead of just suppressing clipboard copy" (closed, R4 C12): "No visual highlight appears on drag or double-click."
  - #1782 "copy_on_select=false disables manual copy (Cmd+C)" (closed, C12)
  - #3841 "Mouse drag selection releases while Codex is actively working" (closed, R4 C14)
  - #3895 "0.9.0 regression: mouse drag-selection lags behind the cursor" (closed, R3 C7)
  - #3710 "Cmd+C is consumed even when the focused pane owns the selection" (closed, C7)
- Principle: user control and freedom; consistency with host terminal conventions.
- Hutan should: separate "select" from "copy-on-select" as independent settings. Cmd+C must go to the focused pane when that pane owns a selection. Drag-select must survive redraws from running agents.

## 7. Copy/scrollback keyboard navigation is not vim-grade

- Problem: held-key repeat doesn't work in copy mode, copy mode exits while the user holds `k`, word motions split on punctuation, and H/M/L are missing.
- Links:
  - #2371 "Copy mode: allow held-key repeat for cursor-movement keys" (open, **R11** C5); PR #2372 "fix: repeat copy-mode navigation keys"
  - #3812 "Copy mode randomly exits while holding k" (closed, C13)
  - #970 "Use vim word boundary in copy mode" (open, R6 C5)
  - #231 "Add a vi-style copy mode for scrollback/selection" (closed, R6 C7)
  - Discussion #1322 "vim/tmux-style H / M / L cursor movement in copy mode" (14 upvotes, 6 comments)
  - Discussion #597 "Triple-click to select and copy full logical line" (16 upvotes, 6 comments)
- Quote (#2371): "held cursor-movement keys (arrows, h/j/k/l, w/b/e, …) do not auto-repeat — one keypress moves the cursor exactly one step."
- Principle: flexibility and efficiency; match with user mental model (vim/tmux).
- Hutan should: ship a vim-compatible copy/visual mode that matches vim motions exactly (w/b/e, H/M/L). Keyboard repeat must work. Mode exit must be explicit (Esc/q), never a side-effect of holding a key.

## 8. Modal text inputs can't be edited or pasted into

- Problem: rename/new-tab dialogs accept only typed characters. No Home/End, no Ctrl+A, no arrows, no Cmd+V.
- Links:
  - #1803 "Can't navigate in the Tab name dialog" (closed, **R14** C16): "I tried control+a, nothing happened. I tried the arrow keys, nothing happened. I had to delete everything…"
  - #302 "paste (Command+V) does not work in modal text inputs" (closed, R5 C5)
  - Discussion #735 "broken cursor movement in inputs" (12 upvotes, 7 comments)
  - PR #3698 "feat: add cursor-based editing to text inputs" (closed; merge status not verified, R6 C11)
- Principle: consistency; error prevention; efficiency.
- Hutan should: ship one shared text-input component for all naming and filter fields, with the standard shell/readline keys (arrows, Home/End, Ctrl+A/E/K/U, Alt+B/F) and system paste. Cover this with tests.

## 9. Prefix and modal keyboard states can trap the user

- Problem: after Ctrl+B the next key is silently ignored, the prefix never expires, and cancelling a confirmation leaves the client in Navigate mode.
- Links:
  - #5255 "0.9.4: prefix commands ignored in Tern; Ctrl+B enters prefix but the next key does nothing" (closed, R8 C8)
  - #5142 "Unfinished prefix never expires; the next keystroke runs a prefix action" (closed, C1)
  - #4917 "Canceling the close-workspace confirmation leaves the client in Navigate mode" (open, R1)
  - #1431 "Ctrl+[ does not close workspace navigate mode" (closed, R2 C5)
  - #3965 "NAVIGATE prefix bar: '?' toggles nav mode instead of opening keybind help" (open, C4)
- Principle: user control and freedom; visibility of system status; consistency (Esc and Ctrl+[ should behave the same).
- Hutan should: show an always-visible mode indicator, time out pending prefixes, make Esc/Ctrl+[ exit every mode, and make cancel leave the previous mode.

## 10. Agents that never appear in the panel (silent detection failure)

- Problem: a running agent is absent from the Agents panel and `agent list` is empty, with no UI explanation. Wrapped launches (nix/devenv, Windows, remote, Qwen) are the common cases.
- Links:
  - #803 "Agent detection misses nix-wrapped Claude Code… devenv agents never appear in the agents panel" (open, **R12** C16)
  - #1514 "Windows: Claude Code panes never get a detected agent label — agents panel stays empty" (closed, C9)
  - #1170 "Remote sessions don't appear in `agents` section" (closed, **R18** C7)
  - #5224 "Qwen agents stay out of the agent registry: detected in pane list, missing from the Agents panel" (closed, C3)
  - Discussion #1883 "Add Mistral Vibe as a supported agent" (25 upvotes, 2 comments)
- Quote (#803): "It doesn't appear in the agent panel and `herdr agent list` is empty, even while the agent runs normally."
- Principle: visibility of system status; recognition over recall; helpful error messages.
- Hutan should: when a pane runs a known agent, say so, and when it doesn't, show "agent not recognized" with a one-click explanation. Offer a manual "mark this pane as <agent>" override.

## 11. Sidebar density and layout trade-offs (status vs. space)

- Problem: users must choose between a full sidebar and terminal width. Collapsed mode hides the status, and the sidebar can't be placed on the right or hidden independently.
- Links:
  - Discussion #1429 "sidebar_collapsed_mode without losing agent status indicators" (16 upvotes, **16** comments): "The existing hidden collapsed mode removes the sidebar, but it also removes the at-a-glance agent status indicators."
  - Discussion #1465 "Config option for sidebar placement (left/right)" (31 upvotes, 9 comments)
  - Discussion #2466 "Option to hide the agents sidebar panel (redundant)" (13 upvotes, 3 comments)
  - Discussion #1953 "Group spaces into user-created folders" (16 upvotes, 8 comments)
  - #222 "Improve Agent Panel Row Layout" (closed, R1 C11): row layout unclear
  - #1072 "Collapsed sidebar clips agent status icons when pane numbers have multiple digits" (closed, C4)
  - #1369 "Agents view drops the tab name when a space has only one (renamed) tab" (closed, R3 C4)
  - #1321 "Sidebar agents panel ignores pane labels set via `herdr pane rename`" (closed, C4)
- Principle: flexibility and efficiency; aesthetic and minimalist design; match between system and real world.
- Hutan should: let users pick density (full, compact, status-only) and keep status icons visible in every mode. Show the user's pane and tab names in the agent list, and let the sidebar go left or right.

## 12. Color-only status and unreadable themes

- Problem: blocked, working, and unseen-idle are distinguished only by color, and several themes hide labels or cursors.
- Links:
  - #2260 "Make sidebar status indicators colorblind-safe (distinct shapes, not just color)" (closed, C10): "`state_dot()` maps three different states to the same `●` glyph… If you are colorblind, those three collapse."
  - #1307 "`theme = "terminal"` unreadable with Ghostty light theme" (open, **R18** C3)
  - #2692 "Windows: sidebar tab names and agent labels render nearly invisible on dark themes in Windows Terminal" (closed, R1 C10)
  - #4300 "Navigate-mode workspace cursor is invisible with theme = "terminal"" (closed, C5)
  - #1711 "Selection highlight hard to see in light themes" (closed, R3 C3)
  - #714 "OSC 11 background query returns light color inside Herdr despite dark host terminal" (closed, R5 C12)
  - Discussion #544 "Dim unfocused panes to cut visual distraction" (17 upvotes, 7 comments)
- Principle: accessibility (WCAG 1.4.1 — don't use color alone); consistency.
- Hutan should: encode state with shape, text, and color. Test every built-in theme against light and dark host appearances. Follow the host's appearance changes live.

## 13. Remote machines: one bad connection locks the whole client

- Problem: a dropped SSH connection blocks switching back to Local, MFA or host-key failures appear as generic disconnects, and password prompts repeat.
- Links:
  - #3903 "0.9.0: disconnected SSH machine blocks switching back to Local while reconnecting" (closed, R3 C8)
  - #3763 "machine add fails after SSH MFA on 0.9.0" (closed, R9 C10)
  - #3731 "machine add hides SSH host-key verification failure as a server disconnection" (closed, R2 C6)
  - #888 "Remote attach prompts for password repeatedly despite correct password" (closed, C10)
  - #3651 "--remote against a Windows OpenSSH target fails with a raw PowerShell error" (closed, C20)
  - #3769 "Remote attach UI becomes sluggish in 0.9.0" (closed, R8 C15): "clicks can take a few seconds to register"
  - Discussion #515 "Support multiple remote Herdr servers" (**104 upvotes**, 37 comments, the top-voted discussion)
  - #277 "Remote workspace" (closed, R5 C2): mixing local and remote workspaces in one session
- Principle: user control and freedom; error recovery; visibility of system status.
- Hutan should: give each machine its own state. Never block the local workspace on a remote failure. Map error classes (auth, MFA, host key, network) to specific, actionable messages. Plan for multi-host from the start, since it is the most-requested remote feature.

## 14. Settings and hints that don't match the effective state

- Problem: hint bars advertise keys that don't work, config changes are not applied while `config check` says OK, and the reload toast disappears.
- Links:
  - #2880 "Settings hint bar always shows ↑↓ select, including on Integrations which has no arrow-key handler" (open, R2 C3)
  - #4815 "Settings → agent integrations list doesn't respond to arrow keys or mouse clicks" (closed, C1)
  - #3678 "Windows 0.8.2: settings changes stop applying after a config edit — config.toml gets a BOM" (closed, C4)
  - #5235 "Windows 0.9.3: config.toml is not applied although `herdr config check` reports ok" (open, C1)
  - #2452 "config check reports ok for an unknown [theme] name" (closed, C3)
  - #5309 "Config reload no longer shows the 'reloaded config' toast" (open)
  - #3780 "ui.sidebar.spaces.row_gap has no effect in 0.9.0" (closed, C1)
  - #372 "feat: fuzzy search in keybinds / settings" (closed, C2); Discussion #862 "Allow overwriting prefix+ctrl+b in config" (17 upvotes, 3 comments)
- Principle: visibility of system status; consistency; help users recognize errors.
- Hutan should: validate config against what the app actually applies, and make the settings hint bar reflect only the controls that exist on that page. Show a visible confirmation when config reloads.

## 15. Onboarding and first-run that show the wrong state

- Problem: the first-run modal shows the default prefix instead of the configured one, onboarding doesn't appear in the default session mode, and a failed save leaves a warning forever.
- Links:
  - #3202 "Onboarding modal displays default prefix instead of configured prefix" (closed, C4)
  - #2190 "Onboarding never shows in default persistent-session mode (works only with --no-session)" (closed, C1)
  - #4455 "Onboarding save failure leaves a persistent config warning" (closed, C1)
  - #1161/#1162 install.ps1 duplicate onboarding line and BOM breaks TOML on first run (closed, C2 each)
- Principle: match between system and real world; consistency; error recovery.
- Hutan should: onboarding reads the effective configuration, appears in every launch mode the user can pick, and has a retry path when saving fails.

## 16. Destructive and blocking actions behave inconsistently

- Problem: close confirmations differ by path, closing the last pane silently closes the workspace or opens a new one in `$HOME`, and worktree deletion blocks the UI.
- Links:
  - #1750 "Close confirmation is inconsistent across close paths" (closed, R6 C4): "Closing a workspace directly asks for confirmation… Closing the last tab… closes that same workspace"
  - #5143 "Closing a workspace's only pane closes the workspace without confirmation" (closed, C3)
  - #5320 "Closing the last pane opens the new workspace in $HOME, and the next start restores it" (closed, C2)
  - #1797 "Cannot delete workspace with submodules" (closed, **R8** C16)
  - Discussion #1428 "Non-blocking / async worktree deletion — large repos hang the UI (~10s)" (17 upvotes, 6 comments)
  - #4578 "endpoint_busy locks the whole client after a slow 'Delete worktree checkout' times out" (open, R4)
- Principle: error prevention; user control; consistency.
- Hutan should: use one confirmation policy for every destructive path (close pane, tab, workspace, worktree). Run slow filesystem work asynchronously with progress and cancel, never on the UI thread.

## 17. Agent input and paste reliability (submit, multi-line, image)

- Problem: multi-line paste submits at the first newline, long prompts drop Enter, and image paste and file drag-and-drop fail.
- Links:
  - #4251 "Windows: multi-line paste into a Claude Code pane submits at the first newline" (closed, **C23**)
  - #3187 "Windows agent prompt drops Enter after long input" (closed, R2 C18)
  - #4314 "0.9.1 Windows input rework breaks empty-bracketed-paste decode: clipboard image bridge" (closed, R2 C22)
  - #986 "Local clipboard image paste is broken after Ctrl+V was unbound" (closed, C8)
  - #4315 "Files dropped onto a pane do not insert their path (macOS, local session)" (open, C2)
- Quote (#4251): "Pasting two or more lines with Ctrl+V into a [Claude Code] pane submits at the first newline."
- Principle: error prevention; consistency with host terminal; efficiency.
- Hutan should: use bracketed paste end-to-end. Never submit on paste. Support drag-and-drop file paths and image paste with a clear, visible fallback message.

## 18. Felt latency and flicker (chrome that moves while idle)

- Problem: the cursor flickers across the whole UI while agents work, and many idle agent panes burn CPU, making typing and the UI feel slow.
- Links:
  - #930 "Cursor flickers rapidly across the whole UI while any agent is working" (closed, C11)
  - #967 "Focused-pane cursor flickers whenever a sidebar agent spinner is animating" (closed, C8)
  - #4506 "Server burns 10+ CPU cores with many populated agent panes" (closed, R2 C18): "plain CLI/API requests over the local unix socket lag: measured p95 latency 90-450 ms"
  - #3217 "Running multiple agents simultaneously causes severe lag, high resource usage, and noisy fans on macOS" (open, R1 C8)
  - #4884 "Windows: ordinary PowerShell typing feels delayed compared with Windows Terminal outside Herdr" (closed, C8)
  - #3745 "terminal popup typing over SSH is slower and sends ~234 KB per key" (closed, R5 C5)
- Principle: aesthetic and minimalist design (no motion competing with focus); response time perception.
- Hutan should: keep the chrome still unless state changes. Animate only the focused element. Budget CPU per idle agent. Measure keystroke-to-paint latency in CI for the terminal.

---

## The 3 most important lessons for Hutan

1. **Make agent state a trustworthy contract, and gate input on it.** Blocked-on-approval must be the most visible state (#3871, #846, #4573, #5263). Input to a pane that may be at a prompt must not auto-approve (#5263). Mark "unknown" honestly instead of showing a confident "idle".
2. **Build attention as a first-class queue.** Sort blocked first, support manual read/unread (discussion #864, 41 upvotes; #318), deliver one notification per event that focuses its pane on click (#1827, #4787, #5377, discussion #625), and let users mute per agent.
3. **Harden the input and feedback layer.** One text-input component with readline keys and paste (#1803, #302, PR #3698). Modes that always show and always exit (#5255, #5142, #4917). Success toasts only after the clipboard write is verified (#2399, #4341). Vim-grade copy mode with working repeat (#2371, #3812).

Scope note: this is a sample of the 800 most recent discussions and the full issue corpus. The most-reacted items are modest (≤18 reactions), so these pain points are weighted by comment volume and upvotes. Items marked open may have partial fixes in later releases. `ux-changing` labels were not applied to any fetched item.
