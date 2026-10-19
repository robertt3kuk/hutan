# Cross-platform direction: measured baseline and platform-layer design

**Status:** research and recommendation for D-4; no Hutan source changes.

## Recommendation

Accept **Linux headless → Linux desktop → Windows**, with one important refinement:

> Establish native Linux CI immediately. The requested Linux headless crates already pass cross-target `cargo check`; the remaining Linux work is runtime correctness, native integration, packaging and regression coverage—not a wholesale port.

Windows is a materially larger port. The experiment found **48 Rust compilation diagnostics** across settings, client, CLI dependencies, agents and terminal code, plus missing Windows C SDK headers. These are distinct from the Linux cross-compiler setup errors.

Keep `hutand` authoritative and headless. Put OS services behind **`hutan-platform`**, with optional desktop integrations so compiling a daemon never brings in GPUI, tray or a graphical-session dependency.

The proposal follows the user’s cross-platform requirement and platform order in `docs/spikes/2026-10-18-direction.md:24-35,81-85,130-138`.

---

## 1. Experiment: what actually compiles

### Method and isolation

Used a temporary detached worktree of **`master` at `05e3881`**, not the potentially changing working checkout:

```sh
git worktree add --detach <temporary-directory>/xp master
rustup target add aarch64-unknown-linux-gnu x86_64-pc-windows-msvc
```

Both standard-library targets installed successfully; the combined target installation took **13.20 seconds**.

For **each** target and **each** requested crate, ran:

```sh
cargo check --locked --target <target> -p hutan-<crate> \
  --message-format short
```

The first 22 checks took **183.73 seconds combined**, sequentially, sharing their temporary Cargo target directory. These are elapsed check times, **not clean-build performance benchmarks**.

Linux packages blocked by C dependencies were then checked with the workstation’s already-installed **Zig 0.17.0** as the target C compiler and archiver. No compiler package was installed. The successful corrected C-dependent checks took **39.32 seconds combined**.

No Hutan executable was launched. No tests, native linking, GUI startup, signing, system registration or secret-store operations were performed.

The worktree was removed using `git worktree remove --force`; its parent directory, logs, compiler adapters and build artifacts were also deleted. Rustup’s installed targets and ordinary shared Cargo/compiler caches remain.

### Results

`PASS` means the package’s normal targets passed `cargo check`; it does **not** mean its tests or native runtime were exercised.

| Crate | Linux aarch64, initial | Linux final | Windows x86_64 MSVC |
|---|---:|---:|---|
| `hutan-core` | PASS, 25.92 s | PASS | PASS, 4.40 s |
| `hutan-protocol` | PASS, 25.04 s | PASS | PASS, 6.76 s |
| `hutan-storage` | C compiler unavailable, 11.87 s | **PASS**, 17.81 s with Zig | Windows C headers unavailable, 1.84 s |
| `hutan-settings` | PASS, 11.51 s | PASS | **2 source errors**, 3.26 s |
| `hutan-index` | C compiler unavailable, 4.09 s | **PASS**, 0.98 s with Zig | Windows C headers unavailable, 2.17 s |
| `hutan-gateway` | C compiler unavailable, 8.18 s | **PASS**, 10.77 s with Zig | Windows C headers unavailable, 3.53 s |
| `hutan-client` | PASS, 15.72 s | PASS | **5 source errors**, 7.94 s |
| `hutan-daemon` | C compiler unavailable, 7.57 s | **PASS**, 9.76 s with Zig | Windows C headers unavailable, 6.67 s |
| `hutan-cli` | PASS, 11.93 s | PASS | **12 dependency-feature errors**, 1.34 s |
| `hutan-agents` | PASS, 4.56 s | PASS | **2 source errors**, 4.36 s |
| `hutan-terminal` | PASS, 9.03 s | PASS | **27 source errors**, 6.04 s |

**Measured conclusion:** all **11 requested packages** pass Linux aarch64 cross-target checking. Only core and protocol pass the initial Windows checks.

Windows storage, index, gateway and daemon are **blocked**, not proven source-portable: compilation stopped in C dependency build scripts before all downstream Rust code could be checked.

### Docker result

`docker info` found Docker CLI **29.8.2**, using the `orbstack` context, but failed to connect:

```text
failed to connect to the docker API at
unix:///Users/workie/.orbstack/run/docker.sock
... connect: no such file or directory
```

Therefore the conditional Linux-container check could **not** run. No container or image was created, and no system service was started to work around that limitation.

**Remaining evidence gap:** a native Linux `cargo check -p hutan-daemon -p hutan-cli`, linking/building, and runtime tests. Cross-target checking is useful evidence, not a substitute.

---

## 2. Complete diagnostic inventory

Repeated Cargo summaries and repeated emissions of the same C error are consolidated below; every distinct observed source diagnostic is represented.

### 2.1 Toolchain/environment failures—not Hutan source defects

| Failure | Packages affected | Classification |
|---|---|---|
| `libsqlite3-sys 0.38.2`: `aarch64-linux-gnu-gcc` not found | Linux storage, index; also encountered during daemon attempts | Missing target C compiler |
| `ring 0.17.14`: `aarch64-linux-gnu-gcc` not found | Linux gateway, daemon | Missing target C compiler |
| SQLite compilation: Xcode Clang `include/mm_malloc.h:13:10`, fatal error **`stdlib.h` file not found**; inclusion starts at `sqlite3/sqlite3.c:15053` | Windows storage, index | Missing Windows target C SDK/sysroot |
| Ring compilation: `ring-0.17.14/include/ring-core/check.h:27:11`, fatal error **`assert.h` file not found** | Windows gateway, daemon | Missing Windows target C SDK/sysroot |

