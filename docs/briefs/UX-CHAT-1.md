# UX-CHAT-1 — Chat lane: safe approval cards, context meter, durable drafts, jump to latest

Agent: implementer (Claude Sonnet 5.5)   Depends on: BLD-1 merged (test layout `tests/it/`)   Branch: `slice/UX-CHAT-1`

## Context
Read `AGENTS.md`, `docs/design.md`, `docs/process.md` (§ Implementer agents → Resources, § Definition of Done, § UI checks). Cards **#2 `UX-APPROVAL-CARD`**, **#4 `UX-CONTEXT-METER`**, **#12 `UX-DRAFTS`**, and the **chat half of #3 `UX-JUMP-LATEST`** of `docs/spikes/2026-10-21-ux-plan.md` §6, in that order (they share `wb/chat/view.rs` / `cards.rs`, so one agent serializes them). Each card has problem, evidence (competitor issues), today's code with paths, scope and acceptance; treat it as the spec. Cross-check the cited lines before editing: they were read on master `4f33add`.

## Scope
`crates/desktop/src/workbench/chat/*`, `crates/desktop/src/workbench/persistence.rs` / `model/persist.rs` (drafts), `crates/agents/src/acp/mapper.rs` only for extra approval detail fields as card #2 says, `crates/chat-view` if the transcript model needs it, `docs/design.md`, `docs/agents.md` where approvals are described.

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
