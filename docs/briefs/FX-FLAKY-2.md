# FX-FLAKY-2 — two daemon tests that fail fast on first try

Agent: implementer (Claude Sonnet 5.5)   Depends on: master `81426d5`   Branch: `slice/FX-FLAKY-2`

## Context
Same rules and method as `docs/briefs/FX-FLAKY-1.md` (read it and its result in `docs/status.md`: the replay fixture's partial-line writes were the cause there). Seen as FLAKY in the gate of the rebased UX-ATTN-1 (after UX-TERM-1/2, UX-SESS-1 landed: terminal resize/reflow changes and default session names):

| Test (`hutan-daemon`, binary `it`) | First-try failure time |
|---|---|
| `blocks::renders_carry_the_same_viewport_top_as_the_blocks_answer` | 0.21 s |
| `close::closing_an_idle_terminal_removes_it_and_announces_it` | 0.27 s |

## Scope
Tests and, if the race is in product code, `crates/daemon`/`crates/terminal`. Reproduce with `cargo nextest run -p hutan-daemon --test it -E 'test(=<name>)' --retries 0` in loops and with the module at `-j 4`; root-cause; fix deterministically (no sleeps, no relaxed assertions, no longer timeouts).

## Acceptance
50/50 `--retries 0` per test, 10/10 module runs at `-j 4`; `just check` green with no FLAKY in your final run (name any other FLAKY you see); one commit per root cause ending `(FX-FLAKY-2)`.

## Parity
None.

## Out of scope
`core`/`protocol`, `docs/status.md`, `docs/roadmap.md`.

## Report
Root cause per test (file:line), fix, run counts.
