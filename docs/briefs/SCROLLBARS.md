# SCROLLBARS — a scrollbar on every scroll surface

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/SCROLLBARS`

**Contract ownership:** none (desktop UI crates; no protocol).

## Context
User 2026-10-27: "scroll down must work correctly". Workability report `docs/spikes/2026-10-27-workability.md` F-07: no scrollbar on any surface (a 1 500-row tree, 1 000-row search result, 950-line file, the chat — no thumb even right after scrolling). FX-CHANGES-VIEW added `ui::Scrollbar` (draggable thumb; `crates/desktop/src/workbench/ui/scrollbar.rs`) and EX-1 put it on the Files tree; FX-WORK-1 added `ui::scroll` wheel rules (`shield_behind`, `scroll_y_contained`).

## Items (in commit order; each with a regression test)
1. **One scrollbar behaviour** (macOS-like): overlay thumb that appears while scrolling and on hover near the edge, fades after ~1 s, draggable, click in the track pages; follows the system "Show scroll bars" setting (`NSScroller.preferredScrollerStyle`: overlay vs always) and a Hutan override `appearance.scrollbars = system|always|scrolling`; theme tokens for thumb/track (`docs/themes.md`). Move the component to a place `code_view`/`terminal_view` can use too (they can't depend on `desktop`): a small shared UI crate or `hutan-ui` module the UI crates already share — check what exists before adding a crate.
2. **Editor** (`code_view`): vertical thumb (with soft wrap: display rows), horizontal when not wrapping; git/diagnostic/find **markers on the vertical track** (VS Code overview ruler-lite: colored ticks; cheap — computed per buffer change, not per frame).
3. **Terminal** (`terminal_view`): thumb over scrollback, hidden on the alternate screen; dragging scrolls the daemon-backed scrollback.
4. **Lists**: Search results, Problems, chat transcript and Mawas, Agents page, History, Settings sheet pages, palette, quick open, Start sheet lists, Memory panel — every `overflow_y_scroll`/`uniform_list` surface gets it (grep and list them in the report).
5. Docs: `docs/design.md` (§ Scrolling), `docs/themes.md` tokens.

## Acceptance
GPUI tests: thumb size/position reflect content (top/middle/end), drag to the end reaches the last row (editor, terminal scrollback, chat, search), track click pages, fade, `always` mode; markers on the editor track; unit test for the system-setting mapping; Cua screenshots (dark + light) on `~/hutan-scratch/e2e6/repos/ripgrep` / `book`; `just check` green; B4 keystroke bench unchanged (run before/after, record).

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(SCROLLBARS)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/scrollbars/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
