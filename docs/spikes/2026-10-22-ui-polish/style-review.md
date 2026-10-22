# Hutan design critique — "prettier style" input

Build: master `1f7ff3ea` (debug build; binaries in `~/hutan-scratch/style/bin/`). Later HEAD moved to `e86bac2e` while I worked; only the ACP fixture was built from it. Homes: `home` (real OMP on `anthropic/claude-haiku-5-5`, fake LSP, repo `repo`), `home2`/`home3` (ACP fixture agents for approval/question; `repo2`). History index off. Everything stopped, no processes left.

Shots: `shots/{dark,light}-<NN>-<name>.png` (1440×900 pt window, delivered 1366×854 px, i.e. 0.949×; narrow = 900×700 pt, delivered 1366×1062 px, `-n..-`), side-by-side dark|light montages in `pairs/` (small, for layout only). Missing/limits: pixel contrast from screenshots is unreliable for thin text (resampling overshoot), so **text ratios below are computed from the pack tokens** (verified: every flat fill I sampled matched the pack, e.g. light bg `#faf9f5`, sidebar `#f3f1ea`, selected `#e8e6dc`; dark `#262624` / `#1f1e1d` / `#2e2d2b`) and fills/pills from sampled pixels. Motion could not be captured (`screencapture -l` is denied, no ffmpeg; Cua gives ≥0.3 s granularity): the only motion evidence is a toast already fully drawn 0.25 s after the click. Hover and keyboard focus ring were not observable (3× Tab in the Start sheet showed no ring; unverified). Idle desktop: 0.1 % CPU, 174 MB RSS (debug), hutand 46 MB.

Model parity (rule 2): `omp models` and `hutan chat models --agent omp` list the same 29 ids (28 anthropic + `apple/on-device`), default `anthropic/claude-opus-5-5` in both; Start sheet and Switch model show the same names. Real turns: two Haiku 5.5 turns (edit + queued follow-up); OMP's default mode did **not** ask for approval, so approvals/questions come from the ACP fixture (`gitlab-duo/duo-chat-opus-4-6`, not a real model). Start sheet showed a second "This sends your prompt to OMP · Claude Haiku 5.5 now" confirm before sending (footer, warning yellow) — worked as intended.

## Overall verdict

The CL/UX-BUTTONS work landed visibly: one primary per surface in most places, no accent borders, calm neutrals, attention carried as a count + group instead of ten copies, approval card with a single primary and one muted sentence. The remaining ugliness is **rhythm and consistency, not colour**: mixed row heights in one list, edge-to-edge selection bars that ignore the page padding, titles drawn twice, one-line titles taken from a 120-character prompt, state-tinted pills that fail AA in light, and a light theme whose cards (`#fff` on `#faf9f5`, 1.05:1) almost disappear. Versus peers: Zed/Linear/Raycast all have *one* selection fill per list, 8-pt-aligned right edges and sentence-length titles; Cursor's chat keeps tool calls as one-line quiet rows with an expand chevron, Hutan's tool cards are heavier (mono title, 16-line dump, card in card for approvals).

## Cross-cutting facts (used by several surfaces)

| Fact | Number / where |
|---|---|
| Light card/bubble/composer `surface #ffffff` vs `bg #faf9f5` | 1.05:1 (no border) — cards read as faint tint; sidebar vs bg 1.07:1; selected vs sidebar 1.10:1 |
| Dark card `#30302e` vs `#262624` | 1.15:1; hairline `#ffffff12` ≈ 1.23:1 |
| `ui.muted` light `#66645d` on bg / sidebar / selected | 5.62 / 5.24 / 4.74 (passes) |
| `faint` as text (disabled) light / dark | 2.34 / 2.93 (disabled round send button, disabled chips `Claude Code`/`Codex` in the Start sheet) |
| State pill "shared checkout" light: `#8a6300` on `#e4ddc9` | **4.01:1** at 11 px → fails AA; dark `#e2ae45` on `#3a3223` 6.25 (ok) |
| `on_accent #1f1e1d` on accent light / dark | 4.94 / 5.84 (ok) |
| Accent as text (links `→`, `Changes to review`, resource session id) | light accent `#e0692e` on bg 3.20:1 (the `markdown.link #b04818` is 5.26 but the Agents page "Changes to review" and Resources `sess…` text are the raw accent) |
| Window title duplicates tab title | every tab/page (`Welcome`, `server`, `Agents`, chat) |
| Chat titles = first 120 chars of the prompt | window title, tab, sidebar, Agents page, Finish dialog: "Add a one-line doc comment above fn count in src/lib.rs. Change nothing …" |
| Panel title drawn twice (36 px header + first row) | Changes, Requests, Problems (tab + header + summary), History (3 tiers), Files |
| `⌘K` | is a chord prefix (`⌘K ⌘↓`); alone it does nothing. `⌘P` = "Go to ›", `⌘⇧P` = commands |

