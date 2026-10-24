# Development Process

How Hutan is built so that any person or agent can stop at any point and anyone can resume from the repository alone.

## Why this process (analysis)

Project facts that drive the choice:

- Mostly agent-driven development, many short sessions, frequent context loss → **state must live in the repo**, not in chat history or an external tracker.
- Several high-uncertainty dependencies (GPUI sourcing, terminal engine, agent protocols, macOS bundling) → **spike before commit**; record the outcome as an ADR.
- Hard architectural boundary (daemon ↔ protocol ↔ clients) → **contract-first**: protocol changes land first, then daemon and clients proceed in parallel.
- Small team, one platform → no heavyweight ceremony; milestones with demoable exit criteria.

| Option | Pros | Cons | Decision |
|---|---|---|---|
| External tracker (GitHub Issues/Linear) as source of truth | Rich UI, notifications | Agents need network/auth; state splits between tracker and repo | Later, mirrored from `roadmap.md` if a team forms |
| Big upfront design | Fewer reworks on paper | High uncertainty makes it fiction; slows first slice | Rejected |
| **In-repo markdown: roadmap + status + ADRs, vertical slices** | Diff-reviewable, works offline, any agent can resume | Needs discipline to update | **Chosen** |

## Work hierarchy

```mermaid
flowchart LR
  V[vision.md<br/>target scope] --> M[Milestone<br/>demoable exit criteria]
  M --> S[Slice<br/>vertical, crate-bounded<br/>≤ 1–2 agent-days]
  S --> T[Tasks<br/>inside one session]
  ADR[ADR<br/>hard-to-reverse decision] -.gates.-> S
  SP[Spike<br/>throwaway evidence] -.feeds.-> ADR
```

- **Milestone** (`docs/roadmap.md`): user-visible capability with acceptance checks. Done only when its demo script passes.
- **Slice**: smallest change that delivers working behavior end-to-end through the layers it touches. Has an ID (`M2.3`), owner, status checkbox in the roadmap.
- **ADR** (`docs/adr/NNNN-*.md`): any decision that is expensive to reverse — dependency choice, protocol shape, persistence format, state ownership, security policy. Status `proposed → accepted → superseded by NNNN`.
- **Spike**: time-boxed throwaway experiment outside the workspace (`/tmp/hutan-spikes/<name>`), producing a `REPORT.md`. Evidence is summarized into the ADR; spike code is never merged.

## Session protocol (humans and agents)

### Start
1. Read `AGENTS.md`, then `docs/status.md` (current milestone, active slice, next action, blockers).
2. Read the milestone section in `docs/roadmap.md` and the ADRs it references.
3. Pick the active slice or the next unchecked slice whose dependencies are met. Write `Active slice` + short plan into `docs/status.md`.

### During
- One slice at a time per worktree. Touching another crate's public API → protocol/contract change first (see below).
- New hard-to-reverse decision → stop, write ADR `proposed`, spike if evidence is missing.
- Keep `docs/architecture.md` / `docs/protocol.md` truthful in the same commit as the change.

### End (mandatory, even when interrupted)
1. Run the quality gate for touched crates.
2. Update `docs/status.md`: what changed, verification run, exact **Next action** (a command or file + function, not a vague goal), open questions.
3. Tick slice checkboxes in `docs/roadmap.md`.
4. Commit with a conventional message (`feat(terminal): …`, `docs(adr): …`).

## Contract-first changes

```mermaid
sequenceDiagram
  participant P as protocol crate
  participant D as daemon
  participant C as clients (desktop, cli)
  P->>P: add types + version/capability + roundtrip tests
  par
    P->>D: implement handler + daemon integration test
  and
    P->>C: implement client against fake server
  end
  D->>C: end-to-end test over real socket
```

Rules: additive changes bump the minor protocol version and are capability-gated; breaking changes bump major and require an ADR. Protocol changes are serialized (one owner at a time); everything else may run in parallel.

