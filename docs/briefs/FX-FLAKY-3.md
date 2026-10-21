# FX-FLAKY-3 — gate hygiene: recurring flakies, leaked test daemons, History focus

Agent: implementer-opus (Claude Opus 5.5)   Depends on: master   Branch: `slice/FX-FLAKY-3`

## Context
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents → Resources, § Testing strategy), `docs/briefs/FX-FLAKY-1.md` and `FX-FLAKY-2.md` with their outcomes in `docs/status.md` (partial-line fixture writes, storage writer races, flood test). Same rules: root-cause, fix deterministically; never relax an assertion, add sleeps or raise timeouts to hide a race.

## Scope
1. **Recurring FLAKY in gates of 2026-10-22** (each failed on first try, passed on retry):
   - `hutan-desktop` `perf_gpui::a_store_notification_that_changes_nothing_they_show_renders_no_panel` (≈ 2 s; seen in 4 gates);
   - `hutan-syntax` `background::a_request_the_worker_has_not_started_is_replaced` and `background::awaited_edits_each_publish_and_equal_a_fresh_parse` (publish-count assertions);
   - `hutan-desktop` `tasks_gpui::the_sheet_creates_a_task_with_dependencies_and_a_worktree_placement` and `tasks_gpui::the_sheet_asks_for_a_prompt_and_shows_the_daemons_refusal`;
   - `hutan-daemon` `notify::a_finished_chat_turn_raises_an_intent_unless_a_client_has_the_session_focused` (if still flaky after CK-2's change).
2. **Leaked test daemons:** after a full gate, `hutand` processes from the worktree's `target/` stay alive with a temp `HUTAN_HOME` (found after E-6, PR-FIRST-RUN, CL-PRIMITIVES, E-7b gates). Find which test(s) start a daemon they do not stop on success/failure/panic (desktop GPUI tests with an autostarted daemon, `settings_r4_gpui::a_daemon_started_from_outside…` is a suspect) and fix the harness so every daemon is stopped (drop guard that SIGTERMs/SIGKILLs the pid from `hutand.lock`). Add a check: a test in the suite (or an `xtask`/`just` post-step) that fails when a daemon started by the run outlives it.
3. **History focus product bug** (found in CL-CHROME): after *Search history…* on a History panel not yet created, keystrokes go to the previous focus (possibly a terminal) for one daemon round-trip; and History status refreshes cancel each other so the index state can stay unknown. Fix: focus the field (or buffer keys into it) as soon as the panel exists; do not cancel an in-flight status request when another refresh is requested (coalesce instead).

## Acceptance
Each test in (1): 50/50 `--retries 0`, module 10/10 at `-j 4`. (2): a full `just check` leaves no `hutand` from the worktree (`pgrep -fl hutand`), proven twice; the guard check exists. (3): a GPUI test types immediately after *Search history…* on a fresh window and the query lands in the History field; a test with three rapid refreshes reaches a known index state. `just check` green with no FLAKY in the final run (name any other you see); commit per root cause, messages end `(FX-FLAKY-3)`.

## Parity
None.

## Out of scope
`core`/`protocol`; `docs/status.md`, `docs/roadmap.md`.