## Surfaces

### 1. First run / Welcome / Setup wizard (`dark-first-02-*`, `light-first-*`, `m-wizard.png`)
Verdict: Welcome and wizard step 1 are the best-composed surfaces (mascot, serif title, one paragraph, one orange button). The later wizard steps break it.
1. **Three orange buttons in step "Bring your agents"** (`Install…` ×2 + `Continue`, `dark-first-02-step4.png`): make the two Install buttons secondary; keep Continue the only primary (`Button::secondary`).
2. **Content is clipped under the footer** (the `Backend None | OMP` row is cut at y≈775): add a 16 px bottom fade/padding and keep the footer 56 px; or let the card grow to 80 % window height max.
3. **Left edges disagree** inside the card: agent rows start at x=448, the hooks paragraph and radio at 436/443; use one 32 px inset for every row (`wizard card padding-x: 32`).
4. **Card height jumps per step** (shots in `m-wizard.png`): fix card min-height 520 px so the Back/Continue buttons never move.
5. **Light scrim is muddy** (`light-first-02-setup.png`: 28 % black over cream gives grey-brown) and inactive progress dots are ~1.3:1: scrim light = `#1f1e1d` at 18 %; inactive dots `muted` at 40 %.
Also: final step has **two primaries** (`Open a folder` + `Finish`) — make `Open a folder` secondary.

### 2. Sidebar (`dark-02-sidebar-project`, `dark-40-needs-you-welcome`, `n10`, `31-comfortable-chat`)
Verdict: one-line session rows and a "Requests · N" group are a big improvement; alignment and hierarchy inside the tree are still loose.
1. **Double selection**: the project row (`acme-api`) and the selected session both carry a fill (dark `#2e2d2b`, light `#e8e6dc`); also `Home` shows selected on a project-less Welcome. Parent row of the selected child: no fill, only fg weight 500.
2. **Inverted hierarchy**: the project title is muted/dim while its children are `fg`; set project rows `fg` medium, children `fg` regular, ages `muted`.
3. **Mixed row heights in one list**: a `shared checkout` pill under the row turns a 26 px row into ≈40 px (`dark-45-after-allow.png`). Move the pill into the row's trailing slot as a 16 px warning glyph + tooltip, or only show it in the Agents page.
4. **Right edges don't line up**: count pill ends ≈3 px right of the age text, hover `×` sits at +18 px (`zoom` in `dark-02-…`). One trailing column: 16 px glyph slot + 28 px right-aligned age, pill width fixed to the same edge (padding-right 12 px for every row).
5. **Two nav rows selected at once** (Mawas + Tasks; Agents + Search; `dark-24`, `dark-17b`): a nav row is selected only when its page is the active tab; panels (Search/Tasks/Files) switch the row below, not a second row.
Also: nav icons 16 px vs project/session icons 14–15 px; comfortable density only adds ~4 px per row and truncates titles earlier (`Add a one-l…`), so comfortable looks tighter than compact.

### 3. Terminal (`dark-03`, `dark-30-terminal-blocks`, `n07`)
Verdict: calmest surface, keep. Block gutters (3 px green/coral) and right-aligned `✓ 42 ms` are good.
1. Block hairline + gutter + exit chip are three signals for one fact; drop the exit chip to `muted` and keep colour only on failure (`✗` stays `state.error`).
2. Cursor is hollow/solid accent in light, absent in a dark screenshot taken in blink-off: fine; ensure unfocused pane cursor is hollow (spec) — not verifiable here.
3. Terminal top padding 15 px under the tab strip vs 12 px side padding: make both 12 pt.

