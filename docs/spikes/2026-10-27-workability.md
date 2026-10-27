# Workability of Hutan 0.3.0 as a daily editor + agent environment (2026-10-27)

**Question (user, 2026-10-27):** "how workable is Hutan really; there's no file explorer; scrolling down must work and things must fit the window size."

**Build:** master `5ee5be96` (version 0.3.0), `just bundle --release` built in this worktree, copied to `~/hutan-scratch/e2e6/Hutan-release.app` (the `target/bundle` that was lying in the worktree was a stale 0.2.0 build of 2026-10-23: rebuilt first). Scratch `HUTAN_HOME=~/hutan-scratch/e2e6/home` (history index off, native notifications off), app started as `HUTAN_HOME=… <bundle>/Contents/MacOS/hutan-desktop &`. Repos: `~/hutan-scratch/e2e6/repos/ripgrep` (Rust, 225 files, branch `e2e6-test`) and `repos/book` (Markdown, 2 653 files, 26 k lines of `src/*.md`). rust-analyzer 0.3.3097 downloaded into the scratch dir and set as `[lsp.servers.rust-analyzer] command` (none is installed on this Mac). Real window driven with Cua Driver, window sizes 800×560, 1000×650, 1280×800, 1440×900 and full screen (2056×1290), dark theme only, screen never locked. Real model turns: two, OMP · Claude Haiku 5.5 (the Start sheet's guard said "This sends your prompt to OMP · Claude Haiku 5.5 now", the chat header confirmed it). Input source switched to ABC and restored to Russian. Screenshots: `~/hutan-scratch/e2e6/shots/NN-*.png` (108 files; numbers below). No processes left (`pgrep -f hutan-scratch/e2e6` empty).

**Measured:** desktop RSS 80 MB, daemon 20 MB, both 0.0 % CPU idle after a session with 1 500-file tree, 1 567-file Changes list, a 5 000-line terminal block and rust-analyzer (280 MB while indexing ripgrep, 51 MB and 0.1 % after ~30 s). ⌘P over 2 653 files: results on screen within the 0.7 s wait. Project search over `book`: 1 041 matches in 243 files (capped at 1 000) shown within 2.5 s. Expanding a folder of 1 500 files: instant. Daemon log: no WARN/ERROR in the whole run.

## Scores (1–5)

| Area | Score | One line |
|---|---|---|
| Navigation | **4** | ⌘P fuzzy path search is fast and good (path:line, recent files, reveal in tree), ⌘O switches projects, tabs scroll with an overflow menu; spaces don't match in ⌘P, no scrollbars to tell where you are. |
| File explorer | **2** | It exists (right dock, ⌘⇧E), is fast on 1 500 files, shows git status, follows the active file, has ↑/↓ and Return; but it cannot create, rename, delete, move or reveal in Finder, has no filter, no root row and is closed/unlabeled on first sight. |
| Editor | **3** | Syntax, multi-cursor, two views of one buffer, save, project replace (one undo step per file), find, go to definition (F12) and blame all work; no soft wrap, no git gutter, a 600 px blame column, scroll can leave a blank view. |
| Changes / review | **3** | Real `git` staging per hunk, *Commit staged*, History and agent review all work; the file list and diff of a large change set are the user's reported mess (horizontal scroll, truncated paths, 8-row list). Fix in flight (FX-CHANGES-VIEW). |
| Terminal | **4** | Tab in the centre, block UI, 5 000-line output, wheel and ⇧PageUp scroll to the very end; ⌘↑ is not "scroll to top". |
| Agents | **4** | OMP chat on Haiku start → edit → card with *Open* → Changes row took two clicks to the diff; model list equals `omp models` (28 Anthropic + Apple, default Opus 5.5); restore after a daemon restart is clear. Noisy "another agent works in this checkout" warning from idle Mawas/terminal. |
| Mawas | **3** | Page opens as a tab and fits at every size; **not exercised with a model** (the default backend is OMP "Automatic", which starts on Opus: not sent). Provisional. |
| Settings | **4** | Sheet with searchable nav; nav and long pages (Keyboard) scroll to the very end, last row not clipped. |
| Layout / resize | **3** | Docks collapse to rails at ≤ 1 000 px and the window adapts down to 800×560, dock widths drag, full screen is fine; but wheel events leak through the palette, the Changes view clips its header in a narrow pane, and no surface draws a scrollbar. |

**Verdict.** Hutan is a good agent cockpit with a capable viewer/editor; as a *daily editor* it is one explorer, one Changes layout and a few scroll affordances away from workable. The three things that make a Zed/VS Code user stop on day one are F-01 (no file operations), F-02/F-03 (Changes view on a real change set) and F-05 (explorer placement and discoverability).

## What worked (evidence)

- **Find and open:** ⌘P `searcher/glue` → one file; `ch17oop` in `book` ranks `ch17-00-oop.md` first with matched characters coloured (shots 11, 106); opening three files shows the tree expanded to the active file (12); Files panel marks (`M`, `U`) and orange folder dots follow edits within a second, also for files created by another process (34, 36, 37, 87).
- **Edit, split, save:** typing in one view of a split shows in the other; ⌘S wrote the file (`git status` `M`), tree got `M` (14–16).
- **Replace:** project replace with preview, a confirm card ("Each file changes in one step: ⌘Z in its tab takes it back"), file written (97–100).
- **LSP:** after *Trust*, rust-analyzer indexed ripgrep, F12 on `HiArgs` jumped to `hiargs.rs:36` (80). `hutan lsp status` agrees.
- **Changes:** Review/Stage toggle, *Stage hunk*, *Unstage*, *Commit staged…* with message → `git log` shows the commit; status bar "Committed 1 file on e2e6-test (cb704a5)" (20–25).
- **Agent:** chat on Haiku, the agent edited `messages.rs`, the chat card shows `edited +1 −0 Open`, the Changes panel lists the file, the Changes tab shows the hunk with *Stage hunk* / *Discard hunk…* (59–62). Daemon restart: "3 sessions: 1 shell respawned, 2 chats resumed" card (77).
- **Scrolling to the very end worked in:** Files tree (7, 35), project search results (18), Changes right-panel list ("1527 more files…", 33), Changes diff vertical (31), editor ⌘↓ (95), terminal wheel and ⇧PageUp (48, 51), Settings nav and Keyboard page (41, 44), chat transcript (65), Start sheet model menu (56).
- **Resize:** 1000×650 and 800×560 keep the content usable, the docks fold into rails (66, 67); full screen is clean (69); the right dock widens by drag (104).

## Findings

Severity: **P0** blocks daily use of the product as an editor; **P1** a main flow is broken or misleading at realistic sizes; **P2** friction you notice every day; **P3** polish. IDs `F-nn`. Screenshots in `~/hutan-scratch/e2e6/shots/`.

### P0

**F-01 — The file explorer cannot change anything on disk** (area: Files panel; shots 39, 40, 108)
- Steps: open the Files panel (rail icon or ⌘⇧E); right-click a file, a folder; select a file and press F2, Delete, ⌘⌫ (shot 89).
- Expected (Zed/VS Code/JetBrains): New File, New Folder, Rename (F2 / Return), Delete to Trash (⌘⌫), Duplicate, Cut/Copy/Paste or drag to move, Reveal in Finder, Copy Relative Path, Open in Terminal, Collapse All, Refresh.
- Actual: the file menu is **Open · Show File History · Copy Path**, the folder menu is **Copy Path** only; F2 and Delete do nothing; the header has two icons (show ignored, settings). The only way to create or move a file is a terminal or an agent.
- Fact: the daemon's file service (`docs/protocol.md` § Files) has `fs.read|stat|list|find|search|watch|write` only — no create-directory, rename, remove or move — so this is protocol work (contract owner) before any UI.

### P1

**F-02 — Changes view: horizontal scroll breaks the diff (the user's report, reproduced on master)** (area: Changes view; shots 28–31, 45)
- Steps: 64 untracked `.md` files with 200-character Cyrillic lines + tables; open the Changes tab, pick one, wheel right over the diff.
- Expected: the line-number/sign gutter stays; added-line tint covers the scrolled width; offset resets with the file.
- Actual: the gutter scrolls away, the green tint is a fixed band that ends under the short table rows while text goes on (29, 30), the offset survives a vertical scroll and a file switch (45 shows the diff of another file at a mid-line offset, the hunk header gone), and *Unstaged · 1 hunk* sits half-hidden under the Review/Stage bar (74, 75). No wrap option for prose.

**F-03 — Changes view: file list is path-first, fixed-narrow and 8 rows tall** (area: Changes view; shots 27, 45, 70, 74)
- Steps: 1 567 changed files (1 500 in one folder) → Changes tab.
- Expected: names first; room that grows with the window; some way to move through 1 567 rows.
- Actual: rows read `zdocs/big/file-001…` (all alike) at 1 440 px and in full screen the list column is still ~190 px (70); at 1 280 px the list is a strip of ~8 rows (27) with no scrollbar, no resizer, and ~1.4 rows per wheel notch (a 1 567-row list takes ~1 000 notches). The right-hand Changes *panel* is fine (name first, folder muted, "1527 more files…").

**F-04 — Changes view in a narrow pane clips its header on the left** (area: Changes view, split panes; shot 20)
- Steps: split the centre (centre ≈ 650 px, pane ≈ 325 px) with a Changes tab in the right pane.
- Actual: the header row starts off-screen: the scope control reads "mmitted · All · Staged · Unstaged · History", the branch chip is "e2e6-…". Right-aligned controls overflow to the left instead of wrapping (UX-C narrow rules cover the list/diff, not this row).

**F-05 — The explorer is hard to find and in an unusual place** (area: layout, first run; shots 02, 04, 05, 66, 67)
- Steps: fresh home → *Open folder* → pick a project.
- Expected: the folder's tree visible, or one obvious affordance for it.
- Actual: the centre becomes "Start an agent" (prompt, Safety, Options), the left dock lists Projects/sessions, the right dock is **closed** and shows only four unlabeled glyphs (Files is the first). The tree opens on the *right* (SB-ATTACH decision, Orca-style), which is the opposite of Zed, VS Code and JetBrains; at ≤ 1 000 px the right dock auto-hides, so on a 13" screen with the left dock open there is no explorer unless you know ⌘⇧E. The tree has no root row (the header chip shows the project name and a branch glyph, not the folder or its path), no *Collapse all*, *Refresh*, *New file*. There is no filter inside the tree (⌘P is the substitute).

**F-06 — No soft wrap in the editor (known) hurts Markdown, TOML, commit-sized prose** (area: editor; shots 08, 12)
- Actual: lines wider than the pane are cut at its edge; with the right dock open the pane is ~650 px (≈ 70 columns at 13 px) and Cargo.toml's description lines, ripgrep's `use …Ordering as AtomicOrdering` and any prose paragraph need horizontal scrolling; the only cue is the cut text (no scrollbar, F-07). Already recorded as "missing" in `docs/spikes/2026-10-25-user-needs/editors.md`; it now matters more because the viewport shrank with the right-hand explorer.

### P2

**F-07 — No scrollbar on any scroll surface** (shots 07, 18, 32, 33, 35, 107). A 1 500-row tree, 1 567-row list, 1 000-row search result, 950-line file and the chat show no thumb even right after scrolling (probe 107: immediate capture). The user cannot tell how much is left, nor drag to the end.

**F-08 — Command palette: the wheel does not scroll its list and scrolls the editor behind it** (shots 90, 91, 93). ⌘K shows 12 rows; wheel over them (100 notches) left the list on its first row while the editor underneath scrolled to its last line (the `hi` line vanished). Rows beyond 12 are reachable only with ↑/↓ (92). The stray offset stayed (a tab opened later showed only its line 2 at the top: 98–100).

**F-09 — Blame column is ~600 px wide and mostly blank** (area: editor; shot 82). *Toggle Blame* on `hiargs.rs` (a long file, 11 commits) reserved 72 cells (≈ 600 px) in front of the code, left the code at ≈ 330 px with the dock open, and showed text on only two rows in view (a run's first line only). `docs/editor.md` states 40–72 cells.

**F-10 — Language-server trust bar: noisy and misleading** (area: LSP bar; shots 08, 12, 14, 77, 78, 86). (a) It appears per pane (two bars in a split), (b) it says "runs code from this folder (build scripts, tools)" for **TOML and Markdown** servers; `hutan lsp status` shows `untrusted marksman` — [INFERENCE] no Markdown server is installed here, so the bar asks for trust of something that cannot run; (c) after *Trust* a blank 35 px band stayed above the first line for a few seconds (78); (d) [INFERENCE from `docs/lsp.md`, not tried: I added the server path before trusting] without a rust-analyzer on the Mac the user gets only an install hint and no definition/hover.

**F-11 — No git change marks in the editor gutter** (shot 16). After editing and saving `walk.rs` the tree shows `M` and the Changes view the hunk, but the file tab shows no added/modified/removed bar beside the line numbers; the only gutter marks are agent-turn highlights. Zed, VS Code and JetBrains show it; it is the cheapest "what did I change" signal.

**F-12 — "Another live agent works in this checkout" when nothing is working** (area: Start sheet, Agents; shots 53, 54, 61). With one idle terminal tab and an empty Mawas session in the project the Start sheet warns (amber) that agents "would write over each other" and suggests *New worktree*; the status bar repeats "Another agent already works in this checkout" after the chat started (60). The Agents page lists Mawas · ripgrep under *Unconfirmed · 1 — State unavailable. This harness has not reported its current state*.

### P3

- **F-13 ⌘P does not take spaces** — `searcher glue` → "Nothing matches"; `searcher/glue` finds `crates/searcher/src/searcher/glue.rs` (10, 11).
- **F-14 Single newlines in an assistant answer collapse** — "print 1 to 120, one per line" renders as one wrapped paragraph (63). Standard Markdown, but chat users expect line breaks kept.
- **F-15 Start sheet: scrolling the model menu to its end scrolls the sheet** (56) — nested scroll chains.
- **F-16 ⌘↑ in a terminal tab leaves the terminal and changes the sidebar** — the sidebar went from Search back to Projects with the terminal row selected and the output jumped to the bottom (49, 50); there is no "scroll to top" key (⇧PageUp works, 51; ≈ 2.5 lines per wheel notch).
- **F-17 After a project replace the notice "Replaced 1 match in 1 file" and the replacement text stay while a new query runs** (101, 102): one click on *Replace all* from changing 243 files (the confirm card is the only guard).
- **F-18 *Open in Split Right* from a tab menu**: the new pane's caret landed on the click's row (`Ln 2, Col 19`), and the left pane's tab strip no longer showed the moved tab (14) — [INFERENCE] the mouse-up reached the editor; the strip may just have scrolled.
- **F-19 Editor can scroll to a blank view** (91, 98) — one line left at the top (VS Code does the same); returning to a tab keeps the stray offset.
- **F-20 Commit sheet** says "Git hooks do not run" — fine as a rule, worth knowing for people with pre-commit hooks.

## File explorer: what to build (the user's "think it through")

Where it stands: it is a *read* tree. The pieces a user expects and what each needs:

1. **Operations** (F-01): New File/Folder (inline name row), Rename (F2/Return, inline), Delete → Trash with confirm (⌘⌫), Duplicate, drag-and-drop move (and ⌘X/⌘V), Reveal in Finder, Copy Path / Copy Relative Path, Open in Terminal, Find in Folder (opens the Search panel with the include glob set), Open to the Side. Needs `fs.create`, `fs.mkdir`, `fs.rename`, `fs.remove` (to Trash) in the daemon's file service with the same confinement and `Mutate` classification as `fs.write`; an agent's use asks like its write does (`docs/control.md`); open buffers must follow a rename and show *deleted* for a removal (E-6 already has the deleted bar).
2. **Placement:** keep the right dock option, but offer **left** as a one-click setting and show the first-run state with the tree open (or the rail labelled); at ≤ 1 000 px keep the explorer reachable through the left rail (it already collapses to icons there).
3. **Chrome:** root row (folder name, path in tooltip), *Collapse all*, *Refresh*, *New file*, *New folder* icons in the header; type-to-filter (VS Code's) in the tree; scroll thumb (F-07); sticky folder rows for deep trees.
4. **Keyboard:** ↑/↓ and Return already work (75, 76). Add ←/→ collapse/expand with parent jump, Home/End, type-ahead, Space to preview, Return opens and *keeps* focus option (today Return moves focus to the editor, which VS Code also does for permanent open).
5. **Scale:** 1 500 siblings and 2 653-file `book` were instant; `fs.list` caps at 5 000 entries per folder (documented) — a message row "N more…" is needed past it (not tested).
6. **Git/diagnostics decorations:** letters and tint exist; add the Problems count badge and "ignored" dim already in Settings → Files.
7. **Changes ↔ Files:** *Show in Files* from a Changes row and the diff header (FX-CHANGES-VIEW item 4).

## Top 10 things that block daily use

1. **F-01** No create/rename/delete/move/reveal in the explorer (and no protocol calls for it).
2. **F-02** Changes diff: horizontal scroll loses gutter/tint, offsets persist; no wrap for prose.
3. **F-03** Changes list: path-first, truncated, fixed 8 rows, no scrollbar for large change sets.
4. **F-05** Explorer closed on first run, behind an unlabeled glyph, on the right, auto-hidden ≤ 1 000 px, no root row/collapse-all/refresh/new.
5. **F-06** No soft wrap, with a viewport that is ~650 px wide once both docks are open.
6. **F-07** No scrollbars anywhere.
7. **F-04** Changes view header clipped in a narrow pane (split pane / small window).
8. **F-11** No git change marks in the gutter.
9. **F-08** Palette: wheel dead and leaking to the editor; only 12 rows.
10. **F-10 / F-09** LSP trust bar noise per pane, misleading for Markdown/TOML; blame column eating 600 px.

## Not covered

History panel with data (index off by instruction; no real transcripts opened), Mawas with a model, Claude Code and Codex agents, light theme, trackpad momentum (the driver sends wheel notches), the native *Open Folder…* panel (the driver cannot operate it; ⌘O's recent list was used), drag-and-drop of tabs, ⌃⌘F (the shortcut did not toggle full screen through the driver; *View → Exit Full Screen* did), folders over `fs.list`'s 5 000-entry cap, window sizes below 800×560.

## Environment notes (not Hutan bugs)

The worktree's `target/` held a 0.2.0 debug/release build from 2026-10-23: `target/bundle/Hutan.app` reported `hutan 0.2.0` while master is 0.3.0; a bundle taken from it would have tested the user's old build. `cargo` is not on the harness' `PATH` (rustup toolchain at `~/.rustup/toolchains/1.98.0-aarch64-apple-darwin/bin`). `just bundle` builds a **debug** bundle; `just bundle --release` took 6 m 48 s from a warm `target/`.
