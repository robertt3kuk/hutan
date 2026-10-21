# PR-FIRST-RUN — a first run that ends at the first value

Agent: implementer (Claude Sonnet 5.5)   Depends on: master   Branch: `slice/PR-FIRST-RUN`

## Context
Read `AGENTS.md`, `docs/spikes/2026-10-21-product.md` §2 (core loop), §4.1 (first 60 s), §4.2 (first-run keep/move/drop table) and bet card `PR-FIRST-RUN`; user decisions 2026-10-21: **two-step first run with just-in-time asks** (D-P3), **Mawas stays a headline feature** (D-P4: keep its nav row and page; enabling it is no longer a wizard step but Mawas is on by default), workspace hidden until created (D-P5); `docs/spikes/2026-10-21-community-opinions.md` (batch just-in-time asks once per project, never start background behaviour without consent; local-first, no account). Current wizard: `crates/desktop/src/workbench/setup.rs` (7 steps), `docs/design.md` § First-run setup, Welcome pane `workbench/render.rs` (only *Start…*).

## Scope
`crates/desktop` (setup, welcome/first screen, just-in-time offers: notifications permission on the first approval or finish while the window is not frontmost; hooks via the existing *Track as* strip; send-key line above the first composer; *Run setup again* keeps the old steps reachable), `docs/design.md` § First-run setup.
1. First screen (not a modal wizard): detected agents row (installed / log in / install), primary **Open a folder**, secondary **New terminal**, one line "Local only — no account".
2. After a folder is opened, the centre becomes the start composer for that project (Prompt + Agent; Options collapsed) — reuse the Start sheet's composer.
3. Dropped steps (Look, History, Background, Command line) move to the moments in product.md §4.2; nothing is written to `settings.toml` unless the user chooses.

## Acceptance (common)
Screenshots with Cua Driver per `docs/process.md` § UI checks (before the final gate); `just check` green (machine-wide lock); commit after each logical step; messages end `(PR-FIRST-RUN)`. Fresh `HUTAN_HOME` for every run; no `hutand` left.

## Parity
List every moved or removed affordance with its replacement (Definition of Done).

## Out of scope
`docs/status.md`, `docs/roadmap.md`; anything not named here.