### 4. Editor, diagnostics, completion (`dark-11-editor`, `12-editor-completion`, `13-problems`, `n05`)
Verdict: Zed-like and clean. Diagnostics glyph (●/▲), squiggles and inline message are right.
1. Tab shows **dirty dot and ×** together (`notes.fake ● ×`): show the dot in the × slot and swap to × on hover (Zed/VS Code).
2. Completion popup has a 1 px border; design.md says popovers have none: remove border, keep the shadow (`0 8 24`), add `radius 10`, inner padding 6.
3. Completion rows are 18 px with 11 px kind glyphs (letters in 14 px boxes): 22 px rows, `muted` kind icons, selected row `accent_soft` (it is brown-orange now, reads like an error row next to the red diagnostic).
4. Gutter: error dot (x=275) and line numbers (x=300) are 25 px apart but the dot hugs the number column; 6 px more left inset.
5. Status bar mixes diagnostics counts, `Ln 5, Col 7`, `Plain Text`, `⌘⇧P` in one run with 12 px gaps: group the first three with 16 px gaps, push the shortcut hint to the far right with 24 px.

### 5. Problems / Files / Search / Resources panels (`13`–`16`, `n11`–`n14`)
Verdict: functional but each panel re-states its title.
1. **Problems**: dock tab "Problems" + header "Problems" + summary "1 error, 1 warning in 1 file" + file group + a footer "Plain Text · ready". Delete the header row; keep tab, summary and rows; footer into the status bar.
2. **Files**: `‹ Projects` + `Files` header + `repo` label = three headers before the first file; keep one (back link as the header). One generic file icon for every type; modified file `README.md M` is drawn in accent orange (git-modified colour = accent): use `state.warning` for M, `state.success` for A, `fg` otherwise.
3. **Search**: selected result row bleeds 4 px past the sidebar padding (x 5–261 vs content 8–257) and the match is truncated at 30 chars; the include/exclude fields are always visible — collapse them behind a `…` toggle (saves 60 px); search glyph sits outside the field (x=17 vs field x=30).
4. **Resources**: group rows are filled bars (4 stacked fills); use a text row (`fg` medium, no fill) with a 1 px divider; the session id `01a1644f-648c…` and `sess…` are accent text — `muted` mono.
5. Narrow (`n14`): a stray rail overflow menu ("Requests") sat over the table while Resources was open [observed once, cause unknown].

### 6. Agent chat: turn, edits, queue (`dark-05b`, `06a-queue`, `light-05`, `n01`)
Verdict: calm prose column; the transcript chrome is still heavy.
1. **Light cards vanish** (1.05:1): in light give tool cards and the user bubble `ui.border` 1 px at 6 % black or `surface = #f6f4ee`; keep dark as is.
2. **Titles are prompts**: window/tab/sidebar all show `Add a one-line doc comment above fn count in src/lib.rs. Change nothing …`; derive a ≤40-char title (first clause) or let the harness name it; drop the window title when a tab shows the same.
3. **Header**: `○ idle · OMP · Claude Haiku 5.5 ▾ · [Mode: Default] · [context 19 % · 19k / 100k]` has two grey pills that look like buttons; the same context numbers repeat under every answer (`context 17.9k / 100.0k`). Make both plain `muted` text, remove the per-turn context repeat, keep the footer `done · 10.1 s`. (Whether the Mode pill is clickable: not verified.)
4. **Top inset**: first bubble starts 7 px under the header hairline, bottom gap to the composer is ≈20 px; use 24 pt top padding in the scroller.
5. **Stop button is a pause glyph** (two bars, `zoom-stop` verified) and the idle send button is a `›` chevron: use ■ (stop, 12 px solid) and ↑ arrow (send); disabled send at opacity .45 is 1.4:1 — use `surface_hover` fill + `muted` glyph when empty.
Also: **queue chip** inside the composer repeats `Interrupt & send` three times (chip row, hint `⌃↩ interrupts and sends`, ghost button): keep the chip's `Edit`/`×` and one `Interrupt & send` ghost; hint line ≤ `⌘↩ queue · ⌘. stop`. Tool-card titles are mono 13 px and the read card dumps 16 lines (`… 16 earlier lines`): collapse by default to the one summary line. A failed card (`failed`, coral, "No file contents were reported for this change.") followed by a successful retry leaves red in the transcript — fold the failed attempt behind the Thought row.

