# Packaging: the app bundle (roadmap M6.2)

`cargo run -p xtask -- bundle` (or `just bundle`) assembles `Hutan.app` from the three real executables of the workspace and signs it ad hoc. It does **not** launch the bundle, register anything with the system (no `SMAppService`, no `launchctl`), call a notification API, or touch the Keychain. The desktop registers the LaunchAgent itself, only when the user turns on "start at login" (§ Start at login). The bundled daemon posts native notifications (M6-B, ADR 0007 accepted, § Native notifications below); trying that out is done by hand with the user present.

Code: `xtask/src/bundle.rs`, `xtask/src/icon.rs`. Assets: `assets/macos/`, brand sources in `assets/brand/`.

## Layout

```text
Hutan.app/Contents/
  Info.plist                                   bundle id dev.hutan.Hutan, version of the workspace,
                                               LSMinimumSystemVersion 13.0, NSHighResolutionCapable,
                                               LSMultipleInstancesProhibited true
  MacOS/hutan-desktop                         the GPUI desktop (CFBundleExecutable)
  MacOS/hutand                                the daemon (signed with the app's identifier dev.hutan.Hutan)
  MacOS/hutan                                 the CLI
  Library/LaunchAgents/dev.hutan.daemon.plist BundleProgram Contents/MacOS/hutand, RunAtLoad true,
                                               KeepAlive {Crashed: true}; inert until "keep running"
                                               registers it (M6-C)
  Resources/Hutan.icns                         the app icon (CFBundleIconFile)
  Resources/shell-integration/…                the OSC 133 / OSC 7 scripts (zsh, bash, fish)
```

