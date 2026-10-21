# Resource manager: processes, usage, stop / kill / interrupt

Roadmap M13 (slice M13-C, protocol 0.18, capability `resources`). An Activity-Monitor-like view of everything Hutan runs, and the ways to stop it: a whole session, one process inside it (a runaway build), an agent's turn, or a single subagent. Part of the control surface of ADR 0016: every action is a daemon request, so agents and the CLI reach it the same way (the policy for agent-originated mutations is M8-C's). The desktop's panel is described in `docs/design.md` § Resources panel (M13-D).

Wire: `docs/protocol.md` § Resources. Code: `crates/terminal/src/procs.rs`, `crates/daemon/src/resources.rs`, `crates/protocol/src/resources.rs`, `crates/cli/src/commands/resources.rs`.

## What is listed

- **Per live session, a process tree.** A terminal session: the PTY's child (the shell or the command) and every descendant. A chat session: the harness process (Codex `app-server`, later an ACP agent) and its descendants. A direct chat has no process and is not listed. A session that is not running is not listed.
- **Language servers** (protocol 0.39, `docs/lsp.md`): every running server is a `service` node of kind `lsp` — `ResourceSample.services`, one entry per server with the server's process and everything below it (rust-analyzer's proc-macro server, `cargo check`, tsserver's workers), the same per-process figures and the same bounds, counted in the 4096 total. They belong to no session: `hutan ps SESSION` leaves them out. `lsp.status` carries each tree's RSS as `rss_bytes` too. The daemon starts and stops them (`max_servers`, idle stop, `hutan lsp stop`); signalling one process of a server is not offered.
- **`hutand`** as one row of its own.
- **Per process:** pid, ppid, name, command line, state, CPU %, resident memory, threads, start time. macOS `libproc` (`proc_listchildpids`, `proc_pidinfo`) and `sysctl(KERN_PROCARGS2)`; other systems list nothing (Hutan is macOS-first). Only the user's own processes can be read; a process that cannot be read is skipped.
- **CPU %** is the delta of user + system CPU time between two readings of the same process (same pid *and* start time, so a reused pid has no history) over the wall time between them, in percent of one core: 100 is one busy core, a multi-threaded build shows more. It is absent when there is no earlier reading, and a window shorter than 200 ms repeats the previous figure instead of showing noise.
- **Command lines are redacted** before they leave the daemon: arguments that look like secrets (`--token=…`, `API_KEY=…`, bearer credentials, `user:password@` URLs, well-known token shapes) are replaced by `[REDACTED]` using the history index's redactor, and the value after a bare `--token` / `--password` / `--api-key` style flag is too. Redaction comes before the cut to 512 characters. A command line that cannot be read shows the process name. Best effort, like the index: a secret in an unusual shape is shown as written.
- **Agents.** Per session the unfinished nodes of the agent tree with their pid when known. A reporter says its pid with `hutan agent report --pid` (the OMP extension and Claude hooks can, where they run as a process); the Codex adapter sets the main agent's node to the app-server's pid and leaves subagent threads without one. A pid is shown (and the process marked) only when it is in the session's process tree: a reporter cannot point the view at somebody else's process. The pid is forgotten when the node finishes.
- **Bounds.** 512 processes per session, 4096 in all; beyond that `truncated`.

## Sampling only while watched

`resources.list` reads the process table once: with nobody subscribed it reads twice, 250 ms apart, so the CPU figures mean something. `resources.subscribe {interval_ms ≥ 1000, default 2000}` starts a sampler task and sends the connection `resources.updated` events; the sampler wakes at the shortest subscribed interval and ends, forgetting its baseline, when the last subscriber unsubscribes or disconnects. The first sample of a subscription has no CPU figures when nobody sampled before. Reads happen on the blocking pool, never on the registry actor. Updates are outside the replay ring (see the protocol doc): a client that missed one just waits for the next.

## Actions

All are mutations (M8-C classifies them; an agent that asks over MCP or the CLI goes through its approval policy) and all are audited.

| Request | Does |
|---|---|
| `session.stop` | exists: SIGHUP, SIGKILL after 3 s |
| `session.kill {session_id}` | SIGKILL for the session's process tree now, then the group; answers when the session ended (error after 10 s). The tree is taken first, leaves before parents, because a shell with job control puts its jobs in groups of their own that a plain group kill misses. Processes that daemonized out of the tree (reparented to launchd) are not reached. A chat session's harness and its descendants are killed the same way; a direct chat just ends |
| `process.signal {session_id, pid, signal}` | `term`, `kill`, `int`, `hup`, `stop`, `cont` to one process. The pid must be the session's own process or a descendant **in a fresh look at the process table**, else `forbidden`; never `hutand`, never pid 0 or 1, never a process outside Hutan's sessions. The window between the check and the signal is microseconds; a pid that left is `not_found` |
| `agent.interrupt {session_id, node_id?, force?}` | the main agent: a chat's running turn (Codex `turn/interrupt`, direct chat stop, ACP `session/cancel` when that adapter lands), or ^C typed into a terminal session (what a terminal agent cancels its turn on; not SIGINT, which would end a TUI). A subagent: a Codex thread → `turn/interrupt` for that thread; a node with a pid in the tree → SIGINT, and with `force` SIGTERM two seconds later if it is still there; anything else is `invalid_state` ("this harness cannot stop a single subagent …"), the main agent can still be interrupted. The adapter seam is `AgentAdapter::interrupt_thread`, which defaults to unsupported |

**Audit.** Every `session.stop`, `session.kill`, `process.signal` and `agent.interrupt` writes one `info` line to `hutand.log` with `target = hutan::audit`: `who` (client kind and connection), `action`, `what` (session, pid, signal, node), `result` (`ok`, or the error code and reason). Agent-originated mutations are additionally written to the approvals audit table by M8-C's policy layer.

## CLI

```text
hutan ps [SESSION] [--watch] [--interval MS] [--json]
hutan top [SESSION] [--interval MS]
hutan kill SESSION [--pid PID [--signal term|kill|int|hup|stop|cont]]
hutan agent interrupt SESSION [NODE] [--force]
hutan agent report … --pid PID
```

`SESSION` is an id, a unique id prefix, `t:<terminal id>` or an exact title, as everywhere. `ps` prints a block per session (tree by indentation: pid, CPU %, memory, state, command; processes of agents marked `◆ agent <label>`; agents without a process listed below), a block per language server (`lsp rust-analyzer /path`) and a `hutand` line; with `--watch` it subscribes and prints at every sample (clearing the screen on a terminal); `--json` prints the sample as JSON, one line per sample. `top` is `ps --watch` with the busiest session and, within each tree, the busiest sibling first. `kill SESSION` is `session.kill`; with `--pid` it is `process.signal` (default `term`). `agent interrupt` prints how it was delivered.

## Tests

`crates/daemon/tests/it/resources.rs` (real shells in real PTYs: three processes from `sleep 60 & sleep 60 & wait`, signalling one, refusals, `session.kill` with job control, sampling that stops with the last subscriber, agent nodes with pids, a pid node interrupted with and without `force`, a Codex subagent thread through the fixture `subagent-interrupt.jsonl` (hand-built in the shape of the recorded transcripts), the main agent of a chat and of a terminal), `crates/cli/tests/it/resources.rs`, unit tests next to the code (tree assembly, CPU delta, redaction, descendant verification, procargs parsing).
