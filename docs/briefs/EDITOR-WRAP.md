# EDITOR-WRAP — soft wrap in the editor

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/EDITOR-WRAP`

**Contract ownership:** none (code_view + desktop; no protocol).

## Context
User 2026-10-27 asked to evaluate how workable Hutan is as the editor inside; the workability report `docs/spikes/2026-10-27-workability.md` ranks **F-06 no soft wrap** in the top 5 blockers (the editor viewport is ~650 px with both docks open; Markdown, TOML and prose are unreadable). `crates/code_view` has no soft wrap today.

## Items (in commit order)
1. **Display map**: a wrap layer in `code_view` mapping buffer lines ↔ display rows for a given wrap width (break at whitespace/punctuation, hard break long tokens; wide characters and tabs counted correctly; CJK/Cyrillic), incremental update on edits (only affected lines re-wrapped), wrap indent (continuation rows keep the line's indent, optional extra indent).
2. **Rendering and hit-testing**: paint wrapped rows; the gutter shows a line number only on the first row of a line; caret, selections, find matches, diagnostics squiggles, inline blame, hover anchors and the minimap/scroll extents use display rows; mouse hit-testing maps back to buffer positions.
3. **Movement**: ↑/↓ move by display row keeping the goal column; Home/End go to the display row's start/end first, then the line's (VS Code behaviour); Vim: `j`/`k` stay logical, `gj`/`gk`/`g0`/`g$` move by display row (Vim's documented behaviour; `docs/editor.md` § Vim table); page up/down by display rows.
4. **Settings**: `editor.soft_wrap = off | viewport | column` with `editor.wrap_column` (default 100), default **viewport for Markdown and plain text, off for code** (`languages.<lang>.soft_wrap` overrides); *View → Toggle Soft Wrap* (⌥Z, the VS Code key; check keymap conflicts) per buffer; status-bar indicator when on.
5. Docs: `docs/editor.md` (wrap section + Vim rows), `docs/design.md` settings sample; perf: keystroke latency budget (B4) unchanged on a 10k-line Markdown file with wrap on — run `just bench --only B4` before/after and record.

## Acceptance
Unit tests for the display map (edits at wrap boundaries, wide chars, tabs, very long tokens, empty lines, resize); GPUI tests: wrapped rendering rows count, caret ↑/↓ across wrapped rows, Home/End, selection across a wrap, click on a continuation row, Vim `gj`/`gk`, toggle per buffer, per-language default; Cua screenshots on `~/hutan-scratch/e2e6/repos/book` Markdown (dark + light) at 1280×800 with both docks open; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(EDITOR-WRAP)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/editorwrap/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
