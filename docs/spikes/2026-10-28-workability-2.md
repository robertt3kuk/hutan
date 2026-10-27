# Workability of Hutan 0.3.0 (master `6ccc205c`), second evaluation (2026-10-28)

**Question (user, 2026-10-27):** "scroll must work, things must fit the window, evaluate how workable Hutan is, there's no file explorer." This repeats `2026-10-27-workability.md` so the scores compare.

**Build and method.** `just bundle --release` of `slice/E2E7` (`hutan 0.3.0`, 7 m 07 s; the `target/bundle` that was lying in the worktree was again a stale 0.2.0 build), copied to `~/hutan-scratch/e2e7/Hutan-release.app`, started as `HUTAN_HOME=~/hutan-scratch/e2e7/home <bundle>/Contents/MacOS/hutan-desktop &` (history index and native notifications off). Driven with Cua Driver (foreground clicks/keys/drags/wheel, `set_window_frame`, `invoke_menu`). Repos: scratch clone of ripgrep (branch `e2e6-test`), copy of `book` (2 653 files), and `cyr` (generated: 60 modified + 40 new `.md` files, 120–200-character Cyrillic lines, tables; 100 changed files). One OMP chat on **Haiku 5.5** (two turns: an edit of `messages.rs`, then "1 to 60 one per line"). Input source was Russian: switched to ABC for the run and restored. Screenshots: `~/hutan-scratch/e2e7/shots/NN.png` (cited as `#NN`). The screen did not lock.

