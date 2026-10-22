# FX-MENU-LOGIN — Daemon menu items that work, recovery items only when they apply, honest start-at-login

Agent: implementer-opus (Claude Opus 5.5)   Depends on: FX-DAEMON-LIFECYCLE merged   Branch: `slice/FX-MENU-LOGIN`   Release: 0.2.0

**Contract ownership:** none.

## Evidence
Release-readiness pass `docs/spikes/2026-10-23-e2e-usertest-3.md` N9 and N1:
- **N9**: in the native *Daemon* menu every item (Stop / Start / Restart / Status… / Keep Running) reported **disabled** to the accessibility driver while the daemon ran with a live chat; *Start with Empty Data…* and *Stop the Stuck Daemon…* were listed although the daemon was healthy (FX-DAEMON-START meant them to appear only for their failure). Cause unknown: GPUI enables a menu item when its action is available in the focused element's dispatch path at validation time — a window without a focused handler, or actions registered on an element that is not in the path, shows them disabled.
- **N1**: from an ad-hoc-signed bundle outside `/Applications`, *Start Daemon at Login* shows "(not found)" and the Setup *Background* step says "The launch agent is missing from Hutan.app", although `Contents/Library/LaunchAgents/dev.hutan.daemon.plist` is in the bundle (`SMAppService.agent(plistName:)` status `.notFound` [INFERENCE: location/signature]).

## Scope
1. **Menu enablement**: reproduce in a GPUI test that drives the platform menu's validation path (the same `is_action_available`/validate call AppKit uses), with focus in a file tab, a terminal, a chat, the sidebar and with no window focused; make Stop/Start/Restart/Status/Keep Running enabled exactly when they can act (register the daemon actions at the window/root level so they are in every dispatch path). Verify by hand once in a real window (Cua: click the menu item, observe the effect).
2. **Recovery items**: *Start with Empty Data…*, *Stop the Stuck Daemon…*, *Copy Update Command* appear only while the matching start failure is current; hidden otherwise (not merely disabled). Same in ⌘K.
3. **Start at login truth**: find what `SMAppService.agent` reports for (a) a bundle in a scratch folder, (b) a bundle in `~/Applications`-like location you create under `~/hutan-scratch/` (read `status` only — **never call `register()`** on the user's system outside a test double), and Apple's documented requirements (location, signing, `BundleProgram`). Make the wording accurate: if the cause is location/signature, say "Start at login works when Hutan is in Applications" (and why), never "missing from Hutan.app" unless the plist is truly absent (check the file). Keep the toggle disabled with that reason.
4. Docs: `docs/packaging.md` (start at login requirements), `docs/design.md` (Daemon menu items and when they show).

## Acceptance
GPUI tests for 1 and 2 (enablement per focus case; recovery items hidden when healthy, shown for each failure kind); unit test for the plist-present-but-notFound wording; a Cua screenshot of the Daemon menu with enabled items and one real click (Restart) observed working → `~/hutan-scratch/fxmenu/shots/`. `just check` green; commit per item, messages end `(FX-MENU-LOGIN)`. Fresh `HUTAN_HOME`; no real login item registered; no `hutand` left.

## Out of scope
Notarization; `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
