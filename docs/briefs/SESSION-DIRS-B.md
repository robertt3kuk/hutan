# SESSION-DIRS-B — extra folders in the desktop

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/SESSION-DIRS-B`

**Contract ownership:** none (desktop; uses 0.57 `session.dirs`).

## Context
ADR 0023 §4, spike `docs/spikes/2026-10-28-best-of/README.md` §3 and §5.3 row 7 (mock `mock-folders.svg` if present). SESSION-DIRS-A (0.57) merged: `ChatInfo.dirs`, `SessionInfo.extra_dirs` with `applied: now|next_turn|restart|paste|unsupported`, `chat.add_dir/remove_dir`. User decisions (2026-10-28): fork destination remembered, always ask on a fallback; session folders user-only; running terminal agents get a pasted `/add-dir` to confirm. ADR 0023 accepted.

Implement §5.3 row 7: a folders chip in the chat header (and terminal agent rows) with a popover listing primary + extra folders, their `applied` state in words, *Add Folder…* (NSOpenPanel) and remove; ＋ menu entries *Add Folder to This Workspace…* and *Add Folder to the Focused Session…*; for a running terminal agent the paste flow (typed `/add-dir …`, the user presses Return); *Restart with this folder* when Contain or a harness needs a restart; Files shows the added folders as roots of that session.

## Acceptance
GPUI tests over a real daemon (chip, popover states per `applied`, add/remove, terminal paste typed without Enter, restart prompt); Cua screenshots (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(SESSION-DIRS-B)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/sessiondirsb/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
