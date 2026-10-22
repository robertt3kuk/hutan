# FX-E2E2-A — layout survives a daemon restart, review clears by looking, Finish message from the change

Agent: implementer-opus (Claude Opus 5.5)   Depends on: FX-REVIEW, E-13a merged   Branch: `slice/FX-E2E2-A`

**Contract ownership:** none expected; stop and report if a wire change seems needed.

## Context
Second e2e pass `docs/spikes/2026-10-23-e2e-usertest-2.md` (top 1, 2, 6; finding 1, T2, T8; shots in `~/hutan-scratch/e2e2/shots/`). Read FX-REVIEW's code (`sessions/presentation.rs`, `sessions/tasks.rs`, the seen mechanism), the desktop's persistence/reconnect (`persist.rs`, `desktop-state.json`, daemon epoch handling), the Finish card in the Changes view.

## Items
1. **[major] File tabs and layout lost after a daemon restart** (shots 49 → 53): after `hutan daemon stop` (the desktop restarts the daemon itself) the open file tabs `a.fake`, `b.fake` vanished and the right dock opened on Changes. Expected: file tabs (`ItemRef::File`) stay — they need no daemon session, only `fs.read` again; dock visibility and sizes unchanged; terminals get their recovery cards as today. Find why reconnect to a new epoch drops non-session items and opens the right dock; fix; GPUI test with a real daemon restart. Also check an app-only restart keeps file tabs (persisted state).
2. **"Ready for review" never clears by looking; "Ready for review · Idle" contradicts itself** (T2, shot 76): FX-REVIEW made a seen review quiet but the row still says *Ready for review* forever and the Agents page pairs it with *Idle · waiting for your next message*. Decide one honest state word: after the user has seen it, the session is **Idle** with a small *Changes to review* link (not a state) until Finish/accept/new turn; unseen it is *Needs you · review*. One word per row (CL-AGENTS rule). Update `presentation.rs`, the Agents page/rail, Today, sidebar lane; tests updated to the new spec.
3. **Finish message describes the change** (T8, shot 86): the pre-fill is the chat's first prompt ("Reply with the single word OK…"), not what was committed. Pre-fill a subject from the changed files of the checkout (git's own words: `Update app.py`, `Update app.py and store.rs`, `Update 4 files in src/`, `Add todo.md` when only untracked), and a body line `Agent: <harness> — <chat title>` for each task whose turns changed those files (newest first). The user edits it as today. No model call.

## Acceptance
GPUI tests (real daemon): file tabs and dock state identical after a daemon restart and after an app restart; a reviewed session reads *Idle* + *Changes to review* after focusing it, *Needs you · review* before; Finish pre-fill matches the rule for 1 file, 2 files, many files, untracked-only. Cua screenshots: tabs after a restart, Agents page row, Finish card → `~/hutan-scratch/fxe2e2a/shots/`. `just check` green; commit per item, messages end `(FX-E2E2-A)`. Fresh `HUTAN_HOME`; no `hutand` left.

## Out of scope
Everything else in the report (FX-E2E2-B, -C); `docs/status.md`, `docs/roadmap.md`.
