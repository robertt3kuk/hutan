# FX-WAVE-A — Mawas and board findings from the 2026-10-31 user test

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/FX-WAVE-A`

**Contract ownership:** none (no protocol change expected).

## Context
Usability test of the 2026-10-30/31 wave on a bundle of master (report kept outside the repo). Findings to fix, verbatim:

### F1 (major) approval card for Mawas's `ask_suggest` shows only a raw ULID
Steps: Mawas asked to suggest an answer to a waiting question → card "Mawas asks Hutan to suggest an answer to the waiting question 01M675AQMHEJN3XQRSHXMXA54C (you decide)" `sg01`. Expected: the question text/agent and the suggested answer+reason (as the message card shows target+text). Actual: user approves blind. Area: approval cards / assistant.


### F2 (major) `agents_board` drops the error text of a failed terminal agent
perf-bench reported `state=error msg="cargo test failed: 3 tests in parser::tests"`; sidebar and `hutan ls` show it, but `hutan agents --board --json` has `"detail":"The main agent failed"` only, so Mawas answered "No cause is recorded" `ask05`. Expected: same message as the sidebar (it is redacted agent text, labelled untrusted). Area: daemon board / agents_board.


### F3 (minor) board `model` of a Mawas chat is stale after Switch model
`hutan agents --board --json` says Mawas `model: anthropic/claude-sonnet-5-5` while the chat runs (and `chat insight` says) `claude-haiku-5-5`, chosen via the header menu. Mawas/MCP clients read the wrong model. Area: board/session info.


### F4 (minor) "hutand restarted just now" banner after `hutan reopen`
`hutan reopen <chat>` of a closed session made the desktop show "hutand restarted just now · 1 chat resumed" `tc03`, though the daemon had run 10 min uninterrupted (`ps etime`). Misleading wording from the recovery report. Area: recovery notice.


### F11 (subset)
- Recent shows raw event names: "task work awaiting_review", "task work none".
- Recent: "Changes: 1 files".
- Flow "Tokens by project: Home 74.1k / 525.9k" counts cache reads+writes per turn; the chat footer shows "27.2k tokens" for the same turn – two different numbers for tokens.
- Terminal blocked prompt shows as "Approval" in the Now tab (it is a question).
- Mawas approval cards repeat "Mawas asks" three times (header, sentence, label).

## Change
Fix each at the root (no protocol change expected; if one is needed, report first). For tokens: pick one definition shown everywhere (footer and Flow) or label each number so they can't be confused — say which. For F4, find why a reopen is reported as a daemon restart. For F3, find where the board reads the model and why it misses Switch model.

## Acceptance
Each finding reproduced before the fix, a regression test where a consumer-visible behaviour is involved (GPUI tests: `common::click_at`, wait on state), Cua screenshots dark + light for the visible ones; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(FX-WAVE-A)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/fxwavea/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
