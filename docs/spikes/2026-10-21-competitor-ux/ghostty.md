# Ghostty — UX/UI pain points (for Hutan)

Source: `ghostty-org/ghostty` issues + discussions (base URLs: `https://github.com/ghostty-org/ghostty/issues/N`, `.../discussions/N`). Reactions = total reactions (+N) on the issue; discussions show ▲ upvotes; c = comment count. Data pulled 2026-10-20. Quotes are verbatim from issue/discussion bodies or comments; paraphrases are unquoted.

## Method (reproducible)
- `gh issue list -R ghostty-org/ghostty --state all --label <l> --limit 300` for labels `gui`, `os/macos`, `input`, `feature-design` (with reactionGroups and comments via `gh issue view --json`). `os/macos` hit the 300 cap, so it may be truncated.
- `gh api -X GET search/issues -f q="repo:ghostty-org/ghostty <term>" -f sort=reactions` for: keybind, quick terminal, split, tab, vim, notification, accessibility, settings, toolbar, confusing, discoverable, unintuitive, tab bar, focus. The REST search quota ran out mid-run (403); later terms ran after the reset, so coverage is uneven.
- GraphQL `discussions(orderBy: UPDATED_AT)` and `discussion(number:)` for ▲ counts and comments.
- Not searched: Discord, Reddit, the tip changelog. One web search gave little beyond GitHub.
- Process note (not product UX): `ghostty-vouch` closed #13503 because non-maintainers must open a discussion first.

## Pain points (ranked by evidence)

**1. Split drag handles cover content and steal clicks.**
- Discussion #10108 (▲29, c9, 2025-12-30, open): "the split handles often appear while changing active split by moving the mouse pointer between splits."
- Quote: "it is now hard or impossible ... to click or select text on the topmost line of a split by mouse."
- PR #10280 (merged 2026-02-23, +4, c14): tristan957: "I am more in favor of holding down a modifier while hovering the grab handles."
- Principle: don't obstruct content; reveal affordances on intent (modifier or dwell).
- Hutan should: show split grab handles only on a modifier or after a short hover delay, and keep the top text row clickable.

**2. Dragging a split divider changes focus instead of resizing.**
- Discussion #11903 (▲8, c5, 2026-03-27, open): drag to resize sometimes "only changes focus."
- Quote (nicpaimnt): "It takes often 3 click drag to finally get the pane to resize."
- Principle: predictable direct manipulation; one gesture, one result.
- Hutan should: a press on a divider starts a resize even if the adjacent pane is not focused. Focus changes on click only when the press is not on a divider.

**3. Split navigation directions differ across platforms and between actions.**
- #2866 (+33, c13, closed 2025-02-15): goto_split focus differs between macOS and GTK for the same layout.
- #3237 (+26, closed 2025-01-02): `new_split` takes `up|down|left|right`, `goto_split` takes `previous|next|top|left|bottom|right`.
- Principle: consistency and predictability.
- Hutan should: one direction vocabulary for all pane actions, identical focus semantics on every platform, and documented wrap behavior.

**4. Active split and zoom state are hard to see; no visible mouse path for split controls.**
- #353 (2023, closed, c10): visual indicator for the focused split, asked for in tmux terms.
- #1541 (+15, c7, open since 2024-02-20): quote: "I sometimes find myself needing/reaching for buttons in a terminal." The request is a hover toolbar with the path, controls and zoomed state.
- #13503 (0 reactions, c1, closed 2026-07-28 by the vouch bot): one-click toolbar buttons (Tilix comparison) vs. the hamburger/right-click menu. Quote: "the one thing I actively miss."
- Principle: visibility of system status; recognition over recall.
- Hutan should: always show which pane is focused and which is zoomed, and give a visible control for split, close and zoom.

**5. Standard text shortcuts are swallowed in Ghostty's own text fields.**
- #10749 (+85, c28, open since 2026-02-16): Cmd+C and Cmd+X fail in the tab-rename and search fields. Title: "Not possible to cmd+c/cmd+x from rename tab, search, other text fields."
- Maintainer root cause (2026-03-17): "We strip the keyboard shortcuts from the menu during startup because our reverse lookup on keyboard shortcuts ignores performable bindings."
- Related: #11253 (+7, c9, closed 2026-04-04): Cmd+G in the search field consumed by the text view. #12512 (+9, open): search bar covers the active match (discussion #9790).
- Principle: platform text-editing conventions; focus-scoped shortcuts.
- Hutan should: every text input (rename, search, Mawas prompt, command palette) inherits native editing shortcuts. Test copy, cut, paste, undo, select-all and find-next in each.

