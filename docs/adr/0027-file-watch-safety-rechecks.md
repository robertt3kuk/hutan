# 0027. Bounded safety rechecks behind every user-file watcher

- Status: accepted (2026-11-04)
- Date: 2026-11-04
- Relates to: `docs/vision.md` § Budgets (idle CPU row, amended here), `docs/roadmap.md` M17 (history index scanning), `docs/architecture.md` § Settings, `docs/protocol.md` § `fs.watch`, `docs/themes.md`, `docs/history-index.md`
- Evidence: slices FX-GATE-TESTS, FX-GATE-DESKTOP, FX-THEME-POLL (2026-11-03/04); the probe table below

## Context

Hutan's watchers relied on macOS FSEvents alone (through the `notify` crate), and the budget row said "no polling loops". Under load FSEvents is both late and lossy. A standalone `notify` probe (outside the repo, load ≈ 26; write after `watch()`, wait up to 10 s, 15 tries per delay) saw:

| Write delay after `watch()` | No event within 10 s | Event later than 1 s |
|---|---|---|
| 0 ms | 8 | 6 |
| 50 ms | 1 | 5 |
| 200 ms | 3 | 6 |
| 1000 ms | 5 | 8 |
| 3000 ms | 6 | 6 |

Lost events are not limited to the moment after a watch starts. User-visible consequences found on master: a `settings.toml` edit or a Mawas memory-file edit left unapplied, a theme edit not repainting, the Files panel missing a change, the history index missing a new transcript. The gate failed on the same class of tests repeatedly.

## Decision

1. **Every watcher of user files has a bounded safety recheck** that runs whatever FSEvents says and publishes only on a real change (a cheap compare; nothing is re-sent when nothing changed):
   - settings, keymap, layout presets, project/workspace settings (`settings/watch.rs`): every 5 s;
   - Mawas memory files (`daemon/memory/files.rs`): every 5 s, only while a folder is watched;
   - themes and icon packs (`settings/themes/registry.rs`): every 5 s when no event came;
   - `fs.watch` (`daemon/files/watch.rs`): each watched file and direct folder listing every 5 s for as long as it is watched, at most 20 000 entries per round with a rotating start; recursive watches are rechecked one level deep (documented limit);
   - history index (`index/indexer.rs`): a stat pass every 30 s (35–40 ms CPU per pass over 10 000 transcripts measured).
2. **Events stay the fast path.** The recheck bounds staleness (5 s, 30 s for the index); it never replaces the event path, never re-walks a whole tree, and does nothing when nothing is watched.
3. **Budget amended**: the idle-CPU row in `docs/vision.md` now reads "≈ 0 %; no polling loops except the bounded safety rechecks of ADR 0027". The rechecks' cost is part of that budget and is re-measured with `just bench` when they change.

## Consequences

- An edit applies within 5 s (30 s for history) even when macOS drops the event; tests no longer depend on FSEvents timing for correctness.
- A small constant cost while something is watched (string compares and `stat` calls; ≈ 0.1 % of a core for the index pass at 10 000 files). Re-measure idle CPU in `docs/perf.md` at the next `just bench`.
- The roadmap's M17 "never re-walking the whole tree on a timer" stands: the index pass stats known files and roots, it does not walk trees.