The first temporary Zig adapter encountered another **experiment-setup** error:

```text
unable to parse target query 'aarch64-unknown-linux-gnu':
UnknownOperatingSystem
```

`cc-rs` passed its Clang-style Rust target triple to Zig. The corrected adapter removed that argument and explicitly selected Zig’s `aarch64-linux-gnu`; its archiver executed `zig ar`. After that correction, storage, index, gateway and daemon all passed without source edits.

There is no Hutan `file:line` for the missing executable or target-query errors; they arose in dependency build commands.

### 2.2 Windows: settings—2 diagnostics

| Location | Error | Classification |
|---|---|---|
| `crates/settings/src/store.rs:1073:18` | E0433: `std::os::unix` unavailable | Unix-only private-directory implementation |
| `crates/settings/src/store.rs:1076:10` | E0599: `DirBuilder.mode` unavailable | Same implementation |

The implementation must establish a Windows user-only DACL, not merely remove `.mode(0o700)`.

### 2.3 Windows: client—5 diagnostics

| Location | Error | Classification |
|---|---|---|
| `crates/client/src/client.rs:68:5` | E0432: `tokio::net::UnixStream` unavailable | Concrete Unix transport |
| `crates/client/src/client.rs:69:17` | E0432: `tokio::net::unix` unavailable | Concrete Unix split halves |
| `crates/client/src/client.rs:1486:10` | E0599: `Command.process_group` unavailable | Unix daemon launch policy |
| `crates/client/src/time.rs:62:18` | E0425: `libc::localtime_r` unavailable | Unix local-time implementation |
| `crates/client/src/time.rs:67:8` | E0609: `tm.tm_gmtoff` unavailable | Unix local-time implementation |

**New finding relative to the direction inventory:** local time formatting is also a Windows blocker. Its current implementation is at `crates/client/src/time.rs:51-67`.

### 2.4 Windows: CLI dependency features—12 diagnostics

All locations below are in **`crossterm-0.29.0/src/`**.

| Location | Error |
|---|---|
| `lib.rs:263:1` | Explicit compile error: Windows build with the `windows` feature disabled |
| `cursor/sys/windows.rs:8:5` | E0433: missing `winapi` |
| `cursor/sys/windows.rs:7:5` | E0432: missing `crossterm_winapi` |
| `style/sys/windows.rs:5:5` | E0433: missing `winapi` |
| `style/sys/windows.rs:4:5` | E0432: missing `crossterm_winapi` |
| `terminal.rs:93:5` | E0433: missing `winapi` |
| `terminal.rs:89:5` | E0432: missing `crossterm_winapi` |
| `terminal/sys/windows.rs:7:5` | E0433: missing `winapi` |
| `tty.rs:12:5` | E0433: missing `winapi` |
| `terminal/sys/windows.rs:6:5` | E0432: missing `crossterm_winapi` |
| `ansi_support.rs:5:5` | E0433: missing `winapi` |
| `ansi_support.rs:3:5` | E0432: missing `crossterm_winapi` |

Classification: **one dependency-feature configuration defect producing 12 diagnostics**. Hutan disables Crossterm default features at `Cargo.toml:34`; enable its Windows feature in a Windows-target dependency declaration. That does not resolve the CLI’s own Unix signal and permissions assumptions.

### 2.5 Windows: agents—2 diagnostics

| Location | Error | Classification |
|---|---|---|
| `crates/agents/src/jsonrpc.rs:550:26` | E0433: `std::os::unix` unavailable | Unix exit-status extraction |
| `crates/agents/src/jsonrpc.rs:551:36` | E0599: `ExitStatus.signal` unavailable | Same implementation |

### 2.6 Windows: terminal—27 diagnostics

| Location | Error or missing API |
|---|---|
| `login_env.rs:17:14`, `18:14` | E0433: `std::os::unix` |
| `sniffer.rs:9:14` | E0433: `std::os::unix` |
| `procs.rs:70:21` | E0433: `libc::pid_t` |
| `sys.rs:27:21`, `51:21` | E0433: `libc::pid_t` |
| `procs.rs:29:39`, `31:38`, `32:39`, `33:39` | E0425: `SIGKILL`, `SIGHUP`, `SIGSTOP`, `SIGCONT`, respectively |
| `procs.rs:76:23` | E0425: `libc::kill` |
| `sys.rs:17:37`, `18:35` | E0425: `SIGHUP`, `SIGKILL`, respectively |
| `sys.rs:31:23`, `35:23` | E0425: `killpg`, `kill`, respectively |
| `sys.rs:57:37` | E0425: `waitpid` |
| `sys.rs:65:18`, `66:44`, `68:18`, `69:46` | E0425: `WIFEXITED`, `WEXITSTATUS`, `WIFSIGNALED`, `WTERMSIG`, respectively |
| `sys.rs:102:30` | E0425: `getuid` |
| `sys.rs:107:30`, `108:36` | E0425: type `passwd` |
| `sys.rs:113:19` | E0425: `getpwuid_r` |
| `login_env.rs:89:10` | E0599: `OpenOptions.mode` |
| `login_env.rs:118:10` | E0599: `Command.process_group` |
| `sniffer.rs:233:34` | E0599: Unix `OsString::from_vec` |

