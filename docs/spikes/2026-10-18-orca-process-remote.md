# Orca process model, remote hosts, cross-platform and performance (stablyai/orca, MIT)

Read-only scout report, 2026-10-18. Official sources only: stablyai/orca (onorca.dev, installed Orca's package.json), herdrdev/herdr (herdr.dev, Cargo.toml), NousResearch/hermes-agent (hermes-agent.nousresearch.com). Feeds docs/spikes/2026-10-18-direction.md.

## Summary

**Important correction to the premise:** current Orca is **not** simply “close window → everything stops.” It has a **detached persistent terminal daemon**, macOS app-without-windows behavior, a menu-bar/tray control, optional Windows close-to-tray, and separate headless `orcad` runtime hosts. Terminal processes normally survive **app quit and updates**.

All paths below are relative to `/Users/workie/proj/orca-src-git`. Read-only source investigation; no commands/tests executed.

## 1. Process model and lifecycle

| Area | Finding and source |
|---|---|
| Desktop | Electron main creates the desktop `BrowserWindow`; its renderer is a separate Electron renderer. `src/main/window/createMainWindow.ts:61-64,102-105`. Main owns services/runtime orchestration; terminal UI uses xterm in renderer. |
| Terminal host | A **separate detached child**, not merely renderer-owned PTYs: `detached: true`, ignored stdin/stdout, stderr pipe and startup IPC: `src/main/daemon/daemon-launched-child-spawn.ts:112-114`. Child startup disconnects IPC and calls `unref()`: `src/main/daemon/daemon-launched-child.ts:219-220`. |
| IPC | Versioned Unix socket on POSIX; versioned Windows named pipe, runtime-directory hash distinguishing installations: `src/main/daemon/daemon-spawner.ts:133-155`. Token/PID files also include protocol version: `:155-162`; PID file publication uses mode `0600`: `:159-168`. |
| Where shells/agents live | Daemon spawns native `node-pty` terminals; shell and terminal-launched agent descendants belong to that host, not the renderer. Windows enables bundled ConPTY: `src/main/daemon/pty-subprocess/native-pty-spawn.ts:23-56`. **Structured/native-chat agents have different lifecycle**: main explicitly tears down structured-agent runtime on quit/update: `src/main/startup/main-process-quit.ts:146-149`. Do not equate “terminal survives” with “every agent transport survives.” |
| Quit | Normal quit **disconnects rather than shuts down daemon**; only dev-parent shutdown takes `shutdownDaemon()`: `src/main/startup/main-process-quit.ts:261-264`. `killAllPty()` kills only in-process fallback PTYs; daemon PTYs are explicitly preserved: `src/main/ipc/pty/kill-all.ts:1-10`. |
| Updates | Same daemon-disconnect preservation path, explicitly recorded as update behavior: `src/main/startup/main-process-quit.ts:125-129,261-264`. State/auth flushed before restart: `src/main/update-restart-preservation.ts:7-16`. This supports warm reattach, **not a guarantee across incompatible daemon protocol upgrades/OS termination**. |
| Window close, macOS | Last-window-close **does not quit** unless already quitting. Windows/Linux normally do quit. Serve mode stays alive independently of platform: `src/main/startup/window-all-closed-quit-policy.ts:1-10`; registration `src/main/startup/main-process-quit.ts:307-319`. |
| Windows tray option | Closing hides the window only when `minimizeToTrayOnClose === true`, renderer is healthy, and quit is not underway; one-time notification says Orca is still running: `src/main/window/main-window-close-lifecycle.ts:63-89`. |
| Menu-bar/tray control | Open/recreate main window, Settings, update check, **Quit for real** are explicit callbacks: `src/main/tray/system-tray.ts:10-25`. Icon retained for app lifetime: `:28-30`; macOS template/Retina appearance handling: `:66-78`. |
| Headless special case | macOS native Quit for a serve host closes desktop windows rather than killing paired clients; explicit process quit, supervisor/signal/update paths bypass protection: `src/main/startup/serve-native-quit-guard.ts:4-23`. |
| Single instance | Electron lock; losing process does not own activation. Winner handles `second-instance` argv: `src/main/startup/single-instance-lock.ts:40-47`. Duplicate `serve` launch must **not** open desktop: `:16-18`. Desktop activation captures deep links/documents, redirects sidecar launch to desktop handoff, then requests activation gate: `src/main/index.ts:47-64`. |
| “Single window” qualification | One primary main-window reference/activation route, **not an absolute one-BrowserWindow design**: separate dashboard popouts and workspace-space windows exist: `src/main/window/dashboard-popout-window.ts:154-158`; `src/main/window/workspace-space-window.ts:44-48`. |

Other quit-owned children are deliberately stopped: plugin hosts with escalation (`main-process-quit.ts:136-143`), browser helpers (`:195-207`), WSL hook relay children (`:192-193`), SSH leases marked detached before final persistence (`:211-215`). Thus a surviving daemon is intentional; unrelated child leakage is not.

## 2. Remote / SSH / WSL / mobile / runtime hosts

### Architecture

`desktop UI → environment-specific runtime connection → authenticated RPC/WebSocket → remote orcad → remote services + terminal daemon/PTys`

SSH is generally **bootstrap + secure tunnel**, not “one SSH exec for every UI operation.”

- **Current SSH-host policy:** deploy managed `orcad` on empty host; journaled conversion for existing Orca state; reconnect/update already-converted hosts. Live or unprovably-exited legacy relay terminals defer migration; unsupported hosts retain pinned-relay fallback. This is documented directly in `src/main/ssh/ssh-host-server-on-connect.ts:1-10`.
- Decision exposes `deploying/converting/connecting/updating/starting`, with managed result containing `environmentId`: `:44-55`. Thus `ssh-host-server*` is largely a **host-selection/migration lifecycle layer**, not the executable server implementation.
- **Installation:** versioned `orcad-<version>/` bundle; ensure pinned remote Node runtime; install lock, upload directory, POSIX executable permissions, version marker, finalize installation: `src/main/ssh/orcad-remote-install.ts:24-48,60-121`. Shared `OrcadHostTransport` enables both SSH and WSL installation.
- **Transport:** prefer SSH local TCP forwarding, supporting ssh2 and system-SSH providers; fall back to stdio bridge when sshd refuses forwarding: `src/main/ssh/orcad-managed-tunnel-transport.ts:1-5,43-47,97-108`.
- **Runtime authentication is additional to SSH authentication:** tunnel identity verification completes pairing handshake/status request; preferred paired connection is checked, not merely “port opened”: `src/main/ssh/orcad-managed-tunnel-identity.ts:1-4,57-82,89-107`. Pairing carries endpoint, device token, pinned public key; demonstrated by identity integration fixtures `src/main/ssh/orcad-managed-tunnel-identity-restart.test.ts:49-65`. Process runtime IDs may change after restart; pinned identity/token remain the authority.
- SSH supports system transport for features ssh2 cannot cover, e.g. ProxyCommand/GSSAPI paths; transport selection considers identities/system binary availability: `src/main/ssh/ssh-transport-selection.ts:5-9,107-109`; corresponding behavior tests `ssh-connection-system-transport.test.ts:210-214`, `ssh-system-transport.integration.test.ts:214-216`.
- **Multiple hosts:** tunnel registry keyed by `environmentId`, bound to SSH target/generation and pairing revision, preventing stale connection ownership: `src/main/ssh/orcad-managed-tunnel-manager.ts:127-165,253-263`; public start/close APIs `orcad-managed-tunnel.ts:79-90`. Host installation identity is persistent UUID plus optional machine-binding hint, explicitly not machine-binding-as-auth: `src/shared/runtime-host-descriptor.ts:3-19`.
- **Host status is not boolean:** live / unverifiable / refused / retired; disconnected cannot be treated as exited. Last-known answer is retained separately: `src/shared/runtime-host-contact.ts:3-35,66-97`. Useful design precedent for Hutan disconnected sessions.

### WSL

- Desktop reaches distro `orcad` through local loopback listener; **each accepted socket gets a `wsl.exe` child running pinned-Node stdio bridge**.
- Explicitly avoids WSL localhost forwarding because it refuses connections for **0.2–1 s** after listen and depends on networking mode.
- `src/main/wsl/wsl-orcad-stdio-bridge.ts:1-6,24-42`.

### Mobile/cloud relay

- Desktop/runtime retains execution; cloud relay brokers connectivity rather than running terminals.
- Relay service requires runtime E2EE keypair/mobile socket wiring and device registry; cloud account token opens broker, region preference participates in assignment: `src/main/runtime/mobile-cloud-relay/desktop-relay-service.ts:53-91`.
- Relay host ID derived from public key; pairing endpoints advertise **E2EE framing v2**: `relay-session-broker.ts:36-43,100-109,130-137`.
- Control channel uses WebSocket with Bearer token, compression disabled, **64 KiB maximum payload**, **15 s connect deadline**: `relay-control-client.ts:28,61-66`.
- Reconnect/readiness: service liveness safety net **5 min**, pairing mint waits **20 s**, phone request waits **10 s**: `desktop-relay-service.ts:35-41`.
- SSH tunnel reconnects use environment-scoped recovery and generation checks rather than reviving stale pairings: `orcad-managed-tunnel-resume.ts:78-82,100-148,253-261`.

## 3. Cross-platform boundaries

| Concern | Implementation / abstraction |
|---|---|
| PTY | `node-pty`, Windows bundled ConPTY; shell fallback code explicitly handles PowerShell → cmd chain: `src/main/daemon/pty-subprocess/native-pty-spawn.ts:22-56`. |
| IPC endpoint | Unix sockets vs Windows named pipes hidden behind daemon endpoint construction: `daemon-spawner.ts:133-155`. |
| Secrets | `SecretStore` abstracts Electron `safeStorage`; plain-Node runtime supplies implementation, avoiding Electron imports in core. “Encryption unavailable” is a real outcome, never assumed safe: `src/shared/secret-store.ts:1-9`. Persistence retains sealed ciphertext rather than writing new plaintext: `src/main/protected-secret-persistence.ts:33-38,84-98`. |
| Native Windows | `native/windows-security/index.d.ts:13-55`: user SID, DACL read/write, atomic file creation with DACL, asynchronous grants. Separate registry addon and Rust CLI/hook launchers exist under `native/windows-registry`, `native/windows-cli-launcher`, `native/windows-agent-hook`. |
| Native macOS | Swift keyboard-layout and notification-status helpers: `native/keyboard-layout-macos/main.swift`; `native/notification-status-macos/main.swift`. Separate computer-use implementation under `native/computer-use-macos/`. |
| Native Linux | Wayland/X11 clipboard publisher under `native/wayland-clipboard/`; computer-use runtime under `native/computer-use-linux/runtime.py`. Windows computer-use uses PowerShell: `native/computer-use-windows/runtime.ps1`. |
| Menus/tray | Electron API plus platform branches, macOS template/Retina vs Windows **16 px** tray icon: `src/main/tray/system-tray.ts:1-8,56-58,66-78`. |
| Notifications | Electron `Notification` appears directly in close-to-tray notice: `main-window-close-lifecycle.ts:76-89`; additional platform helper is separate from common notification service. |
| Linux service lifecycle | Detached POSIX group alone is insufficient under systemd; dedicated daemon scope logic accounts for cgroup-wide stop/restart killing terminals: `src/main/daemon/daemon-cgroup-scope.ts:1-7`. |

Native directory inventory above was observed; detailed native implementation internals were not audited.

## 4. Performance: concrete mechanisms/constants

| Mechanism | Evidence |
|---|---|
| Terminal renderer | xterm.js **WebGL enabled**, DOM fallback—not a blanket canvas renderer. Deferred WebGL attachment: `src/renderer/src/lib/pane-manager/pane-webgl-renderer.ts:28,52-69,276-282`. Settings, capability/context-loss and agent compatibility decide WebGL vs DOM: `components/terminal-pane/terminal-renderer-policy.ts:63-72`. |
| Lazy startup | WebGL addon dynamically imported rather than pulled through nine boot-path modules: `lib/pane-manager/terminal-webgl-addon-loader.ts:4-13`. Noncritical main work waits for first `ready-to-show`, then `setImmediate`; bounded fallback for headless/broken GPU: `src/main/startup/first-window-deferral.ts:3-35`. |
| Daemon batching | **2 ms** batch interval. Source comment attributes approximately **8 ms** fixed tax in an approximately **19 ms DSR-under-load latency** to layered batching; this is a source comment, **not independently measured here**. `daemon-stream-data-batcher.ts:28-29`. |
| Interactive fairness | **128 KiB** socket write gate, **64 Ki-character** bulk slices, **4 Ki-character** small-session bypass, **32 Mi-character** last-resort held write-through threshold. Goal: avoid burying visible-pane echo behind other terminals’ bulk. `daemon-stream-data-batcher.ts:31-40`. |
| Producer flow control | Batcher delegates producer pause/backpressure and background-droppable membership; background/stall release may thin output while retaining recovery behavior: `daemon-stream-data-batcher.ts:57-73,88-99`. |
| Remote multiplex | Chunk **48 KiB**, output batch **64 KiB**; initial/max per-stream ACK window **512 KiB / 2 MiB**; aggregate **2 / 8 MiB**; pending max **256 KiB**; ACK flush **192 KiB or 4 ms**; recovery **1,000 rows**; max **128 streams**, **32 active PTY waits** plus queue. `src/shared/terminal-multiplex-flow-control.ts:1-18`. |
| Hidden terminal work | Hidden-pane cursor blink explicitly suspended: avoiding xterm’s **600 ms** full-cursor-row redraw timer; restores previous setting on reveal: `lib/pane-manager/pane-cursor-blink-suspension.ts:3-26,29-45`. Inactive hidden-output restore paced at **16 ms**: `components/terminal-pane/hidden-output-restore-scheduler.ts:17-18`. |
| GPU resource pressure | Dispose explicitly loses WebGL context and zeros canvas dimensions to release Chromium context budget: `pane-webgl-renderer.ts:166-170`. |
| Render/connect pacing | PTY connect deferred to animation frame with **250 ms fallback**: `components/terminal-pane/pty-connection/run-deferred-connect.ts:200-201`. |
| List virtualization | TanStack virtualization: activity thread list `components/activity/activity-thread-list-pane.tsx:204-207`; automation history `components/automations/AutomationRunHistory.tsx:107-110`; diff sections `components/editor/combined-diff/scroll-viewport/use-combined-diff-virtualizer.ts:36-39`; CSV virtualizes **both axes** `components/editor/csv/CsvGrid.tsx:62-72`. Not evidence that every sidebar/list is virtualized. |
| Idle polling | Daemon process-growth monitor **5 s active / 30 s idle**: `src/main/daemon/daemon-process-growth-monitor.ts:18-19`; Windows idle-shell foreground refresh **15 s**: `daemon/pty-subprocess/foreground-process-tracker.ts:34-37`. |

**Design takeaway for Hutan:** Orca’s responsiveness is supported by explicit scheduling, bounded queues, interactive fairness, hidden-pane suppression, lazy loading and host-aware routing—not by Electron magically being fast or by killing all background work when its window closes.