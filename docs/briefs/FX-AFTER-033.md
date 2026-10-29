# FX-AFTER-033 — two things found while releasing 0.3.3

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/FX-AFTER-033`

**Contract ownership:** none (no protocol change).

## Context
1. **Racy test**: `desktop::it usage_gpui::the_agents_page_shows_each_harness_with_bars_resets_and_estimate_labels` failed 4 of 5 full gates under load (passes alone; panics "a bar" at `usage_gpui.rs:465`); other `usage_gpui` tests flaked too. The release agent's reading: `UsageDirectory::load` asks the real test daemon for `usage.summary` on connect and when the Agents page opens; under load that answer lands after the test's `give(fixture)` and replaces it, so the bars vanish.
2. **Updater vs a fresh release**: right after `gh release create`, GitHub's download URL answered `Not Found` (9 bytes, HTTP 404) for ~40 s while the API already listed the asset with its digest. UPDATES-2's bundle path would report a digest mismatch for that — misleading.

## Items (in commit order; each with a regression test)
1. Fix the `usage_gpui` race at the root (the test must own the data it asserts: e.g. a test daemon whose usage summary is the fixture, or wait for the store's first daemon answer before giving the fixture — no sleeps, no raised timeouts); run the module 20× under load with `--retries 0` and report counts (see `docs/process.md` § Testing strategy, FX-FLAKY-6/7 notes).
2. Updater: a download that answers a non-2xx status (or a body far smaller than the asset's listed size) is a **download** failure with the message "This release isn't downloadable yet — try again in a minute" (stage `download`, *Try again*), never a digest mismatch; the checker doesn't offer a release until its asset answers a `HEAD` with 2xx (one extra request at most per new release; quiet otherwise). Daemon tests with the mock server (404 then 200).
3. Docs: `docs/packaging.md` § Updates (the propagation note), `docs/process.md` if the test rule changes.

## Acceptance
20/20 for the `usage_gpui` module under load; daemon tests for 404/short body; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(FX-AFTER-033)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/fxafter033/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
