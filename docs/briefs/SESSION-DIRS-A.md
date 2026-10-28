# SESSION-DIRS-A — extra folders per session — daemon and protocol

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/SESSION-DIRS-A`

**Contract ownership:** **delegated**: additive protocol minor (next free at merge), capability `session.dirs`, own commit, goldens.

## Context
ADR 0023 (accepted) and spike `docs/spikes/2026-10-28-best-of/README.md` §3 and §5.3 row 6. User decisions (2026-10-28, spike §6 recommendations taken): uppercase 10 px tracked group labels; news line only when there is news; Phosphor Bold ≤ 14 px / Regular above + our own harness monograms; fork destination remembered, always ask on a fallback; session folders user-only, running terminal agents get a pasted `/add-dir` to confirm. No `+/−` counts by default.

Implement §5.3 row 6: `extra_dirs` in `chat.create` / `TerminalSpec`, `ChatSpec.dirs`, `chat.add_dir` / `chat.remove_dir` (UserOnly), per-harness application (Claude ACP `additionalDirectories` resent on load/resume; OMP `/add-dir` local command since it ignores `session/new.additionalDirectories`; Codex `writableRoots`; terminals' launch args for known agents), Contain `extra_write` with `applied: restart`, file-service roots include them, `ChatInfo.dirs`, CLI `hutan chat dirs list|add|remove`.

## Acceptance
Daemon tests with fixture harnesses asserting what each receives, agent-origin `forbidden`, file-service access in an added folder and refusal after removal, Contain roots; CLI tests; goldens; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(SESSION-DIRS-A)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/sessiondirsa/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
