# FX-REINSTALL-SETUP — a reinstall offers the setup again (optional), an update says what is new

Agent: implementer (Claude Sonnet 5.5)   Depends on: —   Branch: `slice/FX-REINSTALL-SETUP`   Release: 0.2.0

**Contract ownership:** none.

## Context
The user (2026-10-23): "make sure setup runs on reinstall, optionally". Today `app.setup_done` lives in `settings.toml` under `~/Library/Application Support/Hutan`, which a normal `brew uninstall --cask` keeps (only `--zap` removes it), so a reinstall opens straight into the old state and the first-run screen never shows. *Run setup again* exists (old walk-through, `workbench/setup.rs`; `docs/design.md` § First-run setup) but is hard to find.
Read `docs/design.md` § First-run setup (D-2), `workbench/first_run.rs`, `workbench/setup.rs`, `desktop-state.json` persistence, `docs/packaging.md` § Homebrew / uninstall.

## Scope
1. **Install identity**: on launch the desktop records `{version, bundle_path, bundle_id_of_install}` in `desktop-state.json`, where the install id is the app bundle's creation time + inode of `Contents/Info.plist` (changes on every reinstall, also of the same version; stays across launches). Development builds outside a bundle record nothing and never offer.
2. **Reinstall detected** (same or lower version, different install id, setup done before): a quiet one-time card at the top of the centre (not a modal): "Hutan was reinstalled. Run setup again? Your projects, sessions and settings stay." **Run setup** (opens the first screen with the current state: detected agents, Vim question, send key, history consent — every answer pre-filled with today's value, nothing reset) / **Not now** (dismissed for this install).
3. **Update detected** (higher version): no setup offer; a one-line *Updated to 0.2.0 · What's new* notice in the status bar for one session, opening the version's section of `CHANGELOG.md` (bundled into `Contents/Resources/CHANGELOG.md` by `xtask bundle`; rendered as text in a tab).
4. **Always available**: *Run Setup Again…* in the app menu (Hutan ▸), Settings → General, and the command palette; `hutan setup` (CLI) prints that it opens the desktop's setup and does so through `ui.run` when a desktop is connected.
5. Docs: `docs/design.md` § First-run setup (reinstall/update rows), `docs/packaging.md` (what a reinstall keeps and how to get a clean start: `brew uninstall --cask --zap robertt3kuk/hutan/hutan`).

## Acceptance
GPUI tests with injected install identities: first launch of a bundle with no record → no offer when `setup_done` is unset (the normal first screen shows); different install id + same version → the card; **Run setup** opens the first screen with current values; **Not now** never shows again for that install; higher version → the What's new notice and no card; dev build → nothing. Unit test for the identity read on a temp bundle. Cua screenshots of the card and the notice → `~/hutan-scratch/fxreinstall/shots/`. `just check` green; commit per item, messages end `(FX-REINSTALL-SETUP)`. Fresh `HUTAN_HOME`.

## Out of scope
Changing what the setup asks; Homebrew cask hooks (the app decides, not the cask); `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
