# CL-AGENTS — Agents page and Mawas Today: quiet rows, visible reducer precedence, responsive headers

Agent: implementer (Claude Sonnet 5.5)   Depends on: CL-CHAT merged   Branch: `slice/CL-AGENTS`

## Context
Read `AGENTS.md`, `docs/design.md` § Visual system, `docs/themes.md`, and the design direction `docs/spikes/2026-10-21-clean-aesthetic.md` ("the quiet workbench"): §1 principles, §2 critique (screenshots under `~/hutan-scratch/tour/shots/`, index `~/hutan-scratch/tour/INDEX.md`), §3 system (tokens §3.2, drawing rules, accent budget ≤ 5 marks per screen, progressive disclosure §3.3, layout §3.4, empty states §3.5), §4 surfaces, and **§5 row `CL-AGENTS`** — that row is the spec (files, acceptance). Decisions D-C1…D-C5 are settled as the spike recommends (a): dark ink on the brand orange, two chrome tones, keyboard-only focus ring with an optional `ui.focus` token defaulting to the macOS focus color, one 6/10 radius scale in both densities, pane header as a status line without the title.

## Scope
`wb/panels/agents_page.rs`, `wb/panels/agents_page/row.rs`, `wb/panels/agents.rs`, `wb/panels/assistant.rs`, `wb/panels/assistant_rail.rs`, `docs/design.md` § Agents page, `docs/assistant.md` § Desktop. Absorbs `UX-MAWAS-RESPONSIVE` (no one-letter-per-line text in narrow panes; toolbar and Mawas header fold into ⋯ below 640 pt). Mawas stays a headline page (user decision D-P4) and leads with Needs you.

## Contract
No protocol change. Themes stay data (ADR 0018); no color literals in views. `docs/design.md` and `docs/themes.md` updated in the same commit as the behaviour. Parity per `docs/process.md`: list any moved affordance with its replacement.

## Acceptance
As spike §5 row `CL-AGENTS` (tests "T" and screenshots "S"). Screenshots with Cua Driver per `docs/process.md` § UI checks, re-shooting the matching `~/hutan-scratch/tour/shots/` files (same names, `-after` suffix) in dark and light, taken before the final gate. `just check` green (machine-wide lock). Commits end `(CL-AGENTS)`.

## Out of scope
Other CL-* rows; `core`/`protocol`; `docs/status.md`, `docs/roadmap.md`.

## Report
Summary, before/after screenshot pairs, contrast numbers, files, commands with results, open questions. Fresh `HUTAN_HOME` for every run; no `hutand` left.