**6. Tab title and colour are missing or awkward.**
- #2509 (+76, c44, closed 2025-02-14): iTerm2-style tab title and colour; commenter: "makes it a lot easier to switch to the right tabs."
- #4217 (+51, c20, merged 2025-02-14) added rename via a modal. qwerasd205 asked for "a popover that drops down from the tab title." francescarpi asked for "double-clicking the tab title."
- #13415 (closed 2026-07-22): tab label and hover tooltip both come from the terminal title (OSC 0/2).
- Principle: user control; naming supports multi-context work.
- Hutan should: inline rename (double-click or popover) and per-session colour. Let label and tooltip differ (e.g., cwd on hover).

**7. Quick terminal: no tabs, poor default sizing, and it doesn't follow the user across Spaces.**
- #9857 (+246, c54, open since 2025-12-10): "feat(macos): add tab support for QuickTerminal - continued."
- #2384 (+107, c55, closed 2025-08-26): default height is "1/4 the height of the screen"; maintainer: "mixing units feels a bit unintuitive."
- #2721 (+26, c10, closed 2025-01-08): quick terminal doesn't appear over fullscreen apps. Reporter notes iTerm2 switches Space: "We should follow suit."
- Principle: fit the user's mental model (iTerm2 parity); sensible, configurable defaults.
- Hutan should: support tabs and sensible sizing in the quick panel, and show it on the current Space, including over fullscreen apps.

**8. Global hotkeys silently stop working.**
- #7215 (+16, c14, closed 2025-05-29): global hotkey for Quick Terminal stopped working (regression on tip).
- #11883 (+5, c8, closed 2026-03-27): global keybind stops working after a few hours of use.
- #12294 (+9, c11, closed 2026-04-22): global keybind works inside the app but not outside.
- Principle: reliability is UX; failure must be visible.
- Hutan should: detect a dead global event tap, re-arm it, and show a visible warning in the UI when it can't.

**9. Keybindings are hard to discover; sequenced/table modes are invisible.**
- #2127 (closed 2025-12-23): "we should investigate UI improvements to make it clear that you are in a sequenced keybind."
- #9995 (closed 2025-12-23): "Our `+list-keybinds` CLI doesn't currently list any key tables."
- #3203 (+37, c4, closed 2024-12-30): an auto-created empty config "doesn't leave you much of a path forward." Commenter jamie: "Came here searching for a listing of default keybinds, that tip at the bottom is gold all on its own."
- Principle: discoverability; visibility of system status.
- Hutan should: show the active key table or pending sequence in the status area; list all bindings in the command palette and CLI; generate a self-documenting default config.

**10. Defaults and keyboard-layer behavior surprise users.**
- #3899 (+17, c29, closed 2025-11-14): Cmd+Shift+Plus inserts a literal "+" instead of increasing font size. Discussion #3888.
- #14716 (▲1, c1, open, 2026-10-15, discussion): Ctrl+[, Ctrl+I and Ctrl+M are sent as CSI-u by default and break zsh, bash and less (paraphrase).
- #6765 (+13, c2, open): `performable:` scroll bindings should not fire in alt-screen apps such as nano.
- #4335 (+17, c9, open since 2025-01-01): modifier-only triggers (e.g., double-tap Ctrl for the quick terminal) are not supported. Maintainer: bindings "process on _key press_."
- #5160 (+106, c17, closed 2026-01-08): `key-remap` for app-level modifier remapping; users wanted it instead of Karabiner.
- Principle: match platform conventions; user control of keymaps.
- Hutan should: ship macOS-native defaults, test defaults inside shells and editors, and plan a remap layer and modifier-only triggers in the keymap design now.

**11. Tab bar and window chrome break after OS updates and theme reloads.**
- Discussion #14244 (▲52, c9, 2026-09-15, closed): on macOS 27, tabs are displaced to the upper right and stack on each other (paraphrase). billimek (▲8): "What fixed it for me was to switch from `macos-titlebar-style = "tabs"` to `"native"`." dancrossnyc (▲8): "This is fixed in the tip builds, but sadly, not in a stable release build."
- #13070 (+15, c30, created 2026-06-22, closed): tabs style checked against macOS 27.
- #3354 (+34, c8, open since 2024-12-27): after a config reload in dark mode, "the tab bar gets stuck in light mode for all newly-created windows and when new tabs are added."
- Principle: native conformance; state must stay in sync.
- Hutan should: stay on native chrome, test against OS betas before release, and make theme reloads update every window and tab bar.

