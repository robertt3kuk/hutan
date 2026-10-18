# Process model and lifecycle: headless daemon, desktop-owned presence

**Status:** research and ADR proposal; D-2 remains a user decision.  
**Recommendation:** a headless daemon on every OS, one desktop-owned main window and tray/menu-bar item, and `app.on_close = "quit"` for new installations. Preserve existing background-running preferences during migration.

This report replaces the process-presence portions of M6-C/M6-D and the daemon-notification decision in ADR 0007. It does **not** propose moving execution into the desktop or adding a notifier helper.

## 1. Findings

### 1.1 Hutan’s current behavior is more complicated than its default suggests

| Area | Observed implementation |
|---|---|
| Defaults | `keep_running=false`, `start_at_login=false`, `on_last_window_close="quit"`, `menu_bar=true`. `crates/desktop/src/settings.rs:893-935`. |
| Ordinary Quit | Stops an idle daemon; with live sessions, offers **Stop Sessions & Quit / Keep Running in Background / Cancel**, unless background-running or approved login registration already lets the daemon outlive the desktop. The background choice permanently enables `keep_running`. `crates/desktop/src/workbench/daemon.rs:541-598`. |
| Dock Quit/logout/shutdown | The platform-quit callback stops only an idle daemon. Live sessions are deliberately left running. `crates/desktop/src/workbench/daemon.rs:600-625`. |
| Desktop signals | SIGTERM/SIGINT/SIGHUP save/disconnect/quit without stopping the daemon. `crates/desktop/src/workbench/daemon.rs:627-630`; `crates/desktop/src/app.rs:443-460`. |
| Hide to menu bar | Keeps the window and its model alive, switches macOS activation policy to accessory, and hides the app. Reopening switches back to regular activation. `crates/desktop/src/workbench/mac.rs:464-495`; `crates/desktop/src/macos/presence.rs:90-123`. |
| Daemon native host | `hutand` creates a native notification/menu host, places Tokio on a separate thread, and gives its main thread to Cocoa when bundled. `crates/daemon/src/main.rs:72-121`. |
| Daemon reopen workaround | The daemon implements `applicationShouldHandleReopen:hasVisibleWindows:` and directly launches sibling `hutan-desktop`. `crates/daemon/src/menu_bar/cocoa.rs:64-103,441-465`. |
| Login startup | `SMAppService.agentServiceWithPlistName` registers the bundled daemon LaunchAgent. The plist uses `RunAtLoad` and `KeepAlive.Crashed`, not unconditional restart. `crates/desktop/src/macos/login_item.rs:215-291`; `assets/macos/dev.hutan.daemon.plist:16-30`. |
| Desktop instance | One desktop per data directory; subsequent launches request activation. `crates/desktop/src/app.rs:314-343`. Reopen finds an existing workbench before opening another. `crates/desktop/src/app.rs:364-431`. |

The extra native daemon lifetime is not intrinsic to the architecture. ADR 0001 already says that `hutand` must not link GPUI/AppKit UI code; M6-B/M6-D subsequently introduced that coupling. `docs/adr/0001-daemon-authoritative-topology.md:27-30`.

### 1.2 “Restore” does not mean that processes survived

Hutan already distinguishes:

- shell respawned in its saved directory;
- agent resume attempted using recorded resume arguments;
- chat conversation resumed, possibly with an interrupted turn;
- session listed but not restarted;
- session ended before the restart.

The recovery DTO explicitly says a shell’s previous foreground command is **not running now**, and that screen/scrollback are lost. `crates/protocol/src/recovery.rs:35-65`.

Clean shutdown preserves the restore set unless `forget=true`. `crates/daemon/src/registry.rs:475-478,2394-2405`. Shutdown stops/reaps sessions, drains connections, records the clean epoch stop, closes storage, and only then releases socket/lock ownership. `crates/daemon/src/daemon.rs:578-632`.

Therefore the default close wording must be:

> Stop local sessions and quit. On next launch, shells restart in their folders and supported agents reconnect to their conversations. Running commands do not continue.

It must not say “resume everything where you left off.”

### 1.3 Orca’s useful precedent is separation—not “everything closes”

