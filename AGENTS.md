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
- `docs/architecture.md` — components, crates, data flows, state ownership
- `docs/protocol.md` — IPC framing, handshake, sequencing, message catalog
- `docs/roadmap.md` — milestones, slices (checkboxes), risk register
- `docs/shell-integration.md` — automatic OSC 133/OSC 7 injection for zsh, bash, fish
- `docs/history-index.md` — opt-in session history index and search, and the session catalog behind `hutan history sessions` (sources, formats, ranking, titles, scopes, privacy, performance)
- `docs/gateway.md` — direct model chat: providers (OpenAI-compatible, Anthropic, Ollama), Keychain credentials, direct chat sessions, `hutan provider` / `hutan chat` commands, tests
- `docs/assistant.md` — Mawas, the assistant: Home page, scopes, modes (Ask before acting / Advisor) and safeguards, context attachments (shell, agent transcripts, files, search), redaction and budgets, permissions, `hutan ask`
- `docs/agents.md` — agent chat sessions: the Codex adapter, ACP harnesses (OMP, Claude Code) and accounts, `AgentEvent`, approvals, journal, how to re-record the test transcripts
- `docs/orchestration.md` — automatic projects and user workspaces (`hutan project`, `hutan ws`), worktrees and session placement (`hutan wt`, `hutan new --worktree`, placement badges, shared-checkout warning) tasks with dependencies (`hutan task`), review and merge of worktrees (`hutan wt diff|merge`) and the Tasks panel / Changes view
- `docs/resources.md` — the resource manager: process trees per session with CPU/RSS, `hutan ps|top|kill|agent interrupt`, `session.kill`, `process.signal`, `agent.interrupt`, redaction, audit
- `docs/control.md` — the agent control surface: `hutan ui`, `hutan mcp`, who counts as an agent, the `[control]` policy and its approvals (protocol 0.21), Mawas's gate: grants, always-ask, turn limit, audit (protocol 0.31)
- `docs/integrations.md` — agent reporting (state + resume command), `hutan agent`, `hutan integration` for Codex, Claude Code, OMP
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

## Parallel work
Anyone can work directly in the main checkout. Concurrent agents split by crate and get one worktree per slice (`../hutan-wt/<slice>` on `slice/<slice>`); a single agent may also run in the main checkout on a `slice/<slice>` branch when nothing else edits the repo (`process.md` § Implementer agents). Implementer = omp `implementer` agent (`.omp/agents/implementer.md`, Claude Sonnet 5.5); one integration owner merges into `master`; only the contract owner edits `core`/`protocol`.

If you are an implementer working from a brief (`docs/briefs/<slice>.md`): stay inside the brief's scope, do not edit `docs/status.md` or `docs/roadmap.md`, report contract problems instead of changing `core`/`protocol`, and finish with `just check` green and a commit on your slice branch.
