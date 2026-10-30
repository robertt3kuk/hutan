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
                                               KeepAlive {Crashed: true}; inert until "start at
                                               login" registers it (D-2, § Start at login)
  Resources/Hutan.icns                         the app icon (CFBundleIconFile)
  Resources/shell-integration/…                the OSC 133 / OSC 7 scripts (zsh, bash, fish)
  Resources/CHANGELOG.md                       the changelog (the app's "What's new" after an update)
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
- **The digest (MAWAS-DIGEST-A).** One more kind of notification: *While you were away*, a quiet banner (no sound, category `hutan.digest`, a single *Open* action) the daemon posts when no Hutan window has been active for `notify.digest_hours` hours and something needs the user (`docs/assistant.md` § Catching up). It uses the same poster, permission and bundle rules as the others, and is **off by default** (`digest_hours = 0`), so a user who never turns it on sees no new notification and, from it, no new permission prompt. When it is on and nothing else has posted before, **the first digest is what makes macOS ask** ("“Hutan” Notifications": the system's one-time authorization prompt, under the app's identifier; the digest is delivered only after the user allows it, and the prompt's answer is the user's alone — a check by hand never answers it, because the identifier is the installed app's too). A click or *Open* brings Hutan forward on Mawas Home's Recent tab, and like every click it needs the daemon to be running.
- **Trying it by hand** (not part of any test; needs the user at the machine): `just bundle`, copy `target/bundle/Hutan.app` to a folder under the home directory (not `/tmp`), start its `hutand` with `HUTAN_HOME` set to a scratch folder, let a fake Codex raise an approval, click *Approve*, *Open* and the body, then kill the daemon and click again (the click is lost, the approval pending). Never run it against the real data folder.

## Start at login (M6-C, D-2)

This was "keep running" before D-2. Keep running (`app.keep_running`) now only leaves the daemon running after quit and registers nothing; both are off by default and only *Run Setup Again…* and Settings → General offer them (`docs/design.md` § First run).

Code: `crates/desktop/src/macos/login_item.rs`, `crates/desktop/src/workbench/mac.rs`; behavior in `docs/design.md` § macOS integration. The desktop registers `Contents/Library/LaunchAgents/dev.hutan.daemon.plist` with `SMAppService.agent(plistName:)` (`register()` / `unregister()`), which adds *Hutan* to System Settings → General → Login Items (macOS shows a notice the first time, and the user may have to allow it: status `requires approval`, button *Open Login Items*). Registration needs the app to run from the bundle; a `cargo run` desktop disables the toggle.

The agent plist, and why:

- `BundleProgram` `Contents/MacOS/hutand`, no `ProgramArguments`, no `EnvironmentVariables`: launchd starts the signed bundled daemon with no `HUTAN_*` variables, so it uses the standard data folder, socket and `hutand.log` (it writes its own log; no `StandardOutPath` is set, so nothing else is created). The desktop of the same user finds that socket by the same default.
- `RunAtLoad` **true**: the daemon starts when the agent is registered and at every login, so a notification click always has a daemon to reach (the point of starting at login, ADR 0007).
- `KeepAlive` `{ Crashed: true }`: launchd restarts the daemon after a crash and **only** then. `hutan daemon stop` and "Stop Daemon" end `hutand` with exit 0, and a daemon that finds another one holding the lock exits 2; neither may bring one back, which a bare `KeepAlive true` would. The xtask bundle test pins all three.
- `AssociatedBundleIdentifiers` `dev.hutan.Hutan`: the Login Items entry is shown as Hutan.

While start at login is active (registered and approved) the desktop starts with a connection that only dials, waits 4 s for a launchd-started daemon, and only then starts `hutand` itself; quitting the app never touches the daemon. The race that matters is a login-time one: the desktop and launchd both starting `hutand`. The daemon's lock makes the loser exit 2, so it cannot corrupt anything, but the desktop avoids the second start altogether.

**What it needs, and what `notFound` means** (FX-MENU-LOGIN). Apple's requirements for an agent registered with `SMAppService` ([`SMAppService`](https://developer.apple.com/documentation/servicemanagement/smappservice), the `ServiceManagement` header): the app is code signed (an unsigned or badly signed bundle makes `register()` fail with `kSMErrorInvalidSignature`; Hutan's ad-hoc signature is a signature); the plist is in the calling app's `Contents/Library/LaunchAgents` and named as passed to `agent(plistName:)`; `BundleProgram`, a path relative to the bundle, "allows apps to support a user relocating the app bundle after installation". No location is required for an agent: the advice to live in `/Applications` is for LaunchDaemons, which must be reachable before anyone logs in. The status `notFound` is documented only as "the framework couldn't find this service", and macOS answers it before the first registration as well: on macOS 27.0.1, a read-only probe of `SMAppService.agent(plistName: "dev.hutan.daemon.plist").status` inside ad-hoc signed copies of `target/bundle/Hutan.app` (nothing was ever registered from them) said `notFound` in `~/hutan-scratch/fxmenu/scratch/`, in `~/hutan-scratch/fxmenu/Applications/`, and with the plist deleted. So `notFound` does not say the plist is missing, and the app checks the file itself (`login_item::not_found_status`): with the plist in the bundle the agent is *not registered* and the toggle works; without it the status is *missing from Hutan.app* and the toggle is disabled with that reason. Before this, a fresh Hutan.app anywhere showed "Start Daemon at Login (not found)" and Setup said "The launch agent is missing from Hutan.app" (`docs/spikes/2026-10-23-e2e-usertest-3.md` N1). Whether `register()` itself succeeds from an ad-hoc signed bundle outside `/Applications` was not tried: nothing is ever registered on the development machine (the *Trying it by hand* step below, with the user present, is where that is checked).

**A moved or reinstalled app.** `SMAppService` does not say which copy of `Hutan.app` a registered agent belongs to, so a registration records it: the app's user default `LoginItemBundle` (`defaults read dev.hutan.Hutan LoginItemBundle`) holds the bundle path it was registered from, and unregistering removes it. When a window opens with start at login on and the agent registered (approved or not) from another path than the running bundle — the app was moved, or reinstalled elsewhere — the app unregisters it and registers it again from itself, once, and logs both paths; a refusal is shown like any other. An agent registered before the record existed (Hutan 0.1) is left as it is until start at login is switched off and on.

**Trying it by hand** (not part of any test; needs the user at the machine; never against the real data folder unless the user wants to): `just bundle`, copy `target/bundle/Hutan.app` to a folder under the home directory, start it, turn *Start daemon at login* on in Settings (`app`) or the Daemon menu, approve it in Login Items if asked, quit the app and check `launchctl print gui/$(id -u)/dev.hutan.daemon` and `hutan daemon status`; turn it off and check that the entry is gone and what happened to the running daemon.

## One desktop per data folder

`LSMultipleInstancesProhibited` in `Info.plist` makes Launch Services reactivate the running Hutan when it is opened again (Dock, Finder, `open`); the xtask bundle test checks the key. It does not cover `open -n` or running `Contents/MacOS/hutan-desktop` directly, so `hutan-desktop` enforces it too (`crates/desktop/src/instance.rs`, behavior in `docs/design.md` § Daemon lifecycle): before any window opens it takes an exclusive `flock` on **`desktop.lock`** in the data folder (`$HUTAN_HOME`, else `~/Library/Application Support/Hutan`) and listens on **`desktop.sock`** next to it (mode `0600`, peers of another user dropped, a stale socket file from a crash replaced under the lock). A second desktop sends `{"cmd":"activate"}` to that socket and exits 0; the first one brings its window forward. Two desktops with different `HUTAN_HOME`s do not know each other, which is what tests and `just bench` rely on.

## The daemon's menu bar item (M6-D)

Code: `crates/daemon/src/menu_bar.rs`, `menu_bar/cocoa.rs` (real), `menu_bar/fake.rs` (tests); behavior and menu in `docs/design.md` § macOS integration → *The daemon's menu bar item*.

- **When.** Only in a daemon that runs from `Hutan.app` (the same check as the native notifications: executable in `*.app/Contents/MacOS/`, main bundle `dev.hutan.Hutan`); a `cargo run` or test daemon shows no item and calls no AppKit. It is shown while no `desktop` client is connected and removed at once when one connects; after the last one leaves it waits 750 ms, so a desktop that is starting or reconnecting never meets a second icon. It is **not optional**: `app.menu_bar` governs the desktop's item only, `[notify] native = false` does not touch it.
- **Threads.** The item lives on the main thread, in the loop that already hosts the notifications. The first time there is something to show, the daemon creates the process's `NSApplication` as an *accessory* (no Dock icon, no application menu) and from then on the loop dequeues and dispatches AppKit events itself (`nextEvent…` / `sendEvent:`): status item clicks and the stop alert are delivered through the application's event queue, which a bare `NSRunLoop` never drains. Queued blocks still run (the main queue is served while the loop waits); a stop posts an application-defined event so that the wait ends. The host's calls only record what is wanted (the content, or none; the stop questions) and schedule one block on the main queue, so tokio never waits for Cocoa; clicks come back over a bounded channel. Shutdown removes the item before the process ends: the daemon's accept loop waits (2 s at most) for the item's task, which takes it off.
- **Opening Hutan.** *Open Hutan*, the *N need attention* entries and a reopen from Launch Services (Dock, Finder, `open`) start `Contents/MacOS/hutan-desktop` directly, in its own process group, with the daemon's environment (so a daemon on a scratch `HUTAN_HOME` / `HUTAN_SOCKET` opens a desktop on that folder). Not through Launch Services: a running `hutand` checks in as an instance of the app (same bundle identifier), so under `LSMultipleInstancesProhibited` Launch Services answers `open` — even `open -n` — with a reopen of `hutand`; when the daemon answered that with `open -n` it looped, launching every 50 ms (seen 2026-10-17, fixed in `ed8ab64`). One launch at a time: another request within 10 s (`LAUNCH_GRACE`) starts nothing unless a desktop connected meanwhile. A second desktop is harmless (it brings the first one forward and exits, § One desktop per data folder). The session or settings the user chose is then asked of the desktop through paths a connected client has already: `ui.open` for a session, `ui.run` of `workbench::OpenSettings` for the settings, as soon as one window has published (`ui.publish` happens after the connection is synchronised, so the window knows the session), and only within 30 s of the click. The application also answers a *reopen* (Launch Services reactivating the process, which carries the app's bundle identifier) by opening Hutan.
- **Tests.** `crates/daemon/tests/it/menu_bar.rs` runs a real daemon over its socket with a fake item and a fake launcher: shown with no desktop, hidden while a desktop is connected, back after the last one leaves, no icon for a desktop that returns within the delay, the live count and the dot (blocked agents, failures, approvals), `start_at_login` from `settings.toml`, open / settings / stop with and without sessions and the confirmation outcomes. The pure model is unit-tested in `menu_bar.rs`. No test touches `NSStatusBar`, `NSAlert` or launches the app; the desktop pins the settings action's name (`macos/status_item.rs`).
- **Trying it by hand** (not part of any test; needs the user at the machine; never against the real data folder): `just bundle`, copy `target/bundle/Hutan.app` to a folder under the home directory, run its `hutand` with `HUTAN_HOME` on a scratch folder (start the desktop with the same `HUTAN_HOME`), quit the desktop with *Keep running* on: the daemon's icon should be on the menu bar after about a second, with the menu above; open Hutan from it: the desktop starts and the daemon's icon goes away, leaving one icon; *Stop daemon…* with a running session asks first. Also check that clicking `Hutan.app` in Finder while only the daemon runs starts the desktop (see below).

