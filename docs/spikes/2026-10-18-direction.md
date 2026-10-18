# Direction research — model, lifecycle, remote, cross-platform, speed, icons, personal assistant (2026-10-18)

Status: **research and proposal, not decided.** Decisions marked **D-n** go to the user; each accepted one becomes an ADR before code (AGENTS.md: hard-to-reverse → ADR first).

Evidence (official sources only, checked against each project's site and the installed app):
- Orca — `stablyai/orca` (MIT): `2026-10-18-orca-layout.md`, `2026-10-18-orca-session-history.md`, `2026-10-18-orca-process-remote.md`.
- herdr — `herdrdev/herdr` (Apache-2.0): `2026-10-18-herdr-model.md`.
- Hermes Agent — `NousResearch/hermes-agent` (MIT): `2026-10-18-hermes-meta-harness.md`.
- Hutan today: `2026-10-18-hutan-layout-map.md`, `2026-10-18-hutan-model-lifecycle-transport.md`.
- Performance: § 6 (measurement in progress: `2026-10-18-perf-lag.md`).
- Icon sets: `2026-10-18-icon-sets.svg`.

## 0. What the user asked (2026-10-18, in order)

1. Icon set: our own, or another one — compare.
2. Speed: Hutan lags compared with Orca; make it fast.
3. A project/workspace must group **terminal tabs and agents**; agents live in a workspace.
4. Daemon like Orca: the user does not like that something keeps running as a separate process; one window like Orca; closing closes.
5. Left: sessions not yet assigned to a workspace, each showing its **cwd**; they can be dragged into a workspace.
6. A workspace can be made **per task** and group things, as in herdr; otherwise there are just sessions.
7. The right columns relate to the selected **workspace** and to the selected **session**.
8. Maybe the window can close while a control at the top (menu bar) turns things off and manages them.
9. `hutand` can run on **another server** and be controlled from here; lay the concept and several connection methods; name the advantages.
10. Do not sit on macOS: **cross-platform**.
11. Everything above switchable by preference.
12. A **meta-harness** in the Assistant — a Home page like Hermes, but independent of the harness, through ACP.
13. UI kept right; every page done by best practice.
14. The Assistant is a **workspace under the hood**, presented as a **personal assistant**; everything important is stored **only locally**; **secrets encrypted** by us so nobody can steal them; it can **drive the whole meta-harness** with tools and any backend; add **traces** and whatever really improves function — "a local Jarvis".
15. Put all of this into research and decide how to do it well (this document).

## 1. Principles

- **Local-first, daemon-owned.** `hutand` stays the source of truth (ADR 0001): sessions, workspaces, memory, skills, schedules, secrets, traces. Clients render.
- **Cross-platform by construction.** OS specifics behind one platform layer; Linux daemon+CLI first (it is also the remote-server story), then Linux desktop, then Windows.
- **Remote-ready.** The desktop talks to a *host*; the local daemon is just the default host.
- **Configurable with good defaults.** Every behaviour change below ships as a setting; defaults follow Orca/herdr where they are better.
- **Measured speed.** No UI work without frame/CPU budgets (docs/perf.md).

## 2. Sessions, workspaces, projects (items 3, 5, 6, 7)

Findings: herdr's model is *workspace → tabs → panes*, agents are occupants of terminals, workspaces are recommended per repo/task/investigation, panes move between workspaces without restarting (`pane.move`), attention rolls up per workspace (blocked > done > working > idle). herdr has **no** unassigned pool — that part is ours. Hutan today: every session has a required `workspace_id` resolved automatically to a *project* by cwd/repo; user workspaces group **projects**, not sessions; `workspace.assign` moves projects.

Proposal:
- **Session** = terminal or agent chat. Always shows its live cwd (OSC 7) and repo/branch badge. Its *project* (repo) stays automatic but becomes an **attribute** (badge, filter, grouping), not the container.
- **Workspace** = a **task container**: a named set of sessions (terminal tabs + agents) with its own center layout (tabs/splits), optional worktree/branch, optional note/issue link. Created on purpose: "New workspace…", "New workspace from selection", from a task, or by an agent through MCP.
- **Sessions (unassigned)** = the list of sessions in no workspace, each with cwd; drag one (or a multi-selection) onto a workspace, or "Move to workspace…" from its menu. Moving keeps the process (herdr's rule).
- Left sidebar: nav (Search, Tasks, Agents, Hutan) → **Workspaces** (each expandable to its sessions, attention roll-up dot) → **Sessions** (unassigned, cwd shown). Optional grouping of unassigned sessions by project.
- Right sidebar follows selection: workspace selected → its Changes (its worktrees/repos), Requests (its approvals/questions), Agents (its agents), History (scope Workspace); a session focused → session context (Info, its diff, its requests). A pin keeps "All".
- Wire/storage, additive path (no major bump): keep the Home row as the internal "unassigned" container so `SessionInfo.workspace_id` stays required; add `session.assign {session_id, workspace_id?}` (none → unassigned), `WorkspaceInfo.kind = task` (or reuse `workspace` kind), per-workspace layout stays desktop-side. `[projects] auto` keeps attributing repos, but no longer *moves* sessions into project containers when the user chose the task model.
- **D-1**: switch the default organisation to *task workspaces + unassigned sessions* (keep "by project" as a sidebar option).

## 3. Lifecycle: one window, close means close (items 4, 8, 11)

Findings: Orca is **not** "close → everything stops": a detached terminal daemon survives quit and updates; on macOS the app stays alive without windows; a tray/menu-bar item offers Open/Settings/Quit for real; Windows can close-to-tray. herdr's server survives client exit on purpose. Hutan today: default `keep_running=false`, last window close quits; the daemon is a separate process with **its own** Cocoa/NSApplication menu-bar item and Dock-reopen path (the source of the launch-loop bug), and Dock-Quit/logout preserve live sessions.

Proposal:
- The daemon becomes **headless everywhere** (no NSApplication, no Dock identity, no menu bar). All UI — window, tray/menu-bar item, notifications — belongs to the desktop process. This removes the Launch Services reopen hacks and is the same code on Linux/Windows.
- **One main window** (single instance already exists); extra windows only by explicit "Open in new window" (setting, off by default).
- Setting `app.on_close` with three behaviours (D-2 picks the default):
  1. **Quit** — closing the window quits Hutan; running sessions are stopped and **restored at next launch** (shells restart in their cwd, agents resume their conversations — RR-1/restore already do this); the daemon exits. Nothing stays in the background.
  2. **Keep sessions, hide** (Orca style) — the window closes, the desktop process stays as a small menu-bar/tray item (Open, sessions that need you, Quit for real); the daemon keeps sessions running.
  3. **Keep sessions, quit UI** — desktop quits, daemon keeps running headless (for remote/power users); `hutan` CLI or reopening the app reconnects.
- Activity Monitor then shows at most "Hutan" + `hutand` (+ the user's own shells/agents); `hutan mcp` bridges are children of agents and named as such.

## 4. Remote hosts and connection methods (item 9)

Findings: herdr remote = **SSH stdio thin client** (`herdr --remote host`) through a local bridge socket, several saved machines in one UI, no TCP listener. Orca = SSH bootstrap + managed `orcad` on the host + authenticated tunnel; WSL via a stdio bridge (avoids WSL localhost flakiness); mobile through an **E2EE** cloud relay that only brokers bytes; host state is live / unverifiable / refused / retired, not boolean. Hutan's frame codec and ADR 0002 rules are transport-independent; the Unix socket, peer-UID check, autostart and "socket file disappeared = stopped" are not.

Proposal — a `Transport` seam in `client` and `daemon` (generic `AsyncRead + AsyncWrite` + an auth policy), keeping handshake, epochs, replay, credit-1 and bounded queues unchanged:
| Method | How | Auth | When |
|---|---|---|---|
| Local | Unix socket / Windows named pipe | OS peer identity | default |
| **SSH** | `ssh host hutan proxy` — a stdio↔local-socket byte proxy on the host; optional "install/upgrade hutand on host" | SSH keys/agent, known_hosts | first remote method (no open ports, works through bastions) |
| TLS (direct) | `hutand serve --listen 0.0.0.0:port` with TLS | pairing code → per-device key, revocable | LAN/VPN/Tailscale |
| Relay (later) | outbound WebSocket to a relay, E2EE frames | device keys, relay never sees plaintext | phone, NAT |
- Desktop: a **Hosts** switcher (Local, + Add SSH host…); each host has its own workspaces/sessions/history; status live/unreachable/refused; never autostart a remote daemon silently.
- Paths, files, credentials and memory belong to the host that runs the session; file attachments are read on that host.
- Advantages: long jobs keep running on a strong server while the laptop sleeps; GPU/large repos stay remote; several machines in one window; the phone can watch later; the same headless daemon is what makes Linux/Windows support useful.
- **D-3**: SSH first; TLS second; relay later.

## 5. Cross-platform (item 10)

Findings: GPUI has Linux (Wayland/X11) and Windows backends; Hutan pins mac features only. Blockers: `xtask/src/proc.rs` (mac rusage, breaks `cargo build --workspace` on Linux), ungated `open`, mac paths (`~/Library/...`) in daemon/client/settings, Keychain-only credentials, Unix-only APIs in client/daemon/terminal/agents (Windows), mac-only process observation, mac packaging. Orca abstracts IPC endpoints (UDS/named pipe), PTY (ConPTY), secrets (`SecretStore`, never assume encryption), cgroup handling on systemd; herdr centralises OS code in `platform/` with capability gates.

Proposal: a `hutan-platform` crate (paths: XDG / Known Folders / Application Support; opener; notifications; tray; autostart: launchd / systemd user / Startup; secret backend; process info; IPC endpoint), cfg-gated implementations, CI that builds and tests Linux (`daemon`, `cli`, `client`, `index`, `storage`, `protocol`) on every change, then the Linux desktop (GPUI x11/wayland features), then Windows (ConPTY via `portable-pty`, named pipes). Packaging per OS later (AppImage/deb, MSI). **D-4**: order Linux headless → Linux desktop → Windows.

## 6. Speed (item 2)

Measurement in progress (agent HutanPerf; results go to `2026-10-18-perf-lag.md` and here). Orca's techniques to adopt where measurements point: 2 ms output batching with interactive fairness (visible pane echo never waits behind bulk output of hidden terminals), producer back-pressure, hidden panes do no work (no cursor blink, no redraw), lazy loading of heavy panels, virtualised lists everywhere, no polling timers while idle (5 s active / 30 s idle at most), deferred non-critical startup work until the first frame. Hutan-specific known misses: idle CPU 0.70 % (GPUI CVDisplayLink runs at 60 Hz with nothing to draw — needs a GPUI patch), sidebar/history re-render on every daemon event, 30 s History timer.

## 7. Icons (item 1)

Compared the same 12 concepts in four permissive sets (`2026-10-18-icon-sets.svg`): **Lucide** (ISC, current; neutral, Orca uses it), **Phosphor** (MIT; 6 weights incl. duotone — distinctive and still consistent), **Tabler** (MIT; 5 000+, slightly heavier), **Remix** (Apache-2.0; line+fill pairs). A fully custom set is possible but costs weeks and drifts; better: a base set + a few **own glyphs** (orangutan mark, agent/harness logos, workspace/task). Icons go through one `Icon` enum, so switching sets is cheap — it can even be a setting. **D-5**: base set (recommendation: Phosphor regular, or stay on Lucide) + own brand glyphs.

## 8. The personal assistant: "Hutan" as a local meta-harness (items 12–14)

Findings (Hermes): the value is the **persistent layer** — curated memory (`MEMORY.md` environment facts, `USER.md` profile; bounded; frozen snapshot per session; explicit write tool with approval), skills (Agent Skills packages, progressive disclosure, create/patch after work), session recall (FTS), scheduled jobs (cron), delegation (subagents with fresh context), messaging gateways, desktop pages (chat, Capabilities: skills/toolsets/connectors, Star Map of memory+skills, Cron, Agents, Settings). Hermes is both an ACP agent and an ACP client, but its outbound ACP keeps its own loop (text tool-call bridge) — **do not copy that**; let the harness run natively and expose Hutan's services through MCP.

Proposal:
- **Under the hood a workspace** (`kind = assistant`, one global "Hutan" plus optional per-workspace assistants): its chats, tabs, history, traces use the same machinery as every workspace. **Presented as a personal assistant**: the left nav item "Hutan" opens the **Home page**.
- **Backend: any** — OMP, Claude Code, Codex over ACP (native tools + Hutan MCP), or a direct provider (needs Hutan's own small tool loop over the gateway; until then marked "chat only").
- **Hutan supplies** (daemon services, exposed as MCP tools to whichever backend runs the turn, assembled into the session's opening context):
  - **Memory**: profile + facts + per-workspace notes; IDs, provenance, revisions; writes are proposals the user can approve, or auto by setting; snapshot per session + deltas.
  - **Skills**: Agent Skills format, shared with Claude/OMP skill dirs where the user wants; list/view/manage tools; "learn from this session" as an explicit action.
  - **Schedules**: daemon-owned cron with an occurrence ledger; a due job runs a normal turn on the chosen backend in the assistant workspace; unattended runs get an explicit capability policy.
  - **Control of everything** (Jarvis): the existing `hutan mcp` (sessions, workspaces, tasks, worktrees, history, UI) extended with workspace/session assignment, schedules, memory, skills, secrets-by-reference, host selection; delegation = start agents in task workspaces and watch them (Agents board); every mutating call goes through `[control]` policy and the approval UI.
  - **Two modes** (setting): *Advisor* (today's read-only assistant) and *Operator* (may act through MCP with approvals; per-tool allow lists).
- **Home page** (best-practice layout): conversation in the middle; right rail with Today (what needs you, running agents, scheduled jobs), Memory, Skills, Schedules, Traces; Capabilities and Memory/Skills management as full pages; everything keyboard-reachable.
- **Local-only storage**: all of it in the daemon's data dir on the host; nothing synced unless the user exports.
- **Secrets vault** (D-6): envelope encryption — a random vault key, wrapped by the OS key store (macOS Keychain, Linux Secret Service/libsecret, Windows DPAPI/Credential Manager) and optionally by a user passphrase (Argon2id); items sealed with XChaCha20-Poly1305 in SQLite; agents and models only ever see **references** (`secret://name`), resolved by the daemon at the moment of use (env of a launched process, HTTP header of a provider call) and **redacted** from journals, traces and the index. Honest limit: malware running as the same user while the vault is unlocked can still ask the OS store — the vault protects copies, backups, other users, stolen disks and prompt-injection exfiltration, and makes every use auditable. Provider keys move from Keychain-only to this vault on every OS.
- **Traces** (observability): every turn, tool call, approval, MCP call, delegation and schedule run of every session becomes a span (OpenTelemetry-shaped: trace/span ids, parent, attributes, tokens, cost, latency), stored locally (SQLite, bounded retention), viewable per session/workspace/assistant, optional OTLP export. Token/cost totals per workspace and backend.
- What else improves function: workspace roll-up attention (herdr), "hand off to another agent" (Orca), resume any past session from History into a workspace, per-workspace budgets/limits, notifications to the phone later via the relay.

## 9. UI by best practice (item 13)

Every page: one primary action, keyboard-first, empty states with a next step, no clipped text, honest status (no fake success), settings searchable, accessibility labels, 30 px rows / 32 px headers / 36 px bars (Orca metrics), user tests through the bounded driver after each wave.

## 10. Plan (after the decisions)

| Phase | Slices | Contract |
|---|---|---|
| A. Speed first | P-3 (fixes from the measurement), GPUI idle fix | none |
| B. Model | WS-1 task workspaces + unassigned sessions + `session.assign` (daemon), WS-2 sidebar/right panels follow selection (desktop) | protocol 0.30, ADR "task workspaces" |
| C. Lifecycle | LC-1 headless daemon, desktop-owned tray, `app.on_close` modes, single main window | ADR "process model" (replaces parts of M6-C/M6-D) |
| D. Cross-platform | XP-1 `hutan-platform` + Linux CI headless, XP-2 Linux desktop, XP-3 Windows | ADR "platform layer" |
| E. Remote | RM-1 transport seam + `hutan proxy` + SSH hosts, RM-2 hosts UI, RM-3 TLS pairing | protocol minor (host identity), ADR "remote transports" |
| F. Personal assistant | PA-1 assistant workspace + Home page, PA-2 memory + skills services/MCP, PA-3 secrets vault, PA-4 traces, PA-5 schedules, PA-6 operator mode | protocol minors, ADRs "vault", "traces" |
| G. Icons | IC-1 chosen set + own glyphs | none |

## 11. Decisions for the user

- **D-1** Default organisation: task workspaces + unassigned sessions (projects become a badge/filter)?
- **D-2** Default `app.on_close`: Quit (stop + restore next launch) / Hide to menu bar keeping sessions (Orca) / Quit UI keeping daemon?
- **D-3** Remote order: SSH → TLS → relay?
- **D-4** Platform order: Linux headless → Linux desktop → Windows?
- **D-5** Icon base set: Phosphor / Lucide / Tabler / Remix (+ own brand glyphs)?
- **D-6** Secrets vault: OS key store wrap only, or OS store + optional passphrase?
- **D-7** Assistant default mode: Advisor (read-only) or Operator (acts with approvals)?

## 12. Research results (2026-10-18 night) and corrections

Detailed reports: `2026-10-18-task-workspaces.md`, `2026-10-18-lifecycle.md`, `2026-10-18-remote.md`, `2026-10-18-cross-platform.md`, `2026-10-18-vault-traces.md`, `2026-10-18-meta-harness-assistant.md`, `2026-10-18-perf-lag.md` (when done).

| Area | Result | Correction to this proposal |
|---|---|---|
| Task workspaces (D-1) | Recommended: Home row = internal unassigned container, reuse `kind = workspace` (no `task` variant), membership independent of project and checkout; MCP-started agents inherit the **caller's daemon-side membership**, never the desktop's selection; right panels follow an explicit scope (workspace / session / pinned All). | `session.assign` alone is a minor, but the full cutover changes meanings of `workspace.assign`, `add_root`, `create(project_ids)`, the `workspace` history scope and launcher defaults → **protocol major** (or a long dual-meaning period). `WorkspaceKind` is a closed serde enum: no new variant without a capability-gated projection. |
| Lifecycle (D-2) | Headless daemon on every OS (ADR 0001 already said no UI in the daemon); desktop owns window + tray; `app.on_close = "quit"` for new installs, old background preferences kept; ADR draft in the report. | Close wording must be honest: "Stop local sessions and quit. Next launch restarts shells in their folders and reconnects supported agents. Running commands do not continue." |
| Remote (D-3) | SSH → TLS → relay confirmed; use the **system OpenSSH** binary first (ProxyJump/agent/known_hosts for free), not a Rust SSH crate; transport adapters must not re-encode frames; one `Connection` per host. | Paths of attachments: upload laptop files instead of sending path strings; disconnect ≠ shutdown (no socket-file check for remote); the socket watchdog needs a multi-listener rule before TLS. |
| Cross-platform (D-4) | Measured: all 11 core/service crates pass `cargo check` for **Linux aarch64** (C deps checked with Zig); Windows: only core/protocol pass, **48 diagnostics** (terminal 27, cli 12, client 5, settings 2, agents 2) + C SDK headers. Docker (OrbStack) not running, so no native Linux run yet. | Linux is runtime/integration/packaging work, not a port; start Linux CI now. Windows is a real port (ConPTY, named pipes). |
| Vault (D-6) | Daemon-owned vault: random root key, per-item keys, XChaCha20-Poly1305; root key wrapped by the OS store; passphrase via Argon2id; `secret://name` resolved only at egress (process env, provider header, registered connector args); **no general get-secret MCP tool**; `keyring` 4.x with explicit per-OS store crates (Windows: force Local persistence). | A passphrase as a *second independent wrapper* gives OS-store **OR** passphrase (recovery), not extra protection: offer a distinct **OS store AND passphrase** mode; passphrase-only for headless hosts; never fall back to plaintext. |
| Traces | Local bounded metadata spans with OTel ids and GenAI conventions (now in `open-telemetry/semantic-conventions-genai`, Development — pin a revision); transcript content stays in the journal, traces reference it; OTLP export opt-in. | — |
| Assistant (D-7) | Advisor by default, visible switch to "Operator — asks before acting"; the assistant is a daemon-owned workspace; harnesses keep native loops; direct providers "Chat only" until a gateway tool loop exists. | "Stored locally" ≠ "never sent to a model": memory chosen for a turn goes to the backend — disclose it. ACP has no universal system-prompt API (per-harness injection). Today's agent-origin marker is **not a security boundary**: unattended schedules need a restricted authenticated control endpoint + OS/harness sandbox, and Advisor needs daemon-side enforcement (native read-only flags do not stop a mutating MCP call). |
