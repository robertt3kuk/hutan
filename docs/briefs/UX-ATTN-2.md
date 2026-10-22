# UX-ATTN-2 — “Requests” instead of “Needs you”: only blocking items, separate Failed and Ready for review

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/UX-ATTN-2`

**Contract ownership:** none (stop and report if a wire change seems needed).

## Context
The user (2026-10-22) asked to call attention differently, both the name and the way. Evidence: `docs/spikes/2026-10-22-ui-polish/attention.md` (most tools keep finished work out of the blocking group; complaints target the mixing; recommendations 1, 2, 8, 9). Decision (orchestrator): the blocking group is named **Requests** — the word Hutan's right-rail *Requests* panel already uses for approvals — so one name means one thing everywhere.

## Items (in commit order)
1. **Model**: `sessions/presentation.rs` / `attention.rs`: three groups — **Requests** (approval, question, pending plan only), **Failed** (failed turn/agent, own error word), **Ready for review** (finished work not yet seen; FX-REVIEW's seen rule clears it). A turn end alone never makes a Request; subagent/background waits never do. Migrate every caller (sidebar groups, Agents page and rail, Mawas Today, status bar, palette, notifications source, tests).
2. **Words**: group headers `Requests · N`, `Failed · N`, `Ready for review · N`; row state words `Approval`, `Question`, `Plan to approve`, `Failed`, `Ready for review`, each with its own glyph shape (not colour only). Status bar: `▲ N requests` (blocking only, `99+` cap); nothing for review-only. The right-rail *Requests* panel lists exactly the Requests group.
3. **Clear on view**: opening a session marks its failed/finished items seen (Ready for review drops after seeing per FX-REVIEW; Failed stays until a new turn or dismiss).
4. Docs: `docs/design.md` (attention section, every surface), `docs/assistant.md` (Today), glossary line in `docs/architecture.md` if it names "Needs you".

## Acceptance
GPUI tests: an approval/question puts the session in Requests and the status bar counts it; a finished turn goes to Ready for review and never counts; a failed turn goes to Failed; opening the session clears what the rules say; no surface shows the words "Needs you" (grep test over `crates/desktop/src` strings). Screenshots: sidebar with all three groups, status bar, Agents page.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks), `docs/spikes/2026-10-21-clean-aesthetic.md`, `docs/design.md`. Commit after every item (messages end `(UX-ATTN-2)`); update existing tests to the new spec keeping what they prove; Cua screenshots dark + light before/after into `~/hutan-scratch/uxattn2/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
