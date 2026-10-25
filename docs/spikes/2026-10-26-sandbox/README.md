# Sandboxed agent runs — what exists, what works on macOS, what to build (2026-10-26)

Slice `SANDBOX-SPIKE`, row 14 of `docs/spikes/2026-10-25-user-needs/README.md`. Docs only; the ADR is `docs/adr/0021-sandboxed-agent-runs.md` (Proposed).

Environment of the measurements: macOS 27.0.1 (26A434) arm64, `/usr/bin/sandbox-exec`, OMP 18.12.3 (`omp --help`, `omp config list`). **The `codex` and `claude` CLIs are not installed on this machine**, so everything said about them comes from their current documentation and upstream source (linked), not from running them; those statements are marked *(docs)*. Unverified claims are marked `[INFERENCE]`. No model call was made: the harness test below is `omp acp` booting and opening a session, not a prompt turn. Prototype files: `~/hutan-scratch/sandbox-spike/` (profiles, drivers; the two profiles are printed in the appendix).

## Answer in one screen

1. **Cheapest win, and it is real: expose what the harnesses already have.** Codex has an OS sandbox (Seatbelt) with three modes and a per-session network switch; Claude Code has a Seatbelt-based Bash sandbox and permission modes, switchable per session by a flag or by `session/new` metadata; OMP has **no OS sandbox** (only approval modes and a tool allowlist). Hutan's wire already carries Codex's `sandbox` and `approval_policy` and every ACP harness's `mode`, but the Start sheet sets none of them (`LauncherField` has no such row; `crates/daemon/src/tasks.rs` creates the chat with `sandbox: None`).
2. **A Hutan-owned Seatbelt wrapper works and is cheap.** An 18-line profile "write only in the worktree (+ the repo's git dir, temp, the harness home)" wrapped a shell on a PTY, git, cargo, python and `omp acp` with **no functional loss and ~9 ms once per process start** (file-op overhead not measurable). A deny-by-default variant (12 lines + `system.sb`) additionally blocks Keychain and all network by default and still ran all of the above. It does **not** stop reads (secrets) unless told, and **cannot be nested**: any process inside it that calls `sandbox-exec` fails with `sandbox_apply: Operation not permitted` — so it replaces Codex's/Claude's own sandbox, it does not stack on it.
3. **Containers are a placement problem, not a flag.** They fit Hutan's daemon-owns-processes model (a PTY child can be `container exec -it …`; ACP and Codex talk stdio over `exec -i`), but need runtime detection, same-path mounts, a per-harness credential volume with the login done *inside* (ADR 0015 holds: Hutan never sees the token), and a way to let the daemon socket in. None of the runtimes is installed here; recommendation is to do it after `REMOTE-SPIKE`, with the same "execution host" abstraction.
4. **Slices:** `SANDBOX-1` safety presets in the Start sheet (harness-native, no new mechanism); `SANDBOX-2` *Contain* — Seatbelt for sessions without a native sandbox (OMP, plain terminals, commands), plus a per-session restricted daemon socket; `SANDBOX-3` container placement (after REMOTE-SPIKE; ask the user). Details in § 4.

## 0. The demand

