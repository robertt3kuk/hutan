# FX-WORK-2 — fixes from the second workability evaluation

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/FX-WORK-2`

**Contract ownership:** none (desktop; no protocol).

## Context
Second evaluation `docs/spikes/2026-10-28-workability-2.md` (user 2026-10-27: "evaluate how workable it is"; screenshots `~/hutan-scratch/e2e7/shots/` by number). Fix its top-10 and P3 list. EX-8 (explorer roots/worktrees, Finder drop) runs in parallel in the Files panel's scope chip and drop code — coordinate via `write agent://ExplorerRoots` before touching `panels/files.rs` shared parts.

## Items (in commit order; each with a regression test)
1. **N-01 first run**: a fresh layout shows the right dock **open on Files** when the window is wide enough (else the rail peek applies); rail icons get hover labels and a one-time hint pointing at Files; verify with `hutan ui layout` on a fresh home that `right` is visible.
2. **N-03 preview tabs**: a single click in the tree opens a **preview tab** (italic title, replaced by the next single click; double-click, an edit or pinning makes it permanent — VS Code/Zed behaviour; `editor.preview_tabs = true`), Open Editors lists the preview once and never shifts rows during a ⇧/⌘ selection (modifier clicks don't open); Open Editors caps at ~6 rows with its own scroll and marks the active file (N-13).
3. **N-02 filter relevance**: exact basename > basename prefix > basename substring > path match, then shorter path; same scorer for ⌘P where it fixes N-12 (`src/…` before `second-edition/src/…` for a query starting with `src`).
4. **F-05 residue**: a root row at the top of the tree (folder name, path tooltip, its menu), *Explorer left* also in the Files ⋯ menu.
5. **F-09 residue**: blame shows the author's short name (not truncated initials when there is room) and no blank rows inside a run.
6. **N-04** left dock resize handle as wide as the right one; **N-05** Collapse all / Refresh scroll to top; **N-06** rename-collision message under the edited row; **N-07** tooltips hide on scroll and never draw above sheets; **N-08** no blank band after dismissing the LSP bar; **N-09** narrow panes: *Wrap lines* and the chat header model collapse into ⋯ instead of clipping; **N-10** counts update promptly after mass deletes (coalesced refresh on `fs.changed` bursts); **N-11** wheel step for lists = 3 lines per notch scaled by row height like macOS (check `NSEvent.scrollingDeltaY` handling), search summary not cut.
7. Docs: `docs/design.md`, `docs/editor.md`.

## Acceptance
GPUI tests per item; Cua screenshots (dark + light) into `~/hutan-scratch/fxwork2/shots/`; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(FX-WORK-2)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/fxwork2/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
