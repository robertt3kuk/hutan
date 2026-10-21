# CK-5 — checkpoints in the desktop: Rewind…, preview, Undo

Agent: implementer-opus (Claude Opus 5.5)   Depends on: CK-4 merged (protocol 0.38)   Branch: `slice/CK-5`

**Contract ownership:** none (the wire exists: `checkpoint.list/diff/preview/restore/remove`, events, `ChatInfo.checkpoints`, capabilities `checkpoint`, `checkpoint.restore`).

## Context
Read `AGENTS.md`, ADR 0019, `docs/spikes/2026-10-21-checkpoints.md` §5.1 (entry, preview, confirm, result — verbatim UX), §5.3 (honest label), §6.3 (desktop), §7 row **CK-5** (acceptance), `docs/agents.md` § Checkpoints, `docs/protocol.md` § Checkpoints, the clean-aesthetic spike (`docs/spikes/2026-10-21-clean-aesthetic.md`: one primary per surface, quiet labels, no colour-only cues), `workbench/closing.rs` (*Closed … · Undo* toast pattern), the chat turn footer in `crates/chat_view` / `crates/desktop/src/workbench/chat/`, the Agents page (`wb/panels/agents_page*.rs`), Settings catalog (`settings_schema.rs`).

## Scope
- **Turn footer**: every turn that changed files shows `3 files changed · +41 −7 · Rewind…`; after a restore the footer of the restored turn shows `Restored — Undo` persistently. Hidden when the capability is absent; a chat whose `checkpoints.state` is `unsupported` shows the reason once in the chat header's ⋯ (not per turn), `off` shows nothing.
- **Preview dialog** (sheet): header facts (harness, turns covered, checkout, `shared with …`, HEAD moved line), groups **clean** (checked), **edited after the turn** (unchecked, label "you or another agent changed this since the turn — restoring overwrites that change"), **edited between turns** / **you edited this too** (same treatment), **not captured** (greyed with reason), per-file diff `now → restored` on selection (reuse the Changes view's diff renderer), the honest label of §5.3. Keyboard: ↑↓ files, Space toggles, Return confirms only when the button has focus, Esc cancels; default focus on *Cancel*. Button `Restore N files` (never default); conflicts need their own checkbox; *Restore all, including edited-after* is a separate second-click action. Busy checkout (`invalid_state`) → the dialog says which agent is working and offers *Interrupt it* (existing `chat.interrupt`). Stale digest (`conflict`) → the dialog refreshes with the new plan and says so.
- **Result**: toast `Restored 4 files · Undo` (5 s), Undo calls restore on the restore checkpoint; open editors reload through `fs.changed` (origin `restore`) — verify an open file tab reloads cleanly.
- **Palette**: *Chat: Rewind Files to Before This Turn…* (picks the turn), *Chat: Undo Last Restore*.
- **Agents page → session ⋯ → Checkpoints**: a list (time, turn prompt first line, files, +/−, skipped), each with *Rewind…*.
- **Settings → Agents & tools → Checkpoints**: mode (on/off), per-project override display, retention numbers (read-only display of `daemon.settings` values if editable there already; otherwise link to the setting).
- Docs: `docs/design.md` (turn footer, preview sheet), `docs/agents.md` § Checkpoints desktop paragraph.

## Acceptance
Spike row CK-5 with GPUI tests (real clicks, real daemon, fixture chat that edits files in a temp git repo): the Rewind button appears only on a turn with changes; the dialog's groups and checkboxes match the daemon's preview; Esc / Return behaviour; a conflict needs its own checkbox; Restore writes the files and shows the toast; Undo restores again; an open editor tab shows the restored text; `unsupported` reason shown once; nothing rendered without the capability. Cua screenshots dark + light of the preview with a conflict, and the turn footer after restore, into `~/hutan-scratch/ck5/shots/`. `just check` green; commit per step, messages end `(CK-5)`. Fresh `HUTAN_HOME`; no `hutand` left.

## Parity
CLI `hutan checkpoint restore|undo` exists (CK-4). The desktop must show the same plan the CLI prints.

## Out of scope
Conversation rewind (CK-7), `hutan checkpoint status` (CK-6); `docs/status.md`, `docs/roadmap.md`.
