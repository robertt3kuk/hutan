# UX-TERM-2 — Terminal: clickable paths and URLs, drop and paste safety, resize phase B

Agent: implementer (Claude Sonnet 5.5)   Depends on: master (wave 1 merged)   Branch: `slice/UX-TERM-2`

## Context
Read `AGENTS.md`, `docs/design.md`, `docs/process.md` (§ Implementer agents → Resources, § Definition of Done, § UI checks). Cards **#10 `UX-LINKS`**, **#25 `UX-TERM-DROP-PASTE`**, and **#20 `UX-TERM-RESIZE` phase B** (the open list in `docs/spikes/2026-10-21-term-resize.md`) of `docs/spikes/2026-10-21-ux-plan.md` §6, in that order. ⌘-click on `path:line:col` opens the file in the system editor for now (the embedded editor arrives with E-5/E-10); make the opener one function the editor can take over. Each card has problem, evidence (competitor issues), today's code with paths, scope and acceptance; treat it as the spec. Cross-check cited lines before editing: they were read on master `4f33add` and wave 1 has landed since.

## Scope
`crates/terminal_view/src/*`, `crates/terminal/src/*` and its `tests/it/` corpus, `crates/desktop` only for the opener hook and drop overlay wiring, `docs/design.md`, `docs/shell-integration.md`. B4/B4b must not regress (run `just bench --only B4` once at the end, alone).

## Contract
- No protocol change unless the card says so; if a card turns out to need one, stop and report (the orchestrator owns `core`/`protocol`).
- Follow `docs/design.md` (visual system, tokens from theme packs, no color literals) and `docs/process.md` § Definition of Done incl. **Parity** and **UI checks**.

## Acceptance
- Every card's acceptance (tests "T" and screenshots "S") in `docs/spikes/2026-10-21-ux-plan.md`. Screenshots: drive a debug build on a scratch `HUTAN_HOME` with Cua Driver per `docs/process.md` § UI checks (history index off; deny any permission prompt; never touch other apps). Put shots under `~/hutan-scratch/<slice>/` and list them in the report.
- `docs/design.md` (and other user docs the card names) updated in the same commit as the behaviour.
- Iterate with targeted `cargo nextest run -p <crate> --test it -E 'test(/^<module>::/)'`; run `just check` once at the end (it waits on the machine-wide gate lock). One commit per card on the slice branch, conventional messages ending `(<card id>)`.

## Parity
List any affordance moved or removed, with its replacement, in the report (expected: none).

## Out of scope
Other cards of the plan; `core`/`protocol` edits; `docs/status.md`, `docs/roadmap.md`.

## Report
Summary per card, files changed, commands with results (test counts), screenshot paths, open questions. Every `hutan`/`hutand`/`hutan-desktop` run with `HUTAN_HOME=<temp>`; no `hutand` left (`pgrep -x hutand`).
