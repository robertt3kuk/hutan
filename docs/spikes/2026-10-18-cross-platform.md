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
…

…

…
The immediate decision need not wait for the Windows port: **accept D-4, land the shared boundary and native Linux headless gate first, and use runtime acceptance—not the absence of compiler errors—as the definition of platform support.**

[Showing lines 1-300 of 685. Use :301 to continue]