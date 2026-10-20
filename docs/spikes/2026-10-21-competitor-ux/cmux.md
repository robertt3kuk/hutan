# cmux: UX/UI pain points from user reports

Repo: https://github.com/manaflow-ai/cmux (verified; description: "Open source Ghostty-based macOS terminal with vertical tabs and notifications for AI coding agents"; ~28.3k stars; default branch main).

## Method (reproducible)
- Search: `gh search issues --repo manaflow-ai/cmux --sort reactions --limit 50 '<q>'` for `ux`, `ui`, `confusing` worked; later queries (`keyboard`, `focus`, `layout`, `panel`) returned HTTP 403 rate-limit. Fallback: `gh api --paginate 'repos/manaflow-ai/cmux/issues?state=all&per_page=100'` (core REST API), PRs filtered out, 4,994 non-PR issues ranked locally by `reactions.total_count` and `comments`. The REST `sort=reactions` parameter was ignored, so sorting was local.
- Reaction counts below are `reactions.total_count` (with `+1` where it differs) and `comments`, read 2026-10-20.
- Labels (`gh label list -R manaflow-ai/cmux`): no UX-specific label; area labels `area: sidebar|notifications|input|layout|agents|settings|appearance|accessibility|workspaces|terminal|performance|remote|browser`, plus `S1–S4` severity and `help wanted`. Area tallies are auto-triage labels, indicative only: `area: agents` 423 issues, `area: layout` 290, `area: workspaces` 326, `area: terminal` 194 (556 reactions), `area: sidebar` 187, `area: notifications` 85.
- Excluded: CI/nightly/bot and loader-filed issues (e.g., #13742 RFC, #13095, #14440, #18868), platform asks (Linux #330, Windows #1012, Intel #293), pricing (#542), cloud internals.
- Caveat: the most-reacted UX issues date from Feb–May 2026. Sep–Oct 2026 items are mostly internal reports with 0 reactions; they are cited only as corroboration and marked thin.

## Top pain points

### 1. Sidebar agent state ("Running" / "Needs Input") is wrong or stale
- Issue: https://github.com/manaflow-ai/cmux/issues/1027 (open; 44 reactions, all +1; 8 comments; 2026-03-06)
- Quote: "Shows 'Running' when not actually running… 'Needs Input' doesn't show up when it should."
- Comments: "Could be nice to have a button to clear the indicators"; "This is pushing me away from cmux."
- Corroboration: https://github.com/manaflow-ai/cmux/issues/16212 (finished sessions flip to needs_input; 0 reactions, 2 comments, thin); https://github.com/manaflow-ai/cmux/issues/2322 (Claude notifications inconsistent; 22 reactions, 2 comments).
- Principle violated: visibility of system status; trust in status indicators.
- Hutan should: derive agent state from explicit, ordered lifecycle events; reconcile out-of-order or missing events on stop and prompt; offer a visible "clear/acknowledge" action; show which event produced the current state.

### 2. Attention noise: notifications for non-events
- Issues: https://github.com/manaflow-ai/cmux/issues/8333 (open; Codex "Automatic approval review approved" dings on every auto-approved review; 10 reactions, 4 comments); https://github.com/manaflow-ai/cmux/issues/10017 (closed; notification swarm on Codex auto-approve; 7 reactions, 3 comments); https://github.com/manaflow-ai/cmux/issues/3602 (closed; request to suppress `idle_prompt` notifications; 11 reactions, 4 comments); https://github.com/manaflow-ai/cmux/issues/1027 (subagent waiting shown as needing attention).
- Principle violated: aesthetic and minimalist design; interruptions must earn attention.
- Hutan should: notify only when the agent is blocked on the user; coalesce repeats per session; offer per-event toggles (idle, auto-approved, subagent wait, job done).

### 3. Notifications fire for the workspace you are already looking at
- Issue: https://github.com/manaflow-ai/cmux/issues/942 (closed 2026-09-24; 15 reactions, 3 comments): sound and tab-border flash still fire for the focused workspace.
- Related: https://github.com/manaflow-ai/cmux/issues/3126 (suppress when window focused; 6 reactions, 1 comment); https://github.com/manaflow-ai/cmux/issues/2038 (per-workspace mute; 4 reactions, 3 comments).
- Principle violated: context-aware feedback; user control.
- Hutan should: suppress sound, badge and flash for the focused surface by default; provide per-workspace mute.

### 4. Silent change of agent permission mode (trust)
- Issue: https://github.com/manaflow-ai/cmux/issues/3547 (closed 2026-05-05; 27 reactions, all +1; 20 comments). Title: "Latest update forces Claude bypass permissions".
- Quotes from comments: "Agreed this seems alarming to silently add this permission access"; "cmux shouldn't tamper with our claude execution at all."
- A commenter quotes the wrapper source (`Resources/bin/claude`) that exposes bypass as an option "without enabling it by default"; the complaint is that it was enabled anyway.
- Related: https://github.com/manaflow-ai/cmux/issues/8136 (open; S1; 21 reactions, 19 +1, 5 comments): "--dangerously-bypass-hook-trust applies to the entire Codex invocation."
- Principle violated: consent and least surprise for safety-critical settings; error prevention.
- Hutan should: never change an agent's permission, approval or trust mode without explicit opt-in; show the active mode in the agent chrome; surface wrapper-injected flags in the UI, not only in release notes.

### 5. Drag-and-drop of files and images into agent terminals
- Issues: https://github.com/manaflow-ai/cmux/issues/3435 (closed; 22 reactions, 21 comments): "Drops are silently ignored - no path inserted, no error shown." https://github.com/manaflow-ai/cmux/issues/3615 (closed; 13 reactions, 11 comments): a drop opens a file-preview panel instead of attaching. https://github.com/manaflow-ai/cmux/issues/3707 (closed; 13 reactions, 7 comments): request for a setting to disable sidebar preview on drag, since Opt/Cmd do not bypass it. https://github.com/manaflow-ai/cmux/issues/3639 (15 reactions, 14 comments): "Cmux image drag and drop not working." https://github.com/manaflow-ai/cmux/issues/3647 (4 reactions, 14 comments).
- Principle violated: predictable direct manipulation; visible drop targets; no silent failure. The same symptom was reported repeatedly.
- Hutan should: highlight the drop target and its action before the drop (insert path or open preview); never swallow a drop silently; offer a setting for drop behavior; add a regression test per drop target.

### 6. Session continuity: layout restored, live work lost on quit
- Issues: https://github.com/manaflow-ai/cmux/issues/480 (closed 2026-09-24; 50 reactions, all +1 reported; 16 comments): cmux "does not persist session information," and updates feel destructive. https://github.com/manaflow-ai/cmux/issues/1663 (open; 27 reactions, 10 comments): running Claude Code and SSH sessions are gone after Cmd+Q; "this is the single biggest gap." https://github.com/manaflow-ai/cmux/issues/2823 (open; 11 reactions, 18 comments): "Sessions are lost after app restart." https://github.com/manaflow-ai/cmux/issues/3342 (open; 2 reactions, 5 comments; help wanted): agent processes gone after crash or relaunch. https://github.com/manaflow-ai/cmux/issues/3499 (Codex session not resumed; 5 reactions, 1 comment).
- Principle violated: recognition over recall; users assume quitting is reversible.
- Hutan should: show per-agent resume status after relaunch (resumable, needs restart, lost); keep resume IDs visible; restore workspaces one at a time to avoid load spikes.

### 7. Pane, tab and workspace model is confusing
- Issues: https://github.com/manaflow-ai/cmux/issues/3297 (closed 2026-09-26; 25 reactions, 8 comments): "Ultimately it just feels backwards -- tabs should CONTAIN the splits." https://github.com/manaflow-ai/cmux/issues/1362 (open; 19 reactions, 6 comments): Ghostty-style top tabs that each own a split layout. https://github.com/manaflow-ai/cmux/issues/3306 (open; 1 comment): the term "Tab" means a workspace for `newTab` but a surface for `closeTab`. https://github.com/manaflow-ai/cmux/issues/1083 (closed; 13 reactions, 3 comments): single-tab pane headers waste vertical space.
- Principle violated: consistent naming; match between system and the user's mental model.
- Hutan should: fix one hierarchy (workspace > tab > pane) and use one name for each level across UI, CLI and shortcuts; let a tab own a split layout; offer a zoom-pane toggle that returns to the layout without losing splits.

### 8. Keyboard layouts and shortcuts that eat text
- Issues: https://github.com/manaflow-ai/cmux/issues/1653 (closed 2026-06-04; 9 reactions, 13 comments): on a Turkish layout, option+q produces "@" but cmux captures it as a shortcut. https://github.com/manaflow-ai/cmux/issues/16235 (Cmd+= does nothing on Dvorak/German; 2 reactions, 2 comments; thin). https://github.com/manaflow-ai/cmux/issues/16236 (Cmd+§ on ISO keyboards; thin). https://github.com/manaflow-ai/cmux/issues/3362 (open; Cmd+ä closes a tab and Cmd+W opens Settings on an alternate layout; 5 reactions, 4 comments). https://github.com/manaflow-ai/cmux/issues/1790 (IME shortcut failures; 9 reactions, 1 comment).
- Principle violated: respect the user's keyboard; printable input must never be eaten.
- Hutan should: match shortcuts by produced character and physical key; never intercept option combinations that produce text in the active layout; test with Dvorak, German, Turkish, ISO and an IME.

### 9. Keymaps are fixed, clash with terminal apps, and lack pane navigation
- Issues: https://github.com/manaflow-ai/cmux/issues/135 (closed 2026-10-02; 31 reactions, 15 comments): "Ctrl-Cmd-] is too hard to press." https://github.com/manaflow-ai/cmux/issues/1756 (closed 2026-09-13; 20 reactions, 4 comments): "No keybindings exist to resize panes. Only mouse dragging works." https://github.com/manaflow-ai/cmux/issues/2076 (closed; 13 reactions, 7 comments): keyboard focus between panes and tabs. https://github.com/manaflow-ai/cmux/issues/2772 (open; 10 reactions, 2 comments; S3): the same Ctrl+h/j/k/l used by cmux and Neovim/tmux navigation. https://github.com/manaflow-ai/cmux/issues/1714 (unbind a key so it reaches the terminal; 4 reactions).
- Principle violated: flexibility and efficiency; keyboard-first operation; coexistence with vim, which matters directly for Hutan's planned vim mode.
- Hutan should: ship fully remappable bindings with conflict detection; offer a vim-aware navigation mode that passes keys to an alt-screen app (nvim, tmux) when it has focus; document every default.

### 10. Scroll position and reading while agents stream
- Issues: https://github.com/manaflow-ai/cmux/issues/945 (closed 2026-10-02; 16 reactions, 5 comments): "Switching tabs always resets the scroll position to the bottom." https://github.com/manaflow-ai/cmux/issues/1847 (closed; 8 reactions, 1 comment): output keeps pulling the view to the bottom while the user is reading. https://github.com/manaflow-ai/cmux/issues/2504 (closed; 6 reactions, 1 comment): scroll position jumps to bottom during agent output. https://github.com/manaflow-ai/cmux/issues/2334 (open; 20 reactions, 6 comments): alternate-screen history is lost; the iTerm2 behavior "Save lines to scrollback in alternate screen mode" is requested.
- Principle violated: user control and freedom; preserve context.
- Hutan should: keep per-pane scroll state; never move the viewport while the user is scrolled up; keep scrollback for TUI agents, or offer a transcript view.

### 11. Copy and paste fidelity
- Issues: https://github.com/manaflow-ai/cmux/issues/3096 (closed 2026-09-30; 17 reactions, 6 comments): copying a soft-wrapped command inserts hard newlines, so a pasted command can run partially. https://github.com/manaflow-ai/cmux/issues/17472 (closed; 5 comments; thin): tmux-mirror selection does not reach the clipboard.
- Principle violated: predictable output; error prevention, since a paste can run commands.
- Hutan should: copy logical lines rather than visual wraps; keep copy and paste identical across local, SSH and tmux panes.

### 12. Perceived latency, memory growth and freezes
- Issues: https://github.com/manaflow-ai/cmux/issues/1519 (open; 13 reactions, 3 comments; S2): typing latency about 125 ms against about 80 ms for Ghostty. https://github.com/manaflow-ai/cmux/issues/4681 (closed 2026-07-15; 15 reactions, 5 comments): "cmux + ssh + tmux is seriously laggy while Ghostty + ssh + tmux is not." https://github.com/manaflow-ai/cmux/issues/4529 (closed; 22 reactions, 25 comments): memory growth to system-freezing levels. https://github.com/manaflow-ai/cmux/issues/4520 (closed; 13 reactions, 12 comments): memory climbs to several GB within minutes. https://github.com/manaflow-ai/cmux/issues/2871 (closed; 8 reactions, 11 comments): 70+ GB on a 16 GB Mac. https://github.com/manaflow-ai/cmux/issues/2962 (open; 10 reactions, 17 comments; 2026-04-17): the 70+ GB problem persists after #2871 was closed. https://github.com/manaflow-ai/cmux/issues/16325 (open; 0 reactions, 6 comments; thin): workspace switching slows to 2.5–4 s after about a day of uptime. https://github.com/manaflow-ai/cmux/issues/2586 (open; 1 reaction, 15 comments; S2; thin): the sidebar layout loop pins the main thread at 100% CPU.
- Principle violated: responsiveness is part of the UI; latency is felt before it is measured.
- Hutan should: publish a key-to-pixel latency budget and compare it against plain Ghostty; cap memory per surface; add a soak test for multi-day agent sessions.

### 13. Sidebar and chrome polish; panel toggles
- Issues: https://github.com/manaflow-ai/cmux/issues/3241 (closed 2026-06-11; 7 reactions, 27 comments): the scrollbar track is always drawn, and "looks broke." https://github.com/manaflow-ai/cmux/issues/3552 (closed; 9 reactions, 4 comments): no shortcut to hide the right sidebar; Cmd+Shift+E only focuses it. https://github.com/manaflow-ai/cmux/issues/2919 (open; 26 reactions, 5 comments): configurable sidebar position. https://github.com/manaflow-ai/cmux/issues/16282 (open; 0 reactions, 2 comments; thin): the repo-changes panel's close "x" does not work and presence differs across windows. https://github.com/manaflow-ai/cmux/issues/16341 (closed; 0 reactions, 4 comments; thin): Update popover clips text and buttons.
- Principle violated: reversibility; consistency across windows; visibility of controls.
- Hutan should: give every panel a visible close control and a toggle shortcut; keep panel state explicit per window; offer sidebar position as a setting.

### 14. Opening files and links goes to the wrong place
- Issues: https://github.com/manaflow-ai/cmux/issues/1544 (closed 2026-09-29; 24 reactions, 6 comments): file paths are treated like URLs and opened in the browser. https://github.com/manaflow-ai/cmux/issues/719 (open; 27 reactions, 14 comments): "Open in browser" behavior. https://github.com/manaflow-ai/cmux/issues/1778 (open; 23 reactions, 2 comments): route `.md` files to a viewer.
- Principle violated: match between intent and result.
- Hutan should: route local paths to the user's editor (with `:line` support) and URLs to a browser; make the rule visible and configurable.

### 15. Settings are hard to find and hard to read
- Issues: https://github.com/manaflow-ai/cmux/issues/964 (open; 5 comments): "every setting from every category in a single flat list"; asks for category navigation and search. https://github.com/manaflow-ai/cmux/issues/2094 (open; 2 reactions, 2 comments): users do not know where the Ghostty config file is or which settings exist. https://github.com/manaflow-ai/cmux/issues/3862 (open; 13 reactions, 4 comments): global UI font size for sidebar and file browser. https://github.com/manaflow-ai/cmux/issues/3617 (open; 2 reactions, 9 comments): the Sign In sheet "closes immediately" with no error shown. https://github.com/manaflow-ai/cmux/issues/3306 (terminology; see #7).
- Principle violated: recognition over recall; visibility of errors and help.
- Hutan should: ship a searchable, categorized settings page; show the config file path next to the settings; surface errors inline, not as a flash.

### 16. Reviewing what agents changed
- Issues: https://github.com/manaflow-ai/cmux/issues/609 (open; 16 reactions, 6 comments): "reviewing the changes each agent has made requires opening the isolated workspace for each task individually." https://github.com/manaflow-ai/cmux/issues/678 (open; 5 reactions, 1 comment): per-session diff panel.
- Principle violated: efficiency; review is where multi-agent work bottlenecks.
- Hutan should: show a per-task diff with accept, revert and follow-up actions in the same surface as the agent chat.

### Honorable mention: blank panes after a split or a notification
- Issues: https://github.com/manaflow-ai/cmux/issues/2774 (closed; 6 reactions, 3 comments): a new pane stays blank until the window focus changes. https://github.com/manaflow-ai/cmux/issues/3080 (open; 11 reactions, 11 comments): the terminal goes blank when a CLI notification fires. https://github.com/manaflow-ai/cmux/issues/2998 (closed; 9 reactions): a new tab renders blank.
- Principle violated: visibility of system status; reliability of the main surface.

## Three most important lessons for Hutan
1. **Make agent status trustworthy and quiet.** The most-reacted live UX complaint is wrong Running / Needs Input state (#1027, 44 reactions), followed by notification noise (#8333, #3602, #10017). Derive state from ordered lifecycle events, notify only when the user is blocked, and give per-event control.
2. **Never change an agent's safety settings silently.** #3547 (27 reactions, 20 comments of alarm) and the S1 issue #8136 show that a wrapper changing permission or trust mode costs more trust than any single bug. Make permission mode explicit, opt-in and visible.
3. **Protect the user's input and their place.** Layout-dependent shortcuts (#1653, #16235, #3362), clashes with vim and tmux (#2772, #135), drag-and-drop that fails silently (#3435, 21 comments), and scroll jumps (#945, #1847) are the daily friction. Build layout-aware shortcuts with passthrough, explicit drop targets and per-pane scroll state, and test them with non-US layouts and Neovim.

## Notes on evidence strength
- Strong (many reactions and comments): #1027, #3547, #3435, #480, #135, #1663, #2823, #3297, #1544, #719, #4529, #2962.
- Thin (0–2 reactions, or internal reports): #16212, #16282, #16325, #16341, #16235, #16236, #2586, #17472. Included only as corroboration.
- [INFERENCE] The "Hutan should" lessons are my design reading of the complaints, not stated by cmux users.
