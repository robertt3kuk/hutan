# Clean aesthetic — the quiet workbench

Date: 2026-10-21 · Author: `DesignDirection` · Status: **proposal** (research only; this file is the only one written; nothing built or run) · Repo state: `master` at `fab8945`.

Goal from the user (2026-10-21): a **clean, calm, intuitive** look. This spike turns that into one implementable direction, grounded in the 94 screenshots of today's master (`~/hutan-scratch/tour/shots/`, index `~/hutan-scratch/tour/INDEX.md`; 1440×900 pt windows shown at 1366×854 px, shots 36–41 at 1000×700 pt). Product scope (what is on screen) is `docs/spikes/2026-10-21-product.md`; this spike decides *how it looks* and only touches scope where the two cannot be separated (§3.4). Pixel defects are `docs/spikes/2026-10-21-ux-audit.md` F8–F18; they are referenced, not repeated.

Labels: **[FACT]** = seen in a screenshot, read in the repo, or computed (WCAG 2.2 relative-luminance ratios, straight sRGB, translucent colors composed over what they are drawn on, the same method as `crates/settings/src/themes/contrast.rs`); **[INFERENCE]** = my reading. Every ratio in §3.2 was computed, not estimated.

Design lens: Impeccable **Operate** mode (`~/proj/research/pbakaus_impeccable/.omp/skills/impeccable/SKILL.md` § Modes: "ambition shows as precision and restraint"; `reference/operate.md`: accent only for action, selection, state; `reference/quieter.md`: fewer background accents, flatter cards, borders thinner or gone; `reference/distill.md`: never nest cards, one primary action, one spacing scale; `reference/craft-floor.md`: contrast ≥ 4.5:1 for body and placeholder text, no colored side borders above 1 px, no kicker above a heading, no modal where none is needed), the GPUI Kit design guide (`~/proj/research/longbridge_gpui-component/skills/gpui-kit-design-guides/references/design-guides.md`: emphasis is a budget, one focal point, selection through the item's own surface and never a leading bar, hairlines on the boundary owner, radius tiers, 2/4/8/12/16/24/32 spacing, primary only for the default commit), and Vercel's interface rules translated to native (`…/vercel-labs_agent-skills/skills/web-design-guidelines/SKILL.md` → `command.md`: `…` not `...`, tabular numerals, text containers truncate, reduced motion, destructive needs confirmation or undo).

---

## 0. Summary

- **Direction: the quiet workbench.** Chrome is two warm tones (sidebars and center), boundaries are hairlines owned by one side, cards are fills without borders, and **orange appears at most five times per screen**: one primary button, one selection, one keyboard focus ring, links, and the attention count. State is always a glyph plus a word, never a colored border or a colored fill.
- **What the screenshots show today [FACT]:** one pending approval is drawn in **ten places** of one window (`dark-15-rail-requests.png` with `dark-12`: the sidebar's Needs-you row, the project's `⚠ 1`, the Agents nav badge, the tab's ⚠ ●, the pane header's `blocked` + `1 approval`, the inline card, two status-bar items, the rail badge, the Requests panel card), the chat view stacks **four orange rectangles** inside each other (pane, tool card, approval card, composer: `dark-12-chat-pending-approval.png`), one screen carries **≈24 accent or warning marks** (counted in `dark-12`), every session row repeats its project's subtitle (`dark-10-terminals-split-git-status.png`), the active item's title is drawn three times (window title, tab, pane header: `dark-21-chat-after-approval.png`), and three of the four state colors fail AA as text in light mode (§3.2).
- **Tokens:** no new required tokens. The light pack changes ten values and the dark pack seven (§3.2): dark text on the brand orange (`ui.on-accent`, 4.94:1 / 5.84:1), AA state colors in light (the ANSI red/green/yellow the pack already ships), a muted that survives selected rows, one shape scale (6 / 10) in both densities, and the right sidebar at the left sidebar's tone. Every text-on-surface pair the direction draws passes AA; the pairs that cannot (state words on a selected light row, state-tinted pills, `faint` placeholders) are removed by a drawing rule, not hidden (§3.2.3).
- **Layout:** `product.md` §4.4 with the user's corrections — Mawas keeps its nav row, page and Today column (as the quiet projection of §4.4); Tasks appears with the first queued task; "workspace" appears when one exists. The title bar shows only the title; the status bar shows only what needs you and the palette hint.
- **Slices (§5):** `CL-TOKENS` → `CL-PRIMITIVES` → `CL-CHROME` → `CL-SIDEBAR` → `CL-CHAT` → `CL-AGENTS` → `CL-SETTINGS` → `CL-EMPTY`; two merge into existing work (`CL-CHROME` absorbs `PR-CLEAN-DEFAULT` and the pane-focus card of `UX-CHROME-1`; `CL-SETTINGS` becomes the visual spec of `ST-5`), and `UX-POLISH-1`'s visual items (F8, F9, F16, F17, F18, F19) are consumed by the slices that own those surfaces.
- **Five decisions (§7):** button text color, two chrome tones, the focus-ring color, one radius scale, and whether the pane header keeps the title.

---

## 1. Method

1. Read every screenshot of the key surfaces in both modes (workbench with terminals, attention state, chat with and without an approval, Agents page and panel, every right-rail panel, Mawas Home, Settings and the pickers, Home and Tasks empty states, launcher, wizard, palettes, Changes, compact density, 1000×700 pt, recovery and quit). What a screenshot shows is cited by file name; INDEX descriptions are only used for shots I did not open.
2. Scored each surface against the Operate rules above: every finding names the noise source (border, fill, competing accent, badge, duplicated title or CTA, icon density, inconsistent radius/spacing/type) and the screenshot.
3. Derived the token set from what the screenshots need, computed WCAG ratios for every text-on-surface pair the proposal introduces, and checked the result against `docs/themes.md` (packs stay data, no new required token, both built-ins remain root packs).
4. Mapped every change to the file that draws it (`crates/desktop/src/workbench/…`, grep for the accent borders in §2.12) so the slices in §5 are briefs, not wishes.

---

## 2. Critique, surface by surface

Severity: **P1** hurts the task or the trust (what needs me, what is selected, what will this click do), **P2** noise that slows scanning, **P3** polish. "Rule" names the skill rule broken.

### 2.1 Window chrome: title bar, tab strip, status bar

| # | Finding [FACT] | Evidence | Sev | Rule |
|---|---|---|---|---|
| C1 | **The focused pane is outlined in orange** (a 1 px `accent` at 0.5 alpha around the whole pane) and its active tab is outlined in orange too: two accent rectangles for one fact, and the largest accent shape on screen. In light the outline reads as a selection box around the terminal. | `dark-10-terminals-split-git-status.png`, `light-11-needs-you-pending-approval-terminals.png`, `dark-18-agents-page.png` (the Agents page outlined); code `wb/render.rs:1788-1789`, `:1852-1853` | P1 | GPUI "selection through the item's own surface", Impeccable quieter "borders: decrease opacity or remove" |
| C2 | **The active item's title is drawn three times**: window title, tab, pane header (`omp · items endpoint` × 3; `Agents` × 3 in `dark-18`). | `dark-21-chat-after-approval.png`, `dark-18-agents-page.png` | P2 | GPUI "same title is not drawn twice" (audit H8) |
| C3 | **Nine status items** at rest: connection, workspace, sessions, "nothing needs you", density, pack id, ⌘⇧P, three dock buttons (one of them drawn pressed in accent). Six of them never change. | `dark-08-home-empty.png`, `dark-10` | P2 | distill "remove redundancy"; GPUI "footer is not a catch-all" |
| C4 | **The title bar holds two commands** (`Layout: Default ▾`, `Agents overview ⌘⇧O`) next to the title, in the same 13 px as the title. | `dark-10` | P3 | GPUI "title bar is window chrome first" |
| C5 | **A tab can carry three indicators** (⚠, ● accent dot, ×) beside its icon and title; the warning glyph and the dot say the same thing twice. | `dark-12-chat-pending-approval.png` tab `omp · items endpoint ⚠ ● ×` | P2 | GPUI "emphasis is a budget" |
| C6 | **The attention sentence in the status bar** ("A permission request is waiting: open Requests to answer it", accent text, 70 characters) sits beside "⚠ 1 needs you" and "1 approval": three status items for one approval. | `dark-11-needs-you-pending-approval-terminals.png`, `light-11` | P2 | distill "say it once" |

### 2.2 Left sidebar

| # | Finding [FACT] | Evidence | Sev | Rule |
|---|---|---|---|---|
| S1 | **Every session row is a two-line card** whose second line (`tour/acme-api · main`) repeats the parent project row in all four of four rows; a hollow state circle leads every idle row. Four sessions take 230 px; `docs/design.md` § Visual system specifies 30 px rows. | `dark-10`, `dark-31-density-comfortable-chat.png`, `dark-32-density-compact-chat.png` (still two lines in compact) | P1 | distill "if it's said elsewhere, don't repeat it"; GPUI "let context carry context" |
| S2 | **The Projects header has four controls** (`▷ Start` in accent text, `+`, sort, ⚙) and the project row adds a ▷ and a counter; the bottom toolbar adds `+ New session`; the empty state adds `Open Folder…`; the center adds `Start…`: **five ways to start** on the first screen (audit F8). | `dark-08-home-empty.png`, `dark-09` (INDEX) | P1 | GPUI "one focal point"; distill "ONE obvious next action" |
| S3 | **Accent text as a control** (`▷ Start`) and accent as a state word (`blocked · approval` in orange under a warning-amber triangle): the action color and the attention color are the same hue family (accent 20°, warning 42° in dark), and orange text means both. | `dark-11`, `dark-12` | P1 | operate "accent for primary actions, selection and state indicators only"; colorize "stable role per color" |
| S4 | **Needs-you is a fourth copy** of the session (the row also sits in its project; the project row gets `⚠ 1`; the Agents nav row gets a badge). | `dark-11` | P2 | product.md §3.2 (eight renderings) |
| S5 | **Two nav rows look selected at once** (Tasks page + Mawas tab). | `dark-44-tasks-page-empty.png` | P3 | audit F17 |
| S6 | **Recovery pills eat the titles**: five `respawned` pills push session names to `…`. | `light-49-recovery-panel-with-quit-dialog.png` | P2 | GPUI "a row of badges is a missing hierarchy decision" |

### 2.3 Terminals

Terminals are the calmest surface in the tour and should set the bar: one 3–4 px block gutter, hairlines between blocks, no chrome inside the pane (`dark-10`, `dark-35-compact-terminals-split.png`). What hurts them is around them: C1's outline, the tab strip's outline, and at 1000×700 the sidebar still expanded at 28 % of the width (`dark-36-narrow-1000x700-terminals-split.png`, INDEX; `layout.auto_collapse` compacts only below 900 pt).

### 2.4 Agent chat and the approval

| # | Finding [FACT] | Evidence | Sev | Rule |
|---|---|---|---|---|
| A1 | **Four nested orange rectangles**: the pane outline, the tool card (`accent` border while running), the approval card inside it (`accent` border), the composer (`accent` border while focused, i.e. always while typing). The user bubble adds a fifth tinted shape (`accent_soft`). | `dark-12-chat-pending-approval.png`, `light-12-chat-pending-approval.png`, `dark-20-chat-fullwidth-pending-approval.png` | P1 | craft-floor "nested cards are always wrong"; quieter "flatter cards" |
| A2 | **Two primary buttons in one decision** (`1 Allow once` and `3 Reject`, both accent fills) plus two secondaries; `ui.rs:168-176` makes both primary on purpose. GPUI: primary is the one default commit; two primaries means neither is. | `dark-12`, `dark-15` (the same four buttons again in the Requests panel) | P1 | GPUI "primary for the explicit default commit" |
| A3 | **Eighty words of explanation under the buttons** ("2 Always allow: beyond this one request, OMP stops asking… Hutan cannot show or revoke that rule." twice) at 12 px on every approval. The sentence is a safety feature from `UX-CHAT-1`; its weight is not. | `dark-12`, `dark-20` | P2 | distill "cut every sentence in half"; GPUI "add text when it changes the decision" |
| A4 | **State pills in color** (`running…` accent-on-accent-soft 2.86:1 in light; `done · 2 m 53 s` green pill), and the turn result drawn twice (`done · 2 m 53 s` in the card header and again as a footer line). | `dark-21-chat-after-approval.png`, `dark-32` | P2 | colorize "contrast in all states"; distill |
| A5 | **Pane header repeats the tab** (icon + title) then adds ⚠ + `blocked` in accent text + `1 approval` in accent text + a disabled `Switch model…` whose label is unreadable (`faint` on `surface`, 2.55:1 dark / 2.46:1 light). | `dark-12`, `dark-37-narrow-chat.png` (INDEX: button clipped) | P2 | H8; craft-floor contrast |
| A6 | **The composer's accent border is on even when idle** (the field is focused), and the round send button is a second accent fill; with an answered card the screen still shows three accent shapes with nothing to do. | `dark-21`, `dark-22-mawas-home.png`, `light-22-mawas-home.png` | P2 | operate "heavy accents on inactive states" |
| A7 | **The user message is a wide tinted bubble** spanning 60 % of the column (`accent_soft`); the accent is spent on the user's own words. | `dark-20`, `light-20` (INDEX) | P3 | colorize "do not spend the primary color on decoration" |

### 2.5 Agents page and the right Agents panel

| # | Finding [FACT] | Evidence | Sev | Rule |
|---|---|---|---|---|
| G1 | **The row says `Working` in accent next to the `Needs you` group header and a warning triangle**: three state words (Needs you, Working, Permission) for one session; the reason (`Permission: edit: src/lib.rs`) is accent text, so action, state and attention share a color again. | `dark-18-agents-page.png`, `light-18-agents-page.png` | P1 | chrome-v3 §2.2 reducer precedence (attention wins); S3 |
| G2 | **Four controls in the filter band** (`All workspaces ▾`, search, `Group: State`, `Finished hidden`) plus a `Start agent…` secondary button in the header: the toolbar is wider than the content and clips at narrow widths. | `dark-18`, `dark-38-narrow-agents-page.png` (INDEX), audit F14 | P2 | GPUI "move low-frequency actions into a menu" |
| G3 | **The page heading is the only semibold text in the app** (`Agents` ≈ 17 px semibold) while `docs/design.md` allows regular and medium only. | `dark-18`, `dark-33-compact-agents-page.png` | P3 | typeset "one family, 2–3 weights" |
| G4 | **The right Agents panel draws `Agents` twice, then a breadcrumb line that truncates twice** (`acme-api › omp · item…  Needs you · Permission: edit: sr…`), then a chip row that clips `All`. | `dark-16-rail-agents.png`, `light-16` | P2 | audit F9, F14 |

### 2.6 Right rail panels: History, Changes, Requests, Resources

| # | Finding [FACT] | Evidence | Sev | Rule |
|---|---|---|---|---|
| R1 | **Every panel title is drawn twice** (36 px header + first row) and History adds a third heading (`Agent Session History / Resume past sessions`) before its one sentence. | `dark-13-rail-history.png`, `dark-14` (INDEX), `dark-15`, `dark-17-rail-resources.png` | P2 | audit F9 |
| R2 | **The Requests panel is the approval a third time** (same card, same four buttons, two of them orange), inside a panel that also has tabs named `Changes` and `Info`. | `dark-15-rail-requests.png` | P1 | product.md §3.2 row 2 (rename Approvals, move behind ⋯) |
| R3 | **Resources: five group rows with a fill each, `-0.0%`, PID over the name, `Memor` clipped.** | `dark-17`, `light-17` | P2 | audit F10 |
| R4 | **The right sidebar is a third neutral** (dark `#1a1918` beside the center `#262624` and the left `#1f1e1d`; light `#f8f7f2` / `#faf9f5` / `#f3f1ea`), so a window with the panel open steps through three grays from left to right plus a fourth for cards. | `dark-15`, `dark-16`, `light-13` (INDEX) | P3 | quieter "reduce layering"; operate "a second neutral layer", not a third |

### 2.7 Mawas Home and Today

| # | Finding [FACT] | Evidence | Sev | Rule |
|---|---|---|---|---|
| M1 | **Six header controls in one row**, the backend chip (`Automatic · OMP`) stretched to 60 % of the width as a full-width select. | `dark-22-mawas-home.png`, `light-22` | P2 | GPUI "do not enlarge a control to create whitespace" |
| M2 | **The empty state is three lines of serif listing six `@` tokens, two CTAs (`Ask a question` primary, `Review what needs you`), and a composer** — three ways to start a conversation on one screen; the Tasks sidebar beside it shows a second mascot. | `dark-22`, `dark-44-tasks-page-empty.png` (two mascots in one frame) | P2 | onboard/empty-state "one CTA"; H3 |
| M3 | **Today shows four sections with four empty-state sentences** (`Nothing needs you.`, `State unavailable · 1`, `What Mawas remembers because you approved it.`, `No schedules yet.`) plus an `Open Memory` button; nothing in it is content. | `dark-22` | P2 | distill "no empty-column scaffolding" (chrome-v3 §1 P2) |
| M4 | **At 470 pt the composer renders one letter per line** and the Today column takes half the pane. | `dark-39-narrow-mawas-home.png`, `dark-40-narrow-right-rail-open.png` | P1 | audit F3 (`UX-MAWAS-RESPONSIVE`, in `UX-CHROME-1`) |

### 2.8 Settings

| # | Finding [FACT] | Evidence | Sev | Rule |
|---|---|---|---|---|
| T1 | **No scrim**: the dialog floats over a fully readable Mawas page; the Today column and the composer behind it compete with the sheet. (The launcher, wizard and quit dialog do dim the window: `dark-45`, `dark-01`, `light-48`.) | `dark-26-settings-appearance.png`, `light-27-settings-light-theme-picker.png` | P2 | GPUI "reserve the strongest elevation for the topmost decision layer" |
| T2 | **Each row is a label, a three-to-four-line description, a layer word (`You`, `Default`) and a license line**: the Appearance page is 20 lines of 12 px prose for five settings; a `Reset` button sits beside every changed value in the same weight as the value. | `dark-26`, `light-28` (INDEX) | P2 | distill "shorter copy"; GPUI "help next to the field", not under every field |
| T3 | **The theme picker opens inline and nests three outlined boxes** (picker card → pack card with accent border → search field with accent border), with four action buttons above the list and a `Current` label in accent text. | `dark-30-settings-theme-preview-canopy-night.png`, `light-27` | P2 | craft-floor nested cards; audit F12 (inline selects) |
| T4 | **The Problems footer grows into the page** with file paths and token ids (`diff.hunk.foreground on diff.hunk.background: contrast 4.26:1`). | `dark-30`, `dark-28` (INDEX) | P2 | audit F11 |
| T5 | **Fifteen flat sections** with no grouping; the selected one uses `accent_soft`, a keyboard-focused one gets an accent border (`settings_sheet.rs:916`). | `dark-26` | P2 | audit F12 (`ST-5`) |

### 2.9 Launcher, wizard, dialogs, palettes

| # | Finding [FACT] | Evidence | Sev | Rule |
|---|---|---|---|---|
| L1 | **Every selected option is an orange outline** (`What` segmented ring + inner selected pill, `Main checkout`, `Start now`, `OMP`): five accent outlines in one sheet; the segmented control has a double outline. | `dark-45-start-launcher-dialog.png`, `dark-46-start-launcher-agent-chat.png` | P1 | GPUI "segmented control is one silhouette"; audit F19 |
| L2 | **Nine rows for an agent start**, a warning line that clips, and a footer sentence that duplicates the primary button's label. | `dark-46` | P2 | product.md `PR-START-1` |
| L3 | **The quit dialog's primary is the destructive choice** (`Stop Sessions & Quit` in accent). | `light-48-quit-confirm-dialog.png` (INDEX), `light-49` | P2 | audit F16; GPUI "danger for destructive commitment" |
| L4 | **The wizard card is the best-composed surface in the tour** (mascot, serif title, one paragraph, dots, two buttons; the scrim dims the window). Keep its composition for every modal. The card resizes per step (audit F15). | `dark-01-wizard-step1.png`, `dark-02-wizard-step2.png` | keep | — |
| L5 | **The command and jump palettes are right**: surface, soft shadow, selected row as a fill, shortcuts right-aligned. Only the Title Case labels (`Session: New Terminal`) differ from the sentence-case rule. | `dark-23-command-palette-open.png`, `dark-25-jump-palette-search.png` | keep | GPUI "sentence case" |
| L6 | **The recovery state stacks three notices**: a banner in each pane, a card in the corner, pills in every sidebar row. | `light-49` | P2 | GPUI "an Alert interrupts hierarchy; use it for exceptional information" |

### 2.10 Empty states

Two mascots and two sentences on the first screen (sidebar + center), with different instructions (`Open a folder, or press ⌘T` vs `Start an agent here, open a session from the sessions panel, or press ⌘T`), and the sidebar's mascot stays while the center shows a third mascot on Mawas or Tasks (`dark-08-home-empty.png`, `dark-44`). The Agents page empty state (not in the tour; audit §2) is the model: one sentence that teaches, one action.

### 2.11 Density and narrow windows

Compact changes paddings but not structure: session rows keep two lines, the chat keeps the same card nesting (`dark-32`, `dark-35`). At 1000×700 the sidebar stays expanded until the right panel opens (`dark-36`, `dark-40`); then Mawas breaks (M4) and the Agents toolbar clips (G2). The direction below makes rows one line and the toolbars foldable so density and width degrade structurally, not by clipping.

### 2.12 The accent inventory [FACT]

`grep` for `border_color(theme.accent` / `border_color(accent` / `accent.opacity(0.5…)` in `crates/desktop/src/workbench` finds accent used as a **border** in 21 places: the pane and tab (`render.rs:1788, 1852`), the composer (`chat/view.rs:1372`), text fields (`panels/common.rs:289`, `panels/history_view.rs:659`, `chat/question_card.rs:589`, `panels/sessions.rs:1338`), selected chips (`ui.rs:466`), selected rows and cards (`panels/sessions.rs:1425`, `panels/memory.rs:785, 922, 1039`, `panels/assistant_rail.rs:474`, `settings_rows.rs:51`, `settings_sheet.rs:916`, `settings_themes.rs:791`, `setup.rs:1032`, `history_view.rs:362`), overlays (`modal.rs:403`, `settings_daemon.rs:1641`, `settings_mcp.rs:664`, `view.rs:384`) and a 2 px colored left bar (`settings_keys.rs:146`). Drag-over targets (`render.rs:1202, 1749, 1889`, `sidebars.rs:647`, `sessions.rs:1474, 1494`) are the only ones that should stay. Counted on `dark-12-chat-pending-approval.png`: **24 accent or warning marks** on one screen.

### 2.13 What works, keep

The three-column shell and its 36/24 px bars; terminal blocks; the palettes; the wizard card; serif prose in chat; Phosphor Regular at 16 px; the warm neutrals themselves (`#faf9f5` / `#262624`); the diff view's tints; the Agents page row contract from chrome-v3. The direction removes noise around these, not them.

---

## 3. The direction: the quiet workbench

### 3.1 Principles (seven)

1. **Orange is spent, not sprinkled.** Per screen at most: one primary button, one selected item, one keyboard focus ring, links, the attention count. Never a border, never a state word, never a pane.
2. **Borders mean containment; fills mean selection.** A region boundary is one hairline owned by one side. A card is a `surface` fill with no border. A selected row is a fill with no outline. A control that is both selected and outlined is a bug.
3. **State is a glyph and a word, drawn once per object per surface.** Attention rolls up as counts; the object itself carries the reason. Warning, success and error are glyph and text colors only, never fills or borders.
4. **One title per object.** The tab (or the window title) names the item; the pane header is a status line, not a second title; a panel's 36 px header is its only heading.
5. **One sans, four sizes, two weights.** 11 / 12 / 13 / 15, regular and medium, sentence case, tabular numbers. Serif only for prose (chat, empty-state sentence, wizard). Mono only for code, paths and commands.
6. **Show the loop, disclose the rest.** The default window shows projects, sessions, agents, changes. Tasks, workspaces, Approvals, Resources, History, density, theme and dock buttons appear when they have something to say or are asked for (§3.3).
7. **Motion explains a state change and is over in 150 ms.** No pulse, no ambient motion, nothing on terminal content.

### 3.2 The token set

The pack schema (`docs/themes.md` § Token reference) already has every role this needs; the direction is mostly **values and rules**, with one optional color token added (§3.2.4). Nothing below adds a required token, so user packs stay valid.

#### 3.2.1 Scales and rules

| Axis | Rule | Values |
|---|---|---|
| **Spacing** | One base-4 scale; `spacing` (4 / 6 per density) remains the unit, and layout code uses only multiples that land on the scale. Relationship → gap, as GPUI Kit: parts of a control 4, related controls 8, one group 12, groups in a section 16, sections 24, region insets 16. | 2 · 4 · 8 · 12 · 16 · 24 · 32 |
| **Radii** | One shape scale in both densities ("density changes space, not shape"): controls and rows 6, cards, composer, popovers, palette 10, pills 999. The inner radius of a nested shape is `outer − gap` (a 6 px button inside a 10 px card with 4 px inset). | `radius.control` 6, `radius.card` 10, `radius.pill` 999 (D-C4) |
| **Type** | Body 13 regular; secondary 12 regular `muted`; section label 11 medium `muted` (no caps, no tracking); page and dialog title 15 medium sans (the prose step, reused: there is no fifth size); prose 15 serif, line height 1.6; mono 13. Compact: 12 / 11 / 11 / 14 / 15. Durations, counts and PIDs in tabular figures. No semibold anywhere. | existing `text.*` keys; the heading uses `text.prose.size` in the UI family |
| **Borders vs fills** | Hairline (`ui.border`, 1 px) only at: dock edges, the tab strip's bottom (broken under the active tab), a pane header's bottom, between list rows **when rows are 2 lines or more**, around text inputs, around secondary buttons, between a modal's header/body/footer. Cards (tool call, approval, empty-state card, Today rail) are `ui.surface` fills with no hairline. Selected: `ui.sidebar.selected` (left dock), `ui.accent.soft` (lists, chips, palette). Hover: `ui.surface.hover` / `ui.sidebar.hover`. Never both a fill and an accent outline. | — |
| **Accent budget** | Fills: `ui.accent` on the primary button and the round send/stop button; `ui.accent.soft` on the one selected row or chip of a list. Ring: the keyboard focus ring only (§3.2.4). Text: links (`markdown.link`). Nothing else: no accent text for state (`blocked`), counts (`1 approval`), labels (`Current`), controls (`▷ Start`), no accent border on a focused pane, tab, composer, field, card or chip, no accent on the user's message. | ≤ 5 marks per screen |
| **State colors** | `state.warning` (needs you: approval, question, blocked), `state.success` (done, added), `state.error` (failed, removed), `state.working` = `ui.accent` by derivation but **drawn only as the 16 px `circle-notch` glyph**, `state.idle` = `muted` hollow circle. A state is a glyph + a word in the state color on `bg` / `surface`; on a selected row the word is `fg` and only the glyph keeps the color (§3.2.3 explains why). Pills are neutral (`counter` style: `fg` on `surface_hover`) and never state-colored. The attention badge on a nav row or rail icon is `state.warning` fill with `on-accent`-dark text, ≤ 2 digits. | light `#8a6300` / `#2f7a48` / `#b93a26`; dark `#e2ae45` / `#6bbf85` / `#f08c7a` |
| **Elevation** | Four levels, nothing else: **0** window (`bg`, docks `sidebar`); **1** cards and inputs (`surface`, no shadow); **2** popovers, menus, palette, tooltips (`surface` + the one shadow, 0 8 24); **3** dialogs and sheets (`surface` + shadow + `ui.scrim` over the window, always). | existing `ui.shadow`, `ui.scrim`, `shadow.*` |
| **Icons** | Phosphor Regular 16 px, `muted` at rest, `fg` when selected, state color only for state glyphs. No icon-only control without a tooltip and an accessible name (`UX-A11Y-ROLES`). | existing icon pack |

#### 3.2.2 Pack diffs (exact)

`assets/themes/hutan-light/theme.toml`:

```diff
@@ [colors]
-"ui.sidebar-right.background" = "#f8f7f2"
-"ui.sidebar-right.selected" = "#f0eee6"
+"ui.sidebar-right.background" = "#f3f1ea"   # = ui.sidebar.background: two chrome tones (D-C2)
+"ui.sidebar-right.selected" = "#e9e6dc"     # = ui.sidebar.selected
-"ui.text.muted" = "#6f6d66"
+"ui.text.muted" = "#66645d"                 # 4.74:1 on the selected row, 5.10:1 on hover (was 4.15 / 4.46)
-"ui.on-accent" = "#ffffff"
+"ui.on-accent" = "#1f1e1d"                  # 4.94:1 on the brand orange (white was 3.37:1; audit F18) (D-C1)
-"state.success" = "#3f8f5a"
-"state.warning" = "#c98a12"
-"state.error" = "#c8452f"
+"state.success" = "#2f7a48"                 # = terminal.ansi.green, 4.99:1 on bg (was 3.77)
+"state.warning" = "#8a6300"                 # = terminal.ansi.yellow, 5.16:1 on bg (was 2.80)
+"state.error" = "#b93a26"                   # = terminal.ansi.red, 5.39:1 on bg (was 4.58)
+"markdown.link" = "#b04818"                 # links are the only accent text; 5.26:1 on bg, 4.91:1 on the sidebar (accent itself is 3.20:1)
@@ [density.comfortable]
-"radius.control" = 8
-"radius.card" = 12
+"radius.control" = 6                        # one shape scale in both densities (D-C4)
+"radius.card" = 10
```

`assets/themes/hutan-dark/theme.toml`:

```diff
@@ [colors]
-"ui.sidebar-right.background" = "#1a1918"
-"ui.sidebar-right.selected" = "#2a2927"
+"ui.sidebar-right.background" = "#1f1e1d"   # = ui.sidebar.background (D-C2)
+"ui.sidebar-right.selected" = "#2e2d2b"     # = ui.sidebar.selected
-"ui.text.muted" = "#a6a49c"
+"ui.text.muted" = "#aeaca4"                 # 5.08:1 on hover (was 4.63), 6.67:1 on bg
-"ui.on-accent" = "#ffffff"
+"ui.on-accent" = "#1f1e1d"                  # 5.84:1 on the accent (white was 2.85:1) (D-C1)
-"state.error" = "#e87562"
+"state.error" = "#f08c7a"                   # = terminal.ansi.bright-red, 5.50:1 on surface, 4.80:1 on hover (was 4.50 / 3.93)
@@ [density.comfortable]
-"radius.control" = 8
-"radius.card" = 12
+"radius.control" = 6                        # (D-C4)
+"radius.card" = 10
```

Unchanged on purpose: `ui.background`, `ui.sidebar.background`, `ui.sidebar.selected`, `ui.surface`, `ui.surface.hover`, `ui.border` (`#e6e3d9` / white at 7 %: a hairline is a boundary, not a contrast element), `ui.foreground`, `ui.text.faint` (disabled text only from now on; **placeholders switch to `muted`** because faint is 2.34:1 / 2.93:1), `ui.accent` (the brand orange stays: `#e0692e` / `#ea7a43`), `ui.accent.soft`, every `terminal.*`, every `syntax.*`, the shadow and scrim, all metrics, the compact density table. Two "Used for" strings in `crates/settings/src/themes/tokens.rs` change with the rules (`ui.accent.soft`: "selected row and chips" — the user bubble leaves it; `ui.text.faint`: "disabled text" — placeholders leave it); `docs/themes.md` regenerates from the registry.

**Mascot and brand:** the orangutan has dark features on orange; a primary button with `#1f1e1d` on `#e0692e` is the same pair. The light `markdown.link` (`#b04818`) is the only "second orange" and only as link text.

#### 3.2.3 AA check (computed, WCAG 2.2)

Text ≥ 4.5:1 (body, secondary, placeholder, button labels at 12–13 px medium are "small text"); UI components, glyphs and the focus ring ≥ 3:1. "sel" = the selected-row fill (`ui.sidebar.selected`), "hover" = `ui.surface.hover`.

| Pair | Light | Dark | Verdict |
|---|---|---|---|
| `fg` on bg / sidebar / surface / hover / sel | 15.80 / 14.73 / 16.64 / 14.33 / 13.33 | 13.77 / 15.11 / 12.01 / 10.49 / 12.49 | pass |
| `muted` (new) on bg / sidebar / surface / hover / sel | 5.62 / 5.24 / 5.92 / 5.10 / **4.74** | 6.67 / 7.32 / 5.82 / **5.08** / 6.05 | pass (was 4.15 light-sel, 4.46 light-hover, 4.63 dark-hover) |
| `muted` on `accent.soft` (chip text) | 5.03 | 5.81 | pass |
| `on-accent` (new, `#1f1e1d`) on `accent` | **4.94** | **5.84** | pass (white was 3.37 / 2.85) |
| `accent` as a **fill** vs bg / surface (3:1 component) | 3.20 / 3.37 | 5.32 / 4.64 | pass; light vs sidebar 2.98 → no accent-filled control lives in the left sidebar (the toolbar's Start is a ghost button) |
| `markdown.link` on bg / surface / sidebar | 5.26 / 5.55 / 4.91 | 5.32 / 4.64 / 5.84 (= accent) | pass |
| `state.success` text on bg / surface / sidebar / hover | 4.99 / 5.25 / 4.65 / 4.52 | 6.80 / 5.93 / 7.46 / 5.18 | pass |
| `state.warning` text on bg / surface / sidebar / hover | 5.16 / 5.43 / 4.81 / 4.68 | 7.49 / 6.53 / 8.22 / 5.70 | pass |
| `state.error` text on bg / surface / sidebar / hover | 5.39 / 5.68 / 5.03 / 4.89 | 6.31 / 5.50 / 6.92 / 4.80 | pass |
| state colors on the **selected row** (light sel `#e9e6dc`) | 4.21 / 4.35 / 4.55 | 6.17 / 6.80 / 5.72 | glyph (3:1) passes; **text does not in light** → rule: on a selected row the state word is `fg`, the glyph keeps the color |
| state color text on a 12 % tint pill (today's `ui::pill`) | 4.27 / 4.40 / 4.51 | 5.46 / 5.91 / 5.14 | light success and warning **fail** → pills become neutral (`fg` on `surface_hover`), state stays a glyph + word |
| `faint` on bg (placeholder today) | 2.34 | 2.93 | fail → placeholders use `muted`; `faint` only for disabled (exempt) |
| hairline `border` vs bg | 1.22 | 1.24 | a separator, not a boundary of a control; inputs also have placeholder/label |
| focus ring 2 px `accent` vs bg / surface | 3.20 / 3.37 | 5.32 / 4.64 | pass; sidebar 2.98 light → D-C3 recommends the system focus color (always ≥ 3:1 by macOS) |

Reference values from the current packs for the record [FACT]: light `state.warning #c98a12` 2.80:1, `state.success #3f8f5a` 3.77:1, `ui.accent` as text 3.20:1 (`blocked`, `1 approval`, `Current` in the screenshots), accent-on-accent-soft pill (`running…`) 2.86:1; dark white-on-accent 2.85:1.

#### 3.2.4 One optional token: the keyboard focus ring

`docs/design.md` § Visual system says "focus rings use `accent`". Under principle 1 that spends the accent on a thing that moves with every Tab press, and in light it is 2.98:1 on the sidebar. Recommended (D-C3): add an **optional** color token `ui.focus` ("If not set: the macOS keyboard focus indicator color, read by the desktop at runtime"), drawn as a 2 px ring with a 1 px `bg` gap, **only while focus came from the keyboard** (the workbench keeps a `focus_from_keyboard` flag set on key down and cleared on mouse down; GPUI has no focus-visible notion [INFERENCE from `wb/render.rs`, which tests `pane_focused` only]). A pack that wants its own ring sets a hex. This is additive (a warning for unknown keys never fires for a registered optional token) and `docs/themes.md` gains one row.

### 3.3 What disappears by default (progressive disclosure)

Everything stays registered and reachable (ADR 0010, parity rule `docs/process.md` § Definition of Done); this is what a fresh install shows.

| Gone from the default view | Comes back when | Where it lives meanwhile |
|---|---|---|
| Pane and tab accent outlines; focus is the active tab's connected fill (§4.1) | never | — |
| Pane header title and kind icon (D-C5) | tab strip hidden (zoomed pane without tabs): the status line gains the title | window title, tab |
| `Layout: Default ▾` and `Agents overview` in the title bar | — | View menu, ⌘⇧O, ⌘⇧1…9, palette |
| Status items `connection` (while connected), `workspace`, `sessions`, `density`, `theme`, `palette_hint`'s neighbors `dock_*` | `connection` returns while reconnecting / stopped; the rest when a preset names them | View menu, Settings → Layout; items stay registered in `panels/status.rs` |
| Nav rows **Search**, **Tasks** | Tasks: the first queued task or dependency (product.md §3.4 row 3) | filter field + ⌘P; palette *Tasks: Open*, Start sheet *Queue* |
| Projects-header `▷ Start`, `+`, sort menu; project-row ▷ | ▷ on hover of a project row (a shortcut to Start, which stays in the toolbar) | header `+` = Open folder… (⌘O), `⋯` = sort, organize, hide empty, settings; toolbar `＋ Start` (⌘T / ⌘⇧T) |
| Session-row second line (`tour/acme-api · main`) | the session's checkout differs from its project's (a worktree: `wt chrome-v3` badge) or the row is in Needs you (project shown) | — |
| Idle hollow circle in session rows | the session is not idle | state lane stays reserved (alignment spine) |
| Rail icons **Approvals** (Requests), **Resources** | an approval is pending → the Approvals icon appears with its count while one exists (never opens the body, `docs/design.md` § Docks) | `⋯` menu, ⌘⌥R, palette |
| The Requests panel's `Changes` and `Info` tabs | — | the Changes panel; `Info` folds into the Agents panel's details (product.md row 2) |
| Right panel body | the user opens it (`docs/status.md` already keeps the rail only on first launch) | rail |
| Today's empty sections and the `Open Memory` button | a section has content | one line "Nothing needs you · nothing working · no schedules" + a `Memory ›` link row |
| Mawas empty-state buttons | — | the composer (focused, placeholder `Ask Mawas…`), Today's *Review what needs you* |
| Always-allow / always-reject paragraphs | hover or focus on an *Always* button (tooltip) | one muted sentence under the row: *Always sets a standing rule for “edit: src/lib.rs” that Hutan cannot show or revoke.* |
| Settings descriptions beyond one line, `Default` / license lines, per-row `Reset` | hover → full description; `Reset` as a ghost ↺ only on rows that differ from the inherited value | — |
| Theme picker's four pack actions | the picker's `⋯` | — |
| Second mascot (sidebar, Tasks) | — | the center (Welcome, Mawas) keeps the mascot |
| `Density` / pack id in the status bar | a preset names them | Settings → Appearance |

### 3.4 The clean default layout

`product.md` §4.4, reconciled with the user's decisions (2026-10-21: two-step first run, main checkout default, **Mawas stays a headline feature with its page and Today**, workspace hidden until created) and with this spike's rules:

```text
┌──────────────────────┬──────────────────────────────────────────────────┬────┐
│ ● ● ●  ▯             │ git                                            ▯ │    │ 36
│ ⊞ Agents         2   ├────┬────┬──────────────────────────┬──────────────┤ ⑂• │ Changes (dot = review waiting)
│ ✦ Mawas              │api │git │ web                      │ acme-api     │ ◉  │ Agents
│                      ├────┘    └──────────────────────────┴──────────────┤ ⟲  │ History
│ Projects        ＋ ⋯ │ ❯ git status                                      │ ⋯  │ Approvals (appears with a count), Resources
│ ⌕ Filter             │ …                                                 │    │
│ Needs you            │                                                   │    │
│ ▲ omp · items endpt  │                                                   │    │
│ ▾ acme-api        4  │                                                   │    │
│   ❯ api-shell        │                                                   │    │
│   ❯ git              │                                                   │    │
│   ❯ web              │                                                   │    │
│   ◆ omp · items en…  │                                                   │    │
│ ▸ Home               │                                                   │    │
│ ＋ Start          ⚙  │                                                   │    │
├──────────────────────┴──────────────────────────────────────────────────┴────┤
│ ▲ 1 needs you                                                       ⌘⇧P    │ 24
└──────────────────────────────────────────────────────────────────────────────┘
   280 (220–500)                                                   rail 40
```

- **Title bar (36):** the two recovery buttons and the active item's title. Nothing else.
- **Left sidebar (280):** nav rows **Agents** (count of sessions that need you) and **Mawas** (while `assistant.enabled`; the default value of that setting is PR-FIRST-RUN's call, not this spike's). Then the **Projects** header with `＋` (Open folder…) and `⋯`, the filter, the **Needs you** group (section label + one-line rows), the tree with **one-line rows** (state glyph lane 16 px · kind icon · title · age in tabular figures · hover ×). Workspace headers appear when a workspace exists. Toolbar: `＋ Start` (ghost) and `⚙`. Tasks row appears with the first queued task.
- **Center:** tab strip on `sidebar` tone; the active tab is a `bg` fill connected to the pane (the hairline under the strip breaks beneath it); inactive tabs `muted`. The focused pane's active tab is `fg` medium; an unfocused pane's active tab is `muted` regular, and its terminal cursor is hollow — that is the whole focus indication (replaces C1; satisfies `UX-PANE-CHROME`'s "the focused pane is visible"). Pane header = a **status line** (D-C5): `● state word · harness · model ▾ · [Chat | Terminal] … ⋯`, 36 / 30 px, hairline below.
- **Right:** the 40 px rail only, three icons (Changes with a dot while a review waits, Agents, History) and `⋯`; Approvals surfaces on the rail with its count while a request is pending and never opens the body. Body opens on click at 350 px on the **same tone as the left sidebar** (D-C2).
- **Status bar (24):** left `▲ 1 needs you` (`state.warning` glyph + `fg` text; empty text when nothing needs you — the bar stays, the words go), `connection` only while not connected; right `⌘⇧P`. Density, pack, dock buttons stay registered items for presets.
- **Presets:** `default` = this (`wb/model/preset.rs:66-73` gets a new dock list and `status_bar` lists); `focus` and `overview` unchanged; existing v6 layouts untouched; *Layout: Use new default* offered once (product.md §3.4).
- **Responsive:** unchanged thresholds (`docs/design.md` § Docks); the Agents toolbar and the Mawas header fold into `⋯` below 640 pt of pane width (`UX-MAWAS-RESPONSIVE` in `UX-CHROME-1`).

**Mawas as a headline, quietly.** The nav row keeps the Mawas glyph and sits under Agents. Mawas Home keeps its page and Today. Today becomes the compact projection it already is by contract (`docs/design.md:99`): section labels only for sections with content, one muted line when all are empty, `Memory ›` as a link row instead of a button, the same row component as the Agents page. The header folds to `acme-api ▾ · Ask before acting ▾ · ⋯` (New chat, Stop, Today, backend) — the backend chip moves to `⋯` and the model name into the status line.

### 3.5 Empty states

One pattern, one component (`ui::empty_state`): mascot (96 px, center surfaces only), **one serif sentence**, **one button** (secondary unless it is the surface's default commit), optional one line of muted hint with the shortcut. Sidebar and panel empties are **text only**: one muted line and one ghost button (`No projects · Open folder…`, `No tasks yet · New task…`). Copy:

| Surface | Sentence | Action | Hint |
|---|---|---|---|
| Welcome (no project) | *Open a folder to start.* | **Open folder…** (primary: the first screen's default commit) | `⌘T` new terminal |
| Welcome (project open, no session) | *Start an agent or a terminal in acme-api.* | **Start…** (secondary) | `⌘T` terminal · `⌘⇧T` agent |
| Mawas Home | *Ask Mawas about this project; attach context with @ or the ＋.* | none (the composer is focused) | — |
| Agents page | *Your agents will appear here.* (audit §2, keep) | Start agent… (secondary) | — |
| Tasks (sidebar) | *No tasks yet.* | New task… (ghost) | — |
| History panel | *Search your agent sessions · off until you choose sources.* (keep) | Set up history… | — |
| Changes (clean) | *Nothing changed since main.* | — | — |

### 3.6 Motion

- Hover and selection fills: 120 ms ease-out (exponential), opacity/color only.
- Popovers, menus, palette, pickers: 150 ms fade + 4 px rise; dismissal 100 ms.
- Expanding cards (tool call, approval, Always disclosure): height 150 ms ease-out; content never fades in behind it.
- Attention entry: the state glyph in the sidebar row and the status bar blinks **once** (200 ms) on entering Needs you (`docs/design.md` § Attention model keeps "pulses once"); nothing loops.
- Reduce Motion: every duration 0.
- Nothing moves in a terminal, and the tab strip never animates reorder.

---

## 4. Before and after: four surfaces

Each wireframe is a self-contained SVG: one `<style>` block with color classes at the light pack's proposed values and the dark pack's values under `prefers-color-scheme: dark` (re-theme by restyling the classes; no `var()`, so rasterizers without CSS-variable support still render it), no external references, system fonts. All four were parsed as XML and rasterized once in light (scratch copies under `~/hutan-scratch/clean-aesthetic/wire-{1..4}.svg`) to check the layout; they show structure and token roles, not pixels.

### 4.1 Workbench with terminals

**Before** (`dark-10-terminals-split-git-status.png`, `dark-11-needs-you-pending-approval-terminals.png`): 4 nav/header controls + 4 Projects-header controls; two-line session rows; focused pane and tab outlined in accent; 5 rail icons; 9 status items; approval drawn in 7 places.

**After:**
- Title bar: `git` centered, two recovery buttons. No Layout, no Overview.
- Sidebar: `Agents 1` · `Mawas` · section `Projects ＋ ⋯` · filter · `Needs you` label · `▲ omp · items endpoint  now` (glyph `state.warning`, title `fg`, no second line, no `blocked · approval` text — the reason is in the Agents page and the chat) · `▾ acme-api 4` · four one-line rows, `git` selected with `ui.sidebar.selected` and **no outline** · `▸ Home` · toolbar `＋ Start   ⚙`.
- Center: two panes; left tab strip `api-shell · git · web`, `git` connected to the pane (bg fill, hairline broken), `fg` medium; right pane's `acme-api` tab `muted` regular (unfocused), its cursor hollow. No pane outline. Terminal padding 12, block gutter 4 px `state.success`, prompt lines as today.
- Rail: Changes · Agents · History · ⋯, plus the Approvals icon with `1` while the request is pending (warning fill, dark text), nothing else lit.
- Status bar: `▲ 1 needs you` left, `⌘⇧P` right.
- Accent marks on screen: **0** (the terminal cursor is terminal paint). Warning marks: 3 (row glyph, rail badge, status glyph).

<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 960 600" width="960" height="600" font-family="-apple-system, system-ui, sans-serif" font-size="9" role="img" aria-label="Workbench with terminals, after">
<style>
/* light pack values; dark pack values under prefers-color-scheme: dark; restyle the classes to re-theme */
.f-bg{fill:#faf9f5}.s-bg{stroke:#faf9f5}.f-sb{fill:#f3f1ea}.s-sb{stroke:#f3f1ea}.f-sel{fill:#e9e6dc}.s-sel{stroke:#e9e6dc}.f-sf{fill:#ffffff}.s-sf{stroke:#ffffff}.f-hv{fill:#f0eee6}.s-hv{stroke:#f0eee6}.f-bd{fill:#e6e3d9}.s-bd{stroke:#e6e3d9}.f-fg{fill:#1f1e1d}.s-fg{stroke:#1f1e1d}.f-mu{fill:#66645d}.s-mu{stroke:#66645d}.f-ac{fill:#e0692e}.s-ac{stroke:#e0692e}.f-acs{fill:#fbe9df}.s-acs{stroke:#fbe9df}.f-on{fill:#1f1e1d}.s-on{stroke:#1f1e1d}.f-ok{fill:#2f7a48}.s-ok{stroke:#2f7a48}.f-wn{fill:#8a6300}.s-wn{stroke:#8a6300}.f-er{fill:#b93a26}.s-er{stroke:#b93a26}.f-scrim{fill:rgba(0,0,0,.28)}.s-scrim{stroke:rgba(0,0,0,.28)}.f-shadow{fill:rgba(0,0,0,.12)}.s-shadow{stroke:rgba(0,0,0,.12)}.bg{fill:#faf9f5}.sb{fill:#f3f1ea}.sel{fill:#e9e6dc}.sf{fill:#ffffff}.fg{fill:#1f1e1d}.mu{fill:#66645d}.ok{fill:#2f7a48}.wn{fill:#8a6300}.hl{stroke:#e6e3d9;stroke-width:1}.btn{fill:#ffffff;stroke:#e6e3d9}.mono{font-family:ui-monospace,Menlo,monospace;font-size:8.5px}.serif{font-family:'New York',Georgia,serif;font-size:10.5px}.med{font-weight:500}.lab{font-size:7.5px;font-weight:500}.ttl{font-size:11px;font-weight:500}.num{font-variant-numeric:tabular-nums}
@media (prefers-color-scheme:dark){.f-bg{fill:#262624}.s-bg{stroke:#262624}.f-sb{fill:#1f1e1d}.s-sb{stroke:#1f1e1d}.f-sel{fill:#2e2d2b}.s-sel{stroke:#2e2d2b}.f-sf{fill:#30302e}.s-sf{stroke:#30302e}.f-hv{fill:#3a3936}.s-hv{stroke:#3a3936}.f-bd{fill:#353533}.s-bd{stroke:#353533}.f-fg{fill:#f5f4ef}.s-fg{stroke:#f5f4ef}.f-mu{fill:#aeaca4}.s-mu{stroke:#aeaca4}.f-ac{fill:#ea7a43}.s-ac{stroke:#ea7a43}.f-acs{fill:#3e2c23}.s-acs{stroke:#3e2c23}.f-on{fill:#1f1e1d}.s-on{stroke:#1f1e1d}.f-ok{fill:#6bbf85}.s-ok{stroke:#6bbf85}.f-wn{fill:#e2ae45}.s-wn{stroke:#e2ae45}.f-er{fill:#f08c7a}.s-er{stroke:#f08c7a}.f-scrim{fill:rgba(0,0,0,.5)}.s-scrim{stroke:rgba(0,0,0,.5)}.f-shadow{fill:rgba(0,0,0,.4)}.s-shadow{stroke:rgba(0,0,0,.4)}.bg{fill:#262624}.sb{fill:#1f1e1d}.sel{fill:#2e2d2b}.sf{fill:#30302e}.fg{fill:#f5f4ef}.mu{fill:#aeaca4}.ok{fill:#6bbf85}.wn{fill:#e2ae45}.hl{stroke:#353533;stroke-width:1}.btn{fill:#30302e;stroke:#353533}}
</style>
<rect class="bg" width="960" height="600"/>
<rect class="sb" width="960" height="24"/>
<circle cx="12" cy="12" r="4" class="mu"/><circle cx="26" cy="12" r="4" class="mu"/><circle cx="40" cy="12" r="4" class="mu"/>
<rect class="s-mu" x="195" y="8" width="10" height="8" rx="1.5" fill="none"/><line class="s-mu" x1="198.5" y1="8" x2="198.5" y2="16"/>
<text x="560" y="15.5" text-anchor="middle" class="fg">git</text>
<rect class="s-mu" x="915" y="8" width="10" height="8" rx="1.5" fill="none"/><line class="s-mu" x1="921.5" y1="8" x2="921.5" y2="16"/>
<rect class="sb" x="0" y="24" width="187" height="560"/>
<line x1="187" y1="24" x2="187" y2="584" class="hl"/>
<rect class="s-mu" x="10" y="33" width="9" height="9" rx="2" fill="none"/><text x="25" y="41" class="fg med">Agents</text><rect x="160" y="33" width="14" height="10" rx="5" class="wn"/><text x="167" y="41" text-anchor="middle" class="lab f-on">1</text>
<path class="f-mu" d="M14 54l1.5 3 3 .5-2.2 2 .6 3-2.9-1.5-2.9 1.5.6-3-2.2-2 3-.5z"/><text x="25" y="61" class="fg med">Mawas</text>
<text x="10" y="86" class="mu lab">Projects</text><text x="156" y="86" class="mu">＋</text><text x="170" y="86" class="mu">⋯</text>
<rect x="8" y="94" width="171" height="18" rx="6" class="sf s-bd"/><text x="16" y="106" class="mu">Filter projects and sessions</text>
<text x="10" y="128" class="mu lab">Needs you</text>
<path d="M14 134l5 9h-10z" class="wn"/><text x="26" y="142" class="fg">omp · items endpoint</text><text x="178" y="142" text-anchor="end" class="mu">now</text>
<text x="10" y="163" class="mu">▾</text><path class="s-mu" d="M20 157l0 7 M20 157h5 M20 164h5" fill="none"/><text x="30" y="164" class="fg med">acme-api</text><text x="178" y="164" text-anchor="end" class="mu">4</text>
<text x="26" y="184" class="mu">❯</text><text x="38" y="184" class="fg">api-shell</text><text x="178" y="184" text-anchor="end" class="mu">1m</text>
<rect x="8" y="192" width="171" height="20" rx="6" class="sel"/><text x="26" y="205" class="mu">❯</text><text x="38" y="205" class="fg">git</text><text x="178" y="205" text-anchor="end" class="mu">1m</text>
<text x="26" y="226" class="mu">❯</text><text x="38" y="226" class="fg">web</text><text x="178" y="226" text-anchor="end" class="mu">1m</text>
<path class="f-mu" d="M19 240l4-4 4 4-4 4z"/><text x="38" y="247" class="fg">omp · items endpoint</text><text x="178" y="247" text-anchor="end" class="mu">now</text>
<text x="10" y="270" class="mu">▸</text><text x="22" y="270" class="mu">⌂</text><text x="34" y="270" class="fg">Home</text>
<line x1="0" y1="559" x2="187" y2="559" class="hl"/>
<text x="12" y="575" class="fg">＋ Start</text><text x="168" y="575" class="mu">⚙</text>
<rect class="sb" x="187" y="24" width="746" height="23"/>
<line x1="187" y1="47" x2="933" y2="47" class="hl"/>
<text x="206" y="39" class="mu">❯ api-shell</text>
<rect class="bg" x="268" y="28" width="46" height="20" rx="6"/><rect class="bg" x="268" y="40" width="46" height="8"/><text x="278" y="39" class="fg med">❯ git</text>
<text x="326" y="39" class="mu">❯ web</text>
<line x1="560" y1="24" x2="560" y2="584" class="hl"/>
<text x="572" y="39" class="mu">❯ acme-api</text>
<rect x="196" y="56" width="3" height="128" rx="1" class="ok"/>
<text x="206" y="66" class="mono fg">homie ~/h/t/acme-api (feature/items-endpoint)&gt; git status</text>
<text x="206" y="78" class="mono fg">On branch feature/items-endpoint</text>
<text x="206" y="90" class="mono fg">Changes not staged for commit:</text>
<text x="206" y="102" class="mono mu">  (use "git add &lt;file&gt;..." to update what will be committed)</text>
<text x="206" y="116" class="mono f-er">        modified:   src/routes.rs</text>
<text x="206" y="128" class="mono f-er">        modified:   src/store.rs</text>
<text x="206" y="140" class="mono f-er">        modified:   web/src/format.ts</text>
<text x="206" y="156" class="mono fg">Untracked files:</text>
<text x="206" y="168" class="mono f-er">        tests/</text>
<text x="206" y="192" class="mono fg">homie ~/h/t/acme-api (feature/items-endpoint)&gt; </text>
<rect class="f-ac" x="478" y="184" width="6" height="10"/>
<text x="572" y="66" class="mono fg">homie ~/h/t/acme-api (feature/items-endpoint)&gt; git diff --stat</text>
<text x="572" y="78" class="mono fg"> src/routes.rs    | 22 <tspan class="ok">++++++++++++</tspan><tspan class="f-er">---</tspan></text>
<text x="572" y="90" class="mono fg"> src/store.rs     |  8 <tspan class="ok">++++++</tspan><tspan class="f-er">--</tspan></text>
<text x="572" y="102" class="mono fg"> web/src/format.ts|  6 <tspan class="ok">+++++</tspan><tspan class="f-er">-</tspan></text>
<text x="572" y="114" class="mono fg"> 3 files changed, 30 insertions(+), 6 deletions(-)</text>
<text x="572" y="138" class="mono fg">homie ~/h/t/acme-api (feature/items-endpoint)&gt; </text>
<rect class="s-mu" x="844" y="130" width="6" height="10" fill="none"/>
<rect class="sb" x="933" y="24" width="27" height="560"/>
<line x1="933" y1="24" x2="933" y2="584" class="hl"/>
<path class="s-mu" d="M943 38v10M950 38v4M943 43h7" fill="none"/><circle cx="952" cy="36" r="2" class="ok"/>
<rect class="s-mu" x="941" y="62" width="11" height="9" rx="2" fill="none"/><circle cx="944.5" cy="66.5" r="1" class="mu"/><circle cx="948.5" cy="66.5" r="1" class="mu"/>
<path class="s-mu" d="M946.5 96a6 6 0 1 1-6-6M940.5 90v-3M940.5 90h3" fill="none"/><path class="s-mu" d="M946.5 92v4l3 2" fill="none"/>
<path class="s-mu" d="M943 118v-4a1.5 1.5 0 0 1 3 0v4M946 118v-6a1.5 1.5 0 0 1 3 0v6M949 118v-4a1.5 1.5 0 0 1 3 0v8a6 6 0 0 1-6 6h-1a6 6 0 0 1-5-3l-3-5a1.5 1.5 0 0 1 2.5-1.5l1.5 2" fill="none" stroke-width="1"/>
<rect x="949" y="108" width="12" height="10" rx="5" class="wn"/><text x="955" y="116" text-anchor="middle" class="lab f-on">1</text>
<text x="946.5" y="150" text-anchor="middle" class="mu">⋯</text>
<rect class="sb" x="0" y="584" width="960" height="16"/>
<line x1="0" y1="584" x2="960" y2="584" class="hl"/>
<path d="M12 589l4 7h-8z" class="wn"/><text x="22" y="596" class="fg">1 needs you</text>
<text x="948" y="596" text-anchor="end" class="mu">⌘⇧P</text>
</svg>

### 4.2 Agent chat with a pending approval

**Before** (`dark-12-chat-pending-approval.png`, `light-12`, `dark-15`): four nested accent rectangles, two primary buttons, 80 words of fine print, the title ×3, `blocked` and `1 approval` as accent text, the approval repeated in the Requests panel.

**After:**
- Tab `◆ omp · items endpoint ▲` (one glyph: the warning triangle in `state.warning` replaces the ⚠ + ● pair; × on hover).
- Status line (no title): `▲ Needs you · approval` (`state.warning` glyph, `fg` words) `·  OMP · opus-4-6 ▾` (`muted`, a ghost menu that replaces *Switch model…*) `·  [Chat | Terminal]` (segmented, one silhouette) `…  ⋯`.
- Reading column 720 px centered. User message: a right-aligned `surface` bubble (neutral, radius 10), max 70 % width, `fg` 13 px sans. Assistant prose: serif 15 on `bg`, no container.
- Tool card: `surface` fill, radius 10, **no border**. Header row: `✎ edit: src/lib.rs` (mono 13, `fg`) … `running…` (muted text, no pill). Body: one muted line.
- Approval group inside the card: indented 16 px, separated from the header by a hairline (the only line inside the card). `Apply these changes?` 13 medium; path mono 12 `muted`; **buttons on one row**: **Allow once** (primary: `accent` fill, `on-accent` dark text — the only accent fill in the view), `Always allow`, `Reject`, `Always reject` (secondary: `surface` + hairline). Keys `1`–`4` shown as `kbd` chips inside the buttons at 11 px `muted`. One muted sentence under the row: *Always sets a standing rule for “edit: src/lib.rs” that Hutan cannot show or revoke.* The full text of `UX-CHAT-1` moves to each *Always* button's tooltip.
- After the answer: the group collapses to one line `✓ Allowed once · from this window` (`state.success` glyph, `muted` words); the card header says `done · 2 m 53 s` once; no footer repetition.
- Composer: `surface` fill, hairline `border`, radius 10, placeholder `Message the agent…` in `muted`, `＋` ghost, `⌘. interrupts the turn` muted, round **stop** button in `accent` (it is the composer's primary; when idle it is **send**, still the one accent fill of the composer). No accent border, focused or not.
- The Requests (Approvals) panel is closed by default; its rail icon shows `1`.
- Accent marks: **2** (Allow once, stop). Warning marks: 2 (tab, status line).

<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 960 600" width="960" height="600" font-family="-apple-system, system-ui, sans-serif" font-size="9" role="img" aria-label="Agent chat with a pending approval, after">
<style>
/* light pack values; dark pack values under prefers-color-scheme: dark; restyle the classes to re-theme */
.f-bg{fill:#faf9f5}.s-bg{stroke:#faf9f5}.f-sb{fill:#f3f1ea}.s-sb{stroke:#f3f1ea}.f-sel{fill:#e9e6dc}.s-sel{stroke:#e9e6dc}.f-sf{fill:#ffffff}.s-sf{stroke:#ffffff}.f-hv{fill:#f0eee6}.s-hv{stroke:#f0eee6}.f-bd{fill:#e6e3d9}.s-bd{stroke:#e6e3d9}.f-fg{fill:#1f1e1d}.s-fg{stroke:#1f1e1d}.f-mu{fill:#66645d}.s-mu{stroke:#66645d}.f-ac{fill:#e0692e}.s-ac{stroke:#e0692e}.f-acs{fill:#fbe9df}.s-acs{stroke:#fbe9df}.f-on{fill:#1f1e1d}.s-on{stroke:#1f1e1d}.f-ok{fill:#2f7a48}.s-ok{stroke:#2f7a48}.f-wn{fill:#8a6300}.s-wn{stroke:#8a6300}.f-er{fill:#b93a26}.s-er{stroke:#b93a26}.f-scrim{fill:rgba(0,0,0,.28)}.s-scrim{stroke:rgba(0,0,0,.28)}.f-shadow{fill:rgba(0,0,0,.12)}.s-shadow{stroke:rgba(0,0,0,.12)}.bg{fill:#faf9f5}.sb{fill:#f3f1ea}.sel{fill:#e9e6dc}.sf{fill:#ffffff}.fg{fill:#1f1e1d}.mu{fill:#66645d}.ok{fill:#2f7a48}.wn{fill:#8a6300}.hl{stroke:#e6e3d9;stroke-width:1}.btn{fill:#ffffff;stroke:#e6e3d9}.mono{font-family:ui-monospace,Menlo,monospace;font-size:8.5px}.serif{font-family:'New York',Georgia,serif;font-size:10.5px}.med{font-weight:500}.lab{font-size:7.5px;font-weight:500}.ttl{font-size:11px;font-weight:500}.num{font-variant-numeric:tabular-nums}
@media (prefers-color-scheme:dark){.f-bg{fill:#262624}.s-bg{stroke:#262624}.f-sb{fill:#1f1e1d}.s-sb{stroke:#1f1e1d}.f-sel{fill:#2e2d2b}.s-sel{stroke:#2e2d2b}.f-sf{fill:#30302e}.s-sf{stroke:#30302e}.f-hv{fill:#3a3936}.s-hv{stroke:#3a3936}.f-bd{fill:#353533}.s-bd{stroke:#353533}.f-fg{fill:#f5f4ef}.s-fg{stroke:#f5f4ef}.f-mu{fill:#aeaca4}.s-mu{stroke:#aeaca4}.f-ac{fill:#ea7a43}.s-ac{stroke:#ea7a43}.f-acs{fill:#3e2c23}.s-acs{stroke:#3e2c23}.f-on{fill:#1f1e1d}.s-on{stroke:#1f1e1d}.f-ok{fill:#6bbf85}.s-ok{stroke:#6bbf85}.f-wn{fill:#e2ae45}.s-wn{stroke:#e2ae45}.f-er{fill:#f08c7a}.s-er{stroke:#f08c7a}.f-scrim{fill:rgba(0,0,0,.5)}.s-scrim{stroke:rgba(0,0,0,.5)}.f-shadow{fill:rgba(0,0,0,.4)}.s-shadow{stroke:rgba(0,0,0,.4)}.bg{fill:#262624}.sb{fill:#1f1e1d}.sel{fill:#2e2d2b}.sf{fill:#30302e}.fg{fill:#f5f4ef}.mu{fill:#aeaca4}.ok{fill:#6bbf85}.wn{fill:#e2ae45}.hl{stroke:#353533;stroke-width:1}.btn{fill:#30302e;stroke:#353533}}
</style>
<rect class="bg" width="960" height="600"/>
<rect class="sb" width="960" height="24"/>
<circle cx="12" cy="12" r="4" class="mu"/><circle cx="26" cy="12" r="4" class="mu"/><circle cx="40" cy="12" r="4" class="mu"/>
<text x="560" y="15.5" text-anchor="middle" class="fg">omp · items endpoint</text>
<rect class="sb" x="0" y="24" width="187" height="560"/><line x1="187" y1="24" x2="187" y2="584" class="hl"/>
<text x="25" y="41" class="fg med">Agents</text><rect x="160" y="33" width="14" height="10" rx="5" class="wn"/><text x="167" y="41" text-anchor="middle" class="lab f-on">1</text>
<text x="25" y="61" class="fg med">Mawas</text>
<text x="10" y="86" class="mu lab">Projects</text><text x="156" y="86" class="mu">＋</text><text x="170" y="86" class="mu">⋯</text>
<text x="10" y="112" class="mu lab">Needs you</text>
<rect class="f-sel" x="8" y="118" width="171" height="20" rx="6"/><path d="M14 123l5 9h-10z" class="wn"/><text x="26" y="131" class="fg">omp · items endpoint</text><text x="178" y="131" text-anchor="end" class="mu">now</text>
<text x="30" y="156" class="fg med">▾ acme-api</text><text x="178" y="156" text-anchor="end" class="mu">5</text>
<text x="38" y="176" class="fg">api-shell</text><text x="38" y="196" class="fg">git</text><text x="38" y="216" class="fg">web</text><text x="38" y="236" class="fg">acme-api</text><text x="38" y="256" class="fg">omp · items endpoint</text>
<text x="12" y="575" class="fg">＋ Start</text><text x="168" y="575" class="mu">⚙</text><line x1="0" y1="559" x2="187" y2="559" class="hl"/>
<rect class="sb" x="187" y="24" width="746" height="23"/><line x1="187" y1="47" x2="933" y2="47" class="hl"/>
<text x="206" y="39" class="mu">❯ api-shell</text><text x="270" y="39" class="mu">❯ git</text><text x="308" y="39" class="mu">❯ web</text>
<rect class="bg" x="350" y="28" width="118" height="20" rx="6"/><rect class="bg" x="350" y="40" width="118" height="8"/><text x="360" y="39" class="fg med">◆ omp · items endpoint</text><path d="M456 32l4 7h-8z" class="wn"/>
<text x="482" y="39" class="mu">Agents</text>
<line x1="187" y1="71" x2="933" y2="71" class="hl"/>
<path d="M200 55l5 9h-10z" class="wn"/><text x="211" y="63" class="fg">Needs you · approval</text>
<text x="300" y="63" class="mu">OMP · opus-4-6 ▾</text>
<rect class="f-sf s-bd" x="392" y="52" width="76" height="14" rx="5"/><rect class="f-sel" x="393" y="53" width="36" height="12" rx="4"/><text x="411" y="62" text-anchor="middle" class="fg med">Chat</text><text x="449" y="62" text-anchor="middle" class="mu">Terminal</text>
<text x="920" y="63" text-anchor="end" class="mu">⋯</text>
<rect x="548" y="88" width="332" height="30" rx="7" class="sf"/>
<text x="560" y="101" class="fg">Add a version field to the /health response and a GET</text><text x="560" y="112" class="fg">/items/:id route, then run the tests.</text>
<text x="320" y="142" class="serif fg">I will edit a file.</text>
<rect x="320" y="158" width="560" height="152" rx="7" class="sf"/>
<text x="334" y="176" class="mono fg">✎  edit: src/lib.rs</text><text x="866" y="176" text-anchor="end" class="mu">running…</text>
<text x="334" y="192" class="mu">No file contents were reported for this change.</text>
<line x1="334" y1="202" x2="866" y2="202" class="hl"/>
<text x="350" y="220" class="fg med">Apply these changes?</text>
<text x="350" y="234" class="mono mu">in ~/hutan-scratch/tour/acme-api  ·  /tmp/hutan-fixture/src/lib.rs</text>
<rect class="f-ac" x="350" y="246" width="82" height="20" rx="5"/><text x="360" y="259" class="med f-on">Allow once</text><text x="422" y="259" class="lab f-on">1</text>
<rect x="440" y="246" width="88" height="20" rx="5" class="btn"/><text x="450" y="259" class="fg">Always allow</text><text x="518" y="259" class="mu lab">2</text>
<rect x="536" y="246" width="60" height="20" rx="5" class="btn"/><text x="546" y="259" class="fg">Reject</text><text x="586" y="259" class="mu lab">3</text>
<rect x="604" y="246" width="90" height="20" rx="5" class="btn"/><text x="614" y="259" class="fg">Always reject</text><text x="684" y="259" class="mu lab">4</text>
<text x="350" y="286" class="mu">Always sets a standing rule for “edit: src/lib.rs” that Hutan cannot show or revoke.</text>
<rect x="320" y="498" width="560" height="68" rx="7" class="sf s-bd"/>
<text x="334" y="518" class="mu">Message the agent…</text>
<circle class="s-bd" cx="340" cy="548" r="8" fill="none"/><text x="340" y="551" text-anchor="middle" class="mu">＋</text>
<text x="356" y="551" class="mu">⌘. interrupts the turn</text>
<circle class="f-ac" cx="862" cy="548" r="9"/><rect class="f-on" x="858.5" y="544.5" width="7" height="7" rx="1"/>
<rect class="sb" x="933" y="24" width="27" height="560"/><line x1="933" y1="24" x2="933" y2="584" class="hl"/>
<text x="946.5" y="44" text-anchor="middle" class="mu">⑂</text><text x="946.5" y="72" text-anchor="middle" class="mu">◉</text><text x="946.5" y="100" text-anchor="middle" class="mu">⟲</text>
<text x="946.5" y="128" text-anchor="middle" class="mu">✋</text><rect x="949" y="112" width="12" height="10" rx="5" class="wn"/><text x="955" y="120" text-anchor="middle" class="lab f-on">1</text>
<text x="946.5" y="156" text-anchor="middle" class="mu">⋯</text>
<rect class="sb" x="0" y="584" width="960" height="16"/><line x1="0" y1="584" x2="960" y2="584" class="hl"/>
<path d="M12 589l4 7h-8z" class="wn"/><text x="22" y="596" class="fg">1 needs you</text><text x="948" y="596" text-anchor="end" class="mu">⌘⇧P</text>
</svg>

### 4.3 Agents page

**Before** (`dark-18-agents-page.png`, `light-18`, `dark-33-compact-agents-page.png`): semibold heading + count + `Start agent…` + four filter controls; a row that says Needs you, Working and Permission in three colors; the page outlined when focused; the right Agents panel a second, truncated copy.

**After:**
- Header (44 px): `Agents` 15 medium · `1 agent session · All workspaces` muted; right: `Start agent…` ghost. No robot icon beside the title (the tab has it).
- Toolbar (36 px): `All workspaces ▾` (ghost menu) · search field (hairline, placeholder `Search…` in `muted`) · `⋯` (Group by, Show finished). Folds to `⋯` below 640 pt.
- Section labels 11 medium `muted`: `Needs you · 1`, `Working · 1`, `Idle`, `Finished ▸` (collapsed). Rows 64 / 80 pt per chrome-v3 §2.3, hairline between rows, **one inset spine at 16 px**, lanes: state glyph (16) · identity + reason · location · harness/model · time (tabular, right) · action · ⋯.
- Needs-you row: `▲` (`state.warning`) · `omp · items endpoint` 13 medium · reason line `Approval · edit: src/lib.rs` 12 in `state.warning` · location `acme-api · Main checkout: feature/items-endpoint` muted · `OMP · opus-4-6` muted · `Running 3m 05s` · **Review request** (secondary) · `⋯`. **No `Working` word**: attention wins (chrome-v3 §2.2 precedence). Selected row: `accent.soft` fill, no outline; expanded request = the same approval group as the chat, inline under the row.
- Working row: `◌` (`circle-notch`, `accent` glyph — the only accent on the page besides a selection) · `Mawas · Assistant` · `State unavailable · no report since start` muted.
- The right Agents panel is the same row component at 240–480 px with location folded into details; its header is the 36 px panel header only, with the scope chips `Workspace · Session · All` as a segmented control.
- Accent marks: ≤ 2 (working glyph, selected row). Warning: 2 (row glyph + reason).

<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 960 600" width="960" height="600" font-family="-apple-system, system-ui, sans-serif" font-size="9" role="img" aria-label="Agents page, after">
<style>
/* light pack values; dark pack values under prefers-color-scheme: dark; restyle the classes to re-theme */
.f-bg{fill:#faf9f5}.s-bg{stroke:#faf9f5}.f-sb{fill:#f3f1ea}.s-sb{stroke:#f3f1ea}.f-sel{fill:#e9e6dc}.s-sel{stroke:#e9e6dc}.f-sf{fill:#ffffff}.s-sf{stroke:#ffffff}.f-hv{fill:#f0eee6}.s-hv{stroke:#f0eee6}.f-bd{fill:#e6e3d9}.s-bd{stroke:#e6e3d9}.f-fg{fill:#1f1e1d}.s-fg{stroke:#1f1e1d}.f-mu{fill:#66645d}.s-mu{stroke:#66645d}.f-ac{fill:#e0692e}.s-ac{stroke:#e0692e}.f-acs{fill:#fbe9df}.s-acs{stroke:#fbe9df}.f-on{fill:#1f1e1d}.s-on{stroke:#1f1e1d}.f-ok{fill:#2f7a48}.s-ok{stroke:#2f7a48}.f-wn{fill:#8a6300}.s-wn{stroke:#8a6300}.f-er{fill:#b93a26}.s-er{stroke:#b93a26}.f-scrim{fill:rgba(0,0,0,.28)}.s-scrim{stroke:rgba(0,0,0,.28)}.f-shadow{fill:rgba(0,0,0,.12)}.s-shadow{stroke:rgba(0,0,0,.12)}.bg{fill:#faf9f5}.sb{fill:#f3f1ea}.sel{fill:#e9e6dc}.sf{fill:#ffffff}.fg{fill:#1f1e1d}.mu{fill:#66645d}.ok{fill:#2f7a48}.wn{fill:#8a6300}.hl{stroke:#e6e3d9;stroke-width:1}.btn{fill:#ffffff;stroke:#e6e3d9}.mono{font-family:ui-monospace,Menlo,monospace;font-size:8.5px}.serif{font-family:'New York',Georgia,serif;font-size:10.5px}.med{font-weight:500}.lab{font-size:7.5px;font-weight:500}.ttl{font-size:11px;font-weight:500}.num{font-variant-numeric:tabular-nums}
@media (prefers-color-scheme:dark){.f-bg{fill:#262624}.s-bg{stroke:#262624}.f-sb{fill:#1f1e1d}.s-sb{stroke:#1f1e1d}.f-sel{fill:#2e2d2b}.s-sel{stroke:#2e2d2b}.f-sf{fill:#30302e}.s-sf{stroke:#30302e}.f-hv{fill:#3a3936}.s-hv{stroke:#3a3936}.f-bd{fill:#353533}.s-bd{stroke:#353533}.f-fg{fill:#f5f4ef}.s-fg{stroke:#f5f4ef}.f-mu{fill:#aeaca4}.s-mu{stroke:#aeaca4}.f-ac{fill:#ea7a43}.s-ac{stroke:#ea7a43}.f-acs{fill:#3e2c23}.s-acs{stroke:#3e2c23}.f-on{fill:#1f1e1d}.s-on{stroke:#1f1e1d}.f-ok{fill:#6bbf85}.s-ok{stroke:#6bbf85}.f-wn{fill:#e2ae45}.s-wn{stroke:#e2ae45}.f-er{fill:#f08c7a}.s-er{stroke:#f08c7a}.f-scrim{fill:rgba(0,0,0,.5)}.s-scrim{stroke:rgba(0,0,0,.5)}.f-shadow{fill:rgba(0,0,0,.4)}.s-shadow{stroke:rgba(0,0,0,.4)}.bg{fill:#262624}.sb{fill:#1f1e1d}.sel{fill:#2e2d2b}.sf{fill:#30302e}.fg{fill:#f5f4ef}.mu{fill:#aeaca4}.ok{fill:#6bbf85}.wn{fill:#e2ae45}.hl{stroke:#353533;stroke-width:1}.btn{fill:#30302e;stroke:#353533}}
</style>
<rect class="bg" width="960" height="600"/>
<rect class="sb" width="960" height="24"/><circle cx="12" cy="12" r="4" class="mu"/><circle cx="26" cy="12" r="4" class="mu"/><circle cx="40" cy="12" r="4" class="mu"/>
<text x="560" y="15.5" text-anchor="middle" class="fg">Agents</text>
<rect class="sb" x="0" y="24" width="187" height="560"/><line x1="187" y1="24" x2="187" y2="584" class="hl"/>
<rect class="f-sel" x="8" y="30" width="171" height="20" rx="6"/><text x="25" y="43" class="fg med">Agents</text><rect x="160" y="35" width="14" height="10" rx="5" class="wn"/><text x="167" y="43" text-anchor="middle" class="lab f-on">1</text>
<text x="25" y="63" class="fg med">Mawas</text>
<text x="10" y="88" class="mu lab">Projects</text><text x="156" y="88" class="mu">＋</text><text x="170" y="88" class="mu">⋯</text>
<text x="10" y="114" class="mu lab">Needs you</text><path d="M14 119l5 9h-10z" class="wn"/><text x="26" y="127" class="fg">omp · items endpoint</text>
<text x="30" y="152" class="fg med">▾ acme-api</text><text x="178" y="152" text-anchor="end" class="mu">5</text>
<text x="38" y="172" class="fg">api-shell</text><text x="38" y="192" class="fg">git</text><text x="38" y="212" class="fg">web</text><text x="38" y="232" class="fg">acme-api</text><text x="38" y="252" class="fg">omp · items endpoint</text><text x="38" y="272" class="fg">Mawas · acme-api</text>
<text x="12" y="575" class="fg">＋ Start</text><text x="168" y="575" class="mu">⚙</text><line x1="0" y1="559" x2="187" y2="559" class="hl"/>
<rect class="sb" x="187" y="24" width="746" height="23"/><line x1="187" y1="47" x2="933" y2="47" class="hl"/>
<text x="206" y="39" class="mu">❯ git</text><text x="246" y="39" class="mu">◆ omp · items endpoint</text>
<rect class="bg" x="358" y="28" width="52" height="20" rx="6"/><rect class="bg" x="358" y="40" width="52" height="8"/><text x="368" y="39" class="fg med">◉ Agents</text>
<text x="203" y="72" class="fg ttl">Agents</text><text x="248" y="72" class="mu">1 agent session · All workspaces</text>
<text x="920" y="72" text-anchor="end" class="fg">Start agent…</text>
<text x="203" y="100" class="fg">All workspaces ▾</text>
<rect x="290" y="88" width="180" height="18" rx="6" class="sf s-bd"/><text x="298" y="100" class="mu">Search…</text>
<text x="484" y="100" class="mu">⋯</text>
<line x1="203" y1="114" x2="920" y2="114" class="hl"/>
<text x="203" y="132" class="mu lab">Needs you · 1</text>
<rect class="f-acs" x="195" y="140" width="733" height="44" rx="6"/>
<path d="M208 154l5 9h-10z" class="wn"/>
<text x="226" y="156" class="fg med">omp · items endpoint</text>
<text x="226" y="169" class="wn">Approval · edit: src/lib.rs</text>
<text x="226" y="180" class="mu">acme-api · Main checkout: feature/items-endpoint  ·  OMP · opus-4-6</text>
<text x="740" y="156" text-anchor="end" class="mu num">Running 3m 05s</text>
<rect x="752" y="150" width="86" height="20" rx="5" class="btn"/><text x="796" y="163" text-anchor="middle" class="fg">Review request</text>
<text x="910" y="163" class="mu">⋯</text>
<line x1="203" y1="192" x2="920" y2="192" class="hl"/>
<text x="203" y="210" class="mu lab">Working · 1</text>
<path class="s-ac" d="M213 230a5 5 0 1 1-5-5" fill="none" stroke-width="1.5"/>
<text x="226" y="228" class="fg med">Mawas · Assistant</text>
<text x="226" y="241" class="mu">State unavailable · no state report since start</text>
<text x="226" y="252" class="mu">acme-api · Main checkout: feature/items-endpoint  ·  OMP · opus-4-6</text>
<text x="740" y="228" text-anchor="end" class="mu num">—</text>
<rect x="752" y="222" width="86" height="20" rx="5" class="btn"/><text x="796" y="235" text-anchor="middle" class="fg">Open session</text>
<text x="910" y="235" class="mu">⋯</text>
<line x1="203" y1="264" x2="920" y2="264" class="hl"/>
<text x="203" y="282" class="mu lab">Finished ▸</text>
<rect class="sb" x="933" y="24" width="27" height="560"/><line x1="933" y1="24" x2="933" y2="584" class="hl"/>
<text x="946.5" y="44" text-anchor="middle" class="mu">⑂</text><text x="946.5" y="72" text-anchor="middle" class="mu">◉</text><text x="946.5" y="100" text-anchor="middle" class="mu">⟲</text><text x="946.5" y="128" text-anchor="middle" class="mu">✋</text><rect x="949" y="112" width="12" height="10" rx="5" class="wn"/><text x="955" y="120" text-anchor="middle" class="lab f-on">1</text><text x="946.5" y="156" text-anchor="middle" class="mu">⋯</text>
<rect class="sb" x="0" y="584" width="960" height="16"/><line x1="0" y1="584" x2="960" y2="584" class="hl"/>
<path d="M12 589l4 7h-8z" class="wn"/><text x="22" y="596" class="fg">1 needs you</text><text x="948" y="596" text-anchor="end" class="mu">⌘⇧P</text>
</svg>

### 4.4 Settings

**Before** (`dark-26-settings-appearance.png`, `light-27-settings-light-theme-picker.png`, `dark-30`): no scrim; 15 flat sections; four lines of prose per setting plus layer and license lines; `Reset` beside every value; the picker inline with three outlined boxes and four actions; Problems as a growing footer.

**After** (visual only; the IA is `ST-5`'s):
- `ui.scrim` over the window; the sheet is elevation 3 (`surface`, shadow, radius 10), 880 × 600 pt, header/body/footer separated by hairlines.
- Left column 200 pt on `sidebar` tone: search field; sections grouped under 11 px labels (`General`, `Agents`, `Data`, `Advanced ▸`, per ST-5 §7); selected row `ui.sidebar.selected` fill, 30 px rows, no outline; keyboard focus = the focus ring, not an accent border.
- Right column: page title 15 medium (`Appearance`), `Reset page…` ghost at the right. Rows 56 px: **label 13 medium**, **one line** of description 12 `muted` (truncated with `…`; the full text in a tooltip and on focus), control at the right (select = secondary button with ▾, switch, field). A row whose value differs from the inherited layer shows a `You` chip (`counter` style) after the label and a ghost `↺` before the control; rows at their default show neither. No `Default`, no license line in the row (the license is in the picker's pack card).
- Pickers are **popovers** (elevation 2) anchored to the control: list on the left (pack name, id, origin chip), preview on the right, `⋯` for Open pack folder / Validate / Duplicate / Reset; Return applies, Escape cancels (TH-3 behavior unchanged). The highlighted pack is an `accent.soft` fill, `Current` is a `✓` in `fg`.
- Footer 32 px: `Problems · 3 ▸` as one muted line; click expands a scrolling list inside the sheet without resizing the body; each line is `file · key · reason` with the token id in mono.
- Accent marks: 0 at rest (there is no primary button on a settings page). Focus ring on the search field only while keyboard-focused.

<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 960 600" width="960" height="600" font-family="-apple-system, system-ui, sans-serif" font-size="9" role="img" aria-label="Settings sheet, after">
<style>
/* light pack values; dark pack values under prefers-color-scheme: dark; restyle the classes to re-theme */
.f-bg{fill:#faf9f5}.s-bg{stroke:#faf9f5}.f-sb{fill:#f3f1ea}.s-sb{stroke:#f3f1ea}.f-sel{fill:#e9e6dc}.s-sel{stroke:#e9e6dc}.f-sf{fill:#ffffff}.s-sf{stroke:#ffffff}.f-hv{fill:#f0eee6}.s-hv{stroke:#f0eee6}.f-bd{fill:#e6e3d9}.s-bd{stroke:#e6e3d9}.f-fg{fill:#1f1e1d}.s-fg{stroke:#1f1e1d}.f-mu{fill:#66645d}.s-mu{stroke:#66645d}.f-ac{fill:#e0692e}.s-ac{stroke:#e0692e}.f-acs{fill:#fbe9df}.s-acs{stroke:#fbe9df}.f-on{fill:#1f1e1d}.s-on{stroke:#1f1e1d}.f-ok{fill:#2f7a48}.s-ok{stroke:#2f7a48}.f-wn{fill:#8a6300}.s-wn{stroke:#8a6300}.f-er{fill:#b93a26}.s-er{stroke:#b93a26}.f-scrim{fill:rgba(0,0,0,.28)}.s-scrim{stroke:rgba(0,0,0,.28)}.f-shadow{fill:rgba(0,0,0,.12)}.s-shadow{stroke:rgba(0,0,0,.12)}.bg{fill:#faf9f5}.sb{fill:#f3f1ea}.sel{fill:#e9e6dc}.sf{fill:#ffffff}.fg{fill:#1f1e1d}.mu{fill:#66645d}.ok{fill:#2f7a48}.wn{fill:#8a6300}.hl{stroke:#e6e3d9;stroke-width:1}.btn{fill:#ffffff;stroke:#e6e3d9}.mono{font-family:ui-monospace,Menlo,monospace;font-size:8.5px}.serif{font-family:'New York',Georgia,serif;font-size:10.5px}.med{font-weight:500}.lab{font-size:7.5px;font-weight:500}.ttl{font-size:11px;font-weight:500}.num{font-variant-numeric:tabular-nums}
@media (prefers-color-scheme:dark){.f-bg{fill:#262624}.s-bg{stroke:#262624}.f-sb{fill:#1f1e1d}.s-sb{stroke:#1f1e1d}.f-sel{fill:#2e2d2b}.s-sel{stroke:#2e2d2b}.f-sf{fill:#30302e}.s-sf{stroke:#30302e}.f-hv{fill:#3a3936}.s-hv{stroke:#3a3936}.f-bd{fill:#353533}.s-bd{stroke:#353533}.f-fg{fill:#f5f4ef}.s-fg{stroke:#f5f4ef}.f-mu{fill:#aeaca4}.s-mu{stroke:#aeaca4}.f-ac{fill:#ea7a43}.s-ac{stroke:#ea7a43}.f-acs{fill:#3e2c23}.s-acs{stroke:#3e2c23}.f-on{fill:#1f1e1d}.s-on{stroke:#1f1e1d}.f-ok{fill:#6bbf85}.s-ok{stroke:#6bbf85}.f-wn{fill:#e2ae45}.s-wn{stroke:#e2ae45}.f-er{fill:#f08c7a}.s-er{stroke:#f08c7a}.f-scrim{fill:rgba(0,0,0,.5)}.s-scrim{stroke:rgba(0,0,0,.5)}.f-shadow{fill:rgba(0,0,0,.4)}.s-shadow{stroke:rgba(0,0,0,.4)}.bg{fill:#262624}.sb{fill:#1f1e1d}.sel{fill:#2e2d2b}.sf{fill:#30302e}.fg{fill:#f5f4ef}.mu{fill:#aeaca4}.ok{fill:#6bbf85}.wn{fill:#e2ae45}.hl{stroke:#353533;stroke-width:1}.btn{fill:#30302e;stroke:#353533}}
</style>
<rect class="bg" width="960" height="600"/>
<rect class="sb" width="960" height="24"/><rect class="sb" x="0" y="24" width="187" height="560"/><rect class="sb" x="933" y="24" width="27" height="560"/><rect class="sb" x="0" y="584" width="960" height="16"/>
<text x="38" y="63" class="mu">Mawas</text><text x="38" y="172" class="mu">api-shell</text><text x="38" y="192" class="mu">git</text>
<rect class="f-scrim" width="960" height="600"/>
<rect x="190" y="100" width="580" height="400" rx="7" class="sf"/>
<rect x="190" y="100" width="134" height="400" rx="7" class="sb"/><rect x="310" y="100" width="14" height="400" class="sb"/>
<line x1="324" y1="100" x2="324" y2="500" class="hl"/>
<rect x="198" y="110" width="118" height="18" rx="6" class="sf s-bd"/><text x="206" y="122" class="mu">Search settings</text>
<text x="200" y="146" class="mu lab">General</text>
<rect class="f-sel" x="196" y="152" width="122" height="20" rx="6"/><text x="204" y="165" class="fg med">Appearance</text>
<text x="204" y="186" class="fg">General</text><text x="204" y="206" class="fg">Keyboard</text><text x="204" y="226" class="fg">Terminal</text><text x="204" y="246" class="fg">Layout</text>
<text x="200" y="272" class="mu lab">Agents</text>
<text x="204" y="290" class="fg">Agents</text><text x="204" y="310" class="fg">Mawas</text><text x="204" y="330" class="fg">Accounts</text>
<text x="200" y="356" class="mu lab">Data</text>
<text x="204" y="374" class="fg">History</text><text x="204" y="394" class="fg">Data &amp; logs</text>
<text x="204" y="420" class="mu">Advanced ▸</text>
<text x="344" y="124" class="fg ttl">Appearance</text><text x="754" y="124" text-anchor="end" class="mu">Reset page…</text>
<line x1="324" y1="136" x2="770" y2="136" class="hl"/>
<text x="344" y="158" class="fg med">Density</text><rect class="f-hv" x="382" y="150" width="22" height="11" rx="5.5"/><text x="393" y="158.5" text-anchor="middle" class="mu lab">You</text>
<text x="344" y="171" class="mu">How much space rows, buttons and panels take…</text>
<text x="660" y="162" class="mu">↺</text><rect x="676" y="149" width="80" height="20" rx="5" class="btn"/><text x="686" y="162" class="fg">Comfortable</text><text x="748" y="162" text-anchor="end" class="mu">▾</text>
<line x1="344" y1="184" x2="756" y2="184" class="hl"/>
<text x="344" y="206" class="fg med">Color mode</text>
<text x="344" y="219" class="mu">Light or dark windows. System follows macOS and switches with it.</text>
<rect x="676" y="197" width="80" height="20" rx="5" class="btn"/><text x="686" y="210" class="fg">System</text><text x="748" y="210" text-anchor="end" class="mu">▾</text>
<line x1="344" y1="232" x2="756" y2="232" class="hl"/>
<text x="344" y="254" class="fg med">Light theme</text>
<text x="344" y="267" class="mu">The color theme of light windows…</text>
<rect x="676" y="245" width="80" height="20" rx="5" class="btn"/><text x="686" y="258" class="fg">Hutan Light</text><text x="748" y="258" text-anchor="end" class="mu">▾</text>
<line x1="344" y1="280" x2="756" y2="280" class="hl"/>
<text x="344" y="302" class="fg med">Dark theme</text>
<text x="344" y="315" class="mu">The color theme of dark windows…</text>
<rect x="676" y="293" width="80" height="20" rx="5" class="btn"/><text x="686" y="306" class="fg">Hutan Dark</text><text x="748" y="306" text-anchor="end" class="mu">▾</text>
<line x1="344" y1="328" x2="756" y2="328" class="hl"/>
<text x="344" y="350" class="fg med">Icon theme</text>
<text x="344" y="363" class="mu">The monochrome icon set…</text>
<rect x="676" y="341" width="80" height="20" rx="5" class="btn"/><text x="686" y="354" class="fg">Phosphor</text><text x="748" y="354" text-anchor="end" class="mu">▾</text>
<line x1="344" y1="376" x2="756" y2="376" class="hl"/>
<line x1="324" y1="476" x2="770" y2="476" class="hl"/>
<text x="344" y="491" class="mu">Problems · none</text>
</svg>

---

## 5. Slice plan

Dependency order; each slice is one brief with Cua screenshots (dark and light, 1440×900 and 1000×700, same scratch seed as the tour so before/after pairs compare) and tests. Effort: **S** ≤ 1 day, **M** 2–4 days (the plan's scale). Ids are `CL-<name>` so they do not collide with the merged `CL-1` (closing sessions). All desktop-only; no protocol change.

| # | Slice | Change | Files / crates | Acceptance | Effort | Overlaps → proposal |
|---|---|---|---|---|---|---|
| 1 | **`CL-TOKENS`** | The §3.2.2 diffs in both packs; `tokens.rs` "Used for" strings (`ui.accent.soft`, `ui.text.faint`) and the optional `ui.focus` token (D-C3); `docs/themes.md` regenerated; `docs/design.md` § Visual system palette and shape tables updated; `contrast.rs` gains the pairs of §3.2.3 (state text on bg/surface/sidebar/hover, `on-accent` on `accent`, muted on selected) so a built-in that stops passing fails a test. | `assets/themes/hutan-{dark,light}/theme.toml`, `crates/settings/src/themes/{tokens,contrast}.rs`, `docs/themes.md`, `docs/design.md` | T: `hutan theme validate hutan-dark --strict` and `hutan-light` print no `contrast` line; the new test lists every pair with its ratio. S: `dark/light-26` re-shot show dark text on the two primary buttons and the warm chrome unchanged. | S | `UX-POLISH-1` F18 → done here; `UX-UI-SCALE` unaffected |
| 2 | **`CL-PRIMITIVES`** | `ui.rs`: `card` = fill only (a new `outlined()` for inputs and secondary buttons); `button` Primary uses `on-accent`; `pill` → neutral; new `state_label(glyph, word, tone, on_selected)`; selected chip = `accent.soft` fill without border; `focus_ring()` keyboard-only; `empty_state` component (§3.5). Then every accent border of §2.12 except drag targets is replaced by the fill, the ring or nothing. A test greps `crates/desktop/src` for `border_color(theme.accent` / `border_color(accent` and fails outside an allowlist of drag-over sites (the same shape as the banned-words test). | `wb/ui.rs`, the 21 call sites of §2.12, `wb/style.rs` | T: the grep test; existing `ui` unit tests updated. S: `dark/light-12`, `-45`, `-27`, `-30` re-shot: no accent outline anywhere; `light-45` segmented control one silhouette. | M | `UX-POLISH-1` F19 (segmented) → here; `UX-CHAT-1` card styles → restyled, behavior unchanged |
| 3 | **`CL-CHROME`** | Title bar = title + recovery buttons (Layout and Overview to the View menu/palette); connected active tab, unfocused-pane dimming, hollow cursor in unfocused terminals; pane header → status line (D-C5); one hairline per boundary (`render.rs` tab strip / pane header / dock edges); status bar default items `[attention, connection-when-not-ok] / [palette_hint]`; right rail default `changes, agents, history` + `⋯`, Approvals surfacing with a count; the `default` preset and *Layout: Use new default*; the right sidebar on the sidebar tone (D-C2 is only a value; the token stays). | `wb/render.rs`, `wb/docks.rs`, `wb/panels/status.rs`, `wb/model/preset.rs`, `crates/desktop/src/app.rs` (View menu), `wb/terminals.rs` (cursor), `docs/design.md` § Layout and § Presets, parity record | T: preset golden (`desktop-state.json` v6 untouched; a new scratch home gets the clean preset); status-item registry test; focus test: focusing a pane changes only its tab's weight. S: `dark/light-10`, `-11`, `-36` re-shot: 0 accent marks on the terminal screen, 3 rail icons, 2 status items. | M | **absorbs `PR-CLEAN-DEFAULT`** (same files, same preset) and **`UX-PANE-CHROME`** (`UX-CHROME-1` card #14): the card's "focused pane visible" is met by the tab treatment, not a ring; zoom indicator and tab-strip overflow stay in #14. One owner for `render.rs`: run `CL-CHROME` as the second commit of `UX-CHROME-1`, or `UX-CHROME-1` first and `CL-CHROME` right after. |
| 4 | **`CL-SIDEBAR`** | Nav rows (Agents, Mawas while enabled; Tasks auto-reveal; Search removed), Projects header `＋ ⋯`, one-line session rows with the state lane and the conditional placement badge, Needs-you as a labeled group of the same rows, project-row ▷ on hover, workspace headers only when a workspace exists, recovery pills → one `state_label` per row + one summary card, text-only sidebar empties. | `wb/sidebars.rs`, `wb/panels/sessions.rs`, `sessions/tree.rs`, `wb/panels/tasks.rs`, `wb/recovery_summary.rs`, `docs/design.md` § Left sidebar | T: tree snapshot tests for the row model (subtitle present only when placement ≠ project); nav-row visibility tests (Tasks appears with a queued task). S: `dark/light-09`, `-11`, `-44`, `-49` re-shot: four sessions ≤ 120 px, no second line, one mascot per window. | M | `UX-POLISH-1` F8 (CTA dedupe), F17 (double selection) → here; product.md §3.4 rows 3, 4, 7 → here |
| 5 | **`CL-CHAT`** | Tool card and approval group per §4.2; one primary; the Always sentence + tooltips; neutral user bubble; status line and model menu; composer without accent border; answered state as one line; turn result once; the Requests/Approvals panel reuses the same approval group. | `wb/chat/{view,cards,input,question_card,widgets}.rs`, `wb/panels/inspector.rs`, `wb/ui.rs` (`approval_button_kind` → only AllowOnce primary), `docs/agents.md` § The desktop chat view | T: `approval_button_kind` test (one primary); card snapshot tests; the Always tooltip text equals the former paragraph. S: `dark/light-12`, `-20`, `-21`, `-15` re-shot: 2 accent marks, no nested borders, buttons on one row at 720 px and two rows at 470 px (`-37`). | M | `UX-CHAT-1` (merged) → restyle only; `UX-APPROVAL-CARD` semantics unchanged (no key but `1`–`4` or a click answers) |
| 6 | **`CL-AGENTS`** | Agents page header/toolbar/rows per §4.3 with the reducer precedence made visible (no `Working` in Needs you); the right Agents panel as the same row component; Mawas header fold and Today as the compact projection (§3.4); Mawas empty state (§3.5). | `wb/panels/agents_page.rs`, `wb/panels/agents_page/row.rs`, `wb/panels/agents.rs`, `wb/panels/assistant.rs`, `wb/panels/assistant_rail.rs`, `docs/design.md` § Agents page, `docs/assistant.md` § Desktop | T: row presentation test: a session with a pending approval renders state word `Needs you · approval` and never `Working`; Today renders no empty section. S: `dark/light-18`, `-16`, `-22`, `-33`, `-38`, `-39` re-shot: toolbar folds at 640 pt, composer never wraps per character. | M | **`UX-MAWAS-RESPONSIVE`** (`UX-CHROME-1` card #17) → the header fold and the Today collapse are its acceptance; `CL-AGENTS` supplies the visual spec, #17 the min-width logic. ADR 0017 d1 untouched (Today stays). |
| 7 | **`CL-SETTINGS`** | Scrim; grouped nav with selected fill; 56 px rows with one-line descriptions and the `You` chip / ghost `↺`; pickers as popovers; Problems footer as a one-line disclosure. | `wb/settings_sheet.rs`, `wb/settings_rows.rs`, `wb/settings_pages.rs`, `wb/settings_themes.rs`, `wb/settings_theme_preview.rs`, `wb/modal.rs`, `docs/design.md` § Settings sheet | T: sheet layout test (footer height constant with 0 / 3 / 30 problems); picker popover closes on Escape and restores the focus. S: `dark/light-26`, `-27`, `-30`, `-41` re-shot. | M | **becomes the visual spec of `ST-5`** (13-page IA, scope/provenance/reset-to-inherit): one brief, ST-5 owns IA and semantics, this section owns rhythm, tones and the picker surface; `UX-POLISH-1` F11/F12 parts → ST-5 |
| 8 | **`CL-EMPTY`** | `empty_state` everywhere (§3.5); launcher selected states as fills, segmented control one silhouette, footer sentence removed; wizard card with a fixed footer (F15); quit dialog primary = *Keep Running in Background*, destructive = `danger` secondary (F16); palette labels sentence case; `…` everywhere. | `wb/render.rs` (Welcome), `wb/panels/sessions.rs` (empty), `wb/panels/tasks.rs`, `wb/launcher_ui.rs`, `wb/setup.rs`, `wb/closing.rs`, `wb/palette.rs`, `wb/actions.rs` (labels), `docs/design.md` § First-run, § Daemon lifecycle | T: a banned-string test (`...`, Title Case verbs in palette labels, duplicate CTA ids per surface); wizard footer y constant across steps. S: `dark/light-01`…`-07`, `-08`, `-45`, `-46`, `-48` re-shot. | S–M | `UX-POLISH-1` F8, F15, F16, F19, part of F13 → here; **`PR-FIRST-RUN`** decides the two steps, `CL-EMPTY` styles the card either way |

**Merges proposed:** `PR-CLEAN-DEFAULT` → `CL-CHROME`; `UX-PANE-CHROME` acceptance → the `CL-CHROME` treatment; `UX-MAWAS-RESPONSIVE` + `CL-AGENTS` → one brief; `CL-SETTINGS` → `ST-5`; F9 (panel titles drawn twice) → `CL-CHROME`'s hairline pass; `UX-POLISH-1` keeps only F10 (Resources columns and `-0.0%`), F13 (keymap docs, palette vocabulary beyond casing) and F14 (clipped chips) — i.e. it shrinks to an S. Sequencing with the `docs/status.md` queue: `CL-TOKENS` and `CL-PRIMITIVES` can start now in a worktree (no overlap with `E-5`, `UX-PROTO-1`); `CL-CHROME` waits for `UX-CHROME-1` or absorbs it.

**Roll-out safety:** every slice keeps saved layouts, settings keys and panel ids; the parity record in `docs/design.md` lists each moved affordance (Layout menu, Overview button, Search row, Projects-header buttons, status items, Requests tabs) with its replacement. Packs: user packs that extend a built-in inherit the new values unless they override them; a user pack that set `ui.on-accent = "#ffffff"` keeps white text (and its own contrast warning).

---

## 6. Alternatives considered and rejected

1. **Keep the accent outlines, lower their alpha.** `render.rs` already does this (0.5–0.55 alpha). Quieter but still four rectangles saying "focused" four ways; the squint test (layout reference) still finds the orange box before the terminal. Rejected: the fix is structural, not tonal.
2. **Darker orange with white button text** (light `#b84e1c`, dark the same) to keep white labels: passes AA (4.86:1) but makes a muddy fill on the dark `bg` (3.4:1 as a component, fine) and splits the accent into "fill orange" and "text orange" everywhere links appear. The brand orange with dark text (recommended) keeps one `ui.accent` and matches the mascot. Available as D-C1(b).
3. **A second hue for attention** (gold `#F5B53D` from the brand, or a blue for *working*): tempting because warning (42°) and accent (20°) are neighbors; rejected because the confusion comes from *using accent as a state color*, not from the hues. With accent restricted to fills and warning to glyph+word they never compete, and the warm palette stays (`docs/design.md` "warm hues").
4. **Flatten to one neutral** (sidebars = center): calmer still, but the tab-strip-on-sidebar-tone trick (connected active tab) and the Orca-derived dock structure need a second tone. Two tones is the floor (operate.md "a second neutral layer for sidebars").
5. **Dissolve Today into the Agents page** (product.md row 1): the user kept Mawas with its Today; this spike makes Today the compact projection chrome-v3 §2.1 already defines, which removes the empty scaffolding without a product change.
6. **A "clean" preset beside `default`** instead of changing the default: would leave the first screen as it is; the parity rule plus *Layout: Use new default* already protects existing users.
7. **Rounder, softer shapes (12 / 16 radii, larger paddings) as "calm"**: the quieter reference warns that quiet without a point of view collapses to generic; Hutan's point of view is the terminal's precision, so radii go down to one 6 / 10 scale and calm comes from fewer marks, not softer ones.

---

## 7. Decisions for the user (five)

1. **D-C1 — Primary button text.** (a) *Recommended:* `ui.on-accent` becomes the dark ink (`#1f1e1d`) on the brand orange in both packs — AA in both modes, one accent token; (b) white text on a darker orange (`#b84e1c` light and dark) — AA, two oranges in light; (c) keep white on today's orange (3.37:1 / 2.85:1, fails AA; audit F18 stays open).
2. **D-C2 — Chrome tones.** (a) *Recommended:* two tones — the right sidebar takes the left sidebar's values (`ui.sidebar-right.*` keep existing as tokens, only the built-in values change); (b) keep Orca's three tones from the 2026-10-18 layout decision.
3. **D-C3 — Keyboard focus ring.** (a) *Recommended:* the macOS keyboard-focus color via an optional `ui.focus` token (system color when unset), ring only on keyboard focus; (b) a 2 px `accent` ring (2.98:1 on the light sidebar, otherwise AA), also keyboard-only.
4. **D-C4 — One shape scale.** (a) *Recommended:* 6 / 10 px in both densities (comfortable loses 8 / 12); (b) keep density-dependent radii.
5. **D-C5 — Pane header.** (a) *Recommended:* a status line without the title (the tab and window title name the item; the title returns when the tab strip is hidden); (b) keep the title in the header and only drop the accent text.

Everything else in this document is a recommendation the slices can implement without further input.

---

## 8. Not verified, risks

- **Nothing of the app was built or run.** The wireframes were rasterized once (light only) to check their own layout; the first `CL-PRIMITIVES` screenshot set is where optical corrections (hairline under the connected tab, glyph baselines, the 16 px spine) get tuned. Dark-mode hairlines at 7 % under a `bg`-filled active tab may need 9 % [INFERENCE].
- **Dark text on orange is a visible brand change** on two buttons per screen at most; it is reversible by one token in a user pack and by D-C1(b).
- **Connected tabs as the only focus cue** rely on an unfocused pane dimming its active tab and hollowing its cursor; a user with both panes showing chats (no cursor) sees only the weight change. `UX-CHROME-1` #14's zoom indicator stays as the second cue.
- **Contrast table scope:** the pairs of §3.2.3 (78 values) were computed for the two built-ins at the values in §3.2.2; user packs are judged by the existing advisory. The `ui::pill` rule change (neutral pills) is what makes the light success/warning pairs pass — if a slice keeps a tinted pill, that pair fails again, which the new `contrast.rs` test will report.
- **Mawas default enablement** is outside this spike (PR-FIRST-RUN); the layout is drawn with the row present because the user named Mawas a headline feature.
- **Screenshot coverage:** 31 of 94 shots were opened and read; the others are cited through INDEX descriptions only where noted "(INDEX)".
