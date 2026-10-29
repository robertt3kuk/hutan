# FX-FLAKY-7 — the desktop that starts the daemon twice

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/FX-FLAKY-7`

**Contract ownership:** none (desktop/client lifecycle; no protocol).

## Context
`lifecycle_gpui::hutan_daemon_stop_is_kept_and_a_relaunch_starts_the_daemon` keeps failing on many branches with `starts() == 2` instead of 1 (sometimes deterministically in isolation, sometimes only under load), and `three_failing_starts_stop_and_the_banner_offers_retry` once with 5 vs 4. RailPeek, Scrollbars and ContextPanel all hit it; one agent guessed "a slow daemon start made the app launch it twice". If the app really can start `hutand` twice on a slow start, users can hit it (two daemons racing for the lock, a wasted spawn, a confusing banner). FX-FLAKY-6 added `common::click_at` and test rules in `docs/process.md` § Testing strategy.

## Items (in commit order)
1. Reproduce under load (`--retries 0`, stress, CPU burners as FX-FLAKY-6 did) and find whether the second start is a product race (e.g. reconnect/autostart racing the first spawn's socket appearance, a timer firing before the lock is taken) or a test artefact (a stale `target/debug/hutand`, counting).
2. If product: make the start single-flight (one in-flight start per data folder; later callers await it), with a unit test that forces the slow path; if test: fix the test to wait on state.
3. 20/20 runs under load with `--retries 0` for both tests; report counts.

## Acceptance
Root cause in the report and as a comment where the fix lives; 20/20; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(FX-FLAKY-7)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/fxflaky7/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
