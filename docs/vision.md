# Hutan — Vision and Target Scope

Hutan is a lightweight, macOS-first, fully Rust **agentic development environment**: a real terminal emulator and an AI agent workspace in one native GPUI app, backed by an independent daemon that keeps everything running when the window is closed.

Inspirations and what we take from each:

| Project | What it is | What we take | What we avoid |
|---|---|---|---|
| [Orca](https://github.com/stablyai/orca) | Electron ADE: parallel agents in git worktrees, terminals, diffs, browser, CLI, remote servers | Worktree-per-task model, agent CLI that drives the app, unread/attention state, server/client split (`orca serve`) | Electron footprint |
| [Herdr](https://herdr.dev) | Rust/ratatui agent multiplexer with server + socket API | Server-owned panes, agent state `idle/working/blocked`, screen-detection manifests + integration reports, env injection into panes, native agent resume commands, live PTY handoff on upgrade | TUI-only presentation |
| [Heeler for herdr](https://heeler.bybee.dev/) | Native iOS console for herdr | Remote-client shape: a thin native client over a daemon API | — |
| Zed | GPUI editor with ACP agent panel | GPUI, ACP client model | Editor scope |

## Product principles

1. **Daemon is the source of truth.** UI crash/close never stops work. Every UI (GPUI, CLI, future remote/mobile) is a client of one protocol.
2. **Terminal-first.** Any TUI agent (OMP, Claude Code, Codex, OpenCode) and any shell must work unmodified in a real PTY. Structured chat is additive, never a replacement.
3. **Native and light.** No Electron, no webview for core UI. Budgets are enforced, not hoped for (see budgets below).
4. **Model-agnostic, policy-honest.** Many providers, local models, API keys, OAuth only where the provider officially supports it. Consumer subscriptions are used only through the vendor's own harness (e.g. Codex/Claude Code login), never re-used as API credentials.
5. **Permissions enforced in the daemon.** UI dialogs only collect decisions.
6. **Boring by default.** Proven crates, explicit state machines, one convention per concern.

## Target scope (end state)

### Workspace and terminals
- Workspaces → tabs → split panes; any pane hosts a terminal session, a harness chat, or a direct model chat.
- Full PTY terminal: truecolor, alt screen, mouse, bracketed paste, kitty keyboard, OSC 8/133, synchronized output, wide chars/emoji; bounded scrollback; search; selection/copy.
- Detach/reattach, multiple simultaneous clients, session restore after UI restart (live) and daemon restart (respawn in cwd + native agent resume).
- Daemon upgrade without killing PTYs (fd handoff), later.

### Agents and chat
- Harness chat: structured sessions over Codex app-server and ACP (Claude Code, OpenCode, OMP, Gemini, …) with streaming, tool calls, diffs, plans, permission requests.
- TUI agents in terminals get state detection (`idle/working/blocked/done`) via integrations/hooks first, screen-detection manifests as fallback.
- Direct model chat via our own model gateway: OpenAI-compatible, Anthropic, Gemini, local (Ollama/LM Studio/llama.cpp server); credentials in Keychain.

### Orchestration
- Per-task placement chosen by the user: **main checkout** (the repo as it is, e.g. one agent or quick fixes) or **git worktree** (isolated parallel work); multiple repositories per workspace, task list with dependencies, parallel agents and subagents, review/diff of agent changes, merge/cleanup. Hutan never forces a worktree, and it warns when two live agents write to the same checkout.
- Attention model: unread, blocked, completed rollups up to tab/workspace/dock badge.

### macOS integration
- Native notifications with actions (approve/deny/open) routed back to the daemon.
- Per-user launch agent (SMAppService) so agents run with the window closed; menu bar status; global shortcuts; Dock badge.

### Editor (ADR 0012)
- Code editor next to terminals and agent sessions: tree-sitter highlighting and structure, LSP on demand (diagnostics, completion, hover, go-to-definition, rename, format), Vim mode, one theme for chrome, terminal, and code.

### Terminal blocks
- Optional block view built on shell integration (OSC 133): command + output units with exit status and duration, jump between commands, copy/collapse a block, send a block to an agent as context. Toggle `terminal.blocks`.

### Projects and workspaces
- One workspace per project (folder or repo, several repos allowed): create from a folder/recent project, switch with ⌘O, sessions/agents/layout grouped per workspace, workspace-level settings overrides. Sort modes (manual drag like Orca, recent, attention, name), pins, project groups, filters; empty workspaces hidden by default. A permanent **Home** workspace bound to a configurable folder for notes, docs, and scratch sessions/chats.

### Assistant chat
- A quick assistant for questions without starting a session (global and per workspace), with persistent searchable history; in a workspace it can read neighbouring sessions (agent transcripts, terminal output/blocks, files, past-session search) only through explicitly attached or permitted context.

### Session history and search
- Opt-in local index of all sessions — Hutan terminals/blocks, agent sessions, assistant chats, and external agent histories (Claude Code, Codex, OMP, OpenCode, Cursor, Gemini) — with full-text search and one-click resume.

### Agent and subagent monitor
- Track every coding agent — structured (Codex app-server, ACP) and TUI agents in terminals (OMP, Claude Code, Codex, OpenCode) — with live state (working / blocked / done / idle) and their **subagents** as a tree (OMP task agents, Codex/ACP subagents): current activity, model, duration, tokens when available, jump to transcript/terminal.

### Embedded browser
- Browser items/panels inside the workbench (docs, localhost previews, PR pages) next to terminals; agents can open URLs into it.

### Visualizations
- Render rich content inline: images in terminals (kitty graphics / iTerm2 inline images), Markdown, Mermaid/SVG diagrams, charts, and agent-produced artifacts as workbench items and in chat.

### Extensibility
- Versioned IPC protocol with published schema; CLI with `--json` everywhere.
- ACP and MCP integration; provider adapters as crates behind traits.
- Plugins (manifest actions + event hooks), later remote clients (TLS over Tailscale/SSH), mobile.

## Non-goals (until explicitly re-scoped)
- Rich shell input editor (Warp-style) — dropped for now.
- ~~Embedded web browser~~ → re-scoped into the target (see Embedded browser).
- Windows/Linux UI. The daemon stays portable (Unix-first); the desktop is macOS.
- Hosting or reselling model access.
- Reusing subscription OAuth tokens outside their official clients.

## Budgets (enforced from M2 on)

| Metric | Budget |
|---|---|
| Idle desktop RSS, 1 window, 4 terminals | ≤ 150 MB |
| Idle daemon RSS, 10 sessions, 10k-line scrollback each | ≤ 120 MB |
| Idle daemon RSS, 10 sessions at 200 columns, 10k-line scrollback each | ≤ 150 MB (ADR 0013) |
| Idle CPU (daemon + desktop) | ≈ 0 %; no polling loops except the bounded safety rechecks of ADR 0027 |
| Keystroke → glyph on screen (local), p95 | Hutan path ≤ 2 ms; on screen ≤ one display frame + 2 ms (ADR 0011) |
| Sustained output `cat` 200 MB | UI stays interactive; daemon memory bounded |
| Cold start to first terminal prompt | ≤ 500 ms with daemon running |

Numbers are initial targets; adjust via ADR with measurements, never silently.