**12. Tabs vs windows: the mental model is unclear.**
- Discussion #12572 (▲1, created 2026-05-03, closed): "Tabs vs windows distinction is not clear on macOS, and appears to be implemented as a hybrid." Native tabs show as separate windows in the Window menu.
- #4329 (+18, c12, closed 2025-01-10): toggle-visibility turns one window's tabs into separate windows.
- #3219 (+24, c8, closed 2024-12-30): quote: "creates a non-interactive 'ghost window'."
- Principle: clear mental model (sessions, tabs, windows).
- Hutan should: show the session/tab/window hierarchy explicitly in menus and the window list, and never create phantom windows when hiding or showing.

**13. Attention and notifications stay stale.**
- #8487 (+26, c5, closed 2026-02-26): quote: "isn't cleared unless you first defocus Ghostty and then refocus." The app-icon badge stays after the user reads the window.
- #9145 (+4, c2, closed 2025-10-11): clicking a notification no longer focuses the originating window; reporter's regression timing suggests the GTK rewrite [INFERENCE].
- Discussion #13952 (▲1, c4, 2026-08-21): GNOME notifications are "**never withdrawn**."
- Discussion #12786 (▲2, c2, 2026-05-23): notification count stuck at 10.
- #4800 (+51, c16, closed 2025-06-27): `copy-on-select = clipboard` causes "Copied to clipboard" toast spam.
- Principle: attention management; visibility of status; don't interrupt.
- Hutan should: clear the attention badge when the user sees the surface, make "needs input" notifications click-through and dismissible, and rate-limit transient toasts.

**14. Appearance and transparency are slow to change.**
- #809 (+17, c17, closed 2024-10-02): dark/light theme sync to the system was requested 2023-11-03 and closed about 11 months later.
- #8282 (+9, c5, closed 2025-10-26): config reload with light/dark themes is inconsistent with the system theme (repro: `theme=light:tokyonight-day,dark:tokyonight`).
- #5047 (+131, c9, open since 2025-01-14): `toggle_background_opacity` keybind. Requester: "I would like to be able to easily turn off transparency" (for screen sharing).
- Discussion #14661 (▲1, c1, 2026-10-12): command palette ignores `window-theme = ghostty`.
- Principle: quick, context-driven appearance changes.
- Hutan should: follow the system light/dark mode with paired themes, and offer a one-key toggle for transparency.

**15. Missing core features users expected from day one.**
- #189 (+1221, c110, closed 2025-11-30): GTK search scrollback, open since 2023-07-06. The macOS search GUI #9709 (+167, c5) was merged 2025-11-26.
- #111 (+179, c19, closed 2025-10-17): scrollbar. Maintainer (2023): "I don't think we're going to support GUI elements like scrollbars in systems such as GLFW." Shipped on macOS via #9232 (+57, merged 2025-10-16).
- #1972 (+230, c71, closed 2026-02-15): cmd+click on file paths, the iTerm2 behavior users miss. Discussion #9546 (▲31, c7): commenter BrennerSpear, "I can't command-click file names in Ghostty but i can in iTerm2."
- #1847 (+125, c31, open since 2024-06-08): scrollback in state restore. Quote: "the single missing feature that is blocking me from jumping fully to ghostty as my daily driver."
- Discussion #10478 (▲59, c17): pasting a screenshot from the clipboard into the terminal does not work (paraphrase).
- Principle: meet the baseline users bring from other tools.
- Hutan should: ship find, scrollback, clickable paths, image paste and session restore early.

**Other notes (below top 15):**
- #4233 (+36, c12, closed 2025-02-11): the window does not remember its last size and position: "like most other MacOS apps."
- #10092 (+52, c2, open since 2025-12-29): drag a split into the tab bar to promote it to a tab.
- #1525 (+91, c8, closed 2026-01-08; superseded by #10224): reorder splits.
- #8208 (+74, c11, closed 2026-03-04): split create/close "flicker."
- #11319 (+46, c11, closed 2026-03-10): mouse-move text selection bug (title only; body not read).
- #14734 (▲1, c1, 2026-10-16): docs say a background-opacity change requires restarting Ghostty, but it reloads; the docs drift from behavior.
- #14774 (▲4, 2026-10-20): the bundled Vim/Neovim config plugin is undocumented; reporter found it after two years.

## Three most important lessons for Hutan
1. Route focus and keyboard input through one layer that every text field and pane respects. The most-reacted open problems are focus and input: #10749 (+85, open since Feb 2026), #11903 (focus steals resize), #2866 (split focus differs by platform).
2. Keep chrome out of the content and make state visible: split focus and zoom indicators, key-table status, notification clearing, theme sync. Ghostty's hover handles (#10108, ▲29) and invisible state (#2127, #8487, #3354) are the pattern to avoid.
3. Ship the expected baseline and keep global entry points reliable: search, scrollbar, clickable paths, quick-panel tabs, and a global hotkey that doesn't die (#189 +1221, #111 +179, #1972 +230, #9857 +246, #11883 +5, #12294 +9).