### 7. Approval and question (`41-approval`, `42-question`, `43-requests-panel`)
Verdict: approval is now the model for the app (one orange `Allow once 1`, three secondaries, one muted sentence). Question card and Requests panel are not up to it.
1. **Question card rhythm**: question text drawn twice (`Which database…?` header and again as description), option description `Production default` sits 6 px under the option and 25 px above `Other` (y 267/289/314): 8 px label→field, 16 px between groups, show the description inside the option.
2. `Decline` and `Cancel` side by side are indistinguishable: keep `Decline` as ghost, drop `Cancel` (Escape).
3. Option chips use ✓ on the selected and ○ on the others on the right; use a leading radio (left) like a native form, selected fill `accent_soft`.
4. **Requests panel** (330 px): `Always reject 4` wraps to a 2nd line; two accent text links (`in Edit the items endpoint →`, `answer in … →`); title `Requests` twice + tabs `Requests/Changes/Info`. Buttons in a 2×2 grid, links `markdown.link` colour, remove the second title.
5. The **`Which key sends a message? ⌘Return | Return ×`** row lives inside the composer card (third nested layer, orange-tint chip) — show it once as a toast-like strip above the composer, chips as plain secondary buttons.
6. Rail badge says `1` (approvals) while sidebar/nav/status bar say `2` (requests): same number everywhere or label the rail badge.

### 8. Agents page (`17-agents-page`, `44-agents-needs-you`, `n04`)
Verdict: good structure (group, row, one secondary per row); polish items.
1. **Selected row is a full-bleed accent_soft bar** (light: peach from x=0 to 1350; dark: brown): inset it to the 16 px page padding with `radius 6`.
2. **Left edges**: heading `Agents` x=15, group label x=15, row content x=42 (light, sidebar hidden): align all to 16 px.
3. **Idle rows say `Running 5m 33s`** beside `Idle · waiting for your next message` — rename to `Age 5m` or `Started 5m ago`.
4. `Changes to review` is raw accent text (3.2:1 light): `fg` + `muted` dot, or `markdown.link`.
5. Right cluster is content-width, so the project/harness columns shift 7 px between rows (`acme-web` at x 686 vs 679 in `dark-44`): fixed 132 px action column.
Also `Start agent…` is ghost text without affordance although it is the page's main action: secondary button (28 px). `State unavailable · This harness has not reported its current state.` (Mawas row, `n04`) is wordy: `State unknown`.

### 9. Mawas Home (`18-mawas`, `n01`)
Verdict: quiet and almost right; the Today column is the loose part.
1. Today column is a different surface (light `#fff`, dark `#232221`-ish) from the pane; use the pane `bg` + one hairline.
2. `No requests · nothing working · no schedules` is left-aligned but `Memory ›` is centred in the column: left-align as a link row (`markdown.link`, 28 px high).
3. Header: `acme-api ▾` and `Ask before acting ▾` left, `Automatic · OMP ▾` + `New chat` right, state dot sits before the backend: group the two selects together (8 px gap), move the backend into `⋯`.
4. Mascot 90 px is *peeking* (cropped) and the first-run one is full — fine; empty sentence 15 px serif + hint 12 px: increase gap sentence→hint to 12 px (now ~16 vs 30 mascot→text).
5. Light disabled send button is a pale peach circle: see 6.5.

### 10. Settings sheet (`19-settings`, `n03`, `50-settings-keyboard2`)
Verdict: big improvement (scrim, search, grouped categories, one-line rows). Remaining:
1. Descriptions are cut mid-sentence with `…` (`Compact fits more on screen; Comfortable uses large…`): write ≤ 70-character descriptions or allow two lines; truncation of meaning is worse than wrapping.
2. Controls have six different widths (Compact 83 px, Hutan Light 96, Phosphor Regular 130): set `min-width 120` so the left edge forms a column.
3. Sheet is placed at y=45 (top) but 835×570 regardless of page; Keyboard page is a dense monospace table at 11 px muted with no section rhythm: section titles 13 medium, 24 px above, 8 px below.
4. `Reset page…` is a ghost button the same weight as the title row; make it a `⋯` menu item.
5. Footer `Problems · none` permanently occupies 30 px: show only when ≥1 problem.

