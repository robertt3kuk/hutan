# UX-ATTN-3 — How Hutan calls you: Dock badge, sound, notifications, in-window toast, tab mark

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/UX-ATTN-3`

**Contract ownership:** none (stop and report if a wire change seems needed).

## Context
The user (2026-10-22): call for attention differently. Evidence: `docs/spikes/2026-10-22-ui-polish/attention.md` recommendations 3–7 (badge only for blocking, sound only for requests and failures, OS notifications only when you are not looking, one per turn per session, toasts ≤ 2, ~5 s, hover pauses). Depends on UX-ATTN-2 (groups Requests / Failed / Ready for review). Read the daemon's notify intents (`crates/daemon/src/notify*.rs`, `docs/resources.md`/`docs/integrations.md` where they describe notifications), `notify.mute` (UX-PROTO-1), the desktop's notification handling.

## Items (in commit order)
1. **Dock badge**: the app icon shows the Requests count (blocking only, `99+`), cleared at 0; setting `notifications.dock_badge` (default on).
2. **OS notifications** (daemon owns intents; desktop decides display): Requests and Failed notify only when the window is not key or the session is not visible; Ready for review notifies at most once per session per 90 s burst and only when unfocused (setting `notifications.finished`, default on); one notification per turn per session across windows; clicking opens the session at the request. Muted sessions (`notify.mute`) never notify.
3. **Sound**: a short system sound only for Requests and Failed (setting `notifications.sound`, default on; respects macOS Focus/DND via the OS).
4. **In-window**: when a request arrives for a session that is not visible while the window is focused — a toast at the bottom-right `Claude Code asks to edit src/lib.rs · Review ⌘⇧R` (≤ 2 visible, oldest replaced, auto-dismiss 5 s, hover pauses, Esc dismisses); the session's tab gets a small accent dot (shape, not colour only) until seen; no blinking, no repeated pulses.
5. Settings → Notifications page shows these settings; docs `docs/design.md` § Attention.

## Acceptance
GPUI tests with a fake notification host: badge count follows Requests only; notifications fire only in the unfocused/hidden cases and once per turn; Ready for review rate-limited; muted session silent; toast stacking/auto-dismiss/hover pause with the test clock; tab dot until seen. Cua screenshots of the toast and the tab dot. Manual note: real macOS notification seen once with the bundle (if permission prompt appears, deny and note it).

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks), `docs/spikes/2026-10-21-clean-aesthetic.md`, `docs/design.md`. Commit after every item (messages end `(UX-ATTN-3)`); update existing tests to the new spec keeping what they prove; Cua screenshots dark + light before/after into `~/hutan-scratch/uxattn3/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