- **The desktop executable is `hutan-desktop`, not `Hutan` (ADR 0007's sketch).** The default macOS volume is case-insensitive, so `Contents/MacOS/Hutan` and the CLI `Contents/MacOS/hutan` would be one file. The name users see is still *Hutan* (`CFBundleName`, `CFBundleDisplayName`, the folder name); the xtask test fails if two executables ever collide ignoring case.
- No `Contents/Helpers/Hutan Notifier.app`: the daemon posts the notifications itself under the bundle id (ADR 0007, § Native notifications); the xtask test fails if a `Helpers` folder appears.
- The icon is the orangutan mascot `assets/brand/05-mascot/svg/icon-dark.svg`. `cargo run -p xtask -- icon` (or `just icon`) renders it into the 824-pixel tile of Apple's 1024 grid at every `.iconset` size and runs `iconutil` to write `assets/macos/Hutan.icns`, which is committed; rerun it after changing the SVG.
- The scripts in `Resources/` are for inspection and for the installer to come; the daemon still installs its own embedded copy into `$HUTAN_HOME` (`docs/shell-integration.md`).

## Building and signing

```sh
just bundle                       # debug binaries → target/bundle/Hutan.app
just bundle --release             # release binaries
cargo run -p xtask -- bundle --out /tmp/x --bin-dir target/debug    # bundle binaries that are already built
```

`bundle` runs `cargo build --locked -p hutan-desktop -p hutan-daemon -p hutan-cli` (unless `--bin-dir` is given), copies the binaries (mode 0755), fills `@VERSION@` into `assets/macos/Info.plist`, then signs: `codesign --force --sign - --timestamp=none` on `hutand` (identifier `dev.hutan.Hutan`, **the app's**: see below) and `hutan` (`dev.hutan.cli`), then on the bundle (which signs `hutan-desktop` with the bundle identifier `dev.hutan.Hutan`). It ends with `codesign --verify --deep --strict` and prints the bundle's path; any failure is an error. An existing `Hutan.app` in the output folder is replaced.

Ad-hoc signatures have no team identity: Gatekeeper treats a copy that was downloaded or AirDropped as unsigned, and the designated requirement of an ad-hoc signature is the code hash, so **every rebuild is a new identity** for the Keychain (ADR 0008: items are bound to the signing identity; expect prompts after each rebuild until a stable identity is used). Neither is a problem for a bundle built and run locally.

## Where the executables find each other

Everything resolves by location, so the bundle works wherever it is put:

| Who | Looks for | Where |
|---|---|---|
| `hutan-desktop` | `hutand` to autostart | beside its own executable (`desktop::app::hutand_beside`), else `PATH` |
| `hutan` | `hutand` to autostart (`hutan daemon start` and the commands that start it) | beside itself, as invoked and with symlinks resolved (so a `/usr/local/bin/hutan` link into the bundle finds the bundled daemon), else `PATH` |
| `hutand` | `hutan` for `HUTAN_BIN` in sessions | beside itself (also through symlinks), else `PATH` |

A unit test per crate builds a fake bundle tree (`Hutan.app/Contents/MacOS/{hutan-desktop,hutand,hutan}`, plus a symlinked CLI) and checks each rule.

CLI startup is explicit: `hutan daemon start` (and `hutan index enable`) requests autostart.
Queries such as `hutan notify ls` do not start a daemon that is down.

## Tests and the gate

`xtask`'s `bundle` tests run in `just test`: one assembles a bundle from the debug binaries `cargo build --workspace --bins` leaves in `target/debug` into a temporary folder, signs and verifies it, parses both plists with the `plist` crate (identifier, version, minimum system version, high-resolution flag, `LSMultipleInstancesProhibited`, icon file, executable and `BundleProgram` targets, `RunAtLoad` true, `KeepAlive` a dictionary with only `Crashed`, no environment, arguments or log paths), checks the layout (executables are Mach-O, names distinct ignoring case, no `Helpers`, the icon and the scripts in `Resources`), reads the signature back (`Signature=adhoc`, `hutand` carries `Identifier=dev.hutan.Hutan`), proves verification fails after a byte of `hutand` changes, and checks that no process runs from the bundle. It takes about two seconds, so it stays in the gate; `just bundle` itself is not part of `just check`. CI notes: run `just bundle --release` as a build artifact step on macOS runners; no step may open the result.

## Native notifications (M6-B)

Code: `crates/daemon/src/notify_native.rs` and `notify_native/`; the wire side is `docs/protocol.md` § Notification intents → Native delivery.

- **`hutand` is signed with the app's identifier `dev.hutan.Hutan`, not `dev.hutan.daemon`.** The system gives notification permission to an app identity: a second executable inside `Contents/MacOS` that is signed with its own identifier is refused (`com.apple.private.usernotifications.bundle-identifiers` is required), one signed with the app's identifier shares the app's permission and its notifications are Hutan's (spike `docs/spikes/2026-10-15-notifications.md`, facts 2–3). The same identity serves the Keychain (ADR 0008). The cost: `codesign -dv hutand` and the app show one identifier, so tools that tell them apart do it by path.
- **Only from the bundle.** The daemon builds the real poster only when its executable is inside a `.app` (`Contents/MacOS/<exe>`) and the main bundle's identifier is `dev.hutan.Hutan`. A daemon started from `cargo run`, `target/debug`, a test, or a bundle in `/tmp` (rejected by `usernoted`, spike fact 1; keep the bundle in a normal location such as the home folder) posts nothing. The first thing it does is log, once, `native notifications are off: not running from Hutan.app`; nothing from the notification framework runs before the first post.
- **Run loop.** In the bundle the main thread of `hutand` is the Cocoa run loop and tokio runs on the thread `hutand-runtime` (`crates/daemon/src/main.rs`); the delegate's callbacks (permission answer, clicks) arrive on the main thread. Outside the bundle the main thread just joins the runtime thread. The `main_thread_loop` test (its own `main`, `harness = false`) runs the loop on the real main thread of a test process, without any notification API.
- **Permission.** The first post makes macOS ask once ("Hutan would like to send you notifications"). If the user says no, the daemon says so once in `hutand.log`, posts nothing more that run (it asks again after a restart), and the intents stay in `hutan notify ls|watch`. `daemon.toml [notify] native = false` turns posting off without touching the permission.
- **What a click needs.** The daemon must be running when the user clicks: a click while it is down is dropped by the system. Approvals then stay pending and are answered in the app or the CLI. "Start at login" or "Keep running" is the remedy.
- **Trying it by hand** (not part of any test; needs the user at the machine): `just bundle`, copy `target/bundle/Hutan.app` to a folder under the home directory (not `/tmp`), start its `hutand` with `HUTAN_HOME` set to a scratch folder, let a fake Codex raise an approval, click *Approve*, *Open* and the body, then kill the daemon and click again (the click is lost, the approval pending). Never run it against the real data folder.

## Start at login (M6-C, D-2)

This was "keep running" before D-2. Keep running (`app.keep_running`) now only leaves the daemon running after quit and registers nothing; both are off by default and the first-run setup offers them (`docs/design.md` § First-run setup).

Code: `crates/desktop/src/macos/login_item.rs`, `crates/desktop/src/workbench/mac.rs`; behavior in `docs/design.md` § macOS integration. The desktop registers `Contents/Library/LaunchAgents/dev.hutan.daemon.plist` with `SMAppService.agent(plistName:)` (`register()` / `unregister()`), which adds *Hutan* to System Settings → General → Login Items (macOS shows a notice the first time, and the user may have to allow it: status `requires approval`, button *Open Login Items*). Registration needs the app to run from the bundle; a `cargo run` desktop disables the toggle.

The agent plist, and why:

- `BundleProgram` `Contents/MacOS/hutand`, no `ProgramArguments`, no `EnvironmentVariables`: launchd starts the signed bundled daemon with no `HUTAN_*` variables, so it uses the standard data folder, socket and `hutand.log` (it writes its own log; no `StandardOutPath` is set, so nothing else is created). The desktop of the same user finds that socket by the same default.
- `RunAtLoad` **true**: the daemon starts when the agent is registered and at every login, so a notification click always has a daemon to reach (the point of starting at login, ADR 0007).
- `KeepAlive` `{ Crashed: true }`: launchd restarts the daemon after a crash and **only** then. `hutan daemon stop` and "Stop Daemon" end `hutand` with exit 0, and a daemon that finds another one holding the lock exits 2; neither may bring one back, which a bare `KeepAlive true` would. The xtask bundle test pins all three.
- `AssociatedBundleIdentifiers` `dev.hutan.Hutan`: the Login Items entry is shown as Hutan.

While start at login is active (registered and approved) the desktop starts with a connection that only dials, waits 4 s for a launchd-started daemon, and only then starts `hutand` itself; quitting the app never touches the daemon. The race that matters is a login-time one: the desktop and launchd both starting `hutand`. The daemon's lock makes the loser exit 2, so it cannot corrupt anything, but the desktop avoids the second start altogether.

**Trying it by hand** (not part of any test; needs the user at the machine; never against the real data folder unless the user wants to): `just bundle`, copy `target/bundle/Hutan.app` to a folder under the home directory, start it, turn *Start daemon at login* on in Settings (`app`) or the Daemon menu, approve it in Login Items if asked, quit the app and check `launchctl print gui/$(id -u)/dev.hutan.daemon` and `hutan daemon status`; turn it off and check that the entry is gone and what happened to the running daemon.

## One desktop per data folder

`LSMultipleInstancesProhibited` in `Info.plist` makes Launch Services reactivate the running Hutan when it is opened again (Dock, Finder, `open`); the xtask bundle test checks the key. It does not cover `open -n` or running `Contents/MacOS/hutan-desktop` directly, so `hutan-desktop` enforces it too (`crates/desktop/src/instance.rs`, behavior in `docs/design.md` § Daemon lifecycle): before any window opens it takes an exclusive `flock` on **`desktop.lock`** in the data folder (`$HUTAN_HOME`, else `~/Library/Application Support/Hutan`) and listens on **`desktop.sock`** next to it (mode `0600`, peers of another user dropped, a stale socket file from a crash replaced under the lock). A second desktop sends `{"cmd":"activate"}` to that socket and exits 0; the first one brings its window forward. Two desktops with different `HUTAN_HOME`s do not know each other, which is what tests and `just bench` rely on.

## The daemon's menu bar item (M6-D)

Code: `crates/daemon/src/menu_bar.rs`, `menu_bar/cocoa.rs` (real), `menu_bar/fake.rs` (tests); behavior and menu in `docs/design.md` § macOS integration → *The daemon's menu bar item*.

- **When.** Only in a daemon that runs from `Hutan.app` (the same check as the native notifications: executable in `*.app/Contents/MacOS/`, main bundle `dev.hutan.Hutan`); a `cargo run` or test daemon shows no item and calls no AppKit. It is shown while no `desktop` client is connected and removed at once when one connects; after the last one leaves it waits 750 ms, so a desktop that is starting or reconnecting never meets a second icon. It is **not optional**: `app.menu_bar` governs the desktop's item only, `[notify] native = false` does not touch it.
- **Threads.** The item lives on the main thread, in the loop that already hosts the notifications. The first time there is something to show, the daemon creates the process's `NSApplication` as an *accessory* (no Dock icon, no application menu) and from then on the loop dequeues and dispatches AppKit events itself (`nextEvent…` / `sendEvent:`): status item clicks and the stop alert are delivered through the application's event queue, which a bare `NSRunLoop` never drains. Queued blocks still run (the main queue is served while the loop waits); a stop posts an application-defined event so that the wait ends. The host's calls only record what is wanted (the content, or none; the stop questions) and schedule one block on the main queue, so tokio never waits for Cocoa; clicks come back over a bounded channel. Shutdown removes the item before the process ends: the daemon's accept loop waits (2 s at most) for the item's task, which takes it off.
- **Opening Hutan.** *Open Hutan*, the *N need you* entries and a reopen from Launch Services (Dock, Finder, `open`) start `Contents/MacOS/hutan-desktop` directly, in its own process group, with the daemon's environment (so a daemon on a scratch `HUTAN_HOME` / `HUTAN_SOCKET` opens a desktop on that folder). Not through Launch Services: a running `hutand` checks in as an instance of the app (same bundle identifier), so under `LSMultipleInstancesProhibited` Launch Services answers `open` — even `open -n` — with a reopen of `hutand`; when the daemon answered that with `open -n` it looped, launching every 50 ms (seen 2026-10-17, fixed in `ed8ab64`). One launch at a time: another request within 10 s (`LAUNCH_GRACE`) starts nothing unless a desktop connected meanwhile. A second desktop is harmless (it brings the first one forward and exits, § One desktop per data folder). The session or settings the user chose is then asked of the desktop through paths a connected client has already: `ui.open` for a session, `ui.run` of `workbench::OpenSettings` for the settings, as soon as one window has published (`ui.publish` happens after the connection is synchronised, so the window knows the session), and only within 30 s of the click. The application also answers a *reopen* (Launch Services reactivating the process, which carries the app's bundle identifier) by opening Hutan.
- **Tests.** `crates/daemon/tests/menu_bar.rs` runs a real daemon over its socket with a fake item and a fake launcher: shown with no desktop, hidden while a desktop is connected, back after the last one leaves, no icon for a desktop that returns within the delay, the live count and the dot (blocked agents, failures, approvals), `start_at_login` from `settings.toml`, open / settings / stop with and without sessions and the confirmation outcomes. The pure model is unit-tested in `menu_bar.rs`. No test touches `NSStatusBar`, `NSAlert` or launches the app; the desktop pins the settings action's name (`macos/status_item.rs`).
- **Trying it by hand** (not part of any test; needs the user at the machine; never against the real data folder): `just bundle`, copy `target/bundle/Hutan.app` to a folder under the home directory, run its `hutand` with `HUTAN_HOME` on a scratch folder (start the desktop with the same `HUTAN_HOME`), quit the desktop with *Keep running* on: the daemon's icon should be on the menu bar after about a second, with the menu above; open Hutan from it: the desktop starts and the daemon's icon goes away, leaving one icon; *Stop daemon…* with a running session asks first. Also check that clicking `Hutan.app` in Finder while only the daemon runs starts the desktop (see below).

## Menu bar glyph

`assets/macos/MenuBarIcon.png` (18 px) and `MenuBarIcon@2x.png` (36 px) are committed, black template images (only the alpha channel counts; macOS tints them for light, dark and highlighted states). `just menubar-icon` (`cargo run -p xtask -- menubar-icon`) renders them from `assets/brand/05-mascot/svg/favicon.svg` without its background tile and cuts the eyes and nostrils out; an xtask test checks sizes, that every pixel is black, and that rendering again reproduces the committed files. The desktop and the daemon (its own menu bar item) embed them (`include_bytes!`), so the executables need no resource folder. The same goes for the desktop's UI icons: the Lucide SVGs of `assets/icons/` (ISC, license beside them) are `include_bytes!`d into `hutan-desktop` and served to GPUI as an asset source, so `bundle` has nothing to copy for them.

## Homebrew

The upstream tap is `robertt3kuk/homebrew-hutan`. It is a third-party tap, not
`homebrew/core` or `homebrew/cask`. After publishing the first release:

```sh
brew install --cask robertt3kuk/hutan/hutan
# Or, CLI + daemon only (not alongside the cask):
brew install robertt3kuk/hutan/hutan
```

The cask links `hutan` from `Hutan.app`; its CLI finds the adjacent bundled
`hutand`. The formula builds only `hutan-cli` and `hutan-daemon` with locked
dependencies, never the GPUI desktop. Cargo still resolves/fetches the pinned
workspace Git dependencies, but does not compile GPUI for those two packages.
The formula requires Homebrew Rust (upstream MSRV 1.98), Apple Silicon and
macOS 13+. It does not register a service; start/stop with `hutan daemon start`
and `hutan daemon stop`. No native notifications are available outside the app.
Do not install both packages: both own the `hutan` command.

### Prepare a release locally

From a clean, committed Apple Silicon checkout with Rust 1.98.0:

```sh
cargo run -p xtask -- release --tap /path/to/homebrew-hutan
```

This uses the same release bundle builder as `bundle --release`, with
`MACOSX_DEPLOYMENT_TARGET=13.0`, signs ad hoc and verifies the bundle, runs
`ditto -c -k --keepParent`, creates a source archive of **HEAD** with `git archive`,
and writes these files under `target/release-artifacts/` (or `$CARGO_TARGET_DIR`):

- `Hutan-0.1.0-arm64.zip`
- `Hutan-0.1.0-source.tar.gz`
- `Hutan.app` (the bundle used to make the zip)

The version comes from the workspace package version, not a separately maintained
release constant. The command prints both SHA-256 checksums and writes
`Casks/hutan.rb` and `Formula/hutan.rb` into `--tap`, using the templates in
`xtask/src/hutan.*.rb.in`. It never launches, installs, tags, uploads or publishes.
Archive names/checksums are reproducible release inputs; signed ZIP bytes are not
promised to be bit-for-bit identical across machines/toolchains or rebuilds.
Upload the **exact** archives whose checksums were generated, never recreate
GitHub's automatic source download in place of the source asset.

### Security and uninstall

The release is ad-hoc signed, not Developer-ID signed or notarized. Homebrew
keeps quarantine enabled. A quarantined first launch may be blocked as an
unidentified/unverified developer or because Apple cannot check it for malicious
software (wording depends on macOS). If the user trusts the release, attempt to
open Hutan, then use **System Settings → Privacy & Security → Open Anyway**,
authenticate if requested and confirm **Open**. There is no cask quarantine
removal, re-signing, `--no-quarantine` advice or global Gatekeeper bypass.
This does **not** meet official `homebrew/cask` Gatekeeper eligibility.
The bundled CLI may require approval separately; whether allowing the app once
also allows `Contents/MacOS/hutan` needs a user-present first-open test. The local
quarantined CLI execution was blocked, so packaging does not promise otherwise.

Before uninstalling, turn off **Start daemon at login** in Hutan Settings so
`SMAppService` unregisters it. The cask also unloads `dev.hutan.daemon`, quits
`dev.hutan.Hutan`, sends TERM to registered processes with that bundle ID, and
uses system `pkill -TERM` scoped to the current user and the exact installed
`Contents/MacOS/hutand` path for a daemon not registered with Launch Services.
Homebrew 7's `signal` only discovers application labels in `launchctl list`,
not arbitrary processes with a matching code-signing identifier. Uninstall
never executes a quarantined Hutan binary, which could block awaiting approval.
A missing/stopped daemon is not an uninstall error. Normal uninstall preserves
data; `brew uninstall
--cask --zap robertt3kuk/hutan/hutan` also removes Hutan's application support,
logs, preferences and a legacy user LaunchAgent plist if present. A bundled
SMAppService plist is not copied to `~/Library/LaunchAgents` by Hutan.
The formula has no uninstall hook: stop its daemon explicitly before uninstall.

### Publish after review (maintainer only)

Merge the source changes before preparing the final release, review both tap
definitions, and run the local tap checks documented in its README. The source
archive must represent the commit to tag. From the Hutan checkout:

```sh
cargo run -p xtask -- release --tap /path/to/homebrew-hutan
git tag -a v0.1.0 -m "Hutan 0.1.0"
git push origin master
git push origin v0.1.0
gh release create v0.1.0 \
  target/release-artifacts/Hutan-0.1.0-arm64.zip \
  target/release-artifacts/Hutan-0.1.0-source.tar.gz \
  --repo robertt3kuk/hutan --verify-tag --title "Hutan 0.1.0" --generate-notes
```

Then, from the tap checkout, commit the generated definitions and publish:

```sh
git add Casks/hutan.rb Formula/hutan.rb README.md .github/workflows/check.yml
git commit -m "feat: package Hutan 0.1.0"
gh repo create robertt3kuk/homebrew-hutan --public --source . --remote origin
git push -u origin main
```

If the tap commit already exists, omit the `git add`/`git commit` pair.
These are manual publishing commands, not actions performed by `xtask release`.
Before public redistribution, resolve the project license (there is no LICENSE;
the formula intentionally does not invent an SPDX identifier) and review
dependency redistribution obligations. Developer ID signing and notarization
remain open distribution decisions (ADR 0007/0008).

## What is still open

- The Keychain behavior across rebuilds.
- A stable signing identity, and notarization when there is a team.
- Verified 2026-10-17 on the real bundle id: with only `hutand` running, `open -a Hutan.app` (the Dock's path) reaches the daemon as a reopen; the daemon starts the desktop once and the window opens; opening again with the window up brings it forward (one desktop process).