**Measured.** Desktop RSS 149 MB (was 80 MB; this run held 3 projects, 32 open files, a 5 200-file folder, a 3 000-line terminal), daemon 41 MB, 0.1 % / 0.0 % CPU idle. Expanding a 5 200-file folder: instant (`Showing the first 5 000 — filter in this folder` row, #97); trashing it: about 1 s, ⌘Z brought back all 5 200 files. Palette, ⌘P, project search (1 303 matches in 309 files, stopped at 1 000) all answered within the 0.7 s wait. Models Hutan lists for OMP equal `omp models` (28 Anthropic + `apple/on-device`, default Opus 5.5; `hutan chat models --agent omp` identical). Daemon log not inspected beyond the run.

## Scores (1–5), before → after

| Area | Before → After | One line |
|---|---|---|
| Navigation | 4 → **5** | ⌘P takes spaces (`ch17 oop`), recents, reveal in tree; scrollbars tell where you are. Nit: `src/…` ranks `second-edition/src` first (#39). |
| File explorer | 2 → **4** | Create/rename/duplicate/move (drag, with a once-per-session confirm)/trash/undo, multi-select menu, Compare, filter, Open Editors, Changed by agents, cap row all work (#07–#22, #94–#101). Loses a point for the closed-on-first-run dock, a filter that ranks `glue.rs` last, a tree that shifts under the pointer. |
| Editor | 3 → **4** | Soft wrap on Markdown, git gutter (added/modified/removed, live while typing) with peek (*Stage/Revert hunk*), scrollbar with drag to the end, 32-cell blame. Blame is still mostly blank and a dismissed LSP bar leaves a blank band. |
| Changes / review | 3 → **4** | 100 changed files: name-first rows, wrapped Cyrillic diff, gutter and tint stay on sideways scroll, scrollbars, header wraps in a narrow pane. The list is ~11 rows next to the diff at 1280×800; a toolbar button clips at 325 px. |
| Terminal | 4 → **5** | ⌘↑/⌘↓ top/bottom, wheel, ⇧PageUp, scrollbar drag, 3 000 lines to the very end (#60–#64). |
| Agents | 4 → **4** | Haiku start → edit → card → *Changed by agents* row → gutter mark in one flow; no false "another agent works here" warning; Agents page shows real state + usage. Narrow chat header drops the model name. |
| Mawas | 3 → **3** | Not exercised with a model again (default would start on Opus); provisional. |
| Settings | 4 → **4** | Nav and Keyboard page scroll to the last row unclipped (#68, #69); Scroll bars setting added. |
| Layout / resize | 3 → **4** | 1440×900, 1280×800, 1100, 900 (rail peek works), 800×560, full screen all fit; right dock drags; palette no longer leaks the wheel. Left dock width did not drag; first-run right dock closed. |

**Verdict.** The first report's "one explorer, one Changes layout and a few scroll affordances away from workable" has been delivered: no P0 or P1 was found. What is left is friction a daily user feels in the explorer (placement on first run, filter ranking, shifting rows) and a few clipped controls in narrow panes.

## F-01 … F-20 status

| ID | Status | Evidence |
|---|---|---|
| F-01 explorer cannot change disk | **fixed** | New file/folder, F2 rename (name without extension selected), ⌘D duplicate, drag move with confirm, ⌘⌫ with confirm, ⌘Z restores (also a 5 200-file folder), open tab follows rename/move, *deleted on disk* bar on trash; menu has Reveal in Finder, Open in Terminal, Compare, Find in Folder, Copy (relative) path (#07–#20, #99–#101, #105, #106). |
| F-02 Changes diff sideways scroll | **fixed** | Gutter fixed, tint spans the line, scrollbar, *Wrap lines* (#54, #58). |
| F-03 Changes list path-first/8 rows | **fixed** | Name first, folder muted; scrollbar; list height follows the pane (~11 rows at 800 px); divider drag was attempted (#109) but its result is not verified. |
| F-04 header clipped in narrow pane | **partly** | Header now wraps into 4 rows at 325 px; the *Wrap lines* button is still cut ("Wrap line", #59). |
| F-05 explorer hard to find | **partly** | Tree has filter, New/Refresh/Collapse all, Explorer-left preset (View → Layout Preset works, #24), rail peek at ≤ 1 000 px (#48). Still: right dock **closed** on a fresh home, rail glyphs unlabeled (#04, #05); the "root row" is the project chip. ⌘⇧1/⌘⇧4 could not be delivered by the driver (menu path works; [INFERENCE] shortcut itself fine). |
| F-06 no soft wrap | **fixed** | Markdown wraps at the pane at every width (#36–#50); code does not. |
| F-07 no scrollbars | **fixed** | Seen and (where noted) dragged: editor (drag to the end, #43), terminal (drag, #64), tree (drag, #96), Changes list (#57), palette (#65), chat (#88), search. Fade after about a second. |
| F-08 palette wheel | **fixed** | 30 notches scroll the palette, the terminal behind stays (#65); rows fill the height. |
| F-09 blame 600 px | **partly** | ≈ 215 px (32 cells), but one text row per run and the author is cut to `C(|` (#44). |
| F-10 LSP trust bar | **partly** | One bar, names the server and path; Markdown says *marksman is not installed … Copy command* instead of asking for trust; a blank 35 px band stays after *Dismiss* in that tab (#37, #38). |
| F-11 git gutter | **fixed** | #31–#33; peek with *Stage hunk* / *Revert hunk* (#32). |
| F-12 false "another agent" warning | **fixed** | No amber note with two idle terminals (#89); Agents page lists the chat as *Idle* with state (#107). |
| F-13 ⌘P spaces | **fixed** | `ch17 oop` (#35). |
| F-14 newlines collapse | **fixed** | 60 numbers, one per line (#86). |
| F-15 model menu scroll chains | **fixed** | Menu scrolled to its end, sheet stayed (#27). |
| F-16 ⌘↑ in terminal | **fixed** | #60, #61. |
| F-17 stale replace notice | not retested | |
| F-18 *Open in Split Right* | not reproduced | Tab moved to the right pane, left strip intact (#91); caret row not checked. Tab menu is only Split Right / Split Down / Close (#90). |
| F-19 blank editor view | **fixed by design** | Scrolling stops half a screen past the end (#43). |
| F-20 hooks note in commit sheet | not retested | |

## New findings

No P0, no P1.

### P2
- **N-01 Right dock still closed on a fresh home** (#04, #05). Steps: new home → add a project → select it. Expected: tree visible or one labelled affordance. Actual: Welcome centre; right rail shows four unlabeled glyphs; `hutan ui layout` says `right hidden`. The changelog says a new window "opens the right side on Files"; only the *page order* is Files-first. Area: layout/first run.
- **N-02 Tree filter ranks by path, not relevance** (#22). Filter `glue`: five results — `bug_report.yml`, `feature_request.md`, `prelude.fish`, `UNLICENSE`, and `glue.rs` last. Expected the exact filename match first. Area: Files filter.
- **N-03 Rows move under the pointer while you click** (#17, #72). Each single click opens a permanent tab and adds an *Open Editors* row, so the tree shifts by one row mid-sequence (my ⌘-click/⇧-click range selected the wrong rows). With 31 tabs the section takes about 330 px of the 800 px window. Expected: preview tab or no shift. Area: Files sections.

### P3
- **N-04 Left dock width did not drag** (#83): a drag from its edge (x 297–299) placed the editor caret instead; the right dock drags (#81). [INFERENCE] hit area too thin.
- **N-05 Collapse all / Refresh keep the scroll offset**: the tree lands mid-list with the top folders off-screen (#71, #92).
- **N-06 Rename collision message is at the bottom of the panel** (`"main.rs" already exists in this folder`, #105), far from the edited row.
- **N-07 Tooltips**: a path tooltip draws above the *Move into* confirm sheet (#12); a row tooltip stays on the previous row after a wheel scroll (#57).
- **N-08 Blank band after dismissing the LSP bar** (#37, #38; new tab has none, #40).
- **N-09 Narrow panes**: Changes toolbar *Wrap lines* clipped (#59); chat header drops "OMP · Claude Haiku 5.5" at ~325 px (#91).
- **N-10 Counts lag**: project row and Changes badge showed `5000`/`99+` for 3–10 s after the 5 200 files were gone (#102, #103); the count itself is capped at 5 000.
- **N-11 Wheel step ≈ 2.5 rows**: reaching the end of 1 000 search matches takes ~800 notches (#76, #77); a trackpad or the scrollbar drag is fine. Search summary is cut in the sidebar ("stopped at 1000 re…").
- **N-12 ⌘P ranking**: `src/ch04-01-what` lists `second-edition/src` before `src` (#39).
- **N-13 Open Editors does not mark the active file** when many entries scroll (#72).
- **N-14 Memory**: desktop RSS 149 MB after a long session (80 MB before); not a regression claim, worth one measurement under the same load.
- **Driver limits, not Hutan bugs:** shifted digits (⌘⇧1/⌘⇧4) are not delivered by `hotkey`; the `drag` tool reports `unverifiable` yet drags worked (state changed); after a click opens a file the keyboard focus is in the editor, so `End` did not move the tree (#95).

## Top 10 things that still hurt daily use

1. N-01 first-run right dock closed, rail unlabeled.
2. N-03 rows shift under the pointer / permanent tab per click / big Open Editors block.
3. N-02 filter relevance.
4. F-05 residue: no root row; Explorer-left only through View menu or ⌘⇧4.
5. F-09 blame column mostly blank, truncated author.
6. N-04 left dock width.
7. F-04/N-09 clipped controls in 325 px panes.
8. N-05 Collapse all / Refresh scroll position.
9. F-10/N-08 LSP bar leftovers.
10. N-11 wheel step and cut search summary.

## Not covered

Reveal in Finder, Open in Terminal, Attach to Chat and dropping rows on a chat/terminal, multi-row trash, Copy/Cut/Paste of Finder files, Mawas with a model, Claude Code/Codex, light theme, trackpad momentum, F-17, F-20, folder-wide LSP trust flow, divider drag result in Changes (#109), native *Open Folder…* panel.
