# FX-FLAKY-4 — the flaky tests seen in the 2026-10-22 gates

Agent: implementer (Claude Sonnet 5.5)   Depends on: FX-FLAKY-3 merged   Branch: `slice/FX-FLAKY-4`

**Contract ownership:** none. Test and product fixes only; no protocol change.

## Context
Read `AGENTS.md`, `docs/process.md` (tests wait on state, never on time), FX-FLAKY-3's commits (`git log --grep FX-FLAKY-3`: the reproduction method — run the one test N times at `-j 4` with `--retries 0`, dump state at the timeout, fix the cause, then 50/50).

Each of these failed at least once in a full `just check` on 2026-10-22 and passed on retry:

| Test | Seen |
|---|---|
| `hutan-daemon::it daemon::shutdown_replies_removes_socket_and_releases_lock` | failed twice in 0.05 s, passed on try 3 |
| `hutan-desktop::it chat_gpui::a_short_reconnect_during_an_approval_keeps_the_card_and_answers_afterwards` | 22 s timeout once (first gate after UX-QUEUE-STEER) |
| `hutan-terminal-view::it daemon::keys_mouse_and_paste_bytes_arrive_as_the_terminal_expects` | 20 s timeout once |
| `hutan-daemon::it control::a_plain_shell_sessions_cli_is_not_gated_while_an_agent_sessions_is` | once |
| `hutan-daemon::it checkpoint_turns::a_fixture_chat_gets_one_ref_per_turn_that_changed_files_and_none_for_a_read_only_turn` | once |

## Scope
For each test: reproduce under load, find the cause, fix it in the test (wait on the right state) or in the product when the product is wrong (say so in the commit). A test that cannot be made to fail in 200 runs gets its run count noted in the report and stays unchanged.

## Acceptance
Each fixed test passes 50/50 with `--retries 0` at `-j 4`; the commit message names the cause. `just check` green with no FLAKY for these tests; commit per test, messages end `(FX-FLAKY-4)`. Fresh `HUTAN_HOME`; no `hutand` left.

## Parity
None.

## Out of scope
`agents_page_gpui::*` (CL-AGENTS is changing that page); `docs/status.md`, `docs/roadmap.md`.
