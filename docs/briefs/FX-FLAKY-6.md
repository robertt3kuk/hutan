# FX-FLAKY-6 — the swallowed click and the second Finder drop

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/FX-FLAKY-6`

**Contract ownership:** none.

## Context
Recurring gate flakes across many branches (2026-10-27/28): `staging_gpui::the_stage_pane_stages_and_unstages_single_hunks`, `staging_gpui::discarding_a_hunk_asks_first_and_undo_puts_it_back` (a click on a button that was just redrawn never reaches its handler under parallel load — UI-RHYTHM made the tests retry clicks, but whether it is a test-harness quirk or a real hit-test bug is open), `files_roots_gpui::files_dropped_from_finder_on_a_root_row_are_copied_into_that_project` (EX-8 noted "a second drop in one window never fired"), `agents_page_gpui::the_list_builds_only_the_rows_in_view…`, `history_gpui::the_history_panel_searches_filters_resumes_and_manages_the_index` (timeouts under load).

## Items (in commit order)
1. Reproduce each under load (`cargo nextest run -p hutan-desktop` with the same parallelism as `just check`, `--retries 0`, repeated) and find the root cause; for the swallowed click decide whether GPUI hit-testing uses stale bounds from the previous frame (a real bug a user could hit when a button moves under the pointer) — if real, fix it in the product (e.g. hit-test against the current frame / ignore a click on an element whose bounds changed since mouse-down) and prove it with a test; remove UI-RHYTHM's `click_until` retries once the cause is fixed.
2. Fix the Finder-drop second-drop problem (product or test harness).
3. Make the timing-dependent tests wait on state, not time; never raise timeouts as the fix.
4. Run each fixed test 20× under load with `--retries 0` and report the counts.

## Acceptance
Root causes written in the report and in comments where the fix lives; 20/20 runs per test under load; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(FX-FLAKY-6)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/fxflaky6/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
