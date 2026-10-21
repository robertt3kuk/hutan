# CL-CHROME — chrome: title bar, connected tabs, status line, one hairline, quiet status bar, 3-icon rail

Agent: implementer-opus (Claude Opus 5.5)   Depends on: CL-PRIMITIVES merged   Branch: `slice/CL-CHROME`

## Context
Read `AGENTS.md`, `docs/design.md` § Visual system, `docs/themes.md`, and the design direction `docs/spikes/2026-10-21-clean-aesthetic.md` ("the quiet workbench"): §1 principles, §2 critique (screenshots under `~/hutan-scratch/tour/shots/`, index `~/hutan-scratch/tour/INDEX.md`), §3 system (tokens §3.2, drawing rules, accent budget ≤ 5 marks per screen, progressive disclosure §3.3, layout §3.4, empty states §3.5), §4 surfaces, and **§5 row `CL-CHROME`** — that row is the spec (files, acceptance). Decisions D-C1…D-C5 are settled as the spike recommends (a): dark ink on the brand orange, two chrome tones, keyboard-only focus ring with an optional `ui.focus` token defaulting to the macOS focus color, one 6/10 radius scale in both densities, pane header as a status line without the title.

## Scope
`crates/desktop/src/workbench/render.rs` (title bar, tab strip, pane header → status line per D-C5, hairline rules, unfocused-pane dimming, hollow cursor hook), `wb/panels/status.rs` (default items), `wb/model/preset.rs` (the `default` preset: right rail Changes, Agents, History + ⋯, Approvals surfacing with a count; *Layout: Use new default* offered once for existing layouts; new installs get it), `crates/terminal_view` only for the hollow-cursor-when-unfocused hook, `docs/design.md`. Absorbs `PR-CLEAN-DEFAULT` and `UX-PANE-CHROME` (zoom indicator stays).

## Contract
No protocol change. Themes stay data (ADR 0018); no color literals in views. `docs/design.md` and `docs/themes.md` updated in the same commit as the behaviour. Parity per `docs/process.md`: list any moved affordance with its replacement.

## Acceptance
As spike §5 row `CL-CHROME` (tests "T" and screenshots "S"). Screenshots with Cua Driver per `docs/process.md` § UI checks, re-shooting the matching `~/hutan-scratch/tour/shots/` files (same names, `-after` suffix) in dark and light, taken before the final gate. `just check` green (machine-wide lock). Commits end `(CL-CHROME)`.

## Out of scope
Other CL-* rows; `core`/`protocol`; `docs/status.md`, `docs/roadmap.md`.

## Report
Summary, before/after screenshot pairs, contrast numbers, files, commands with results, open questions. Fresh `HUTAN_HOME` for every run; no `hutand` left.
