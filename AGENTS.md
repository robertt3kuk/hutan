# Hutan — agent and contributor guide

macOS-first Rust agentic development environment: `hutand` (Tokio daemon, source of truth) + GPUI desktop + CLI over a versioned Unix-socket protocol.

## Resume work (do this first, every session)
1. Read `docs/status.md` → current milestone, active slice, **Next action**, blockers.
2. Read that milestone in `docs/roadmap.md` and the ADRs it references (`docs/adr/`).
3. Follow `docs/process.md` (session start/end protocol, Definition of Done).
4. Before stopping — even mid-slice — update `docs/status.md` with an exact next action.

## Document map
- `docs/vision.md` — target scope, principles, non-goals, performance budgets
- `docs/perf.md` — how budgets are measured (`just bench`), latest numbers, misses and fixes
- `docs/themes.md` — color and icon packs: where they live, `theme.toml` / `icon-theme.toml`, every token and semantic icon id, selection (`appearance.light_theme` / `dark_theme` / `icon_theme`, `terminal.theme`), the Settings pickers and their reversible preview, the local `hutan theme ls|validate|new|export`, overrides, hot reload, SVG security, limits, diagnostics
- `docs/architecture.md` — components, crates, data flows, state ownership
- `docs/protocol.md` — IPC framing, handshake, sequencing, message catalog (incl. § Files: the daemon's file service, `hutan fs`)
- `docs/roadmap.md` — milestones, slices (checkboxes), risk register
- `docs/shell-integration.md` — automatic OSC 133/OSC 7 injection for zsh, bash, fish
- `docs/history-index.md` — opt-in session history index and search, and the session catalog behind `hutan history sessions` (sources, formats, ranking, titles, scopes, privacy, performance)
- `docs/editor.md` — the code editor: what each E-slice added, file tabs (buffer store, reads through the daemon, syntax only while shown, large-file mode, drawing only the lines in view, find), settings, tests
- `docs/lsp.md` — language servers in the daemon: which servers are found (never fetched), `[lsp]` config, the root of a server, trust per folder (user-only), documents and diagnostics, `hutan lsp` / `hutan diagnostics`, MCP `diagnostics`, limits, troubleshooting (protocol 0.39)
- `docs/gateway.md` — direct model chat: providers (OpenAI-compatible, Anthropic, Ollama), Keychain credentials, direct chat sessions, `hutan provider` / `hutan chat` commands, tests
- `docs/assistant.md` — Mawas, the assistant: Home page, scopes, modes (Ask before acting / Advisor) and safeguards, memory (curated entries, proposals, what each backend is given), context attachments (shell, agent transcripts, files, search), redaction and budgets, permissions, `hutan ask`, `hutan memory`
- `docs/agents.md` — agent chat sessions: the Codex adapter, ACP harnesses (OMP, Claude Code) and accounts, safety presets (Ask · Contained · Read-only · Full access per harness, effective state read back), Contain (Hutan's Seatbelt boundary for terminals and OMP: what it holds back, fail closed, never nested; protocol 0.52), `AgentEvent`, approvals, journal, how to re-record the test transcripts
- `docs/orchestration.md` — automatic projects and user workspaces (`hutan project`, `hutan ws`), worktrees and session placement (`hutan wt`, `hutan new --worktree`, placement badges, shared-checkout warning) tasks with dependencies (`hutan task`), review and merge of worktrees (`hutan wt diff|merge`), worktree setup, teardown and cleanup (`.hutan/worktree.toml`, `hutan wt setup|prune`) and the Tasks panel / Changes view
- `docs/resources.md` — the resource manager: process trees per session with CPU/RSS, `hutan ps|top|kill|agent interrupt`, `session.kill`, `process.signal`, `agent.interrupt`, redaction, audit
- `docs/control.md` — the agent control surface: `hutan ui`, `hutan mcp`, who counts as an agent, the `[control]` policy and its approvals (protocol 0.21), Mawas's gate: grants, always-ask, turn limit, audit (protocol 0.31), who may touch Mawas's memory (protocol 0.32), and what an agent's file write asks (protocol 0.33)
- `docs/integrations.md` — agent reporting (state + resume command), `hutan agent`, `hutan integration` for Codex, Claude Code, OMP, what each does with an image pasted into its terminal, and § User hooks: `[[hooks]]` of `daemon.toml` (commands run on turn, approval, attention, session, task and finish events; payload per event, limits, log, ntfy/sound/journal examples, `hutan hooks`, Settings → Automation; protocol 0.48)
- `docs/packaging.md` — the signed app bundle (`just bundle`): layout, ad-hoc signing, how the executables find each other, native notifications, start at login, the daemon's own menu bar item (shown while no desktop is connected)
- `docs/process.md` — how we work, gates, testing strategy
- `docs/adr/` — decisions; `docs/spikes/` — evidence behind them

## Invariants (review blockers)
- Daemon owns processes, PTYs, terminal state, sessions, conversations, approvals, credentials, worktrees. Clients only render and send commands (ADR 0001).
- Dependencies point inward: UI crates (`desktop`, `terminal_view`) and `cli` → `client` → `protocol` → `core`. Nothing depends on `daemon`. Only UI crates link GPUI; `settings`, `client`, service crates never do (ADR 0003, ADR 0010).
- Every UI feature is a registered panel / pane item / status item with its own settings namespace; nothing in the layout is hard-wired (ADR 0010, `docs/design.md`).
- Permissions are enforced in the daemon; UI only collects decisions.
- Secrets only in Keychain via the daemon; never in SQLite, logs, events, or fixtures (ADR 0008).
- Subscription logins are used only through harnesses (Codex, Claude Code, OMP): Hutan lists accounts and starts the harness's own login, and never reads, stores or forwards their tokens (ADR 0015).
- Bounded everything: scrollback, event ring, per-client queues. No unbounded channels.
- Protocol changes: additive = minor bump + capability; breaking = major bump + ADR (ADR 0002).
- Hard-to-reverse decision → ADR first; missing evidence → spike outside the repo, report into `docs/spikes/`.

## Conventions
- Rust 1.98.0 (`rust-toolchain.toml`), edition 2024, packages named `hutan-<dir>` under `crates/`, `thiserror` in libraries, `anyhow` only in binaries, `tracing` for logs. New dependencies go into root `[workspace.dependencies]`.
- No `unwrap()`/`expect()` outside tests and proven invariants (comment why); clippy enforces it.
- Conventional commits, scope = crate or `docs`/`adr`.
- Quality gate: `just check` (fmt, clippy `-D warnings`, nextest, cargo-deny). Tools: `brew install just cargo-deny cargo-nextest`.
- Protocol wire changes: bump `PROTOCOL_VERSION`, then `just update-golden`; fixture diffs are reviewed like code.
- Real model/API calls never run in the default gate; use recorded fixtures.
- `CHANGELOG.md`: newest on top — new entries go under **Unreleased** at the top, released versions stay below in order, never edited or reordered. The integration owner adds the user-facing lines when merging a slice (implementers don't, to avoid conflicts); a release renames **Unreleased** to `x.y.z — date` and opens a new **Unreleased** above it.

## Parallel work
Anyone can work directly in the main checkout. Concurrent agents split by crate and get one worktree per slice (`../hutan-wt/<slice>` on `slice/<slice>`); a single agent may also run in the main checkout on a `slice/<slice>` branch when nothing else edits the repo (`process.md` § Implementer agents). Implementer = omp `implementer` agent (`.omp/agents/implementer.md`, Claude Sonnet 5.5); which model does what (orchestrator, scouts, research, UI checks, the rare Opus/Fable cases, providers not logged in) is the routing list in `process.md` § Implementer agents → Resources. The orchestrator (Claude Opus) only writes briefs, reviews, gates and merges; one integration owner merges into `master`; only the contract owner edits `core`/`protocol`.

If you are an implementer working from a brief (`docs/briefs/<slice>.md`): stay inside the brief's scope, do not edit `docs/status.md` or `docs/roadmap.md`, report contract problems instead of changing `core`/`protocol`, and finish with `just check` green and a commit on your slice branch.
