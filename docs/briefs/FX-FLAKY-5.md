# FX-FLAKY-5 — flakes blocking the 0.2.0 freeze

Agent: implementer-opus (Claude Opus 5.5)   Depends on: FX-VIM-2 merged   Branch: `slice/FX-FLAKY-5`

**Contract ownership:** none. Test fixes, or product fixes when the product is wrong (say so in the commit).

## Context
Release 0.2.0 needs a `just check` with no FLAKY entries. Method as FX-FLAKY-3/4 (`git log --grep FX-FLAKY-4`): reproduce the single test under load (`--retries 0`, `-j 4`, many runs), dump state at the timeout, fix the cause, then 50/50.

| Test | Seen |
|---|---|
| `hutan-desktop::it checkpoints_gpui::rewind_previews_restores_a_turns_files_and_undo_brings_them_back` | failed 3/3 tries once (FX-E2E2-B gate), passes on retry in several gates, 1 in 6 on `a2003d4` |
| `hutan-daemon::it session_names::the_command_in_the_foreground_follows_the_name` | intermittent (FX-FLAKY-4 could not reproduce in 300 runs; hypothesis: zsh OSC 133 command name at preexec, Ctrl-C before the command runs) |
| `hutan-cli::it control::a_plain_shell_sessions_cli_is_not_gated_while_an_agent_sessions_is` | 3 in ~110 fresh runs under load; `hutan approvals` never listed the approval within 20 s |

## Acceptance
Each test 50/50 with `--retries 0` at `-j 4` under load, cause named in the commit; `just check` with no FLAKY; commit per test, messages end `(FX-FLAKY-5)`. Fresh `HUTAN_HOME`; no `hutand` left.

## Out of scope
`docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