| Issue | Ask | Note |
|---|---|---|
| Orca [#13665](https://github.com/stablyai/orca/issues/13665) (24 👍, 5 comments) | run agents in Docker Sandboxes (`sbx run claude` instead of `claude --dangerously-skip-permissions`) and "forward host credentials for agent into sbx" | the credential question is the open part |
| Superset [#3957](https://github.com/superset-sh/superset/issues/3957) (7) | run workspaces in a container "so I can safely use YOLO mode / `--dangerously-skip-permissions`" | the goal is **safe autonomy**, not isolation for its own sake |
| Superset [#2419](https://github.com/superset-sh/superset/issues/2419) (13) | `--dangerously-skip-permissions` hard-coded in the Claude preset; make the permission mode configurable | a visible, per-session mode picker is the fix |
| Agent-orchestrator [#6663](https://github.com/OrchestratorInc/agent-orchestrator/issues/6663) (0) | harness permission modes "are policy inside the agent, not a boundary around it"; agents can read `~/.ssh` | states the threat model |

What users want: *let the agent run without asking, because it cannot do much damage.* So the deliverable is a pair — **an autonomy level and a boundary** — chosen together in one place.

## 1. What each harness already offers

### Codex *(docs; not installed here)*
- Two independent axes: **sandbox mode** `read-only | workspace-write | danger-full-access` and **approval policy** (`on-request`, `never`, …). Defaults: no network, writes limited to the workspace and temp dirs; `<root>/.git` (and the resolved gitdir of a worktree), `.codex`, `.agents`, `.aws` are read-only inside the writable roots ([Agent approvals & security](https://learn.chatgpt.com/docs/agent-approvals-security), same text at developers.openai.com/codex/sandbox).
- Enforcement: **macOS = Seatbelt via `sandbox-exec -p <profile>`**, Linux = bubblewrap + seccomp (same page, § OS-level sandbox). `codex sandbox macos [--log-denials] CMD…` runs any command under Codex's policy (handy for tests).
- Per session: `--sandbox/-s`, `--ask-for-approval/-a`, `--add-dir`, `-c sandbox_workspace_write.network_access=true`, `--dangerously-bypass-approvals-and-sandbox` ([CLI reference](https://developers.openai.com/codex/cli/reference)). Network can be proxied with a domain allow/deny list (`features.network_proxy`).
- App server (what Hutan runs): `thread/start` takes `sandbox` and `approvalPolicy` (Hutan sends them, `crates/agents/src/codex/mod.rs:325-364`); `turn/start` takes a typed `sandboxPolicy` — `readOnly`, `workspaceWrite {writableRoots, networkAccess, readOnlyAccess}`, `dangerFullAccess`, and **`externalSandbox {networkAccess}` = "you already sandbox the server process, skip your own enforcement"** ([App Server](https://learn.chatgpt.com/docs/app-server), "Start a turn" / `command/exec`).
- **Risk to the current adapter:** the same page says the `untrusted` approval policy is **retired** ("can prevent either client from starting") and the app-server example uses `unlessTrusted`. Hutan's `ApprovalPolicy::Untrusted` serializes `untrusted` and the adapter is pinned to `>=0.160,<0.170` (`docs/agents.md`); whether 0.162–0.169 still accept it is covered by Hutan's recorded fixtures (0.162), the current docs are for a newer build. `[INFERENCE]` — check when the pin moves; `SANDBOX-1` must not add UI for `untrusted` without that check.

### Claude Code *(docs; not installed here)*
- **Permission modes** (`--permission-mode`): `default` (shown as "Manual"), `acceptEdits`, `plan`, `auto`, `dontAsk`, `bypassPermissions` ([permission modes](https://code.claude.com/docs/en/permission-modes.md)); `--dangerously-skip-permissions` = `bypassPermissions`; the docs say to use it only inside a container/VM/sandbox runtime ([sandbox environments](https://code.claude.com/docs/en/sandbox-environments.md)).
- **Sandboxed Bash tool** ([sandboxing](https://code.claude.com/docs/en/sandboxing.md)): off by default; `/sandbox` or `sandbox.enabled` in settings; **macOS = Seatbelt**, Linux/WSL2 = bubblewrap; writes: working dir + temp + `--add-dir`; reads: most of the machine; network: via a local proxy with a domain allowlist (empty by default); a *linked git worktree* also gets the main repo's shared `.git` writable except `hooks/` and `config` (line 530 of the page); modes *auto-allow* (sandboxed commands run without a prompt) vs *regular permissions*; **escape hatch**: Claude may retry a failed command unsandboxed unless `allowUnsandboxedCommands: false`. Per session without touching files: `claude --settings '{"sandbox":{"enabled":true,"allowUnsandboxedCommands":false}}'` (same page).
- **What it does not cover:** Read/Edit/Write/WebFetch tools, local MCP servers, hooks, LSP servers run **outside** the sandbox (permission rules only). For those the docs say to put the whole process in a container, VM or `srt` ([`@anthropic-ai/sandbox-runtime`](https://github.com/anthropics/sandbox-runtime), "beta research preview", `sandbox-exec` underneath).
- **Through ACP** (how Hutan runs it): the adapter ([`claude-agent-acp`](https://github.com/agentclientprotocol/claude-agent-acp), `src/acp-agent.ts` on `main`) builds its SDK options from `session/new` `_meta.claudeCode.options` and whitelists `sandbox` and `settings` among the pass-through keys, loads user/project/local/managed settings (so `sandbox.enabled` in `~/.claude/settings.json` already applies to Hutan's chats), and reports modes as ACP session modes (what Hutan's `--mode` and mode chip use). Hutan declines `terminal/*` requests, so the SDK's own Bash tool runs — inside the harness's sandbox. `[INFERENCE]` that the pass-through works with the adapter version a user has installed; Hutan sends no `_meta` today (`NewSessionParams` has only `cwd` and `mcpServers`, `crates/agents/src/acp/wire.rs:64`).

### OMP 18.12.3 *(measured locally)*
- `omp --help`: `--approval-mode always-ask|always-ask-cached|write|yolo`, `--auto-approve`, `--tools <list>`, `--no-tools`, `--add-dir`, `--max-time`, `--profile` (isolated auth/sessions/settings), `--no-pty`.
- `omp config list` shows `tools.approvalMode` (same four values), `bash.*`, `task.isolation.*` / `isolation.backend` (apfs, overlayfs, …: copies of the tree for subagents — a merge workflow, **not** a security boundary).
- **No OS sandbox setting exists**; `strings` on the binary finds no `sandbox-exec`/Seatbelt/sandbox-mode setting — the one hit, `suggestedSandboxMode`, is a protobuf field of a bundled provider's wire schema (`agent.v1.CommandClassifierResult`), not an OMP option. So "sandboxed OMP" can only come from outside.
- Hutan already uses this lever for Mawas: `omp acp --tools read,grep,glob` (Advisor) and `--approval-mode always-ask` (Ask before acting) (`docs/control.md` table, `crates/daemon/src/harness.rs`).

### What Hutan passes today
| | Codex | Claude Code (ACP) | OMP (ACP) |
|---|---|---|---|
| Start sheet | no policy row | no mode row | no mode row |
| `chat.create` | `approval_policy`, `sandbox` accepted (default `on-request` + `workspace-write`) | `mode` accepted; `approval_policy`/`sandbox` **refused** (`registry.rs:3102`) | same |
| What the user sees | header `approvals … · sandbox …`; no way to change | mode chip, `approves everything` in the warning colour for `bypassPermissions`/`yolo` | same |
| Terminal agent (`Start → Terminal agent`) | `argv = [agent, prompt]` (`sessions/launch.rs`): no flags | same | same |

### What Hutan could pass (the cheapest win)
| Containment goal | Codex | Claude Code | OMP |
|---|---|---|---|
| **Read-only / plan** | `sandbox: read-only` (+ `never` to refuse writes) | session mode `plan` | `--tools read,grep,glob` (no OS guarantee) |
| **Contained autonomy** (no prompts inside a boundary) | `workspace-write` + `on-request`, network off (the docs' *Auto* preset); network on = per-session switch | mode `acceptEdits` (or `auto`) + `sandbox.enabled`, auto-allow, `allowUnsandboxedCommands:false` | none natively → needs `SANDBOX-2`; **do not** offer `yolo` as "contained" |
| **Ask** (today) | `untrusted`/`on-request` | `default` | `always-ask` |
| **Full access** (flagged) | `danger-full-access` | `bypassPermissions` | `yolo` |
| Terminal-agent argv | `-s <mode> -a <policy>` | `--permission-mode <m> --settings '<json>'` | `--approval-mode <m>` |

The mapping is already how Mawas picks its harness profile (`assistant_profile`), so the daemon grows one more table, not a new mechanism.

## 2. OS-level options on macOS

| Option | Verdict |
|---|---|
| **Seatbelt (`sandbox-exec`, SBPL profiles)** | The mechanism Codex, Claude Code (Bash tool), `srt`, and Homebrew use. `man sandbox-exec`: "DEPRECATED … adopt the App Sandbox" (man page dated 2017-03-09) — still present and working on macOS 27.0.1. Unprivileged, per process tree, no entitlements, no install. **Use it**, behind one `Contain` interface so a replacement can slot in. |
| App Sandbox (entitlements) | Not applicable: it is a property of a signed *app bundle* decided at launch; Hutan would have to run each agent as a separate signed sandboxed helper app, and agents run arbitrary toolchains from the user's PATH. |
| Endpoint Security | Not applicable: needs the `com.apple.developer.endpoint-security.client` entitlement (Apple approval), root/system extension, and only *observes or authorizes* — not a per-session boundary the user opts into. |
| Other (`sandbox_init` API) | Same engine as `sandbox-exec`; the CLI is the simplest and what Codex shells out to. |

### Prototype (`~/hutan-scratch/sandbox-spike/`)
Two profiles, appendix. **A** = `(allow default)` + `(deny file-write*)` + allow-list of write roots (+ optional `(deny network*)`, read-denies). **B** = `(deny default)` + `(import "system.sb")` + reads open, writes as A, no network, no `mach-lookup` beyond `system.sb`. Parameters (`-D WT=… -D GITCOMMON=… -D TMP=… -D HOMEDIR=…`) are **real paths**: a rule with `/tmp/x` does not match `/private/tmp/x` (tested: the symlink spelling denied the write, the real path allowed it), so the daemon must `canonicalize` every root.

Tool/harness test matrix (✔ works, ✘ denied/broken; "A" = prototype A, "B" = strict). All results were observed unless marked `[INFERENCE]`.

| Probe | Result |
|---|---|
| write in worktree / `$TMPDIR` / harness home | ✔ A, B |
| write outside (`../x`, `~/x`, `~/.cargo/x`) | ✘ `Operation not permitted` (A, B) — as designed; grandchildren (`cargo` → `rustc`) obey the same rules, the sandbox is inherited |
| read a file outside the worktree | ✔ **allowed** in A and B (reads are open by design); `(deny file-read* (subpath SECRETS))` blocked a decoy secret ✔; carve-out `(deny … HUTAN_HOME)(allow … HUTAN_HOME/shell-integration)` worked (later rule wins) |
| `git status/add/commit/log` in a **linked worktree** | ✔ when the repo's common `.git` (e.g. `<repo>/.git/worktrees/wt`, `objects`, `refs`) is writable; hooks and `config` kept read-only ✔ (`pre-commit` write and `git config` write denied). **With the common `.git` read-only (Codex's default) even `git add` fails**: `Unable to create '…/.git/worktrees/wt/index.lock': Operation not permitted`. Hutan's Finish (commit, merge, push) is done by the daemon (ADR 0020), so a protected `.git` costs agents little *here*; agents that commit themselves need the carve-out |
| PTY: zsh interactive via a real pty (`tty`, `stty size` after `TIOCSWINSZ`, `jobs`, `kill %1`) | ✔ A and B (B needed `(allow signal (target same-sandbox)) (allow system-sched)`; without them `nice` fails and job control hangs — found by the test) |
| `cargo build` of a tiny crate | ✔ A, B. With a registry dependency and a **warm** `~/.cargo` cache ✔ (cargo tolerated failed cache writes); a **cold** fetch needs a writable `CARGO_HOME`: with `CARGO_HOME` in the writable harness home ✔ (network on), ✘ network off (`index.crates.io` unreachable) |
| `python3 -m venv` | ✔ A |
| npm / pnpm / pip install | not run (no node here). `[INFERENCE]` same shape as cargo: caches under `~/.npm`, `~/.cache` must be writable or relocated (`npm_config_cache`) |
| Network: `curl https://example.com` | A: ✔; A + `(deny network*)`: ✘ (connect fails); B: ✘ by default, `(allow network*)` ✔ |
| **DNS under "network off"** | With `(allow network* (remote unix-socket))` (to let ssh-agent / docker.sock through) `gethostbyname("example.com")` **resolved** (mDNSResponder is a unix socket): a DNS-tunnel channel. With `(deny network*)` and no unix sockets it failed ✔. So "off" means *no* unix sockets except named ones |
| Daemon socket carve-out | `(allow network-outbound (remote unix-socket (literal "<HUTAN_HOME>/hutand.sock")))` under `(deny network*)`: connect to that path ✔, to another socket ✘ |
| **Keychain** | `security find-generic-password` / `list-keychains` work in A (allow-default); in B (no `mach-lookup`) they fail with `parameters … not valid` — securityd unreachable ✔. Whether a given item's ACL would have let an agent read it is Keychain's call `[INFERENCE]`; the point is that A leaves the door open and B closes it |
| `omp acp` as the sandboxed process (`HOME=<scratch>`, no credentials; `initialize` + `session/new`) | ✔ A, A+no-net, B, B+net: `session/new` returns a session with the mode option, warm 0.16 s in all four (plain 0.15 s). Not exercised: a model turn (needs credentials and a paid call) and OMP's writes under the real `~/.omp` `[INFERENCE]: sessions/ and logs must be writable; settings/extensions/hooks should not be` |
| **Nested sandbox** | `sandbox-exec` run by a process **already inside** any non-trivial profile → `sandbox_apply: Operation not permitted`, exit 71. Reproduced with outer profiles containing a single unrelated `deny` (network, file-read of a missing path, mach-lookup, signal); only an outer `(allow default)` with nothing else nested fine. Upstream confirms for Codex: openai/codex [#45657](https://github.com/openai/codex/issues/45657) (app-server under an outer Seatbelt cannot run `workspace-write` commands) and [#30615](https://github.com/openai/codex/issues/30615); Homebrew [discussion #59](https://github.com/orgs/Homebrew/discussions/59) ("The macOS sandbox doesn't nest") |
| Same pid | `sandbox-exec` execs the target in place (child printed `$$` = the pid `Popen` returned): no wrapper process in `hutan ps` trees, signals and `process.signal` unchanged |
| Violation reporting | `(deny … (with message …))` and `log show` did not surface the denial for an unprivileged process in my test (not reproduced; `srt` advertises a violation monitor `[INFERENCE]`). UX falls back to the `EPERM` text in the output |

### Overhead
200 runs of `/usr/bin/true`: plain 2.4 ms; A 11.1 ms; A+no-net 11.1 ms; B 19.2 ms (the `system.sb` import). `sandbox-exec -p '(allow default)'` alone: 11.2 ms — the fixed cost is profile compilation + exec, **paid once per sandboxed process tree**, not per command. Heavy work inside (median of 5): 5 000 file creates 435 ms plain / 435 ms in A; `git status` on the Hutan checkout (2 104 tracked files) 31 / 41 ms; `grep -r` over `crates/` 144 / 155 ms; zsh login shell start 38 / 44 ms. Conclusion: not a budget issue (perf budgets in `docs/vision.md` concern typing latency, not process start).

### What breaks / costs
1. **Nesting** (above). Whole-process Seatbelt (ours) and a harness's own Seatbelt (Codex `workspace-write`, Claude Bash sandbox) exclude each other. Codex needs `danger-full-access` or `externalSandbox` when we wrap it; Claude needs `sandbox.enabled:false` `[INFERENCE]: its docs only describe the missing-dependency fallback, not a failed apply, so the failure mode is unconfirmed`.
2. **Network can't be "off" around a harness.** The harness process itself must reach its model API (`api.anthropic.com`, OAuth hosts, …). Whole-process containment therefore gives write + secret-read + daemon-socket containment, and network containment only via an allowlist proxy (what `srt` and Codex's `network_proxy` are). Per-command network-off is what the *native* sandboxes do well — one more reason to prefer them where they exist.
3. **Reads are open** unless Hutan denies a list (`~/.ssh`, `~/.aws`, `~/.config/gh`, `~/.gnupg`, other harnesses' homes, Hutan's own data dir with a carve-out for `shell-integration/`, other projects). Maintaining that list is policy work, not engineering.
4. **Harness home must be writable** (sessions, logs, caches) — and a writable `~/.omp` / `~/.claude` lets the agent persist hooks, MCP servers or `yolo` for the *next* unsandboxed launch (Claude's docs warn the same for `srt`). Per-harness write-allow fragments (sessions/cache yes; settings, hooks, extensions, agents no) are needed and need a real-turn test per harness.
5. **Package-manager caches** (`~/.cargo`, `~/.npm`, `~/.cache`) and `git`'s `~/.gitconfig` writes need either a writable shared cache dir or env redirection.
6. **The control policy's boundary becomes real.** `docs/control.md` says an agent can unset `HUTAN_SESSION_ID` and "that boundary is the user's sandbox for the agent". Under Seatbelt with `(deny network*)` + a literal allow for **one per-session daemon socket**, the daemon can attribute by *which socket connected*, not by an environment variable (ADR 0017 already promises "a stronger restricted endpoint + sandbox"). Without it, a contained agent that may connect to the main socket can act as the user.

## 3. Containers

None of Docker, OrbStack, Colima, Podman or Apple's `container` is installed here, so this section is documentation-based *(docs)* and untested.

| Runtime | Facts |
|---|---|
| **Apple `container`** | macOS 26+, Apple silicon; one lightweight VM per container (Virtualization.framework); OCI images; `run/exec -i -t`, `--mount type=bind,…`, `-v`, `--publish-socket host_path:container_path`, `--ssh`, `--read-only`, `--network` ([README](https://github.com/apple/container), [command reference](https://github.com/apple/container/blob/main/docs/command-reference.md), [technical overview](https://github.com/apple/container/blob/main/docs/technical-overview.md): per-container VM = mount only what is needed, partial memory ballooning). Hutan targets macOS 27 on arm64 — the only runtime that is Apple-native, needs no daemon of someone else's, and supports publishing a host unix socket. |
| **OrbStack / Docker Desktop / Colima** | Docker-compatible CLI. Bind mounts cross the macOS↔VM boundary: OrbStack reports 75–95 % of native on `pnpm install` and `rm -rf node_modules` ([blog](https://orbstack.dev/blog/fast-filesystem)) and says "bind mounts are slow because they live on the macOS side" for the others; Docker Desktop offers VirtioFS and a sync option ([Docker blog](https://www.docker.com/blog/file-sharing-with-docker-desktop/)). Mounting an arbitrary *host unix socket* into a container works on Linux hosts; across the macOS VM boundary `[INFERENCE]` it is not generally available (only special-cased sockets like docker.sock), so reaching `hutand.sock` would need a TCP or relay path. |
| **Docker Sandboxes (`sbx`)** | microVM per sandbox with its own Docker daemon; mounts the project dir read-write (or a private clone); network presets Open / Balanced / Locked Down; for Claude "the session token stays on your host and is never stored inside the sandbox" ([get started](https://docs.docker.com/ai/sandboxes/get-started), [Claude Code](https://docs.docker.com/ai/sandboxes/agents/claude-code)). Requires installing Docker's `sbx` and signing in to Docker. This is what Orca #13665 asks for; for Hutan it is a third-party credential broker (not Hutan touching tokens, so ADR 0015 is not violated, but a hard dependency on a proprietary CLI). |
| **Dev containers** | The `devcontainer` spec/CLI; Claude publishes a reference container with a default-deny iptables firewall and says it supports `--dangerously-skip-permissions` ([dev containers](https://code.claude.com/docs/en/sandbox-environments.md)); Codex's docs describe running it in one with `danger-full-access` and warn that a malicious repo can exfiltrate anything inside, including Codex credentials. |

### How Hutan would place a session in a container
- **PTY bridging is free.** Hutan's terminal is "a process on a PTY"; the command becomes `container exec -it <id> -- <shell|agent>` (or `docker exec -it`). Window size reaches the container through the CLI's SIGWINCH forwarding `[INFERENCE]`; OSC 133/7 shell integration must be mounted in and `HUTAN_SHELL_INTEGRATION_DIR` set. **ACP and `codex app-server` are stdio**: `exec -i` instead of a local spawn — `Launch { program, args, … }` (`crates/agents/src/jsonrpc.rs:44`) already is the seam.
- **Paths: mount the worktree at the same absolute path** (`-v $WT:$WT`, plus the main repo's `.git` for a linked worktree, hooks/config read-only where the runtime supports it). Then `file_diff`, checkpoints, editor links and `cwd` in events mean the same thing on both sides; otherwise every path in `AgentEvent` needs mapping. File ownership (uid mapping) and case-sensitivity of the VM's view are the usual macOS gotchas `[INFERENCE]`.
- **Credentials (ADR 0015).** The rule is that Hutan never reads, stores or forwards tokens. Options:
  1. *(recommended)* a **named volume per harness (and per account)** mounted as the harness home (`~/.claude`, `~/.codex`, `~/.omp` in the container; Claude's `CLAUDE_CONFIG_DIR`, `auth.json` for Codex). The user logs in **inside the container** with the harness's own flow; Hutan only runs the login command in a terminal session placed in that container (exactly what `account.login` does on the host). Tokens exist only in the volume, written and read by the harness. Claude stores credentials in the **Keychain** on macOS and in `~/.claude/.credentials.json` on Linux ([authentication](https://code.claude.com/docs/en/authentication.md)), so a host login cannot simply be mounted in. Headless-friendly flows exist: Claude `/login` (URL + code), Codex `codex login --device-auth` ([auth](https://developers.openai.com/codex/auth)); OMP's `omp login` in a container is `[INFERENCE]`. This is the same per-home mechanism `ACCOUNTS-MULTI` needs.
  2. bind-mount the host's harness home: gives the container the host's tokens (Hutan still does not read them) but defeats the isolation, and does not carry Claude's Keychain login.
  3. `sbx`'s host-side credential proxy: tokens never enter the VM; third-party dependency.
  4. API keys in the environment from Hutan's Keychain: possible under ADR 0008 (Hutan-held API keys) but visible in `inspect`, and contradicts "secrets never in events/logs" unless handled carefully — **not recommended**.
- **The daemon socket.** Hooks (`hutan agent …`) and `hutan mcp` inside the container need the daemon. Apple `container` can publish a socket (`--publish-socket`); Docker-family runtimes need a relay. ADR 0002's transport is a unix socket with peer-UID check; a TCP relay needs authentication — the same problem `REMOTE-SPIKE` solves for remote hosts, so containers should reuse its answer.
- **Cost:** image management (toolchains per project), cold start (a VM per container on Apple's runtime), resource limits (`--cpus`, `--memory`), and the macOS file-sharing penalty for build-heavy trees. Container placement also loses everything that is macOS-only inside the agent's reach (Xcode, simulators, Keychain-backed tools) — often the point, often a blocker.
- A container is not a free pass: the mounted worktree is writable (agent can still wreck it — checkpoints and worktrees are the answer, ADR 0019) and network egress is open unless configured.

## 4. Recommended path

**Principle: native first; Hutan's Seatbelt where there is no native sandbox; containers as a placement, later.** Autonomy level and boundary are chosen together, in one control, and the UI says what is enforced and by whom.

### SANDBOX-1 — safety presets in the Start sheet (harness-native)
- Start sheet row **Safety** (Chat and Terminal agent): *Ask* (default, today's behaviour) · *Contained* · *Read-only* · *Full access* (warning colour). Per harness, only the presets it can honour are enabled, with the reason in a line below ("OMP has no OS sandbox; *Contained* needs **Contain** (SANDBOX-2)"). A project/global default in `settings.toml` `[launcher]`.
- Daemon maps preset → per-harness arguments (table in § 1) in the code that already builds Mawas's profiles; Codex gets `sandbox`/`approval_policy` (+ network flag), Claude gets a session mode and `_meta.claudeCode.options.{settings|sandbox}` in `session/new`, OMP gets `--approval-mode`. Terminal agents get the flags in § 1's last row (and the existing `approves_everything` styling decides the warning).
- Chat header/mode chip shows the *effective* state (`Codex · workspace-write · network off`, `Claude · sandbox on`), read back from what the harness reports (Codex `thread/start` result includes `sandbox`; Claude mode via `current_mode_update`); never Hutan's request alone.
- **Protocol impact:** additive minor + capability `chat.safety`: `chat.create.safety?: "ask"|"contained"|"read_only"|"full"`, `ChatInfo.safety {preset, enforced_by: "harness"|"hutan"|"none", network?: bool}`; `chat.models` additionally returns the harness's `modes` so the sheet can show them before the session exists. `approval_policy`/`sandbox` stay as the expert pair for Codex; the refusal for ACP harnesses stays. `task.create` `TaskRun::Chat` gains `safety?`.
- **Risks:** `untrusted` retirement (§ 1); Claude `_meta` pass-through is adapter-version dependent (feature-detect, show "not applied" rather than pretend — AGENTS.md "gaps are shown, not faked"); a preset that silently degrades is worse than none.

### SANDBOX-2 — *Contain*: Seatbelt for sessions without a native sandbox
- New daemon module (`sandbox`, macOS-only, `#[cfg]`-gated, a `Contain` trait with one Seatbelt impl) that renders a profile from a typed policy `{write_roots, read_deny, network: off|on, sockets: [path]}` into `<HUTAN_HOME>/sandbox/<session>.sb` (0600) and wraps the launch as `sandbox-exec -f … -D … -- <argv>`. One choke point per spawn kind: terminal `command()` (`crates/terminal/src/terminal.rs:299`), `Transport::spawn_v2` (`jsonrpc.rs:141`, ACP and `codex app-server`). All roots `canonicalize`d; refuse to start (never fall back to uncontained) when `sandbox-exec` is missing or the self-test (`sandbox-exec -p '(version 1)(allow default)' /usr/bin/true`) fails.
- Policy defaults: write = worktree (or the checkout) + the repo common `.git` minus `hooks`,`config` + `$TMPDIR` + a per-harness fragment; read-deny = credential dirs + Hutan's data dir (carve-out `shell-integration/`) + other harness homes; network = **on** for chat harnesses (they need their API; allowlist proxy is a later option), **off or on** for *Command* and *Plain terminal*, default on; unix sockets: only the session's own daemon socket. Profile **B** (deny default) as the base, A as fallback if a system tool needs more.
- Start sheet: **Contain** checkbox (What = Terminal agent / Command / Plain terminal / chat with OMP), *Network* toggle for commands, status chip. Applying it to Codex/Claude is an *advanced* opt-in that turns their native sandbox off (`danger-full-access` / `externalSandbox`, `sandbox.enabled:false`) so the boundary also covers file tools, MCP servers and hooks — documented trade-off, not default.
- **Per-session restricted socket** (`SANDBOX-2B`, same slice or directly after): the daemon listens on `<HUTAN_HOME>/s/<session>.sock`; the profile allows only that one; connections on it are *always* agent-origin with that session id (no env-var trust), classified by the existing `[control]` policy. Gives `docs/control.md`'s "boundary" a mechanism.
- **Protocol impact:** additive minor, capability `sandbox`: `terminal.create`/`chat.create`/`task` `contain?: {network?, extra_write?: [path]}`, `SessionInfo.containment {kind: "seatbelt", network, write_roots, applied: bool, reason?}`, `daemon.toml [sandbox]` (global/project defaults, `extra_write`, `read_deny`).
- **Tests:** real PTY + real `sandbox-exec` (skipped off macOS): write outside → `EPERM`, inside OK, git commit in a linked worktree OK / hooks write denied, daemon socket only, Keychain probe in B, nested `sandbox-exec` fails (documents the rule). A real OMP turn with `--profile` is needed to fix the per-harness write fragment (not done in this spike).
- **Risks:** `sandbox-exec` deprecation (mitigation: one trait, self-test, fail closed; same dependency as Codex/Claude/srt, so Apple removing it breaks them first); SBPL is undocumented — keep the profile small and snapshot-tested; reads-open means *Contain* is damage containment, not confidentiality, and the UI must say so ("Writes stay in this worktree. It can still read your files.").

### SANDBOX-3 — container placement (after REMOTE-SPIKE; **ask the user first**)
- Detect runtimes (`container`, `docker`/OrbStack), project setting `[sandbox.container] image|devcontainer`, placement `Placement::Container` next to `Local` (same abstraction as a remote host), same-path mounts, harness home volumes per account, login-in-container through `account.login` with a placement argument, session socket published (Apple) or relayed.
- Protocol: `placement` field on session create (shared with REMOTE), `SessionInfo.containment {kind: "container", runtime, image}`.
- Not before: (a) REMOTE-SPIKE fixes the transport/authentication story, (b) a runtime is actually installed for a first user (none is on this machine), (c) `ACCOUNTS-MULTI`'s per-home mechanism exists.

### Order and what we deliberately do not do
`SANDBOX-1` → `SANDBOX-2` (+2B) → `SANDBOX-3` if asked. No bundled runtime, no Hutan-run network proxy in v1 (later: domain allowlist for chat harnesses, reusing `srt`'s or Codex's design), no claim that anything is "secure" — the UI names the boundary (*writes limited to this worktree*, *harness sandbox: workspace-write, network off*, *container*) and its holes (*can read your files*, *network on*).

## Open questions for the user
1. Is "contained autonomy without prompts" the product goal (Superset #3957) or "safer prompts"? It changes whether *Contained* may default `approval_policy` to *never*/auto-allow.
2. Should the default for a **new worktree session** be *Contained* where a native sandbox exists (Codex/Claude), i.e. opt-out instead of opt-in?
3. Is a hard dependency on a container runtime acceptable for SANDBOX-3, or should Hutan only ever *detect* what the user has?

## Appendix: the prototype profiles
Run: `sandbox-exec -f P.sb -D WT=<realpath worktree> -D GITCOMMON=<realpath repo/.git> -D TMP=<realpath $TMPDIR> -D HOMEDIR=<harness home> CMD…`.

**A — permissive default, writes only in the worktree** (`worktree-write.sb`):
```scheme
(version 1)
(allow default)
(deny file-write*)
(allow file-write*
  (subpath (param "WT")) (subpath (param "TMP")) (subpath (param "HOMEDIR"))
  (literal "/dev/null") (literal "/dev/tty") (literal "/dev/dtracehelper")
  (regex #"^/dev/ttys[0-9]+$") (literal "/dev/ptmx"))
(allow file-write* (subpath (param "GITCOMMON")))          ; a worktree's commits land here
(deny file-write*
  (subpath (string-append (param "GITCOMMON") "/hooks"))
  (literal (string-append (param "GITCOMMON") "/config")))
(deny file-write* (subpath (string-append (param "WT") "/.git/hooks")))
; variants appended: (deny network*) | (deny file-read* (subpath (param "SECRETS"))) |
;   (allow network-outbound (remote unix-socket (literal (param "HUTANSOCK"))))
```

**B — deny by default** (`worktree-strict.sb`, no network, no Keychain):
```scheme
(version 1)
(deny default)
(import "system.sb")
(allow process-exec) (allow process-fork)
(allow signal (target same-sandbox)) (allow process-info* (target same-sandbox)) (allow system-sched)
(allow file-read*)
(allow file-write* (subpath (param "WT")) (subpath (param "TMP")) (subpath (param "HOMEDIR"))
  (subpath (param "GITCOMMON")) (literal "/dev/null") (literal "/dev/tty") (regex #"^/dev/ttys[0-9]+$"))
(deny file-write* (subpath (string-append (param "GITCOMMON") "/hooks"))
                  (literal (string-append (param "GITCOMMON") "/config")))
(allow sysctl-read) (allow ipc-posix-shm) (allow user-preference-read)
(allow pseudo-tty) (allow file-ioctl)
; network access: add (allow network*)
```
A generic reference profile with ~1 700 lines of the real thing: [`srt` `macos-sandbox-utils.ts`](https://github.com/anthropics/sandbox-runtime/blob/main/src/sandbox/macos-sandbox-utils.ts) — `(deny default)`, explicit `mach-lookup` list, `(allow pseudo-tty)` option, proxy-port-only network, mandatory denies for `.git/hooks`, `.git/config`, shell startup files.

## Sources
Codex: [approvals & security](https://learn.chatgpt.com/docs/agent-approvals-security), [App Server](https://learn.chatgpt.com/docs/app-server), [CLI reference](https://developers.openai.com/codex/cli/reference), [auth](https://developers.openai.com/codex/auth). Claude Code: [sandboxing](https://code.claude.com/docs/en/sandboxing.md), [sandbox environments](https://code.claude.com/docs/en/sandbox-environments.md), [permission modes](https://code.claude.com/docs/en/permission-modes.md), [CLI reference](https://code.claude.com/docs/en/cli-reference.md), [authentication](https://code.claude.com/docs/en/authentication.md), [claude-agent-acp](https://github.com/agentclientprotocol/claude-agent-acp). [sandbox-runtime](https://github.com/anthropics/sandbox-runtime). Containers: [apple/container](https://github.com/apple/container), [OrbStack](https://orbstack.dev/blog/fast-filesystem), [Docker Sandboxes](https://docs.docker.com/ai/sandboxes/get-started). Issues: Orca #13665, Superset #2419 / #3957, agent-orchestrator #6663, openai/codex #45657 / #30615. Local: `man sandbox-exec`, `omp --help`, `omp config list`, repo files cited inline.
