# UI wave in a real window — master `de6080e5`, evaluated 2026-10-29

**Question (user, 2026-10-28):** "a normal UI — compare apps, take the best; nicer icons and cards like Superset; the session name must come through; tab bell and status; settings with icons; fork from any point; another folder in a session; context on the right." Everything landed on master; this is the first look at it in a real window.

**Build and method.** `just bundle --release` of `slice/E2E8` (9 m 23 s; `hutan 0.3.1`, daemon build `de6080e5f103`), copied to `~/hutan-scratch/e2e8/Hutan-release.app`, started as `HUTAN_HOME=~/hutan-scratch/e2e8/home …/hutan-desktop` (never `open`), history index off (default), `notify.enabled=false`. Driven with Cua Driver 0.37 (foreground clicks and keys, `set_window_frame`, the real pointer moved with `move_cursor scope=desktop` to check hover states — they *can* be checked this way; native Open panel driven with ⌘⇧G). Window 1280×800 (default) and 1440×900, dark and light (`appearance.appearance`). Scratch repos: a tiny `demo` (python) and a copy of `book`. Real model turns: one OMP **terminal** (`omp --model anthropic/claude-haiku-5-5`, 5 prompts) and one OMP **chat** picked on *Claude Haiku 5.5* in the Start sheet (2 turns, 3 forks, 1 `/context`); nothing else was sent. Input source switched to ABC and restored to Russian. Screenshots: `~/hutan-scratch/e2e8/shots/NN-*.png` (88 files, 1366-px wide captures of a 1280-pt window, i.e. ×1.067; `hires.sh` crops are native 2×).

**Measured.** Desktop RSS 206 MB after 25 min with 8 tabs, 7 sessions, 3 chats (earlier release: 149 MB), daemon 82 MB, three `hutan mcp` helpers 11 MB each, all at 0.0 % CPU idle. Start sheet, Settings, palette, Fork popover, Context panel, quit sheet each painted inside the 0.7–1.0 s wait; no lag felt while typing into the OMP terminal or the chat. Models Hutan lists for OMP equal `omp models`: 19 Anthropic ids + `apple/on-device` (Hutan names it *Apple AFM 3 Core*), default *Claude Opus 5.5* = OMP's `modelRoles.default`; `hutan chat models --agent omp` identical to the Start sheet (#22, #22b, #22c).

## Verdict

Nothing blocks daily use and no P0/P1 was found. The agent-aware sidebar and tabs (glyph, OMP's own session name, spinner → bell/question → check, the question under the row, a rename that sticks) work end to end with a real `omp`, forking from every entry point works including into a new worktree, the Context panel and *Measure context* work, Settings is the calmest surface of the app. What still looks and feels unfinished is **density and disambiguation**: forks, parent and tabs all carry the same truncated title; three overlays sit on top of content they should not cover (the folders chip on the terminal's last row, tooltips on the row you are pointing at, the model chip dropped from the chat header); and a few state rules are inconsistent between spec and screen (ended ≠ hollow ring, exit 0 = "Ready for review", OMP chat = robot not π).

## Scores — "looks good" 1–5