### 11. Start sheet (`04-start-sheet`, `04b-start-confirm`, `n02`)
Verdict: coherent, but long (9 rows) and the model picker is the weakest part.
1. **Model picker opens inline** and pushes the form 190 px down, drawn as a brown (accent-tinted) box with a border; sheet scrolls under the header. Use a popover list (surface, shadow, no tint) max 8 rows and pre-scroll to the current model.
2. After click-selecting a model, Return did not close the picker although the hint says `↩ or Esc done` [observed once].
3. List order is not by family/version (Opus 4.5, 4.5 (2025…), 4.1, 4.1 Opus, 4, 4 Opus, then Sonnet 5.5): group by family, newest first; hide dated duplicates behind `Show all`.
4. Disabled agent chips (`Claude Code`, `Codex`, `faint` 2.3:1) read as illegible, not as unavailable: `muted` + `not installed` tooltip, or hide.
5. Footer: left summary truncates (`…chat in acme-api (feat…`), two buttons `Start and new ⌥⌘↩` + `Start OMP chat ⌘↩`: drop the summary (title already says it), keep `Cancel | Start and new | Start` with the primary label `Start`.
Also the confirm strip (`This sends your prompt to OMP · Claude Haiku 5.5 now.` in warning yellow + `Don't ask again for this model` radio + shrunken Start) changes the footer layout under the cursor: keep button positions fixed.

### 12. Palettes (`21-palette-p`, `22-command-palette`, `32-model-menu`)
Verdict: right look (surface, shadow, fill selection, right-aligned shortcuts); ranking and the Switch-model dialog need work.
1. `⌘K` alone does nothing (chord prefix) while Linear/Raycast/Cursor users expect it to open the palette: bind `⌘K` (outside terminal focus) to the command palette, or show "Press ⌘⇧P" hint on the chord's first stroke.
2. Fuzzy ranking: query `new` ranks `Chat: New Direct Chat…`, `Layout: Use New Default`, `Move Tab to New Split Right…` before `Session: New Terminal ⌘T`: boost actions with shortcuts and prefix matches.
3. Palette is 530 px wide, rows 28 px, Title Case with `Category:` prefix; show the category as a muted 11 px right-hand tag instead of a prefix for scan speed (Raycast).
4. **Switch model** is a modal with a `Model` label above a `Filter:` label above the field, and the current model is not visible without scrolling: one search field with the placeholder `Filter models`, current model pre-selected with a check, primary `Switch ↩` stays.

### 13. Changes + Finish + Rewind (`10-changes-*`, `08-rewind-sheet`, `09-changes-panel`)
Verdict: Rewind is good; the Changes toolbar is overloaded.
1. **Toolbar** (`feature… against…` clipped, `3 files +7 −0`, `Staged | Unstaged | All`, `With uncommitted`, `updated just now`, `Refresh`, `Finish…`): drop `updated just now` (tooltip on Refresh), make Refresh an icon button, move the two chip groups to one segmented control, ellipsis the branch text only.
2. **Two `Finish` primaries** when the inline Finish panel is open (toolbar `Finish…` and panel `Finish`): while open, toolbar `Finish…` becomes `secondary` + disabled.
3. File rows show `unstaged` yellow pill + `Open` ghost on every row (3 rows = 6 extra marks): put the state in a 14 px glyph, `Open` on hover.
4. Finish text wraps `feature /items-endpoint` mid-path: use `overflow-wrap: anywhere` only on whitespace-less tokens → insert `\u200b` after `/`, or put the branch in mono and nowrap.
5. Rewind: removed-line tint (brown-orange) equals the accent_soft row tint above it, so "will be written back" and "removed" look identical; diff removed = `state.error` at 14 %, selected row = neutral fill. Title `Rewind files to before this turn` is 13 px like the body: 15 px medium.
Right panel `Changes` has the title twice, row path `~/hutan-scratch/style/repo · main checkout · uncom…` truncated: show the branch + `3 files +7 −0` only.

