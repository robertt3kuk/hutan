# UX-TERM-1 — Terminal lane: jump to latest, prompts stay clean through resizes (phase A)

Agent: implementer (Claude Sonnet 5.5)   Depends on: BLD-1 merged (test layout `tests/it/`)   Branch: `slice/UX-TERM-1`

## Context
Read `AGENTS.md`, `docs/design.md`, `docs/process.md` (§ Implementer agents → Resources, § Definition of Done, § UI checks). The **terminal half of #3 `UX-JUMP-LATEST`** and **#20 `UX-TERM-RESIZE` phase A** (reproduction against Terminal.app/Ghostty behaviour, a golden test of the daemon's reflow, and a written reflow decision; implement the fix only if it is small and clearly right, otherwise report the decision) of `docs/spikes/2026-10-21-ux-plan.md` §6; audit F2 in `docs/spikes/2026-10-21-ux-audit.md` with its reproduction recipe §7. Each card has problem, evidence (competitor issues), today's code with paths, scope and acceptance; treat it as the spec. Cross-check the cited lines before editing: they were read on master `4f33add`.

## Scope
`crates/terminal_view/src/*`, `crates/terminal/src/*` (emulator reflow) and its `tests/it/` corpus, `docs/design.md` § terminal, `docs/perf.md` if a bench changes. B4/B4b latency budgets must not regress.

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