Paths in this table are relative to `crates/terminal/src/`.

Classification: Unix account/environment handling, Unix-byte path conversion, POSIX signalling and child reaping. This is **not** a missing PTY-library problem: Hutan already depends on `portable-pty = 0.9.0`, whose Windows implementation uses ConPTY.

---

## 3. Compilation success hides important Linux runtime gaps

The earlier inventory described Linux service portability as an inference. This experiment upgrades the requested package-level compilation result to **observed**, but does not resolve these behavioral gaps:

| Concern | Source evidence | Consequence |
|---|---|---|
| Default paths are macOS paths | `crates/daemon/src/paths.rs:64-77`; `crates/client/src/socket.rs:4-28`; `crates/settings/src/store.rs:43-64` | Linux currently resolves into `~/Library/...`, not XDG directories |
| Credentials lack a non-macOS system backend | `crates/gateway/src/credentials.rs:68-99` | Non-macOS reads return no key; storing errors |
| Process observation is empty outside macOS | `crates/terminal/src/procs.rs:334-343` | Process trees/resource-aware behavior cannot be called implemented on Linux |
| macOS environment probes run without OS gating | `crates/terminal/src/env.rs:194-246` | Linux attempts `defaults`, `launchctl` and macOS PATH discovery before falling back |
| Desktop opener invokes `open` | `docs/spikes/2026-10-18-hutan-model-lifecycle-transport.md:86` | File/log opening is not portable |
| Whole-workspace tooling remains macOS-specific | `xtask/src/proc.rs:25-55` | Successful selected-package checks do not establish `cargo check --workspace` portability |

The `xtask` finding is **source inspection**, not an exercised Linux workspace failure: its Mach timebase and `proc_pid_rusage` types/functions are unconditional.

Windows has additional downstream source work hidden by the first dependency failures:

[Showing lines 1-200 of 685. Use :201 to continue]

- CLI Unix signals: `crates/cli/src/commands/attach.rs:18,310-314`.
- CLI executable checks and integration-file modes: `crates/cli/src/commands.rs:503-506`; `crates/cli/src/integration/mod.rs:384-386,484-485`.
- Daemon Unix filesystem primitives and signals: `crates/daemon/src/daemon.rs:5-6`; `crates/daemon/src/main.rs:26`.
- Security-sensitive `openat`/`O_NOFOLLOW` file traversal: `crates/daemon/src/context_file.rs:189-244`.
- Index priority and CPU accounting: `crates/index/src/sys.rs:17-40`.

These are **inspected blockers**, not additional observed compiler diagnostics.

---

## 4. Pinned GPUI: Linux and Windows requirements

Inspected the requested checkout:

```text
~/.cargo/git/checkouts/zed-a70e2ad075855582/ef37600/
```

Hutan pins commit **`ef376006f32728b15e6a67706a04849cb781c812`** at `Cargo.toml:80-86`.

### Linux features

The feature-selection gap is real:

- Hutan enables `font-kit` and `runtime_shaders`, but not `x11` or `wayland`: `Cargo.toml:83-86`.
- Those two existing features forward to the **macOS** backend: pinned `crates/gpui_platform/Cargo.toml:21,25`.
- Linux forwarding features exist at `gpui_platform/Cargo.toml:26-27`.
- Zed’s workspace dependency disables `gpui_linux` defaults: pinned root `Cargo.toml:369`.
- Linux modules and text-system inclusion are gated by `x11`/`wayland`: `gpui_linux/src/linux.rs:7-27`.

**Design:** expose Hutan desktop features `x11` and `wayland`, forwarding to `gpui_platform`, and enable both in normal Linux desktop artifacts. Keep native macOS features target-specific. CI must also compile each Linux backend separately to catch accidental feature coupling.

Dependencies evidenced by the pinned backend:

- Wayland client/backend/cursor and protocols; `xkbcommon/wayland`.
- X11 via `x11rb`, XKB, RandR, XInput, XIM and `xkbcommon/x11`.
- `gpui_wgpu`, enabling font-kit for Linux.
- `ashpd` portals, `notify-rust`, and `oo7`.
- Fontconfig and FreeType through the pinned font-kit dependency.

Sources: `gpui_linux/Cargo.toml:18-48,55-88,93-130`; `gpui_wgpu/Cargo.toml:40-45`; pinned `font-kit` checkout `Cargo.toml:59-63`.

**Initial Linux desktop CI package set [INFERENCE; not installed or verified here]:**

```text
build-essential pkg-config clang
libfontconfig1-dev libfreetype6-dev
libxkbcommon-dev libxkbcommon-x11-dev
libwayland-dev libx11-xcb-dev
libvulkan1 mesa-vulkan-drivers
```

For graphical tests add a session D-Bus, fonts, Xvfb and a headless Wayland compositor such as Weston. Portal integration requires an installed portal implementation and a graphical session; absence must produce a capability/status result, not a fake successful operation.

