# herdr workspace model and architecture (herdrdev/herdr, Apache-2.0)

Read-only scout report, 2026-10-18. Official sources only: stablyai/orca (onorca.dev, installed Orca's package.json), herdrdev/herdr (herdr.dev, Cargo.toml), NousResearch/hermes-agent (hermes-agent.nousresearch.com). Feeds docs/spikes/2026-10-18-direction.md.

## Summary

Herdr’s useful model is **session/server namespace → workspaces → tabs → terminal panes; agents are recognized occupants of terminals**, not a separate task runtime. Workspaces are explicitly recommended per **repo, task, or investigation**, but no task/ticket object enforces that. Crucially, **Herdr is not evidence for an “unassigned sessions” inbox**: its user-facing sessions are independent servers, and its normal panes already belong to workspace/tab topology. Its intentional background-server lifecycle also differs from Hutan’s desired close-to-quit behavior.

Paths below are relative to `/Users/workie/proj/herdr-src`. Documentation prefix **D/** means `docs/next/website/src/content/docs/`.

## 1. Data model, creation, movement, persistence

| Concern | Finding and source |
|---|---|
| Hierarchy | `Workspace` owns stable `id`, optional `custom_name`, identity cwd, Git/worktree metadata, `tabs`, `active_tab`, and monotonically allocated public pane/tab numbers: `src/workspace.rs:175-211`. Documentation explicitly recommends one workspace per repo/task/investigation and tabs for agents/logs/server/review: `D/concepts.mdx:8-24`. |
| Session meaning | A session is a **persistent server namespace**, not a terminal or agent conversation. Named sessions isolate panes, workspaces, sockets and runtime state; configuration remains global: `D/concepts.mdx:55-69`, `D/persistence-remote.mdx:18-32`. `SessionInfo` exposes name/default/running/connection_error/socket_path/session_dir, **not cwd**: `src/session.rs:10-29`. |
| Terminal versus agent | Pane state refers to an attached terminal; terminal identity, cwd, labels and agent metadata are terminal-owned: `src/pane/state.rs:5-9`. An agent can enter/exit a pane without creating/removing that pane; agent-start needs an existing available shell pane: `skills/herdr/SKILL.md:47-57,137-155`. |
| Creation | `workspace create` accepts optional cwd, label, source workspace, focus and launch environment. Explicit cwd wins; otherwise creation resolves the configured cwd policy, using the source/focused workspace context: `src/app/api/workspaces.rs:39-80`. New workspace creation initializes a terminal/tab; moving an existing pane can instead create the workspace without restarting it: `src/workspace.rs:237-277`. |
| Cwd policy | `new_cwd` defaults to `Follow`; alternatives are Home, Current, or a configured path. Applies to new panes, tabs **and workspaces**: `src/config/model.rs:219-239,258-260`. Focused-pane-follow behavior has an explicit regression test: `src/app/api/workspaces.rs:389-465`. |
| Naming | Custom workspace name overrides automatic identity; otherwise repo-root basename, falling back to cwd basename, `~` for HOME, or full path: `src/workspace.rs:179-188`, `src/workspace/git/discovery.rs:24-42,66-71`. Multiple task workspaces can therefore share a repo; task-specific labels are user-provided rather than inferred ticket identities. |
| Loose/unassigned | **No user-facing unassigned-session concept found** in model/docs or broad searches for unassigned/loose/unattached. “Unattached terminals” appear in cleanup paths, not an inbox. Session snapshots contain workspaces, whose tabs contain pane maps: `src/persist/snapshot.rs:15-29,52-112`. [INFERENCE] Hutan needs an explicit additional assignment model rather than copying a Herdr feature. |
| Moving | `pane.move` destinations: existing tab with split placement, new tab in a workspace, or new workspace: `src/api/schema/panes.rs:85-117`; handler: `src/app/api/panes.rs:947-1132`. Move preserves the terminal/process; moving the last pane can remove the emptied source workspace. Cross-workspace public pane ID changes, while inherited caller context retains an alias: `src/app/api/panes.rs:1140-1185,1246-1319`; user-facing contract: `skills/herdr/SKILL.md:65-73`. Do not confuse workspace reorder (`workspace.move`) with pane reassignment. |
| Persistence | Snapshot stores workspace IDs/names/cwd/worktree membership/public counters, tabs/layout/focus/zoom, and pane cwd/label/agent identity/native session reference/resume argv: `src/persist/snapshot.rs:52-129`. Restart restores **shape and new processes**, not original running processes. Live-shell cwd is preferred; missing cwd/start failure remains visible as a failed pane rather than silently changing directories: `D/session-state.mdx:30-38`. Screen-history replay is opt-in; native agent resume defaults on: `D/session-state.mdx:61-101`. |

## 2. UI and keyboard flows

- **Sidebar has workspace and agent sections**, with adjustable vertical split: `src/ui/sidebar.rs:30-75`. Agent entries gather across **all workspaces/tabs**, then apply agent-view filtering/sorting—not inherently only the selected workspace: `src/ui/sidebar.rs:78-103`.
- The agent section omits plain shells: a pane must have an agent name or effective recognized agent label: `src/workspace/aggregate.rs:20-46`. Thus this is **not a sidebar list of every session/terminal**.
- Defaults:
  - Agent row 1: state icon + machine + workspace + tab; row 2: agent.
  - Workspace row 1: state icon + workspace; row 2: branch + Git status.
  - `src/config/sidebar.rs:437-474`.
- **Cwd is not a built-in sidebar token**, nor shown by those defaults: token enums in `src/config/sidebar.rs:107-140`. Paths are available in **Goto**, whose selected destination shows location/path; search includes names, kinds, tabs, workspaces, branches and paths: `D/keyboard.mdx:65`.
- Workspace attention rolls up across its panes: **blocked > unseen idle (“done”) > working > seen idle > unknown**: `src/workspace/aggregate.rs:49-74`. Done is an attention/read state, not a distinct process lifecycle state.
- Each client tracks displayed completions independently; CLI/API uses server-seen state: `D/concepts.mdx:43-53`.
- Keyboard: prefix defaults Ctrl+B; `Shift+N` new workspace, `Shift+W` rename, `w` workspace navigation, `c` new tab, `v`/`-` split right/down, `h/j/k/l` focus, `g` Goto, `q` detach: `D/keyboard.mdx:14-65`. Goto supports blocked/working/idle/done filters and direct pane jumps.
- Docs establish clicking panes/tabs/workspaces/agents, split-border dragging, selection, and context menus: `D/concepts.mdx:26-35`. **Do not claim Herdr implements dragging loose sessions into workspaces** from this evidence; the concrete reassignment evidence is the pane-move API above.

## 3. Architecture, transport, remote operation, lifecycle

- **One Rust binary, terminal UI, not a desktop GUI**: `README.md:29-38`. Server owns terminals/process state; attached clients render UI. Detach or client/SSH loss leaves server and work running: `README.md:29-32`, `D/concepts.mdx:71-87`.
- **Multiple clients** supported. Each can select a different workspace/tab. Different viewed tabs size independently; on the same tab, the last focus/select/interaction controls pane sizing: `D/concepts.mdx:73-79`.
- Local IPC uses `interprocess::local_socket`:
  - Unix: filesystem local sockets.
  - Windows: namespaced named-pipe transport, with security descriptor and on-disk discovery marker.
  - `src/ipc.rs:10-11,35-51,94-121`.
- Public automation has CLI wrappers and raw socket request/response/event subscriptions with bundled JSON schema: `D/socket-api.mdx:6-34`. This is distinct from the private binary wire paths.
- **Version compatibility worth copying:** endpoint generation is deliberately independent of private same-install CLI/direct-terminal/handoff protocol. Generation 1 is a durable compatibility floor; new JSON fields must be optional/defaulted, enums tolerate unknown values, unknown named controls are ignored: `src/protocol/endpoint.rs:1-18`.
- Remote paths:
  1. SSH manually and run `herdr` there.
  2. Local thin client: `herdr --remote host` / `ssh://user@host:port`.
  3. Saved SSH machines: local + several remotes in one UI, with independent connections.
  - `D/persistence-remote.mdx:45-63`; `README.md:31`.
- Thin-client remote implementation is explicitly **SSH command stdio**, backed by a local bridge socket: `src/remote/attach.rs:1,53-97`. Remote server owns processes and sends terminal content/session state; local client draws sidebar/menus/theme: `D/persistence-remote.mdx:58-63`.
- No native TCP listener/client transport surfaced in broad `TcpListener`/`TcpStream`/`tcp://` searches. **SSH is the evidenced remote mechanism**, not raw TCP.
- Saved-machine automation uses `--machine label-or-id`; IDs/names are scoped to a server. Removing a profile disconnects but does not stop remote sessions; API forwarding requires an already-running compatible server and does not install/start/restart it: `skills/herdr/SKILL.md:99-119`.
- **Lifecycle is deliberately persistent**, unlike requested Hutan semantics: Ctrl+B q detaches; explicit `server stop` ends panes. Unix server ignores SIGHUP: `D/persistence-remote.mdx:10-16`. Full restart can recreate layout/resume conversations but original processes are gone: `D/session-state.mdx:8-15,30-34`.

## 4. Agent integration and what Hutan should copy

- Discovery combines **foreground process/job identity**, screen manifests, OSC title/progress and optional integrations: `src/detect/mod.rs:244-285,304-327`; `D/concepts.mdx:37-41`.
- Full lifecycle official hooks are authoritative while live; session-only/custom hook paths retain screen-based recovery/fallback. Process exit clears matching authority: `src/terminal/state.rs:5-8`.
- Polling/screen heuristics deliberately stabilize ambiguous working→idle transitions: 100-ms rechecks, three confirmations, capped at 700 ms; unchanged detection text can be reused: `src/pane/agent_detection.rs:5-13,29-77,106-145`. **Not a benchmark**, but relevant when comparing “responsiveness” with detection debounce.
- Agent hooks inherit `HERDR_ENV`, pane ID, owning binary path and socket path. They report:
  - `idle|working|blocked`, optional blocker message and monotonic sequence;
  - native conversation ID / resume argv;
  - release on exit.
  - `D/add-herdr-support.mdx:17-48,52-95`.
- Reports can use portable CLI or equivalent socket methods. Recommended integration behavior: background/short-timeout reporting, ignore failures, retain only newest queued state: `D/add-herdr-support.mdx:99-110`.
- Identity survives Ctrl+Z/backgrounding while the exact process remains alive, avoiding spurious agent disappearance: `src/pane/background_agent.rs:1-19,45-68`.

**Hutan design takeaways [RECOMMENDATION]:**
1. Copy workspace→tabs→panes with agent identity orthogonal to terminal placement; use task labels without requiring Git worktrees.
2. Add the requested **unassigned terminal/session pool explicitly**; Herdr does not supply that product model. Keep runtime identity stable when assignment changes, even if presentation IDs are workspace-qualified.
3. Copy aggregate attention and client-local seen/completion state; keep workspace-context panels distinct from an optional global agent inbox.
4. Copy layered detection with explicit reports preferred, readable fallback state, native resume references, and visible cwd/restore failures.
5. Copy durable endpoint compatibility and SSH stdio bridging; **do not copy indefinite daemon survival as the default** if Hutan promises closing its only window quits.

## 5. Platforms and code split

- Supported clients: **Linux, macOS, Windows**. Remote hosts: Linux/macOS x86_64 and aarch64; Windows x86_64; Windows ARM64 best effort through x64 emulation: `D/persistence-remote.mdx:76`.
- Platform boundary explicitly centralizes process/filesystem OS behavior: `src/platform/mod.rs:1-4`. Target-selected modules and exports: Linux/macOS/Windows plus fallback: `src/platform/mod.rs:564-585`; shared Unix helpers: `src/platform/mod.rs:224-227`.
- Separate OS implementations cover process discovery/cwd, launch behavior, power/shutdown, clipboard/notifications and SSH-related differences. Local socket adaptation lives centrally in `src/ipc.rs`, rather than leaking Unix assumptions into callers.
- Capability gates mark **live handoff and direct terminal attach Unix-only**: `src/platform/mod.rs:300-307`; direct attach is documented unavailable on native Windows: `D/persistence-remote.mdx:116-122`.
- Shared Rust dependencies include Ratatui-era terminal UI architecture, portable PTY and local IPC; terminal emulation is the local `crates/ghostty-vt` workspace member: `Cargo.toml:23-39`. This is a reusable cross-platform runtime architecture, **not a GPUI desktop implementation**.

Read-only investigation; no builds, tests, or mutations performed.