Official Orca source inspected: `/Users/workie/proj/orca-src-git`, corresponding to [`stablyai/orca`](https://github.com/stablyai/orca), MIT.

- Detached daemon spawn uses a separate process lifetime and a stable data-directory cwd. `src/main/daemon/daemon-launched-child-spawn.ts:74-114`.
- Ordinary quit and update cleanup disconnect the terminal daemon instead of shutting it down. `src/main/startup/main-process-quit.ts:123-129,261-264`.
- Structured-agent runtimes are separately stopped on quit/update. Terminal survival must not be generalized to every agent transport. `src/main/startup/main-process-quit.ts:146-149`.
- The desktop owns the tray callbacks: Open, Settings, Check for Updates, Quit. `src/main/tray/system-tray.ts:10-29`.
- macOS normally stays alive after the last window closes; other platforms normally quit. `src/main/startup/window-all-closed-quit-policy.ts:1-10`.
- Orca versions daemon endpoint/token/PID names by its daemon protocol. `src/main/daemon/daemon-spawner.ts:134-153`.

**Adopt:** desktop-owned presence, deliberate daemon preservation during compatible UI updates, explicit quit policy, bounded teardown.

**Do not copy:** a second live daemon for each incompatible protocol when both would own the same Hutan database.

### 1.4 Detached POSIX processes do not escape systemd lifecycle

Orca explicitly documents that `detached: true` creates a process group, not a different cgroup. A combined desktop/server unit can therefore kill the daemon and PTYs when stopped or restarted. Its solution uses a sibling `systemd-run --user --scope`, with capability and user-manager-lifetime checks. `src/main/daemon/daemon-cgroup-scope.ts:1-18,132-166`.

This matches official systemd documentation:

- `KillMode=control-group` kills remaining processes in the unit.
- `KillMode=mixed` eventually SIGKILLs remaining cgroup members, including when the main process exits.
- `KillMode=process`/`none` are discouraged rather than a sound persistence solution.

Source: [systemd.kill](https://www.freedesktop.org/software/systemd/man/latest/systemd.kill.html#KillMode=).

---

## 2. Options and tradeoffs

| Option | Benefits | Costs / why not |
|---|---|---|
| Keep daemon-owned native UI | Existing code; background daemon remains visible | Two presence owners; Launch Services identity/reopen complexity; desktop settings do not consistently control daemon presence; poor cross-platform boundary |
| Desktop owns PTYs; daemon exists only in background mode | Superficially fewer processes in foreground | Two execution topologies; breaks daemon-authoritative model, remote symmetry and consistent CLI access |
| **Headless daemon; desktop owns window/tray/notifications** | One execution topology; one native UI owner; cross-platform and remote-ready; no daemon reopen hacks | Two normal processes remain while using local Hutan; UI-free mode intentionally has no tray or native notification renderer |
| Separate always-running notifier/tray helper | Notifications/presence without full desktop | Third lifecycle owner and IPC client; contradicts the simpler user-requested model; unnecessary when hide-to-tray already exists |

**Recommendation:** the third option. Keeping the two-process architecture is an implementation choice, not permission to leave both processes running after default Close.

---

## 3. ADR draft: Process ownership and lifecycle

### Context

Hutan’s daemon is the source of truth for sessions, workspaces, terminals, agents and durable state. Desktop and CLI are clients.

Current macOS integration gives both desktop and daemon native application behavior. The daemon can own a menu-bar item, receive application reopen events and deliver notifications. The resulting ownership handoff requires platform-specific behavior and has caused Dock/Finder reopen problems. Other OSes lack equivalent implementations.

Users need three distinct workflows:

1. close Hutan and leave nothing running locally;
2. close the window but keep Hutan available in the tray while sessions run;
3. quit the desktop while a headless daemon continues executing sessions.

Explicit user shutdown, UI restart/update, UI crash, and OS logout are different lifecycle events. They must not accidentally share destructive behavior.

### Decision

#### A. Process ownership

1. `hutand` is headless on every OS. It owns execution and durable state, but no windows, tray/menu-bar item, Dock identity, notification permission UI or application-reopen delegate.
2. `hutan-desktop` owns all graphical presence and native notifications.
3. There is one main workbench window per desktop instance. Reopen, notification activation, tray Open and duplicate launch all converge on the same activation function.
4. Preferences and other ordinary surfaces remain inside the main workbench. No implicit second main window is created.
5. Desktop restarts, crashes and compatible UI updates do not stop daemon-owned sessions.
6. An intentional user-selected stop-and-quit operation may stop the **local** daemon through the protocol. A client disconnect alone never stops a daemon.

This clarifies ADR 0001’s survival rule: **unintentional UI loss and UI restart preserve execution; an explicit lifecycle command may deliberately stop it.**

#### B. Close modes

```toml
[app]
on_close = "quit"              # quit | hide_to_tray | quit_ui
tray = true
start_at_login = "off"        # off | window | tray | daemon
dock_badge = true
setup_done = false
```

Default for new installations: `on_close="quit"`, login startup off.

| Close mode | Main window | Desktop | Local daemon/sessions |
|---|---|---|---|
| `quit` | Closes after shutdown decision | Exits | Stop cleanly, preserve restore records |
| `hide_to_tray` | Hidden | Remains with tray | Continue running |
| `quit_ui` | Closes | Exits | Continue headlessly |

`hide_to_tray` is available only when the desktop can actually provide a tray/menu-bar item and `app.tray=true`.

If tray creation fails or the tray host disappears, Hutan keeps or reveals the main window and explains the problem. It must not silently substitute stop-and-quit.

The hidden workbench may remain allocated initially, matching the existing implementation. It must suspend hidden rendering, blink timers and unnecessary refresh work. “Small tray presence” must be measured rather than inferred from window invisibility.

#### C. Quit commands and native termination

Provide unambiguous commands:

- **Quit Hutan & Stop Local Sessions**
- **Quit UI — Keep Sessions Running**
- **Open Hutan**
- **Settings**

The normal application Quit command follows the selected lifetime:

- `quit` or `hide_to_tray`: stop local sessions and quit;
- `quit_ui`: quit only the desktop.

In `quit_ui` mode, label the application-menu command **Quit UI — Keep Sessions Running**. The explicit stop-and-quit command remains available.

In `hide_to_tray`, tray **Quit Hutan** must genuinely exit the desktop; it must not invoke the window-close/hide operation again.

Before an interactive stop with live work, show the running-session count and choices:

- **Stop Sessions & Quit**
- **Keep Sessions This Time**
- **Cancel**

“Keep this time” does not change settings. Persistent behavior changes belong in Settings.

Dock Quit uses the same resolved lifetime as application Quit, not the old “live sessions always survive” exception. Native termination must be integrated with GPUI’s existing delegate rather than installing a competing delegate.

Where the native callback cannot reliably distinguish user Quit from logout, execute the already configured policy without a new modal. Never block OS logout awaiting user input.

Apple documents that application Quit/logout invokes `applicationShouldTerminate:` and does **not** imply `windowShouldClose:`. A close-button hook alone is insufficient. See [Apple graceful termination guidance](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/AppArchitecture/Tasks/GracefulAppTermination.html).

#### D. Local versus remote ownership

`app.on_close` controls this desktop and its local application lifecycle—not all hosts displayed in the window.

- Closing/quitting never automatically stops remote daemons.
- SSH disconnect is not a remote shutdown.
- Remote shutdown requires a separate, explicitly targeted command naming the host and affected sessions.
- Stopping a local daemon shared with CLI or another client affects those clients too. The confirmation must say so; connection ownership is not inferred from parent PID.

For a UI connected only to remote hosts, `quit` simply exits the desktop.

#### E. Notifications

The daemon raises notification intents. The desktop renders native notifications and routes actions back through the existing protocol.

When no desktop is running:

- sessions and daemon notification policy continue;
- pending intents remain available through `notify.list` and CLI consumers;
- no tray/menu-bar item exists;
- no native notification is promised;
- no desktop/helper is silently launched just to notify.

Settings and onboarding explicitly explain:

> To receive native notifications while the window is closed, use “Keep sessions, hide to tray.” “Quit UI” has no Hutan tray or native notifications.

Existing notification intents are bounded, pending until acknowledged/acted/expired, and approval intents follow approval lifetime. They are not currently a durable notification inbox. `crates/protocol/src/notify.rs:78-119`; `crates/daemon/src/notify.rs:440-495`.

Desktop behavior:

1. Subscribe, obtain a notification snapshot/watermark, then reconcile newer events.
2. Deduplicate by host key + daemon epoch + intent ID.
3. Posting a banner is **not** `notify.ack`; the user has not necessarily seen it.
4. Remove banners on intent withdrawal, expiry, resolution or epoch replacement.
5. Invoke `notify.act` for actions; daemon authorization and current approval state remain authoritative.
6. On reconnect, do not replay an old burst as fresh banners. Show pending attention in the app, with a bounded catch-up summary.
7. On desktop exit, remove actionable native banners where supported. Any later stale activation must revalidate against the current daemon epoch and intent state.

Moving the poster does not prove that macOS will relaunch the desktop for a notification click after exit. The earlier spike found dropped clicks when its poster was absent; retain that limitation unless separately demonstrated. `docs/adr/0007-macos-packaging-and-notifications.md:11,26-27`.

#### F. Login startup

Login startup is independent of close behavior:

- `off`: register nothing.
- `window`: start the desktop normally.
- `tray`: start the desktop hidden, after confirming tray availability.
- `daemon`: start only headless `hutand`.

A login registration never overrides `on_close`. A clean explicit stop remains stopped for the rest of that login; it may start again at the next login.

No root service, automatic lingering, or unattended registration is introduced.

#### G. Updates

An update is a restart transaction, not a user Close:

1. Flush desktop layout/settings and disconnect.
2. Preserve the daemon if the replacement desktop can interoperate with it.
3. Start the new desktop and negotiate protocol/capabilities.
4. Reattach without replacing sessions or changing daemon epoch.
5. If the daemon must change, present a separate restart decision. Do not silently kill sessions.

Major mismatch must not trigger another daemon against the same data directory. Continue with the old compatible UI or explain that a coordinated daemon restart is required.

### Consequences

- Removes daemon AppKit run-loop hosting, tray handoff and Dock reopen machinery.
- Restores ADR 0001’s headless execution boundary.
- Default close can leave no local Hutan process running, while opt-in persistence remains available.
- UI-free background operation is intentionally invisible except through OS process tools and CLI.
- Native notifications require a running desktop.
- Users must understand that stop-and-restore reconstructs processes; it does not preserve an in-progress command.
- Cross-platform integration becomes desktop/platform work rather than daemon behavior.
- ADR 0007’s daemon notification ownership and M6-D’s mandatory daemon menu item are superseded. ADR 0009’s current restart limitation remains; live PTY handoff is not claimed by this ADR.

---

## 4. OS lifecycle contract

### 4.1 Startup and service ownership

| OS | Daemon login target | Desktop window/tray login target |
|---|---|---|
| macOS | Existing bundled LaunchAgent via SMAppService; headless `hutand`; retain clean-exit-does-not-restart behavior | Separate desktop login entry/LaunchAgent starting `hutan-desktop`, with a startup-mode argument |
| Linux/systemd | Dedicated `hutan-daemon.service` owned by the user manager | Graphical autostart entry; on supporting desktops this may be managed by a user unit, but must wait for valid graphical-session environment |
| Windows | Per-user Startup shortcut to installed `hutand.exe` | Per-user Startup shortcut to desktop with normal/hidden startup argument |

For Linux, do not attach the daemon’s lifetime to the desktop unit with `PartOf=hutan-desktop.service` or `BindsTo=`. The daemon must occupy a separate service cgroup. For on-demand launch without a persistent unit, use a supported sibling transient user service/scope. Reuse a running daemon through the normal lock/handshake.

A daemon service can use:

```ini
[Service]
Type=simple
ExecStart=/absolute/installed/path/hutand
Restart=on-failure
RestartPreventExitStatus=2
KillMode=mixed
TimeoutStopSec=15s
```

Here `mixed` is acceptable because stopping **the daemon’s own unit** is intended to stop its terminals. It is not a technique for letting terminals survive daemon shutdown. The daemon must finish its own bounded session shutdown before exiting; otherwise systemd finishes cgroup cleanup.

For a user without a usable systemd user manager, direct detached spawn is a fallback with explicitly weaker supervisor/logout guarantees. Never display “survives service restart” based only on `setsid`/process-group detachment.

Graphical startup follows the [XDG Autostart specification](https://specifications.freedesktop.org/autostart/latest/), rather than assuming a user-manager process already has `DISPLAY`, `WAYLAND_DISPLAY` and the graphical session bus.

Windows Startup is login launch, not crash supervision. Microsoft identifies per-user Startup folders as a supported startup mechanism and emphasizes user control through Startup Apps/Task Manager: [Desktop Startup apps](https://learn.microsoft.com/en-us/windows/compatibility/startup-apps). Resolve the Startup folder through Known Folders; do not hard-code a localized pathname.

### 4.2 Quit/logout/shutdown matrix

| Event | Expected behavior |
|---|---|
| Close, `quit` | Confirm live work when interactive; stop local daemon; preserve restore records; exit desktop |
| Close, `hide_to_tray` | Hide only after tray is functional; keep execution |
| Close, `quit_ui` | Save/disconnect/exit desktop; daemon continues |
| Dock/application Quit | Resolve lifetime as specified above; never reinterpret it as Close-to-tray |
| Compatible UI update | Preserve daemon irrespective of `on_close` |
| Desktop crash / SIGKILL | Daemon survives where the OS supervisor permits; no claim that final UI state was flushed |
| Desktop SIGTERM/SIGINT/SIGHUP | Noninteractive save/disconnect; leave daemon; do not convert generic automation signals into “stop all sessions” |
| OS logout/shutdown | No interactive confirmation; save promptly; local service/supervisor termination handles headless daemon shutdown; restore at next launch |
| Remote connection loss | Mark host unreachable; never mark its processes exited solely from disconnect |

The desktop-signal behavior deliberately differs from an explicit Quit action. This protects execution during supervisor replacement and automation; it must be documented rather than hidden.

…

…
This was source/documentation research only. No repository files, user application state, login registration, credentials or Keychain contents were changed. No Hutan binaries, builds, tests, model calls or lifecycle experiments were run.

[Showing lines 1-300 of 652. Use :301 to continue]