| Surface | Dark | Light | One line |
|---|---|---|---|
| Sidebar session cards | 4 | 4 | Lane glyph + badge + name-first + age matches §1.5 closely; second line only with news; uppercase group labels. Loses a point for identical truncated names, the permanent hooks-hint line, a crowded project row (branch pill + ring/count + `5/7`). |
| Tab strip | 4 | 4 | π + spinner / ✓ / bell trailing the name, ✕ on hover. Four tabs with the same truncated title; a half-scrolled first tab shows only its ✕ (#29, #31). |
| OMP terminal card/state signals | 5 | – | Spinner, question line in warning colour, ✓, bell, rename-keeps-name, all within ~1 s of OMP's OSC 7501 (#08, #09, #11, #13, #18). |
| Chat view (header, footer, hover bar) | 3 | 3 | Footer and hover bar are clean; the header at 1280/1440 drops the model name and clips chips (#29, #33, #71). |
| Fork popover | 4 | 4 | Honest about OMP's cut (`omp forks only at its newest turn over ACP`), "Where" remembered, button label follows the choice. Amber for a plain limitation, radio dots sit above the text baseline (#26–#28, #32, #76). |
| Folders (chip, popover, ＋ menu) | 3 | – | Works and says how each agent takes the folder; the chip covers the terminal's last row; popover and ＋ menu have no icons or separators (#39, #41–#43, #45, #46). |
| Context panel | 4 | 4 | Numbers, breakdown bar, fill-by-turn, turn rows with Fork…/Rewind… on hover, tools, tokens, honest "Terminal agent" variant (#47, #48, #49, #51, #52, #74). Fill-by-turn is two identical slabs; "Files touched" absent. |
| Status bar + usage ring | 3 | – | Daemon state, project·branch, request/working counts are clear; the ring is 14 px, grey, low contrast; its hover card is good (#53, #53b). |
| Settings (5 groups) | 4 | 4 | Icons, 20 pt title, rows of one height, `@modified`/`@restart` work. Descriptions truncated by design; first section of a page has no heading; chips AND together (#54–#58, #78). |
| Quit sheet | 4 | – | "Quit Hutan?", lists sessions, *Keep running* default. One awkward sentence (#65). |
| Start sheet | 3 | – | Default is **Claude Opus 5.5** and must be changed by hand each time; sheet jumps ~6 px when the model list arrives; the "sends your prompt to …" confirm line works (#21–#24). |
| Agents page | 3 | – | Correct, but plain rows (open circle, no harness glyph/badge) next to the sidebar's cards (#66, #70). |
| OMP in a *light* terminal | – | 2 | OMP draws near-black blocks with light-theme text: "Output", box borders unreadable (#74). |

## Per-feature results

### 1. Real OMP in a terminal
- **Works:** tab and row show the π glyph (#04); the name comes from OMP (`➖ SUB: Add sub function to app.py`, #08b) and `hutan ls` shows it too; while working: spinner on the tab and an accent arc on the π (#08a); question: row jumps to **REQUESTS**, warning badge on π, bold name, project chip + age, the question in warning colour under it (#09, #10), tab shows a bell (#11); answered → "done"; out of sight: ✓ on tab and a green ✓ on the row with semibold name (#18, #19); returning to it clears the check. Rename (`hutan rename`, same path as the inline rename) keeps *My demo agent* and shows OMP's title under it in muted text (#13).
- **Friction:** the OMP title begins with an emoji (`➖`) that GPUI draws as a flat grey dash in the tab and sidebar (#08b, #19) — Hutan strips only `π`. The first agent row carries a permanent second line "Hooks add model… *Agent integrations…*" (#04, #05): it is an ad, not news, and its first half is cut mid-sentence. After `/exit` the row loses π (reverts to `>_`), moves to **READY FOR REVIEW** with a ✓ although §1.5 says ended = hollow ring (#79b).

### 2. Sidebar cards vs §1.5 (and honest comparison)
Matches: lane 16 / badge 8, name 13 first, age 11 on the right, × on hover swapping the age, uppercase tracked group labels (PROJECTS, REQUESTS, FAILED, READY FOR REVIEW), fills not outlines, selection = `sidebar_accent`, 8 pt inset, Bold small icons that read clearly (π, ⑂, ✕, ✓ ring). Compared with Superset/Orca-style polish, what still looks off: see "Top 10 visual fixes".

### 3. Fork
- **Footer** *Fork…* (#24–#26), **hover bar** on the user message (copy · fork, #34; none on assistant messages — by design), **sidebar ⑂** on a chat row (eye · ⑂ · ✕ on hover, #30, #31), **context panel** row (#49, #50), **⌘K** *Chat: Fork from Here* (#75; fuzzy "fork" also lists *Add Folder to Active Workspace…* because f-o-r-k is a subsequence of "Folder", noise only).
- **Into a new worktree:** from turn 1 with *summary* (OMP cannot cut earlier): daemon created `fork/reply-with-the-single-word-ready-t1` beside the repo, a worktree row appears with the fork under it, a toast "Forked with a summary of turn 1 into worktree …", the transcript starts with an info line that says what the agent got (#27–#29). From the latest turn *This folder*: exact fork, row under the parent, info line "Forked from … after turn 2 of 2" (#32, #33). Lineage chip *Forked from “…” · turn N* in the header (#29, #33, #71).
- **Terminal agents:** the context menu has *Fork Session* but it is **disabled** for the real OMP: "omp has not reported its session id" (#38) — the ids only arrive with the hooks, and the message does not say so (and installing hooks edits `~/.omp`, which this test must not do), so fork of a terminal agent is unverified here.
- **Friction:** parent, fork and worktree-fork rows are all "Reply with the single w…" (#33, #36); tabs likewise (#41). The popover shows *Restore files…* and *Archive the original* above the choice of cut, so the reading order is "options before the question".

### 4. Folders
- Chat: header chip `demo +1`, popover *Folders in this session* ("OMP is given /add-dir, a local command: applied now, no model call, not in the transcript"), Files shows `demo` + `extra` roots (`hutan chat dirs add` for the add; the chip and Files followed within 1.5 s, #45, #46).
- Terminal agent: menu *Folders…* → popover that says "Hutan types /add-dir <folder> at the prompt … press Return" (#39); *Add Folder…* opens the macOS Open panel; after choosing, Hutan typed `/add-dir <path>` into OMP's prompt and a status-bar toast said "press Return in the terminal to confirm" (#41). Files showed the folder as a root already (before Return) and the chip became `demo +1`.
- ＋ menu: *New Workspace…, Open Folder…, Open Folder into Workspace (disabled: no workspaces yet), Add Folder to This Workspace…, Add Folder to the Focused Session…* (#43).
- **Friction:** the chip (`📁 demo +1`) sits bottom-left of **every terminal** and hides the first characters of OMP's own footer (`Haiku 5.5` → `.5`; #04, #41, #42, #74). The chat header drops the model name when `+1` appears (#46).

### 5. Context panel
Numbers for the agent in view (20k / 100k · 19 %, same as the header), *Measure context* → breakdown bar and rows (System prompt 4.2k, tools 8.2k, context 6k, messages 1.6k, reserved 16k, free 64k) with "as of /context at …; the context has moved since" and *Measure again* (#47, #48, #52); turn rows T2/T1 with `+200 context · 1 tool call · 5.1 s · <$0.01`, hover → *Fork…* (T1) and *Fork…* + *Rewind…* (T2) (#49, #51); tokens/cost/time, "OMP does not report reasoning tokens.", Sources chip. The measurement is not a chat bubble (#48) ✓. For a terminal agent it says plainly what Hutan can see and offers *Agent integrations…* (#74).
- **Bug:** the measurement still marks the chat **unread/done** in the sidebar (bold name, green ✓ badge, news line "Context window: 1…" with a model chip) and puts a ✓ on its tab even while that chat is the focused one (#48). It went away on the next look.
- *Files touched* does not appear although the turn footer says "1 file changed"; OMP's edit event carried no contents ("No file contents were reported for this change.") [INFERENCE: panel needs paths the agent did not send].

### 6. Status bar, quit sheet, Settings, dock toggles, tab bell
- Usage ring: one ring with π, filled to the share of its tightest window; hover card lists 5-hour / weekly / weekly (fable) with reset times and today / this week (#53); click → Agents page, whose Usage section agrees (#66).
- Quit sheet (⌘Q): "Quit Hutan?", six live sessions with state, **Keep running** primary, *Stop sessions and quit* destructive outline, Cancel (#65).
- Settings: General · Agents · Workspace · Data · Advanced (collapsed) — 12 pages + 2 under Advanced, each with an icon; 20 pt title + one-line description; `@modified` showed exactly the one setting I changed (scope chip *You* and a revert arrow); `@restart` listed the *after restart* rows with their chip (#54, #55, #57, #58).
- Dock toggles: the title-bar buttons hide each side completely, rail included; with the left side hidden the toggle moves next to the traffic lights and with the right side hidden it jumps to the window's right edge (#60, #61, #62).
- Tab bell: BEL printed in a background shell → bell on that tab (#63b). 

### 7. Size and theme
1280×800 and 1440×900, dark and light: nothing overlaps at either size except the header problem above; at 1440 the sidebar and Agents page get room (#70, #71, #72). Light is clean and calm in the sidebar, popovers, Settings (#73, #76, #77, #78); the palette selection in light is salmon (#75).

## Findings

**P2**
1. **Measuring context marks the chat unread** (sidebar ✓ + bold + news line "Context window: …", tab ✓) although the panel promises "never shows up in the chat" (#48). *Area: sidebar / attention, ContextPanel.*
2. **Folders chip covers the last terminal row** of every terminal, hiding OMP's footer start (#04, #41, #42, #74). *Area: terminal chrome.*
3. **Identical truncated titles**: a chat, its forks and the worktree-fork are "Reply with the single w…" in sidebar and tabs; "(fork)" is cut off (#33, #36, #41, #62). *Area: sidebar, tabs, fork naming.*
4. **Chat header drops the model name** (shows only `▾`) when the lineage chip and folder chip are present, at 1280 and at 1440 with the context panel open; the header is also clipped at the right without ⋯ (#29, #33, #71). A user cannot see which model a fork runs on (safety rule: Haiku only). *Area: chat header.*
5. **Terminal-agent *Fork Session* is disabled for a real OMP** ("has not reported its session id") and the message does not say that hooks supply it (#38). *Area: fork, integrations.*
6. **OMP chat shows a generic robot, OMP terminal shows π**: §1.5 asks for the harness monogram on every row/tab of that harness (#24, #36 vs #04). *Area: icons.*
7. **Exited sessions go to READY FOR REVIEW with a ✓** (exit 0, no changes) and an exited OMP drops its π for `>_`; §1.5 says ended = hollow ring (#64, #79b). *Area: sidebar state rules.*

**P3**
8. OMP's emoji title prefix renders as a grey dash (#08b, #19); *OMP's `smol`/`commit` roles in this user's config are Sonnet 5.5 — [INFERENCE] its title generation may have used that model for the terminal session; not controllable from Hutan, flag for the rule "Haiku only".*
9. Permanent "Hooks add model… Agent integrations…" line on an idle row (#04, #05) breaks "news only with news" and is truncated mid-sentence.
10. Project row clutter: branch pill, ring+count (uncommitted files, same ring glyph as "working"), a `5/7` pill with no legend (#64, #66).
11. Tooltips cover the row/button under the pointer (#30, #37, #42, #49, #53); a row title clips under the hover icons without "…" (#30).
12. Tab strip scrolled by a new tab shows a lone ✕ of the half-hidden first tab, no overflow indicator (#29, #31, #41).
13. Stale "✓ 0 ms" status lines stay at wrong rows of OMP's transcript after it scrolls (#13, #15, #41) [INFERENCE: OMP TUI partial-scroll redraw under Hutan's terminal; OMP draws them correctly in #08].
14. In a light terminal OMP's dark blocks have unreadable labels (#74).
15. The divider turns into a ~5 pt solid brown bar on hover (#01, #02, #80) — heavy next to a 1 px hairline.
16. Usage ring is 14 px, grey, thin; the amber/red states are not visible in this run (#53b).
17. Context menu of a row: 17 items, no separators, disabled items with long explanations (#38).
18. Settings: `@modified @restart` AND-combine into "No settings match" and the chip does not toggle off; page description cut next to the scope switch (#56, #58).
19. Agents page rows are plain (open circle) beside the sidebar's cards (#66, #70).
20. Plain shell gets bold name + ✓ after a command finishes out of sight (`shell-demo`, #64) — noisy for shells.
21. Fork popover: amber for an informational limit; radio dots above the text baseline; options before the cut choice (#26–#28).
22. Start sheet: default model is Opus 5.5; layout moves ~6 px when the model list loads (#21, #21b).

**P4:** FAILED/REQUESTS labels start ~1.5 pt right of PROJECTS (#10, #64, #73); quit-sheet sentence "Keep running turns on “Keep daemon running after Hutan quits”" (#65); the ⌘K palette's fuzzy "fork" also matches *Add Folder…* (#75); *Files touched* missing (see §5); the question line in the REQUESTS card starts at the lane edge, not at the name (#10).

*Harness note (not a Hutan bug):* background `type_text` into GPUI was dropped; foreground delivery works.

## Top 10 visual fixes

1. Make forks readable: show `⑂` + parent-relative suffix (or middle-truncate) so parent / fork / worktree-fork never read identical in rows and tabs.
2. Move the folders chip out of the terminal's cell area (title bar, tab, or the right of the last row) — never over output.
3. Keep the **model** in the chat header at all widths: shrink the lineage and folder chips first (icon only, text in tooltip), fold the rest into ⋯.
4. Give OMP *chat* the π (and Claude/Codex their marks) in rows, tabs and the Agents page, as the spec says; keep the badge on the tab's glyph like the sidebar, not as a trailing bell only.
5. Ended / exit-0 rows: hollow ring, keep the agent glyph, and stay out of READY FOR REVIEW when nothing changed.
6. Calm the project row: one branch chip, one counter; label the ring as "N uncommitted" in a different glyph than the working spinner; drop `5/7` or explain it.
7. Replace the permanent hooks hint with a one-time dismissible banner (or a ⓘ on the first agent row).
8. Strip or replace emoji prefixes in agent titles (`➖ SUB:` → `SUB:`), and render agent titles in the same weight as names.
9. Tooltips anchored below/right of the pointer and delayed; hover icons should push the title's ellipsis instead of clipping it.
10. Thin the divider hover (1–2 px accent line) and enlarge/ring-contrast the status-bar usage ring; add separators to the row context menu.

## What was not exercised
Claude Code and Codex (not installed here), hooks install (would edit `~/.omp`), terminal-agent fork and *Restart with this folder*, shared-checkout warning chip, a failed OMP turn, light-mode Start sheet, window sizes below 1280×800, trackpad momentum.
