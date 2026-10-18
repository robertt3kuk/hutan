# Hutan model, lifecycle, transport and macOS coupling inventory

Read-only scout report, 2026-10-18. Official sources only: stablyai/orca (onorca.dev, installed Orca's package.json), herdrdev/herdr (herdr.dev, Cargo.toml), NousResearch/hermes-agent (hermes-agent.nousresearch.com). Feeds docs/spikes/2026-10-18-direction.md.

## Summary

- Hutan already has **Home → automatic projects → optional user workspace grouping**, not an unassigned-session/task-workspace model. “No workspace” currently means **projects without a parent**, not sessions without membership.
- Default app lifecycle already aims at “close quits; daemon follows app,” but **platform quit and signals deliberately preserve live sessions**, and older settings migrate to background-running behavior.
- The protocol framing is transport-independent; connection implementation, authentication, autostart, shutdown detection and filesystem-facing UX are not.
- **Full workspace Linux compilation is blocked by `xtask`’s unconditional macOS process APIs.** Linux service crates look substantially portable by static inspection; this is not a verified build. Windows has widespread unconditional Unix dependencies.
- Read-only investigation; no builds/tests executed.

## 1. Workspace / project / session model

| Concern | Current implementation and evidence |
|---|---|
| Kinds | `WorkspaceKind::{Home, Project, Workspace}`. All three share `WorkspaceInfo`; project grouping is `parent_id: Option<WorkspaceId>`, project identity is `project: Option<ProjectInfo>`. `crates/protocol/src/messages.rs:475-481,791-819`. |
| Home | Private canonical folder, default documented as `~/Hutan`; root `/`, user home, Home folder and non-repository descendants of Home resolve to Home. Missing/unresolvable starting directories also resolve to Home. Git repositories underneath Home can become projects. `crates/daemon/src/projects.rs:15-35,80-138`; `docs/architecture.md:188-190`. |
| Automatic projects | Registry reads `[projects] auto`; when false, implicit assignment is Home. Otherwise deepest enclosing Git repository identifies project by canonical common Git directory; linked worktrees share project identity, nested repositories/submodules do not. Outside Git, the starting folder is a project. `crates/daemon/src/registry.rs:319-330`; `crates/daemon/src/projects.rs:37-76,109-138`. |
| Session creation | `SessionCreate.workspace_id` is optional; absence derives project/Home from cwd. Explicit membership bypasses automatic assignment. Registry creates/reuses project before returning its ID. `crates/protocol/src/messages.rs:566-573`; `crates/daemon/src/registry.rs:427-440,1631-1639`. |
| Membership vs cwd | Every returned session has **required** `workspace_id`; `cwd` and `folder` are optional. `folder` is latest OSC 7 cwd or starting cwd; membership remains the project selected at start and does not follow `cd`. `crates/protocol/src/messages.rs:829-856`. |
| User workspaces | Group projects, and may contain sessions started directly into the workspace. `workspace.assign` moves **a project**, not a session; absent target unparents that project. Deleting user workspace leaves projects ungrouped. `crates/protocol/src/project.rs:34-50`; `crates/desktop/src/sessions/tree.rs:53-63,118-123`. Adding a root creates/reuses its project and changes that project’s parent; it can move out of another workspace. `crates/daemon/src/registry.rs:1680-1700`. |
| Sidebar | Builds user workspace nodes with child projects and direct sessions, then unparented projects; alternative organize-by-projects mode flattens project grouping. Home is appended separately. `crates/desktop/src/sessions/tree.rs:144-237`. Project rows show live folder and checkout-placement badges; “Needs you” section above. `crates/desktop/src/workbench/panels/sessions.rs:1-6`. |
| Drag/drop today | Two domain drag types: workspace reorder and project grouping. “No workspace” drop target unparents a **project**. `crates/desktop/src/workbench/panels/sessions.rs:68-80,883-904,1195-1218`; `crates/desktop/src/workbench/actions.rs:323-335`. |
| Worktrees | Git checkout placement is independent of membership: main/worktree root, branch, and live agent sharing. Worktree service operates beside registry and invalidates derived placements after changes. `docs/architecture.md:138-140`; badge rendering `crates/desktop/src/sessions/placement.rs:19-45`. |
| Storage | `sessions.workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE`; cwd/spec persisted separately. `crates/storage/migrations/0001_init.sql:17-32`. Workspace schema has three-kind CHECK, `common_dir`, and `parent_id ... ON DELETE SET NULL`; one-Home unique index. `crates/storage/migrations/0008_projects.sql:16-42`. |
| Wire mutation/events | Workspace operations include create/open/add/remove root/assign/delete/move/pin/rename. Canonical changes use `workspace.updated`, `workspace.closed`, `session.updated`; worktree changes use `worktree.changed`. `crates/protocol/src/messages.rs:94-99,504-558,930-935,1024-1027`. |

**Meaning of proposed “unassigned”:** [INFERENCE] It cannot simply reuse current “No workspace”: current sessions always belong to a project/Home/workspace, and existing drag changes all sessions’ effective grouping through their project. A session-level task-workspace model needs **independent task-workspace membership** or an explicit migration of membership semantics, plus session assignment mutation/events/storage and session drag UX. Existing `cwd`/`folder` already provide the directory label.

Architecture boundary is favorable: daemon owns membership; desktop owns tabs, splits, docks and per-workspace layouts (`docs/architecture.md:90-98`). Inspector already derives the displayed workspace from selected session membership (`crates/desktop/src/workbench/panels/inspector.rs:132-135`).

## 2. Lifecycle today

- **Who starts daemon:** desktop/client connects and optionally spawns sibling `hutand`, else PATH; CLI startup is explicit (`hutan daemon start`, also index enable), while queries do not start a stopped daemon. `crates/client/src/client.rs:1452-1490`; `docs/packaging.md:42-54`. Spawn is independent process group, null stdio, `kill_on_drop(false)`—not a child lifetime tied automatically to desktop termination.
- **Defaults:** `keep_running=false`, `start_at_login=false`, last-window-close=`Quit`, desktop menu bar enabled. `crates/desktop/src/settings.rs:924-945`.
- **Normal quit/window close:** connected daemon with no live sessions stops silently; live sessions prompt **Stop Sessions & Quit / Keep Running in Background / Cancel**. Background choice persists `keep_running=true`; registered/approved start-at-login also leaves daemon running. `crates/desktop/src/workbench/daemon.rs:540-599,640-650`.
- **Important exception:** platform termination such as Dock Quit/logout/system shutdown stops only idle daemon; live sessions survive. SIGTERM/SIGINT/SIGHUP explicitly disconnect without stopping daemon. `crates/desktop/src/workbench/daemon.rs:600-628`. This can explain background processes despite expected default lifecycle.
- **Legacy migration:** settings predating split lifecycle keys with `keep_running=true` gain `start_at_login=true`; existing background behavior intentionally survives upgrade. `crates/desktop/src/settings.rs:967-999`.
- **Start at login:** `SMAppService.agent` registers bundled LaunchAgent. `RunAtLoad=true`, `KeepAlive={Crashed:true}`; clean shutdown does not restart it. Desktop gives launchd 4 seconds before fallback autostart. `docs/packaging.md:74-88`; `crates/desktop/src/macos/mod.rs:36-39,84-88`.
- **Daemon’s native process:** bundled daemon owns Cocoa main-thread host while Tokio runs on `hutand-runtime`; outside bundle the main thread just joins runtime. `crates/daemon/src/main.rs:11-15,72-99`.
- **Daemon menu bar:** bundled daemon shows its own accessory `NSApplication`/status item only without desktop clients; hides immediately upon desktop connect, reappears after 750 ms disconnected. Not governed by desktop `app.menu_bar` or native-notification toggle. `docs/packaging.md:94-100`; gated Cocoa module `crates/daemon/src/menu_bar.rs:61-62`.
- **Dock/Finder relaunch:** because daemon shares bundle identity, Launch Services can deliver reopen to daemon. Delegate sends `OpenHutan`; launcher starts sibling desktop **directly**, not `open`, in separate process group preserving daemon environment. `crates/daemon/src/menu_bar/cocoa.rs:80-92,457-464`; explanation and prior reopen loop `docs/packaging.md:99-100`.
- **Desktop single instance:** exclusive flock + `desktop.sock` per data directory, peer UID checks, second process sends activate then exits. Linux peer credentials implementation exists. `crates/desktop/src/instance.rs:1-10,164-218`; Launch Services prohibition also `assets/macos/Info.plist:33-35`.
- **Visible processes:** actual desktop + actual `hutand` + PTY programs / agent subprocesses. `hutan mcp` is additionally an **agent-host-started stdio MCP bridge**, not duplicate daemon: Codex/ACP wiring supplies CLI command, `mcp` args, session ID and socket environment. `crates/daemon/src/mcp_wiring.rs:1-7,36-51,55-78`; `crates/cli/src/mcp/mod.rs:1-4`. Multiple agent MCP hosts can therefore produce multiple CLI bridge processes.

## 3. Transport / remote-control implications

**Current assumptions**

- Client uses concrete `UnixStream`, Unix owned read/write halves in handshake and loops: `crates/client/src/client.rs:68-69,1347-1349,1374,1404-1406,1452-1465`.
- Daemon binds Unix listener after stale-socket cleanup: `crates/daemon/src/daemon.rs:192-196,550-553`.
- Connection entry verifies peer UID equals daemon UID, rejecting unavailable credentials, **before handshake**. It splits into concrete Unix halves; writer signature also fixes Unix type. `crates/daemon/src/conn.rs:234-269,278-280`.
- Codec itself is reusable: BE `u32` **payload length**, kind byte, payload; max 4 MiB; JSON control / postcard terminal-render / raw terminal-input. `crates/protocol/src/frame.rs:1-25,98-120`.
- ADR 0002 preserves negotiated major/minor/capabilities, epoch+sequence replay/snapshot, subscribe-before-snapshot, terminal credit-1 acknowledgements, bounded outbound queue and disconnect-on-overflow. Remote wrappers must retain these properties. `docs/adr/0002-ipc-transport-and-framing.md:19-44`.
- Shutdown UI additionally waits for **local socket pathname disappearance**; unsuitable for remote endpoint lifetime detection. `crates/desktop/src/workbench/daemon.rs:119-157`.
- Client socket resolution: `HUTAN_SOCKET` → `$HUTAN_HOME/hutand.sock` → `~/Library/Application Support/Hutan/hutand.sock`. `crates/client/src/socket.rs:4-28`. Daemon data/log defaults mirror mac paths; settings do too. `crates/daemon/src/paths.rs:74-76`; `crates/settings/src/store.rs:43-64`.

**Required direction [INFERENCE]**

| Option | Minimum conceptual changes |
|---|---|
| TCP + TLS | Endpoint configuration distinct from local socket; generic async read/write connection plumbing; TLS server identity validation and client authentication/authorization replacing peer UID; listener configuration; remote disconnect/status semantics; never local-autostart remote daemon. Preserve frame/replay/credit rules. |
| SSH stdio tunnel | Add raw framed-protocol stdio proxy command that connects to remote daemon’s local UDS; supervise `ssh` process and use its stdin/stdout as stream. SSH authenticates host/user; remote proxy’s local connection retains UID security. Existing `hutan mcp` **is not** this proxy—it speaks MCP JSON-RPC. Alternative SSH Unix-socket forwarding can retain current UDS client with endpoint UX, but remains Unix-oriented. |
| WebSocket | Binary WS-to-frame adapter (define whether one frame per WS message or stream reassembly), WSS identity/auth and origin policy for browser usage; same handshake/replay/backpressure. ADR explicitly allows TLS/WebSocket wrapper, but does not define remote trust policy. |

**Credentials/filesystem:** daemon `CredentialStore` trait is a usable seam; system store is macOS Keychain, non-macOS backend returns no keys and **refuses storing**. DB stores provider reference, not secret (`crates/gateway/src/credentials.rs:1-5,28-36,69-104`). Remote credentials should live on remote daemon; headless server cannot depend on interactive macOS Keychain prompts. Remote filesystem semantics also need explicit handling: project roots/cwd refer to daemon filesystem, whereas attachment selection currently creates local path chips and daemon reads those paths (`crates/desktop/src/workbench/chat/rich.rs:1-5,77-80`; `rich_view.rs:152-155`).

## 4. macOS / cross-platform coupling inventory

| Module / subsystem | Native coupling; cfg/fallback | Linux / Windows consequence |
|---|---|---|
| `desktop/src/macos/login_item.rs:215-220` | Gated objc2 ServiceManagement `SMAppService`; unavailable trait implementation selected via `macos/mod.rs:104-149`. | No login-item registration elsewhere. Needs OS service/autostart integration, not just renamed labels. |
| `desktop/src/macos/notifications.rs:140-145` | Gated UserNotifications permission API; unavailable fallback. | Native permission integration absent. |
| `desktop/src/macos/status_item.rs:206-211` | Gated AppKit `NSStatusItem`; `Nothing` fallback. | No desktop tray control. |
| `desktop/src/macos/dock.rs:54-62` | Gated AppKit Dock badge; `Nothing` fallback. | No taskbar/dock badge. |
| `desktop/src/macos/presence.rs:85-96` | Gated NSApplication activation/accessory mode; `Nothing` fallback. | Menu-bar hide/presence semantics are not implemented natively. |
| `desktop/src/macos/hotkey.rs:252-260` | Gated Carbon global shortcut FFI; `Nothing` fallback. | No global assistant shortcut. |
| `daemon/src/notify_native/cocoa.rs:35-50`; `notify_native.rs:41-90` | Gated objc2/dispatch/AppKit/UserNotifications main-thread host; explicit non-mac host stub. | No native daemon notifications / Cocoa loop. Wire notification intents remain. |
| `daemon/src/menu_bar/cocoa.rs:35-43`; `menu_bar.rs:61-62` | Gated AppKit status item, alerts, NSApplication delegate/reopen; no non-mac native host. | Headless background daemon lacks corresponding tray management/reopen surface. |
| `gateway/src/credentials.rs:69-113` | Gated security-framework Keychain. UnsupportedStore fallback, **not MemoryStore**. | API-key persistence unavailable; provider config can exist but key storage errors. |
| `terminal/src/procs.rs:143-145,334-336` | Gated libproc/sysctl process observation; other platforms report no process. | Terminal process/resource observation loses data; affects features relying on process tree. |
| `terminal/src/sys.rs:143-165` | Darwin confstr temp-dir gated; fallback `None`. Remaining killpg/waitpid/passwd code is **unconditional Unix** (`:24-138`). | Linux Unix primitives fit; Windows terminal crate needs replacement platform implementation. |
| `terminal/src/env.rs:194-246` | **Ungated** macOS `/usr/libexec/path_helper`, `/usr/bin/defaults`, `/bin/launchctl`; command failures yield optional facts, PATH/locale fallbacks (`:41-49,102-107`). | Linux compiles these command calls but gets reduced/wrong login-environment facts; launchd SSH socket unavailable. Windows model is structurally Unix. |
| `index/src/sys.rs:7-20` | macOS pthread QoS gated; “not macOS” uses Unix `setpriority`. Watcher uses cross-platform `notify` dependency despite FSEvents wording. | Linux fallback exists; Windows fails on Unix libc API unless gated/replaced. |
| `desktop/src/workbench/daemon.rs:106-113` | **Ungated** `Command::new("open")`. | Data/log open fails on Linux/Windows; needs OS opener. |
| Paths: `daemon/src/paths.rs:74-76`, `client/src/socket.rs:4-28`, `settings/src/store.rs:43-64`, `desktop/src/crash.rs:17-25` | **Ungated** `~/Library/Application Support/Hutan`, `~/Library/Logs/Hutan`; `HUTAN_HOME` override. | Linux uses mac-style directories, not XDG; Windows needs known-folder layout and transport addressing. |
| Desktop single-instance: `desktop/src/instance.rs:1-10,191-218` | Unix sockets/flock unconditional; mac/FreeBSD `getpeereid`, Linux `SO_PEERCRED` implementations. | Linux supported in this module; Windows cannot compile unchanged. |
| Daemon/client/CLI execution | Unconditional Unix listener/stream (`daemon.rs:15`, `client.rs:68-69`), Unix signal handlers (`daemon/main.rs:26,126-137`), Unix filesystem APIs (`daemon/context_file.rs:7-9`, `projects.rs:8`, `cli/commands.rs:503-506`), process groups (`client.rs:1480-1487`). | Linux generally fits; Windows blocked across daemon/client/CLI and transitive consumers. |
| Agent subprocess reap: `agents/src/jsonrpc.rs:549-551` | **Ungated** Unix `ExitStatusExt::signal`. | Windows agent crate compile blocker. |
| GPUI UI crates: root `Cargo.toml:83-91` | GPUI platform pinned with `default-features=false`, features only `font-kit`, `runtime_shaders` (mac backend features); no Hutan x11/wayland selection. | Platform library has Linux/Windows support, but Hutan’s display/backend feature configuration needs explicit Linux review; do not claim GPUI itself is mac-only. |
| Bundle: `xtask/src/bundle.rs:94-98,197-218`; `assets/macos/Info.plist:33-39` | Ungated commands for macOS deployment target/codesign; NSApplication principal class, macOS 13 minimum, Launch Services single-instance. **No `LSEnvironment` key present** in complete plist. | Current packaging pipeline is macOS-only. |
| Install/release: `xtask/src/install.rs:18`; `release.rs:16-18` | Hard-coded CoreServices `lsregister`; release rejects non-Apple-Silicon/macOS; icon/archiving pipeline documented in `docs/packaging.md:29-38,113-139`. | No Linux/Windows installers or release artifacts. |
| **Workspace build blocker**: `xtask/src/main.rs:13`, `xtask/src/proc.rs:48-55` | `mod proc` unconditional; uses `libc::rusage_info_v4`, `proc_pid_rusage`, `RUSAGE_INFO_V4`. No Linux fallback. | **[INFERENCE, definitive source-level blocker] `cargo build --workspace` cannot compile on Linux unchanged.** Windows also blocked here. |

### Compilation conclusion, carefully scoped

- **Full workspace on Linux:** no, source-level `xtask` blocker above; not an exercised build result.
- **Linux CLI + daemon and service libraries:** [INFERENCE] plausible after selecting packages instead of full workspace; macOS Objective-C dependencies are correctly target-gated (`crates/daemon/Cargo.toml:44-50`, `desktop/Cargo.toml:38-44`, `gateway/Cargo.toml:34-35`). Process observation, credential persistence, native notifications/tray and environment behavior remain incomplete. No claim that these packages have been compiled.
- **Linux desktop / terminal-view / chat-view:** [INFERENCE] separate GPUI system-dependency/backend-feature work needed; absence of x11/wayland features in Hutan is a concrete configuration gap, not proof of an observed compiler failure.
- **Windows:** service stack and desktop cannot compile unchanged because client, daemon, terminal, agents, CLI, index and desktop instance/signal code use unconditional Unix APIs.
- **Core/protocol/storage/settings/gateway:** [INFERENCE] strongest portability candidates; gateway native credential backend still intentionally unavailable outside macOS.

Pinned GPUI evidence inspected locally: `/Users/workie/.cargo/git/checkouts/zed-a70e2ad075855582/ef37600/crates/gpui_platform/Cargo.toml:15-27,47-60` defines Linux `wayland`/`x11` features and platform dependencies; `src/gpui_platform.rs:67-90` selects macOS, Windows and Linux constructors.