## Menu bar glyph

`assets/macos/MenuBarIcon.png` (18 px) and `MenuBarIcon@2x.png` (36 px) are committed, black template images (only the alpha channel counts; macOS tints them for light, dark and highlighted states). `just menubar-icon` (`cargo run -p xtask -- menubar-icon`) renders them from `assets/brand/05-mascot/svg/favicon.svg` without its background tile and cuts the eyes and nostrils out; an xtask test checks sizes, that every pixel is black, and that rendering again reproduces the committed files. The desktop and the daemon (its own menu bar item) embed them (`include_bytes!`), so the executables need no resource folder. This native identity is not part of icon packs. The default in-window icon pack is `assets/icon-themes/phosphor/` (official Phosphor Regular, MIT, pinned revision and notice beside it); its manifest and SVGs are embedded into `hutan-desktop` and served only after the safe SVG pipeline, so `bundle` has nothing to copy for them.

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

- `Hutan-<version>-arm64.zip` (e.g. `Hutan-0.3.0-arm64.zip`)
- `Hutan-<version>-source.tar.gz`
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

### What a reinstall keeps, and a clean start

A reinstall or an update replaces `/Applications/Hutan.app` and nothing else: the data folder
(`~/Library/Application Support/Hutan`: `settings.toml`, `keymap.toml`, `daemon.toml`,
`desktop-state.json`, the session database, the logs) stays, so projects, sessions, settings and
`app.setup_done` stay. The app decides what to say about it, not the cask (no cask hook):