## Parallel agent work

- Split by crate boundary (`terminal`, `agents`, `gateway`, `storage`, `desktop` views) — they share only `core`/`protocol` types.
- Each parallel slice runs in its own git worktree/branch: `../hutan-wt/<slice>` on `slice/<slice>`.
- One integration owner merges, resolves conflicts, runs the full gate.
- Implementers get self-contained briefs (`docs/briefs/<slice>.md`): slice ID, files/crates in scope, contract, acceptance checks. They must not edit `protocol` unless they own the contract change.

## Implementer agents

Roles: the **orchestrator** (main omp session) owns contracts (`core`, `protocol`), writes briefs, reviews, integrates, and updates `status.md`/`roadmap.md`. **Implementers** are omp subagents of the project agent `implementer` (`.omp/agents/implementer.md`, model `anthropic/claude-sonnet-5-5:high`), one per slice, each confined to the checkout its task names.

Two working modes:

| Mode | When | Where |
|---|---|---|
| **Worktree** (default) | Parallel slices, or the orchestrator keeps working in the main checkout meanwhile | `../hutan-wt/<slice>` on `slice/<slice>` |
| **Main checkout** | One slice at a time, nothing else editing the repo (small fixes, debugging, work that needs the user's live environment) | the main checkout on `slice/<slice>` (task says so explicitly); no worktree |

Humans and the orchestrator can always work directly in the main checkout; only concurrent agents need worktrees.

Loop per slice:
1. Commit the brief (`docs/briefs/<slice>.md`, template in `docs/briefs/README.md`) on `master`.
2. Worktree mode: `git worktree add ../hutan-wt/<slice> -b slice/<slice>`; main-checkout mode: `git switch -c slice/<slice>` in the main checkout (working tree clean). Dispatch a `task` with `agent: "implementer"` naming the checkout path, the mode, and the brief. Independent slices go in one batch (worktree mode) so they run concurrently.
3. Review the diff against the brief's contract and `AGENTS.md` invariants; run `just check` in the slice checkout; exercise acceptance steps yourself.
4. Send fixes to the same agent (`write agent://<id>`) or fix directly; then rebase onto `master`, merge fast-forward, remove the worktree (worktree mode) or `git switch master` (main-checkout mode), delete the branch.
5. Record the slice in `status.md` (what landed, verification run, deviations).

Rules: implementers never edit `status.md`/`roadmap.md`; a needed contract change comes back as a report item, not an edit. Dependent slices start after their dependency merges. Changing the implementer model = edit `.omp/agents/implementer.md` (one place).

Resources (dev machine: M1 Pro, 10 cores, 16 GB RAM, shared with the user's other projects and agents): builds use 4 jobs (`.cargo/config.toml`), tests 4 threads (`.config/nextest.toml`); debug builds carry line tables only (`Cargo.toml` `[profile.dev]`); `just check` runs at `nice 10`, and on this machine every rustc goes through a nice-10 sccache wrapper (`~/.cargo/config.toml`, machine-local). Scheduling:
- **One heavy job at a time** (a cargo build/clippy/nextest run, by the orchestrator or one implementer); research, review and UI-check agents are light and may run beside it. `just check` takes a machine-wide lock (`~/.cache/hutan-gate.lock`, `lockf`), so parallel implementers may code and run targeted `cargo nextest run -p <crate>` side by side while their full gates queue. Before starting a heavy job outside `just check`, `uptime`: if the 1-minute load is above 20 (2× cores), wait.
- Coding agents (implementers in worktrees): **two** by default, **three** when they touch disjoint crates and `memory_pressure` reports ≥ 40 % free (the gate lock still runs one full gate at a time). Light agents: at most ~8 concurrent research subagents; cheap web/GitHub research goes to `researcher-haiku`.
- Model routing (user, 2026-10-22, replaces 2026-10-21): **every implementation slice goes to `implementer` (Sonnet 5.5)**, including protocol/GPUI/concurrency work; **Opus 5.5 only coordinates** (the orchestrator: briefs, reviews, gates, merges) — `implementer-opus` is not used for slices. `architect-fable` (Fable 5.1) only for the hardest open design questions; `researcher-haiku` for cheap evidence gathering; `usertester` (Sonnet 5.5) for hands-on UI checks with Cua Driver. Because Sonnet slices stop at the request budget more often, briefs are split smaller and every brief lists its items in commit order.
- Before dispatching, check `df -h ~` and `just sweep`; after merging, `git worktree remove` the slice (its `target/` goes with it). A running desktop/daemon from a UI check is stopped when the check ends.
- Every test run uses a temporary `HUTAN_HOME`; after a run, `pgrep -x hutand` must find no test daemon. `just test` runs the suite under `xtask watch-daemons`, which fails the gate when a `hutand` of the checkout's `target/` started during the run is still running and stops it. A test that runs `hutand` as a process stops it with `hutan_daemon::testing::DaemonReaper` (feature `test-support`; SIGTERM, then SIGKILL, also when the test fails), declared after the home's temporary directory.
- **The user runs Hutan on this machine** (`/Applications/Hutan.app`, real data in `~/Library/Application Support/Hutan` and `~/Hutan`, installed 2026-10-23 at the user's request). Never stop, signal or `pkill` by name (`hutand`, `hutan-desktop`, `Hutan`) — only processes you started, by pid or by an executable path under your worktree / scratch dir; never `open` a test bundle without `-n` and a scratch `HUTAN_HOME`; never quit the app through AppleScript or Cua; never `git init` or write in `~/Hutan`. Updating `/Applications/Hutan.app` is the orchestrator's job, on the user's request.

Integration-test layout: every crate with integration tests has exactly one test binary, `it` (`crates/<crate>/tests/it/main.rs`, one `mod <file>;` per test file, shared helpers in `tests/it/common/`), because each test binary links the crate's whole dependency graph; fixtures stay in `tests/fixtures`, `tests/corpus`, `tests/golden`. A new test file is a new `mod` line in `main.rs`. A test's full name is `<file>::<test>`: run one file with `cargo nextest run -p hutan-desktop -E 'test(/^harness_gpui::/)'`, one test with `cargo nextest run -p hutan-daemon -E 'test(=chat::some_test)'` (or `cargo test -p hutan-daemon --test it chat::some_test`). The one exception is `crates/daemon/tests/main_thread_loop.rs` (`harness = false`, its own binary).

## Branching and commits

- Trunk-based on `master`; short-lived `slice/<id>` branches; rebase before merge; `master` always passes the gate. Remote: `origin` = <https://github.com/robertt3kuk/hutan> (public). Push `master` after each merge + status update; slice branches stay local.
- Conventional commits; scope = crate name or `docs`, `adr`, `ci`.
- No commits of spike code, secrets, recorded transcripts with tokens.

## Quality gates

| Gate | Command | From |
|---|---|---|
| Format | `cargo fmt --all --check` | M0 |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | M0 |
| Tests | `cargo nextest run --workspace` (fallback `cargo test`) | M0 |
| Licenses/advisories | `cargo deny check` | M0 |
| Protocol compat | golden JSON fixtures per protocol version | M1 |
| Terminal conformance | recorded PTY transcript corpus replayed through engine, grid snapshots compared | M1 |
| Perf budgets | `hutan bench` scripts: output flood, RSS, keystroke latency | M2 |
| Milestone demo | `docs/demos/Mx.md` checklist executed and recorded in status | every milestone |

All gates wrap into one entry point (`just check`) created in M0.

## Testing strategy

- **core**: state machines (session lifecycle, approvals, tasks) with exhaustive transition tests.
- **protocol**: serde roundtrip + golden fixtures; old-client/new-daemon compatibility tests.
- **terminal**: replay recorded PTY transcripts (shell, vim, htop, OMP, Codex TUI) → compare grid snapshots; fuzz the input path.
- **daemon**: integration tests over a real Unix socket in a temp dir; fake agents are scripted stdio fixtures speaking Codex app-server / ACP, so tests are deterministic and free.
- **desktop**: GPUI `TestAppContext` for view/state logic; visual checks through the milestone demo, not pixel tests.
- Real model calls only in opt-in smoke scripts, never in the default gate.

## Definition of Ready (slice)
- Milestone and dependencies identified; required ADRs accepted.
- Contract (protocol messages/types) sketched.
- Acceptance check is observable (command, test, or demo step).

## Definition of Done (slice)
- Behavior works end-to-end through the real daemon/socket path it touches.
- Gate green; new behavior covered by a test that would catch a regression.
- Docs (architecture/protocol/ADR) updated; `status.md` and `roadmap.md` updated; committed.
- **Parity**: every removed or moved affordance (button, panel, shortcut, menu item, setting, layout state) is listed with its replacement, or kept for one release. A change that moves user layout state shows a one-time note and keeps *Reset Layout*. Six comparable tools lost users this way (`docs/spikes/2026-10-21-ux-plan.md` § UX-PARITY-RULE).
- **UI changes**: checked in a real window (see § UI checks), with screenshots referenced in the report.

## UI checks (Cua Driver)

Real-window checks use Cua Driver (`cua-driver call <tool> '<json>'`, profile `.omp/agents/usertester.md`) against a debug or bundled build on a scratch `HUTAN_HOME`, never the user's data. Rules learned the hard way (`docs/spikes/2026-10-21-ux-audit.md` §4, §6):
- GPUI stops presenting frames while the window is **fully covered** and repaints when shown; this is not a bug. Keep the Hutan window frontmost (`bring_to_front`) for pixel checks, wait ≥ 0.7 s after a change, and avoid foreground-mode actions that restore another app on top of Hutan.
- Clicks: prefer `delivery_mode: "foreground"` with Hutan already frontmost; background clicks are unreliable (GPUI hit-tests from the last hovered position). Background keys work.
- GPUI content exposes no accessibility tree (only window chrome and menu bar): target pixels from the latest screenshot and confirm each action with a new screenshot. Hover and tooltips cannot be checked.
- Real agents in UI checks (user, 2026-10-23): chats with a real model run only through the **real OMP harness inside Hutan on a Haiku model** (`anthropic/claude-haiku-5-5`), chosen in the model menu before the first send; small prompts, scratch repos only; never another model. Every such check also confirms that the models Hutan lists for OMP (Start sheet, Switch model, chat header) match `omp models` — ids, display names, default (`hutan chat models --agent omp` gives the daemon's view). The automated gate still never calls a real model.
- Disable the history index in the scratch home so the user's real transcripts never appear in screenshots; answer any macOS permission prompt the driver triggers with Deny and report it.

## Milestone cadence
1. **Kickoff**: refresh risk register, list slices with IDs, confirm ADRs.
2. **Execution**: slices; status updated every session.
3. **Exit**: run demo checklist, record results + measurements in `status.md`, short retro (what to change in this process), then open next milestone.

## Document map

| File | Purpose | Update when |
|---|---|---|
| `AGENTS.md` | Entry point, invariants, commands | Conventions change |
| `docs/vision.md` | Target scope, principles, budgets | Scope decision (with ADR) |
| `docs/architecture.md` | Components, crates, data flows | Any structural change |
| `docs/protocol.md` | IPC framing, handshake, message catalog | Any protocol change |
| `docs/roadmap.md` | Milestones, slices, risks | Every session |
| `docs/status.md` | Handoff log, next action | Every session |
| `docs/adr/` | Decisions with evidence | Each hard-to-reverse decision |
| `docs/demos/` | Milestone demo checklists | Milestone kickoff |
