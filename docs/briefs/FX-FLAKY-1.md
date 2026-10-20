# FX-FLAKY-1 — make the gate deterministic: four flaky tests

Agent: implementer (Claude Sonnet 5.5)   Depends on: master `9664cc8`   Branch: `slice/FX-FLAKY-1`

## Context
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents → Resources, § Testing strategy), `.config/nextest.toml` (retries hide flakes; nextest still reports them as FLAKY). The user wants builds that pass reliably. Recent `just check` runs on master-equivalent trees reported these as FLAKY (fail, then pass on retry):

| Test (binary `it`) | Seen | First-try failure time |
|---|---|---|
| `hutan-desktop` `review_comments_gpui::send_to_agent_is_one_chat_message_with_the_comments_a_file_range_and_the_hunk` | 3 runs, once needed the third try | ≈ 1.1 s |
| `hutan-desktop` `recovery_gpui::the_chat_header_says_the_turn_was_interrupted_and_retries_it` | after UX-CHAT-1 (chat header chips) | ≈ 1.0 s |
| `hutan-desktop` `setup_gpui::logging_in_pauses_the_setup_and_the_palette_continues_it` | after UX-SESS-1 (7-step wizard) | ≈ 1.0 s |
| `hutan-daemon` `assistant_backend::an_ask_before_acting_claude_assistant_runs_in_its_default_permission_mode` | after UX-MAWAS-LAZY | 0.05 s |

Fast failures (≈ 1 s, 0.05 s) point to ordering/race bugs, not slow machines.

## Scope
Tests and, where the race is in product code, the product code (`crates/desktop`, `crates/daemon`). For each test: reproduce with `cargo nextest run -p <crate> --test it -E 'test(=<name>)' --retries 0` in a loop (e.g. 30 runs, and again with `stress`-style parallel load such as running the whole module with `-j 4`); read the failure output; find the root cause; fix it so the test is deterministic. Never relax an assertion, add sleeps, or raise timeouts to hide a race; wait on the event/state the test means. If the race is real product behaviour (e.g. a header that briefly shows a wrong state), fix the product and say so.

## Acceptance
- Each of the four passes 50 consecutive runs with `--retries 0`, and its module passes 10 runs with `-j 4`.
- Report per test: root cause (file:line), fix, run counts.
- `just check` green once at the end (machine-wide lock). One commit per test, conventional messages ending `(FX-FLAKY-1)`.

## Parity
None.

## Out of scope
Other tests, retries config, `core`/`protocol`, `docs/status.md`, `docs/roadmap.md`.

## Report
As above, plus any other test that showed as FLAKY in your runs (name it; do not fix unless trivial).