- It records which install it ran from in `desktop-state.json` (`install`: version, bundle path
  and an install id made of the bundle's creation time and the inode of `Contents/Info.plist`).
- **Reinstalled** (same or lower version, another install id, setup done before): a one-time card
  at the top of the centre, *Run setup again? Your projects, sessions and settings stay.* —
  **Run setup** (every answer starts at today's value, nothing is reset) or **Not now**.
- **Updated** (higher version): no setup offer; *Updated to x.y.z · What's new* in the status
  bar for that session, which opens the bundled `Contents/Resources/CHANGELOG.md` at the
  version's section.
- A development build outside an app bundle records nothing and never offers.
- *Run Setup Again…* is always there: Hutan ▸ Run Setup Again…, Settings → General, the command
  palette, and `hutan setup`.

For a **clean start** (the first screen again, as on a new Mac), remove the data too:
`brew uninstall --cask --zap robertt3kuk/hutan/hutan`, then
`brew install --cask robertt3kuk/hutan/hutan`. `--zap` removes the application support folder,
logs and preferences listed above; export what you want to keep first.

### Updates (the check and the brew update)

ADR 0024, protocol 0.60 (`docs/protocol.md` § Updates); the desktop's side is `docs/design.md` § Updates. Hutan tells you when a newer version exists and updates itself the way you installed it, when you ask. It never updates by itself.

**What is contacted.** The daemon (never the app) asks GitHub's public release list, at most once every 24 hours and whenever you choose *Check for Updates…* (the Hutan menu, ⌘K) or run `hutan update check`; the one more thing it ever contacts is, after you press *Update now* on a copied app, the release's archive and the tap's cask (§ A bundle copied by hand):

```text
GET https://api.github.com/repos/robertt3kuk/hutan/releases?per_page=30
User-Agent: Hutan/<version>
Accept: application/vnd.github+json
```

When the list names a release you do not have yet, one more request follows for that release: a `HEAD` for its archive (`https://github.com/robertt3kuk/hutan/releases/download/<tag>/Hutan-<version>-arm64.zip`, redirects followed, same `User-Agent`, nothing downloaded), see *What counts as an update*. Nothing else is sent: no identifier, no token, no cookie, nothing about your machine or your work except what any request reveals (your address, the Hutan version). GitHub's unauthenticated limit is 60 requests an hour per address; offline or rate limited, the check is silent and tries again after an hour. A development build (`cargo run`, `target/debug`) never checks by itself. The first-run setup (Background step) and **Settings → General → Updates** show the same words.

**How to turn it off.** `updates.check = "off"` in `daemon.toml` (Settings → General → Updates → *Check for updates*: *Only when I ask*, or `hutan config set updates.check off`): the daemon then contacts nothing until you ask. `updates.channel = "stable"` ignores releases that are marked pre-release (every release is one for now, so it offers nothing yet). `updates.skip_version` is what *Skip this version* writes.

**What counts as an update.** The highest version above the running one in the channel whose release carries `Hutan-<version>-arm64.zip`. The daemon keeps the release notes (bounded to 32 KiB) and the digests GitHub computed, and tells the desktop (`update.available`): *Update to x.y.z* in the status bar, with the notes in a sheet.

**A release that was just published (propagation).** GitHub lists a new release's assets, with their digests, some time before its download URL serves them: with 0.3.3 the URL answered `Not Found` (HTTP 404, 9 bytes) for about 40 seconds after `gh release create`. So a release is offered only once its archive answers a `HEAD` with a 2xx status — one extra request for a new release, none for one that was offered before, and none when the list has nothing new. Until then the automatic check says nothing (it is recorded as an ordinary check, no failure, no announcement), while a check you ask for — *Check for Updates…*, `hutan update check` (exit code 0) — says *Hutan x.y.z was just published; it can be installed in a minute* instead of "up to date" (the words are the check's `message` with outcome `checked`, which `hutan update status` repeats); whatever was offered before stays offered, and the daemon looks again after five minutes instead of 24 hours (`UpdateTuning::unready_retry`; the time survives a restart in `update.json`). If the download itself meets the same gap — any status but 2xx, or a body less than half of the size GitHub lists (`Not Found` in place of megabytes) — *Update now* on a copied app fails in the **Download** stage with *This release isn't downloadable yet — try again in a minute* and the sheet's **Try again**; it is never reported as a digest mismatch, which is kept for an archive that arrived whole and is not the one GitHub and the tap list.

**How Hutan was installed decides how it updates.** The daemon reads it from its own executable and Homebrew's Caskroom and Cellar (read-only; it never runs `brew` to find out): a cask (`brew install --cask robertt3kuk/hutan/hutan`), a formula (`brew install robertt3kuk/hutan/hutan`, CLI and daemon only), a bundle copied by hand, or unknown (a development build, a folder it cannot write).

- **Cask**: *Update now* (or `hutan update install`) opens a terminal session that runs `brew update && brew upgrade --cask robertt3kuk/hutan/hutan`. You see Homebrew's output, its password prompt and trust questions, and macOS's own dialogs (the release is ad-hoc signed: see § Security and uninstall). When Homebrew exits 0 the window offers *Relaunch*; a non-zero exit is said with its code and the terminal stays.
- **Formula**: the same with `brew update && brew upgrade robertt3kuk/hutan/hutan`; only the command line and the daemon change.
- **Bundle copied by hand**: *Update now* (or `hutan update install`) downloads the release, checks it and — when you press **Quit and Install** — replaces the app and opens the new one: § A bundle copied by hand below. Nothing changes before that button.
- **Unknown** (a development build, a folder Hutan cannot write): the release and its notes are shown, with the command to copy; nothing is offered automatically.

`update.install` is **user-only**: an agent can never update or downgrade Hutan. What the update does to running work is in § Updates and the running daemon below: sessions keep running while Homebrew works; after the relaunch an idle daemon moves onto the new build by itself, and one with sessions running asks first. `hutan update status|check|install [--wait]` does the same from a terminal.

**Tests** run a real daemon against a local mock of the releases API and a fake `brew` first on `PATH` (`crates/daemon/tests/it/update.rs`, `crates/cli/tests/it/update.rs`, `crates/desktop/tests/it/update_gpui.rs`): no test contacts GitHub, runs a real `brew upgrade` or touches `/Applications`. By hand, `HUTAN_UPDATE_API=http://127.0.0.1:<port>` points a `hutand` at a mock (it also lets a development build check by itself, and — unless `HUTAN_UPDATE_DOWNLOADS` names another server — the mock also answers the `HEAD` for a release's archive, so a hand-made source never leads to GitHub); `HUTAN_UPDATE_DOWNLOADS` and `HUTAN_UPDATE_TAP` do the same for the release download and the tap's cask (a mock serves `HEAD` and `GET` of the archive); the recorded release JSON the screenshots use is the one the tests serve.

#### A bundle copied by hand (UPDATES-2)

Code: `crates/daemon/src/update/bundle.rs`; the window's side is `crates/desktop/src/workbench/update.rs` (`docs/design.md` § Updates). The daemon replaces the app itself, in three steps, and only the third changes anything outside Hutan's data folder:

1. **Download, verify, unpack, check** (*Update now*, or `hutan update install [--wait]`). The daemon downloads `Hutan-<version>-arm64.zip` from the release (`https://github.com/robertt3kuk/hutan/releases/download/<tag>/…`, redirects to GitHub's asset host followed, nothing sent but `User-Agent: Hutan/<version>`) into `update-staging/` in the data folder, at most 512 MiB and at most 15 minutes (30 s of silence stops it). Its SHA-256 must equal **both** the digest GitHub lists for the asset and the `sha256` in the tap's `Casks/hutan.rb` (`https://raw.githubusercontent.com/robertt3kuk/homebrew-hutan/<tag>/Casks/hutan.rb`; a tap that has no tag for the release is read on its `main`, and the cask's `version` must be the release's). Then `ditto -x -k` unpacks it, `codesign --verify --deep --strict` must accept the unpacked `Hutan.app`, and its `Info.plist` must say `dev.hutan.Hutan` and the release's version (an older archive behind a newer release is refused). Any refusal stops here with the reason in the sheet (*the download's SHA-256 … is not the one GitHub lists*, *… not the one in the tap's Casks/hutan.rb*, *macOS does not accept the new Hutan.app's signature*, …), removes the staging folder and leaves `Hutan.app` exactly as it was. The archive is downloaded without the quarantine attribute (Hutan is not a browser), so macOS asks nothing about the new app.
2. **Ready.** The sheet shows *Download · Verify · Ready* with the bytes received while it downloads, then **Quit and Install**. Nothing has changed yet; *Later* leaves the verified app staged (it stays staged until the daemon restarts or the next update).
3. **Quit and Install.** The window asks the daemon to replace the bundle after the window's own process has exited, and quits (never asking; the daemon and its sessions keep running). The daemon waits for that process (60 s at most), copies the new bundle beside the old one (`.Hutan-update-<pid>` in the same folder), moves the old `Hutan.app` to the Trash (`NSFileManager trashItemAtURL`, like `fs.trash`: it can be put back), renames the copy into place and opens it with `open -n` — the one place Hutan opens its own app, by design. A step that fails puts the old bundle back, says why, and opens the old app again so the window returns with the explanation. The new app finds the old daemon on the socket and the usual build-mismatch flow of § Updates and the running daemon applies.

**A folder Hutan cannot write** (`/Applications` of a managed Mac, a read-only volume) is never attempted: the app is still downloaded and verified, and the sheet shows the command to run by hand instead of *Quit and Install* (`sudo mv <app> "$HOME/.Trash/Hutan (old).app" && sudo ditto <verified new app> <app> && open -n <app>`, with the paths filled in; *Copy command*). A folder that stops being writable after the check is found out again right before the window quits. A Hutan that is not in a writable folder when the daemon starts is `unknown` and shows the release page and the Homebrew command instead.

From a terminal: `hutan update install --wait` prepares and waits for `ready`; after quitting the app, `hutan update install --apply` puts the new app in place (`--after-pid PID` waits for that process first and opens the new app; without it nothing is opened beside a running app).

**Tests** (`crates/daemon/tests/it/update_bundle.rs`, `crates/desktop/tests/it/update_bundle_gpui.rs`, `crates/cli/tests/it/update.rs`): a mock of GitHub serves the release list, a real `ditto`-made archive of a fake `Hutan.app`, and the tap's cask; `codesign` and `open` are fakes that log their arguments, the Trash is a folder, `quit_timeout` and the limits are tuned. Covered: a digest that GitHub does not list, a release without a GitHub digest, a digest the tap does not list, a tap without or with another version's cask, a signature that macOS rejects, an archive of another version, another app or no archive, a download over the limit, a refused or redirected download, the stages and byte counts, the swap after a process exits (old app in the Trash, new one in place, `open -n`), a swap that fails (old app kept, app reopened), a quit that does not happen in time, an unwritable folder (the command), and an agent that is refused. No test touches `/Applications`, the real Trash or a user's Hutan.

### Updates and the running daemon

An update replaces the bundle while the old `hutand` may still run (started by the old app, by
launchd, or by the CLI); the new app finds it on the socket and compares its build
(`Welcome.daemon_build`, protocol 0.42; architecture.md § Daemon lifecycle) with the `hutand`
beside itself:

- **Idle old daemon** (no live session): the app restarts it onto its own `hutand` by itself —
  a graceful `daemon.shutdown`, then a start — and says *Updated the background service to
  x.y.z*.
- **Old daemon with live sessions**: a line under the title bar asks first: *The background
  service still runs Hutan 0.1.0 (from …). Restart it with 0.2.0? Running terminals are
  restored; agent turns in progress are interrupted.* — **Restart Now** or **Later** (asked again
  at the next launch).
- **Newer daemon than the app** (an older app opened after an update): *update the app*, with
  *Copy Update Command*; the daemon is left alone.
- **A daemon from another executable** (a formula install, a development build): *Connected to
  a Hutan service from <path>* — **Use This App's Service** (restart onto the app's `hutand`) or
  **Keep**.

From a terminal, `hutan daemon status` prints the running build and binary and how they differ
from the `hutand` beside the CLI; `hutan daemon restart` starts that one.

### Publish after review (maintainer only)

Merge the source changes before preparing the final release, review both tap
definitions, and run the local tap checks documented in its README. The source
archive must represent the commit to tag. The release notes are the version's
`CHANGELOG.md` section plus install/upgrade commands and the Gatekeeper note, written
to `target/release-artifacts/NOTES.md`. From the Hutan checkout (0.3.0 as the example):

```sh
cargo run -p xtask -- release --tap /path/to/homebrew-hutan
git push origin master
git tag -a v0.3.0 -m "Hutan 0.3.0"
git push origin v0.3.0
gh release create v0.3.0 \
  target/release-artifacts/Hutan-0.3.0-arm64.zip \
  target/release-artifacts/Hutan-0.3.0-source.tar.gz \
  --repo robertt3kuk/hutan --verify-tag --prerelease --title "Hutan 0.3.0" \
  --notes-file target/release-artifacts/NOTES.md
```

Then, from the tap checkout, commit the generated definitions and publish:

```sh
git add Casks/hutan.rb Formula/hutan.rb README.md
git commit -m "hutan 0.3.0"
git tag -a v0.3.0 -m "hutan 0.3.0"
git push origin main v0.3.0
```

The tap's tag is the release's tag: a Hutan.app copied by hand checks its download against
`Casks/hutan.rb` **at that tag** (§ A bundle copied by hand); without the tag it reads the
tap's `main`, which is only right until the next release is published there.

These are manual publishing commands, not actions performed by `xtask release`.
Hutan is MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`; the formula says
`license any_of: ["MIT", "Apache-2.0"]`), its dependencies' licenses are checked by
cargo-deny. Developer ID signing and notarization remain open distribution decisions
(ADR 0007/0008).

## When the daemon does not start (troubleshooting)

A `hutand` that cannot start says why in three places (architecture.md § Start errors):
its log (`hutand.log`, the whole error chain), a small record next to the socket
(`hutand-start-error.json`) and, when the app or the CLI started it, what it printed
(`hutand-start.log` next to `hutand.log`). The app shows the reason in one line under the
title bar, in the daemon popover and in the *Daemon* menu; `hutan daemon start` prints it and
exits non-zero; `hutan daemon status` adds it while the daemon is down; `hutan daemon why`
prints the record and the ends of both logs.

- **Data from a newer Hutan (downgrades).** Migrations only go forward: a database written by
  a newer Hutan is refused, never rewritten ("Your Hutan data was written by a newer Hutan
  (database version 13; this build reads up to 12). Update Hutan: `brew update && brew
  upgrade --cask robertt3kuk/hutan/hutan`."). Updating is the fix. If the newer version is gone for good, *Start with Empty
  Data…* (app) or `hutan daemon reset-data --yes` (CLI) moves `hutan.db` and its `-wal`/`-shm`
  into `backups/hutan-<ms>-v<found>-newer.db` next to it and starts with empty data: no
  workspaces, sessions or chats. Nothing is deleted, pruning of the migration backups never
  touches these files, and a newer Hutan can open the moved database again (move it back to
  `hutan.db` while no daemon runs). Both refuse while a daemon holds the lock.
- **Another `hutand` holds the lock but does not answer.** The record names its pid. The app
  offers *Show the Process* (pid and executable) and *Stop It…* (SIGTERM to exactly that pid,
  after a confirmation, only if it is a `hutand`), then starts a new daemon. From a terminal:
  `hutan daemon why`, then `kill <pid>`.
- **Anything else** (a socket path that is too long, permissions, a migration error, a broken
  `daemon.toml`): the message, *Open Logs* and *Copy Details* in the app; `hutan daemon why`
  in a terminal.

## What is still open

- The Keychain behavior across rebuilds.
- A stable signing identity, and notarization when there is a team.
- Verified 2026-10-17 on the real bundle id: with only `hutand` running, `open -a Hutan.app` (the Dock's path) reaches the daemon as a reopen; the daemon starts the desktop once and the window opens; opening again with the window up brings it forward (one desktop process).
