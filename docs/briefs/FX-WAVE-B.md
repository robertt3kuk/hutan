# FX-WAVE-B — tab, settings and layout findings from the 2026-10-31 user test

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/FX-WAVE-B`

**Contract ownership:** none (no protocol change expected).

## Context
Usability test of the 2026-10-30/31 wave on a bundle of master (report kept outside the repo). Findings to fix, verbatim:

### F5 (minor) Undo toast lives only ~4–6 s
Measured by polling `toast_0..7.png` (present at 2 s/4 s, gone by 6 s). Closing a chat with a long conversation then can't be undone after reading the screen; the first Undo I tried (≈20 s) was gone, I had to `hutan reopen`. Suggest longer lifetime or a reopen entry in ⌘K/sidebar "Recently closed". Area: tabs/toast.


### F7 (minor) Settings modal does not take keyboard focus; Esc goes to the terminal under it
With Settings open (opened by click) and a font popover closed, Esc did not close Settings and two `^[` landed in the focused terminal behind (`pl01` shows `^[^[`). Area: settings sheet focus. [INFERENCE: other keys may leak too; only Esc tested.]


### F8 (minor) "Side preview" with an agent beside it makes three cramped columns
Agent split + file + "To the Side" gave 530/270/260 px, then after closing/reopening the preview 710/175/175 px `md05`, `set15`; the preview is unreadable at 175 px. The file pane is split in half again instead of using more of the window; the pane widths also changed by themselves. Area: split layout.


### F9 (minor) first paint after hiding the right panel is stale
Clicking the title-bar right-panel toggle while Mawas was open: Mawas page showed no Today button/rail (`m02`, page already 980 pt wide) until the next input (a menu open, then `m04` shows both). [INFERENCE: missed invalidate on layout change; re-test.]


### F12 (observation, not judged) closing a tab activates and opens an unrelated agent's session
After closing the ut-note chat, `fix-toc` (needs-you terminal) appeared as a new tab and became active `tc01`; after closing busy-shell, `regex-tidy` did the same `pl07`. Possibly intended (attention order) but unexpected, and the sidebar project row collapsed/expanded inconsistently when selecting sessions (`pl05/06`).

## Cleanup
Desktop (pid 6186) killed, `hutan daemon stop --stop-sessions` run, no process of the bundle/scratch left (checked with ps). Scratch repos left under repos/ (book has `ut-preview.md`, edited unsaved `ut-note.md`).

### F11 (subset)
- Font picker preview line shows ".SystemUIFont" for System default `set02`.
- Tab menu has both "Close session…", "Close Tab, Keep Running" and "Close" `pl12` – "Close" vs "Close session…" is unclear.
- Light theme: Today rail background nearly equals the page, weak separation `light04`.

## Change
Fix each at the root. F5: keep the Undo toast long enough to read (≥ 10 s, paused on hover) and make closed sessions reopenable from ⌘K (*Reopen Closed Tab*, ⇧⌘T if free). F12: closing a tab must activate a neighbour tab, never open a session that wasn't open; check the sidebar expand/collapse noted there. F8: the side preview of a file next to an agent takes space from the file's pane sensibly and never below a readable minimum; pane widths must not change by themselves. F7: the Settings sheet owns keyboard focus while open (Esc closes it, nothing leaks to the terminal). F11 tab menu: one clear naming for the close choices.

## Acceptance
Each finding reproduced before the fix, a regression test where a consumer-visible behaviour is involved (GPUI tests: `common::click_at`, wait on state), Cua screenshots dark + light for the visible ones; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(FX-WAVE-B)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/fxwaveb/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