Zed’s broader Linux installer additionally includes ALSA, VA-API, Git, SSL, PipeWire and other application-level packages. **Do not copy that entire list as “GPUI requirements.”** Evidence: pinned `script/linux:19-52`. Hutan’s minimal linked dependency set must be established by a native Linux desktop build.

GPUI’s Linux renderer enables Vulkan and GLES among its WGPU backends: `gpui_wgpu/Cargo.toml:44-45`. Test software rendering in CI and hardware rendering on real hosts; neither was exercised here.

### Windows

GPUI already selects its Windows backend without a Linux-style display feature: `gpui_platform/src/gpui_platform.rs:67-84`.

The backend uses Direct3D 11, DirectComposition, DirectWrite, DXGI, accessibility and Windows notification APIs: `gpui_windows/Cargo.toml:23-68`.

**Release builds require Windows SDK `fxc.exe`.** The build script compiles HLSL in release builds and searches `GPUI_FXC_PATH`, PATH and the installed Windows SDK. Sources: `gpui_windows/build.rs:3-10,23-31,73-140`.

Therefore:

- Use a **native Windows MSVC runner**, with Windows SDK and C/C++ build tools.
- Include a release desktop build in CI; debug checking alone misses the shader-compiler requirement.
- Cross-checking on macOS cannot establish Windows GUI packaging or shader readiness.