### 14. Toasts and empty states (`dark-23-toast-b`, `light-23-toast-crop`, `24`, `25`, `46`)
Verdict: toast exists and is fast; placement is wrong.
1. **Toast covers the sidebar's `+ Start` button** (bottom-left, x 15–177, y 779–820): place it bottom-center of the *pane* or bottom-right above the status bar, 16 px from edges, max 360 px.
2. `Undo` is the same weight and colour as the label (`Closed server  Undo`): `markdown.link`/medium, separated by 12 px.
3. Light toast is white on `#f3f1ea` with an almost invisible shadow (`shadow 0 8 24 @12 %`): raise to 18 % or add the hairline.
4. Empty states: Welcome (`Welcome. Start an agent here…` + orange `Start…`) duplicates the sidebar's `+ Start`; Tasks row (`✓ Add a one-line d… 👁 Awaiting review done` / `main · omp chat · Claude Haiku 5.5 10s` / `(turn completed)`) is three crammed lines — one line + muted second line; History empty is a 3-tier heading stack (`History` / `Agent Session History / Resume past sessions` / `Search your agent sessions`): keep one sentence + `Set up history…`; clean Changes state `main ✓ · … · 3 live` is fine.

### 15. Narrow 900×700 (`n01`–`n14`)
Verdict: responsive behaviour is good (Today column hides, Agents toolbar folds, sidebar collapses to an icon rail when the right panel opens). Issues:
1. Left sidebar stays 280 pt = 31 % of the window until a right panel opens; at <1000 pt width default to 232 pt.
2. Tab strip scrolls with the first tab cut mid-label (`e doc comment…`, `n04`) and no fade: 16 px edge fade + auto-scroll the active tab fully into view.
3. Settings and Start sheets use 97 % / 87 % of width with 11 pt margins; keep ≥ 24 pt margins.
4. Transcript is 20 px from the sidebar divider and from the rail: 24 pt (matches the 24 pt top inset proposed in 6.4).
5. Right panel opens at ≈350 pt = 39 % of the window and squeezes the center to 440 pt: cap at 30 % below 1000 pt.

## Top 15 changes, ranked by visual impact per effort

| # | Change | Where | Effort |
|---|---|---|---|
| 1 | One selection fill per list: no fill on the parent project row of a selected child; no second selected nav row | sidebar, nav | S |
| 2 | Light-mode cards: add `border 1 px @ 6 %` (or `surface #f6f4ee`) to tool cards, user bubble, toast; toast shadow 18 % | `ui.rs` card, theme.toml light | S |
| 3 | Short titles: first-clause ≤ 40 chars for chats; never show the same title in window bar and tab | chat title, titlebar | S |
| 4 | Move the toast off the sidebar toolbar; make `Undo` a link-weight action | toasts | S |
| 5 | Inset selection bars to page padding (Agents page, Search results): 16 px, radius 6 | Agents, Search | S |
| 6 | Pills: neutral for `shared checkout`/`unstaged`/state words; warning only as a 16 px glyph (fixes 4.01:1 in light, and row heights) | sidebar rows, Changes file list, Tasks | M |
| 7 | One primary per surface in wizard (`Install…`, `Open a folder`) and while Finish is open | wizard, Changes | S |
| 8 | Fix send/stop glyphs (↑ arrow, ■ square); disabled send = neutral fill | composer | S |
| 9 | Remove duplicate headings (Changes, Requests, Problems, Files, History) | panels | M |
| 10 | Start sheet model picker as a popover list, grouped by family, pre-scrolled to current; Switch-model dialog same list | Start sheet, chat | M |
| 11 | Changes toolbar: one segmented control, icon Refresh, drop `updated just now`; unstaged glyph instead of pill | Changes view | M |
| 12 | Chat header: remove pill fills for Mode/context; drop repeated per-turn context line; 24 pt top inset | chat | S |
| 13 | Question card spacing (8/16 px), single Decline, radios on the left; Requests panel 2×2 buttons | question card, Requests | M |
| 14 | Settings: ≤70-char descriptions or wrap; control column `min-width 120`; footer only with problems | Settings sheet | M |
| 15 | `⌘K` opens the command palette (outside terminals); rank shortcut/prefix matches first | palette | M |

Not reproduced / dropped: a question card once showed `Declined` during the narrow pass; I could not reproduce it with typing or Escape and think my own click landed on `Decline` — not a finding.
