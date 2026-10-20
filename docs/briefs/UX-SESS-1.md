# UX-SESS-1 — Sessions lane: distinguishable names, history consent, lazy Mawas

Agent: implementer (Claude Sonnet 5.5)   Depends on: BLD-1 merged (test layout `tests/it/`)   Branch: `slice/UX-SESS-1`

## Context
Read `AGENTS.md`, `docs/design.md`, `docs/process.md` (§ Implementer agents → Resources, § Definition of Done, § UI checks). Cards **#5 `UX-SESSION-NAMES`**, **#6 `UX-HISTORY-CONSENT`**, **#11 `UX-MAWAS-LAZY`** of `docs/spikes/2026-10-21-ux-plan.md` §6 (audit findings F1, F5, F20 in `docs/spikes/2026-10-21-ux-audit.md`). Each card has problem, evidence (competitor issues), today's code with paths, scope and acceptance; treat it as the spec. Cross-check the cited lines before editing: they were read on master `4f33add`.

## Scope
`crates/daemon/src/registry.rs` (default titles; no wire change), `crates/desktop/src/workbench/panels/{sessions,history,assistant}.rs`, `wb/render.rs`/`wb/terminals.rs` for inline rename, the first-run wizard (`wb/setup.rs`), jump palette in `wb/actions.rs`, `crates/daemon/src/assistant.rs` for lazy start, `docs/design.md`, `docs/assistant.md`, `docs/history-index.md`.

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