Official pinned source: [Zed GPUI platform manifest](https://github.com/zed-industries/zed/blob/ef376006f32728b15e6a67706a04849cb781c812/crates/gpui_platform/Cargo.toml).

---

## 5. Reference architectures: what to borrow

### herdr: central OS boundary and honest capabilities

herdr explicitly centralizes OS-dependent behavior rather than scattering cfg branches through product logic: `/Users/workie/proj/herdr-src/src/platform/mod.rs:1-4`.

Useful patterns:

- Target-selected Unix/Linux/macOS/Windows modules.
- Common process and quit abstractions: `platform/mod.rs:139-200`.
- Explicit capabilities for Unix-only live handoff/direct attach: `platform/mod.rs:295-307`.
- Bounded Linux process-tree scanning, including process, thread and byte ceilings: `platform/linux.rs:34-79`.
- Windows pipe connections with timeouts and explicit security descriptors: `platform/windows.rs:105-168`.

Borrow the **boundary and capability discipline**, not herdr’s terminal-UI design or indefinite server persistence. Official source: [herdr platform module](https://github.com/herdrdev/herdr/blob/main/src/platform/mod.rs).

### Orca: endpoints, secrets and process ownership

Useful patterns from the verified official checkout:

- Windows pipe name includes protocol generation and a runtime-directory hash; Unix uses a socket: `src/main/daemon/daemon-spawner.ts:134-145`.
- Secret capability distinguishes “can seal” from “meaningfully protected”: `src/shared/secret-store.ts:1-27`.
- POSIX detachment does **not** escape a systemd cgroup; lifetime must account for the service manager: `src/main/daemon/daemon-cgroup-scope.ts:1-18`.
- Windows terminal process ownership needs more than a parent-PID walk; Orca’s PTY patch uses explicit Job Object ownership: `patches/node-pty@1.1.0.patch:895-896`; its repository guidance warns about MSYS descendants breaking away: `AGENTS.md:88-89`.

Official sources: [Orca secret contract](https://github.com/stablyai/orca/blob/main/src/shared/secret-store.ts), [daemon scope handling](https://github.com/stablyai/orca/blob/main/src/main/daemon/daemon-cgroup-scope.ts).

Do not copy Electron-specific implementations into Rust, or assume Orca’s Node PTY patches exist in portable-pty.

---

## 6. Concrete `hutan-platform` design

This section is a proposed design, not existing implementation.

### 6.1 Crate layout and ownership

```text
crates/platform/
  src/lib.rs
  src/capabilities.rs
  src/paths.rs
  src/private_fs.rs
  src/ipc.rs
  src/process.rs
  src/shell.rs
  src/time.rs
  src/secrets.rs
  src/opener.rs
  src/notifications.rs
  src/tray.rs
  src/autostart.rs
  src/macos/...
  src/linux/...
  src/windows/...
```

Use cfg-selected modules with ordinary functions and concrete types where practical. Introduce traits only for genuinely interchangeable implementations or existing test seams.

**Dependency rules:**

- No dependency on daemon, client, settings, gateway, storage or GPUI.
- Default features support paths, permissions, local IPC and process primitives.
- Optional `desktop` functionality enables native notifications, tray and autostart.
- OS key-store dependencies are target-specific and optional for headless builds.
- `hutan-terminal` retains PTY/emulation ownership; platform supplies shell discovery, process ownership, signalling, waiting and path decoding.
- The desktop owns tray/notification presentation; the daemon owns notification intents and policy.

This removes duplicated path resolution currently justified by “client may not depend on daemon” at `crates/client/src/socket.rs:8`.

### 6.2 Paths

Return a resolved `AppPaths` value with separate `config`, `data`, `state`, `cache`, `runtime` and `logs` fields.

| Kind | macOS | Linux | Windows |
|---|---|---|---|
| Config | Existing `~/Library/Application Support/Hutan` | `$XDG_CONFIG_HOME/hutan` | `FOLDERID_LocalAppData\Hutan\config` |
| Durable data | Existing Application Support directory | `$XDG_DATA_HOME/hutan` | `FOLDERID_LocalAppData\Hutan\data` |
| State/logs | Existing `~/Library/Logs/Hutan` for logs | `$XDG_STATE_HOME/hutan`, logs beneath it | `FOLDERID_LocalAppData\Hutan\state` |
| Cache | `~/Library/Caches/Hutan` | `$XDG_CACHE_HOME/hutan` | `FOLDERID_LocalAppData\Hutan\cache` |
| Runtime endpoint | Preserve existing socket location | `$XDG_RUNTIME_DIR/hutan` | Named-pipe namespace; no socket pathname |

Keep existing macOS defaults to avoid an unnecessary migration.

Linux must reject relative XDG values, follow the specified defaults and validate runtime ownership/mode. If `XDG_RUNTIME_DIR` is unavailable, use a securely created user-private local temporary directory with a deterministic profile-specific name, rejecting symlinks or ownership mismatches and logging the fallback. Do not put sockets on a shared/network home filesystem.

Keep the **daemon’s data-directory lock** independent of runtime-directory choice: two processes with differing XDG runtime environments must not open the same daemon database as separate owners.

`HUTAN_HOME` retains its existing isolated flat layout: settings, daemon config and databases at its root, logs under `logs`, Home workspace under `Hutan`; runtime files remain isolated to that profile. Windows derives an isolated pipe name from its normalized profile identity.

Use Known Folder APIs, not string concatenation from `USERPROFILE`. Choose **LocalAppData**, including for settings, to honor the user’s local-only requirement rather than silently enabling roaming.

Specifications: [XDG Base Directory](https://specifications.freedesktop.org/basedir/latest/), [Windows Known Folders](https://learn.microsoft.com/en-us/windows/win32/shell/knownfolderid).

### 6.3 Filesystem security

Provide narrowly scoped operations:

- Create user-private directories/files.
- Replace a settings file atomically while preserving intended permissions/ACLs.
- Hold an exclusive profile lock.
- Open a regular file beneath an authorized root without following an attacker-substituted link.

POSIX permissions and Windows DACLs are different implementations of the same **security contract**. In particular, do not replace `context_file.rs`’s `openat`/`O_NOFOLLOW` walk with `canonicalize` followed by ordinary `open`: that reintroduces a path-replacement race. Windows requires handle-based reparse-point checks.

### 6.4 Local IPC, including Windows named pipes

```rust
enum LocalEndpoint {
    UnixSocket(PathBuf),
    WindowsPipe(OsString),
}
```

Expose local connect/listen operations returning an authenticated stream compatible with `AsyncRead + AsyncWrite`. Client and daemon protocol loops should be generic over streams, not over `tokio::net::unix::OwnedReadHalf`.

Endpoint rules:

- macOS: preserve the existing default endpoint.
- Linux: short socket path under the validated runtime directory; profile identity distinguishes installations.
- Windows: byte-mode, duplex, overlapped named pipe, for example:

```text

[Showing lines 201-400 of 685. Use :401 to continue]
\\.\pipe\hutan-v0-<user-and-profile-hash>
```

Use protocol **major**, not minor, in new endpoint names. A minor upgrade must reconnect to an existing compatible daemon rather than create another daemon for the same data.

Windows requirements:

1. Explicit user-restricted DACL; never accept default pipe security.
2. Reject remote pipe clients.
3. First-instance ownership for the initial listener, plus the durable profile lock.
4. Authenticate the connecting token’s user SID before parsing `hello`.
5. Check server identity from the client side where supported; a predictable name is not authentication.
6. Treat EOF/connection state—not pathname disappearance—as shutdown evidence.
7. Avoid elevation by default; elevated/unelevated communication is an explicit security policy.

Microsoft documents that a default pipe security descriptor can grant read access to Everyone and anonymous users: [Named Pipe Security](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights).

Preserve `HUTAN_SOCKET` as the existing Unix override. Add `HUTAN_ENDPOINT` accepting explicit `unix:` or `pipe:` addressing; conflicting nonempty overrides should fail clearly. Do not reinterpret Windows pipe names as filesystem paths.

### 6.5 Process information and PTY lifecycle

**Linux:** implement `/proc` process identity, parentage, command line, cwd, CPU time, RSS and thread count, bounded by existing protocol limits and scan budgets. Track `(pid, start_time)` rather than PID alone.

**Windows:** use process handles and native enumeration, creation times, `GetProcessTimes`/memory APIs and Job Objects. Do not fork PowerShell for every resource sample.

**macOS:** move existing native implementations without changing their semantics.

Process discovery and teardown are separate responsibilities. Enumeration is not proof of tree ownership.

Hutan already opens PTYs via `native_pty_system()` (`crates/terminal/src/terminal.rs:27`). Portable-pty exposes cross-platform `Child::wait`, `try_wait`, `clone_killer`, and a Windows raw process handle: portable-pty `src/lib.rs:128-157`. ConPTY implementation is at `src/win/conpty.rs:12-42,84-115`.

However, the pinned Windows spawn path calls `CreateProcessW` without suspended creation or a Job Object assignment: portable-pty `src/win/psuedocon.rs:127-152`. Its ordinary killer targets a process, not an owned tree: `src/win/mod.rs:41-55,70-72`.

**Therefore Windows support requires a race-free Job Object spawn integration**, preferably upstreamed to portable-pty and then pinned, or a narrowly maintained official-upstream patch. Assigning an already-running child to a job after spawn leaves an escape race.

Keep the existing one-reaper invariant. Current Hutan deliberately uses `waitpid` as the only reaper (`crates/terminal/src/sys.rs:48-57`); Windows must instead retain and wait on its actual child handle.

- Ctrl-C: terminal input/console-appropriate interrupt.
- Graceful stop: platform-specific owned-session shutdown.
- Force kill: owned process group on Unix, owned job on Windows.
- Stop/continue/HUP: expose only where actually implemented; Windows must not pretend to deliver Unix signals.

Retain bounded output queues, emulator state and render framing unchanged.

### 6.6 Opener, notifications, tray and autostart

| Service | macOS | Linux | Windows |
|---|---|---|---|
| Open file/URL | Native workspace/open integration | Desktop portal or `xdg-open` fallback | `ShellExecuteExW` |
| Notifications | UserNotifications | Freedesktop notification service/portal | Native toast integration |
| Tray | Existing status-item implementation | StatusNotifierItem, capability-detected | `Shell_NotifyIconW` |
| Desktop login start | launchd/SMAppService | XDG desktop autostart | Per-user Startup shortcut |
| Optional headless service | launchd agent | Explicit systemd user unit | Explicit later service integration, not required for first Windows desktop |

Pass opener arguments directly; never build a shell command from a path or URL. Restrict automatic URL schemes.

A desktop without a tray host must not make the only window disappear into an unreachable process. Refuse the unavailable hide-to-tray mode with an explanation.

Keep desktop login startup separate from daemon persistence. On Linux, a daemon launched under a desktop service may share its cgroup; background-survival mode must use an independently owned user service/scope when available. Do not automatically enable systemd linger or promise survival after logout.

### 6.7 Secrets

Adapt the existing gateway credential seam rather than creating a second provider-key registry:

- macOS: Keychain.
- Linux: Secret Service; locked, unavailable or missing session bus are explicit outcomes.
- Windows: user-bound DPAPI for wrapping material, or Credential Manager for the existing credential-store contract.

Return structured status such as `available`, `locked`, `unavailable`, with a reason. Never fall back to plaintext storage.

The direction’s encrypted vault remains a separate D-6/PA slice. `hutan-platform` supplies **OS key wrapping/storage**, not SQLite ownership, model-access policy or encryption format. A headless Linux host without a key service needs the separately approved passphrase-wrapped vault option; XP must not invent an unprotected “server fallback.”

---

## 7. Wire messages, storage and settings

### Wire changes

**Transport portability itself requires no wire change.** Keep the frame codec, handshake, replay epoch/sequence, subscribe/snapshot ordering, credit-1 rendering and bounded outbound queues exactly as ADR 0002 specifies: `docs/adr/0002-ipc-transport-and-framing.md:18-39`.

Add a capability-gated, additive query:

```json
{"id": 42, "method": "platform.status", "params": {}}
```

Illustrative result:

```json
{
  "id": 42,
  "result": {
    "os": "linux",
    "arch": "aarch64",
    "local_transport": "unix",
    "process_info": {"state": "available"},
    "secret_store": {
      "state": "unavailable",
      "reason": "No Secret Service session"
    },
    "process_controls": ["interrupt", "terminate", "kill", "stop", "continue"]
  }
}
```

Capability: **`platform.status`**.

This describes the **daemon host**. The desktop obtains its own tray, notification and opener capabilities locally; a Linux remote daemon must not determine whether the local macOS desktop has a tray.

Use the existing notification-intent protocol and `notify.act`; no OS-specific notification event family is needed. The existing separation is already explicit at `crates/protocol/src/notify.rs:1-7,113-142`.

Do not silently change required resource fields to optional. Unsupported operations return `unsupported`; missing process metrics must not be fabricated as zero. Existing resource bounds and subscription-only sampling remain in force: `crates/protocol/src/resources.rs:13-20,118-129`.

### Storage

**No new SQLite tables are required for XP.**

- Existing workspaces, sessions, configuration and history remain daemon-owned.
- Endpoint names, OS availability and tray state are runtime facts.
- Autostart registrations are OS-managed, not a parallel database.
- The encrypted vault’s tables belong to the separate vault ADR, not this platform port.

Path migration must include databases and sidecar state coherently; do not copy a live SQLite main file while ignoring its WAL.

### Settings

Prefer existing user-facing settings over an unnecessary `[platform]` namespace.

| Key | Design |
|---|---|
| `app.on_close` | Lifecycle proposal’s `quit`, `hide`, `quit_ui`; gate by actual desktop capabilities |
| `app.start_at_login` | Existing preference; OS-specific implementation |
| `app.menu_bar` | Migrate to `app.tray` in the lifecycle/settings cutover, including all callsites; avoid an everlasting alias |
| `notifications.native` | Desktop presentation preference; daemon still generates intents |
| `terminal.shell` | Existing shell-selection surface; platform default differs by OS |
| `terminal.env_inherit` | Existing policy retained; platform supplies appropriate facts |
| `secrets.backend` | `auto` for OS backend selection; passphrase choice belongs to vault ADR |

Paths and endpoint overrides remain environment/bootstrap configuration; do not make a live settings reload move an open database or listener.

---

## 8. Compatibility and migration

Current protocol is **0.29**, with matching-major compatibility and additive minors: `crates/protocol/src/lib.rs:68-84`.

1. **Platform crate extraction, Linux paths and Windows pipe transport:** no protocol version bump by themselves.
2. **`platform.status`:** next available **minor**, capability-gated. Do not reserve `0.30`; the workspace/model proposal already proposes that number.
3. **New OS support:** not a reason for a major bump.
4. **Changed required JSON types, event semantics or binary terminal representation:** require a **major** under ADR 0002, regardless of whether package version remains pre-1.0.
5. Keep binary terminal schemas unchanged in XP. If Windows raw exit-code fidelity needs an additional field, make it optional/additive; do not silently reinterpret a signed Unix-style field.
6. Keep macOS paths, explicit `HUTAN_HOME` isolation and existing Unix socket overrides working.
7. For users who ran an older Linux build using `~/Library/...`, provide an **explicit offline native-path migration**. Acquire the old data lock, refuse live-daemon migration, migrate settings/data coherently and report the chosen destination. Do not indefinitely search both locations or start two sources of truth.
8. Keep protocol-major generations from sharing writable state concurrently. Endpoint separation alone does not prevent database ownership conflicts.
9. Export/import across operating systems is not “copy the entire data directory”: filesystem paths, process IDs, shell commands and OS-wrapped secrets are host-bound.

The transport abstraction should be shared with remote work: it must not grow a separate Windows-only framing implementation.

---

## 9. Options and tradeoffs

| Option | Advantages | Costs / risks | Recommendation |
|---|---|---|---|
| Scatter cfg branches through existing crates | Smallest immediate edits | Repeated path/auth rules, difficult review, Unix leakage returns | Reject |
| `hutan-platform` with cfg-selected modules and optional integrations | One security boundary, headless dependency hygiene, testable paths | Requires a coordinated caller migration | **Choose** |
| Put all desktop and daemon behavior behind one giant `Platform` trait | Easy fake implementation | Bloated interface, unnecessary indirection, thread-affinity mistakes | Reject |
| Linux through WSL only; no native Windows daemon | Reuses Linux runtime | Does not satisfy native Windows support; filesystem and identity boundaries remain | Optional later host type, not replacement |
| Replace GPUI to obtain portability | Different ecosystem choices | Large unrelated rewrite; pinned GPUI already has required backends | Reject |
| Use cross-target checks as the release gate | Fast initial feedback | Cannot prove target linking, service behavior, ConPTY or GPU startup | Supplemental only |

---

## 10. GitHub Actions plan

Use native runners for required gates; keep cross-checks as a quick supplemental lane.

### Headless matrix—required first

| Runner/target | Commands and scope |
|---|---|
| Ubuntu 24.04 x86_64 GNU | Check, build and test all 11 requested headless crates with `--locked` |
| Linux aarch64 GNU | Same headless suite on a native ARM runner; use a supported hosted label or an isolated self-hosted runner |
| macOS arm64 | Existing suite plus platform contract tests |
| Windows x86_64 MSVC | Core/protocol initially; expand to all headless packages in XP-3 before declaring Windows supported |

Install only headless prerequisites on Linux—C compiler, pkg-config and packages actually required by the selected dependency graph. Bundled SQLite and rustls/ring do not justify installing the entire Zed desktop dependency list.

Each integration test receives its own temporary `HUTAN_HOME`. Tests must not use the runner’s real home configuration or secret store. Native key-store tests are opt-in and use isolated test stores; no paid provider calls.

### Desktop gates

- Linux: build `x11`, `wayland`, and both together.
- Linux X11: Xvfb plus Mesa software rendering.
- Linux Wayland: headless compositor plus session D-Bus.
- Windows: debug tests **and release desktop build**, including `fxc.exe`.
- macOS: existing native desktop/bundle coverage.
- Platform-specific settings and packaging tests must compile only where valid; macOS measurement commands in `xtask` need explicit platform boundaries rather than blocking all workspace tooling.

### Runtime acceptance lanes

[Showing lines 401-600 of 685. Use :601 to continue]

Exercise real local IPC and terminal behavior:

1. Start an isolated daemon; connect CLI and client; verify peer-auth failures.
2. Create terminal, send input, resize, detach and reconnect.
3. Exercise render credit-1, slow consumers and replay/snapshot recovery.
4. Stop a session with grandchildren; prove unrelated processes survive.
5. Test process-resource queries and denied-access outcomes.
6. Test all supported close modes and duplicate desktop activation.
7. On systemd, exercise UI restart separately from daemon/service stop.
8. On Windows, exercise PowerShell, cmd and Git Bash/MSYS descendants.

Run release packaging checks on tags. Run GPU, suspend/resume, session-manager and notification-interaction tests on isolated real-host lanes; ordinary hosted headless CI does not prove them.

Pin third-party Actions by reviewed commit, use minimal permissions, and cache by OS, architecture, toolchain and lockfile.

---

## 11. Packaging

### macOS

Retain the `.app`/CLI distribution initially; native integration moves to the desktop under the lifecycle ADR. Preserve the current executable naming and adjacent-binary discovery.

Current constraints and layout are documented at `docs/packaging.md:9-24,41-54`; current releases are Apple-Silicon/macOS-specific at `docs/packaging.md:128-146`.

### Linux

Ship in this order:

1. **Headless tarball:** `hutan`, `hutand`, licenses, shell integration and an optional example systemd user unit. No automatic service registration.
2. **Deb package:** split headless runtime from desktop where practical; install binaries/resources, desktop entry, icon and MIME/deep-link registration.
3. **AppImage:** desktop plus adjacent daemon/CLI/resources, tested on the oldest supported glibc baseline.

Do not bundle host glibc or GPU drivers. Choose the supported Linux baseline before release; building on a newer distro does not establish compatibility with older glibc.

### Windows

Start with an **x64 MSI** and a portable archive. Include desktop, daemon, CLI, shell integration and required runtime resources; use per-user install by default.

- Add a stable AppUserModelID/shortcut for toast activation.
- Keep Startup registration opt-in.
- Ensure `.exe` discovery and `PATHEXT` handling replace Unix executable-bit checks.
- Test Unicode paths, spaces, UNC paths, long paths, case sensitivity and junctions.
- Coordinate installer updates with a running daemon: stop-and-restore explicitly or defer the update. Do not assume a running executable can be overwritten or promise that every live process survives an incompatible update.
- Sign release artifacts; signing is separate from actually testing SmartScreen/enterprise-policy behavior.

Windows ARM64 and Linux musl are additional support commitments, not implied by the initial targets.

---

## 12. Risks

1. **False confidence from compilation:** Linux checks passed, but process observation and credential persistence currently remain incomplete.
2. **Security regression while “making it compile”:** deleting POSIX mode bits, peer checks or no-follow traversal is not a Windows implementation.
3. **ConPTY teardown:** portable-pty transport portability does not provide race-free process-tree ownership.
4. **systemd lifecycle:** detached process groups do not imply persistence outside the caller’s cgroup.
5. **Unavailable Linux desktop services:** tray, portals and secret services vary by desktop/session; capabilities must explain limitations.
6. **GPUI feature drift:** builds must cover each backend and release shader compilation against the pinned revision.
7. **Dependency contamination:** a daemon must remain buildable without graphical libraries.
8. **Protocol compatibility:** OS differences do not permit breaking existing DTOs under an additive minor.
9. **Path migration and split-brain:** runtime-directory changes must not create two daemons for one data directory.
10. **Secret portability:** OS-wrapped material cannot simply be moved to another OS or user account.

---

## 13. Slices, estimates and acceptance tests

Estimates are **[INFERENCE]**, in engineer-days for an experienced Rust engineer, including implementation and focused tests. They exclude the separate lifecycle redesign, full encrypted vault, hardware procurement, signing-account setup and external review waiting time.

| Slice | Estimate | Deliverable and acceptance |
|---|---:|---|
| **XP-1a: platform foundation** | 2–3 days | ADR, crate, shared path resolver, private filesystem and time helpers. Pure path tests cover all OS defaults, empty/relative overrides and `HUTAN_HOME`; macOS paths unchanged; Windows settings/client low-level blockers removed without weakening permissions. |
| **XP-1b: Linux headless correctness** | 3–5 days | Linux process observation, shell/environment gating, native IPC/security tests and headless CI. All 11 packages check/build/test natively; session/resource operations work; no `~/Library` creation. |
| **XP-1c: Linux secrets and service operation** | 3–5 days | Secret Service adapter and optional systemd user deployment. Isolated available/locked/unavailable tests; no plaintext fallback; stop/restart/logout semantics documented and exercised. |
| **XP-2a: GPUI Linux enablement** | 3–5 days | Explicit X11/Wayland features, dependency recipe, both graphical CI lanes. Real window opens, text renders, IME/clipboard/terminal resize work; single-backend builds pass. |
| **XP-2b: Linux desktop integration** | 4–7 days | Opener, notifications, tray and login startup through platform APIs. No-tray host cannot strand UI; notification actions use existing intents; preferences accurately reflect unsupported services. |
| **XP-3a: Windows service-library port** | 4–7 days | Permissions, paths, time, Crossterm features, agents, index and CLI assumptions. All requested packages reach native Windows checking; tests cover Unicode paths and private DACLs. |
| **XP-3b: Windows IPC and terminal lifecycle** | 6–10 days | Authenticated pipes, ConPTY, race-free job ownership and shell integration. Powershell/cmd/Git Bash terminals pass create/input/resize/reconnect/kill-tree tests; cross-user pipe access rejected; unrelated processes survive. |
| **XP-3c: Windows GPUI and integrations** | 4–7 days | Release shaders, opener, toasts, tray, startup, activation. Release build works with SDK; packaged notification activation and close modes work on a real Windows host. |
| **XP-P: release packaging** | 4–6 days | Linux headless/deb/AppImage and Windows MSI/archive pipelines, install/uninstall/update tests, supported-platform documentation. Uninstall preserves user data by default and removes only registrations owned by that installation. |

**Total: approximately 33–55 engineer-days [INFERENCE]**, with Linux headless support available much earlier than completion of every desktop/package slice.

The immediate decision need not wait for the Windows port: **accept D-4, land the shared boundary and native Linux headless gate first, and use runtime acceptance—not the absence of compiler errors—as the definition of platform support.**