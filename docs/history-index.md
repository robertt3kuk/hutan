# Session history index (roadmap M17)

A local, opt-in full-text index over what you did and what your agents did: Hutan terminal blocks and agent state changes, and the transcripts of Claude Code, Codex and OMP. `hutan search "<query>"` finds the session, `--resume N` opens it again.

Code: `crates/index` (`hutan-index`: adapters, scanner, store, search, redaction, indexer thread), `crates/daemon/src/history.rs` (lifecycle, Hutan feed, requests), `crates/protocol/src/history.rs` (DTOs), `crates/cli/src/commands/history.rs`. Wire: `docs/protocol.md` § Session history search.

## Privacy

- **Opt-in.** `[history] enabled = false` is the default. While disabled, no transcript directory is opened, listed or watched, no Hutan record is tapped, and `index.sqlite` is not created. Enabling is `hutan index enable`, **Enable indexing** in the History panel or the On/Off in Settings → History (the daemon records it in `daemon.toml`), or editing the file; the panel's **Disable** and the same switch turn it off again.
- **Local only.** The index is `$HUTAN_HOME/index.sqlite` (default `~/Library/Application Support/Hutan/`), mode `0600` in a `0700` directory, its own database so that **Clear is a file delete** (`hutan index clear`). Nothing is sent anywhere.
- **Redaction (best effort).** Every text goes through `hutan_index::redact` before it is chunked and stored: `sk-…`, `ghp_…`/`github_pat_…`, `AKIA…`/`ASIA…`, JWTs, Slack `xox…`, Google `AIza…` keys, PEM `PRIVATE KEY` blocks (to the end marker or the end of the text), `Bearer …` and `Authorization:` credentials, `scheme://user:password@host` passwords, and `name=value` / `"name": "value"` pairs whose name contains `token`, `secret`, `password`, `passwd`, `credential`, `api_key`, `private_key`, `access_key` (the daemon's own env-secret name list; counts like `max_tokens` are left alone). Matches become `[REDACTED]`. A secret in an unusual shape is indexed as written: do not treat the index as a vault, and clear it if in doubt.
- Originals are never copied: the index holds redacted chunks only; the transcripts stay where the agents wrote them.
- Real transcripts are never part of this repository: every fixture is synthetic, written by hand in the observed shapes.

## Settings (`daemon.toml`)

```toml
[history]
enabled = false      # opt-in
range_days = 0       # 0 = everything, 30, or 90: older files are not read, older rows are pruned at start
hutan = true        # per-source switches (used when enabled)
claude = true
codex = true
omp = true

[history.roots]      # defaults: $CLAUDE_CONFIG_DIR/projects, $CODEX_HOME/sessions, $PI_CODING_AGENT_DIR/sessions,
claude = "/path"     # else ~/.claude/projects, ~/.codex/sessions, ~/.omp/agent/sessions
```

Any other `range_days` is a config error (the daemon does not start). `hutan index enable|disable` rewrites only `[history] enabled`, keeping comments and other keys.

## Sources

Each adapter is versioned (`Adapter::format_version`, all 1 today); a bump re-indexes that source's files from scratch the next time they are looked at. Adapters are pure functions from a JSONL line to rows; the scanner owns files and offsets.

| Source | Where | Rows | Session id → resume |
|---|---|---|---|
| `hutan` | the daemon (no files) | `command` (the command line of a finished terminal block), `output` (its output, first 16 KiB), `tool` (agent state changes from `agent.report`: `claude working: …`) | Hutan session id; none |
| `claude` | `<root>/<cwd-slug>/<session>.jsonl` | `user` / `assistant` text parts, `tool` for `tool_use` (name + `command`/`path`/… arguments) and `tool_result`; `ai-title` → title; `cwd` from the lines; `thinking`, `isMeta`, bookkeeping lines skipped | file name; a file below a session directory (`<session>/subagents/…`) takes that session's id; `claude --resume <id>` |
| `codex` | `<root>/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl` | `response_item` messages (user/assistant; injected `<environment_context>`-style user messages and `developer` skipped), `function_call` / `custom_tool_call` and their outputs as `tool`; `session_meta` → id, cwd; `turn_context` → cwd; `event_msg` ignored (it repeats messages). `~/.codex/*.sqlite` is not read | `session_meta.id`; `codex resume <id>` |
| `omp` | `<root>/<cwd-slug>/<ts>_<id>.jsonl`, sub-agents in `<ts>_<id>/…jsonl` | `message` lines: user, assistant text, `toolCall` (name + arguments), `toolResult` as `tool`; first line `title` (fixed width, **rewritten in place**: the scanner reads it again whenever the file changes) and `title_change` → title; `session` → id, cwd | `session.id`, a sub-agent keeps its parent's id; `omp --resume=<id>` |
| `opencode` | `~/.local/share/opencode/opencode.db` | — | **deferred**: the installed OpenCode (2.0.14) keeps the legacy `session`/`message`/`part` tables (empty) beside a newer `session_v2`/`session_message` layout with typed event rows, i.e. two live schemas in flux; a read-only adapter belongs after the layout settles |

A title falls back to the first line (80 characters) of the first user message. Rows without a timestamp take the previous row's, else the file's mtime.

`resume_argv` is built from `(source, session id)`, then validated with the `agent.report` rules (`agent::validate_resume_argv`: plain command name, ≤ 64 words, no apostrophes or control characters); a hit that fails is returned without one.

## The index

```text
sources   path, source, size, mtime_ns, offset, tail_hash, format_version, adapter state (JSON), rows, errors, last_indexed_ms
sessions  one per transcript file or Hutan session: source, key, title, cwd
entries   one per chunk: session, role, ts_ms, ext_key (Hutan records), terminal_id + block_id (Hutan block rows, schema 2)
fts       FTS5(body, idents; tokenize = 'unicode61 remove_diacritics 2'), rowid = entries.id
meta      small facts
```

- **One row per chunk.** Texts are redacted, then split into pieces of at most 4 KiB at line breaks (or character boundaries inside an overlong line). User and assistant messages keep at most 16 chunks, tool calls/results and Hutan output 4, and a session keeps at most 2 000 tool rows. Per-row metadata lives in `entries`/`sessions` (not as UNINDEXED FTS columns) so that deleting a file's rows is an indexed lookup, not a table scan.
- **Identifiers get their own column.** `idents` holds paths (`crates/daemon/src/lib.rs`), flags (`--locked`, `-p`), Rust paths (`crate::paths`), symbols (`parse_line`, `FooBar`, `run()`) and backticked spans of the chunk; for a `command` row, the whole command line. The tokenizer splits on punctuation, so `cargo test -p daemon` is the phrase-free word set `cargo`, `test`, `p`, `daemon` in both columns.
- **Ranking.** `bm25(fts, 1.0, 2.0)` (identifier column counts double) multiplied by a role weight in SQL: command 1.4, user 1.3, assistant 1.0, tool 0.7, output 0.6. Then one hit per session: the best row, with the number of matching rows (`ROW_NUMBER`/`COUNT` window over the session), best session first; the snippet comes from FTS5 `snippet()` with `«»` around matches.
- **Query language.** Words and `"quoted phrases"`, all must match; no operators (everything is quoted before it reaches FTS5), so punctuation can never raise a syntax error. Filters: sources, roles, project (cwd prefix), `since_ms`/`until_ms`.
- **Where a hit comes from** (schema 2, protocol 0.15). A hit of a transcript source carries the absolute `path` of the file (`sources.path` of its session). A `hutan` command or output row stores the `terminal_id` and `block_id` of the block it was indexed from (`HutanRecord.block`), and its hit carries them. `terminal_id`s belong to one daemon run, `block_id`s to one terminal: a client compares the hit's terminal with the session's current one before it jumps to the block. Agent-report and chat rows have none, and neither have Hutan rows written before schema 2.
- The database is a cache: a schema version (`PRAGMA user_version`) that is neither 1 nor the build's (2) drops it and starts empty. **Schema 1 → 2 is upgraded in place**: `terminal_id` and `block_id` are added to `entries`, the rows of the transcript files and their `sources` rows are dropped so that the first stat pass finds every file new and reads it again (their hits then carry `path`), and the Hutan rows stay (nothing can derive them again; they have no block). A crash in the middle leaves schema 1 and the upgrade runs again.

## Incremental scanning

- **Startup: one `stat` pass.** The worker lists the source roots (`readdir` + `stat` only) and compares size, mtime and format version with the `sources` table; only differing files are opened. Unchanged files are never read. After that there are **no periodic scans and no timers**: the worker sleeps in `recv`.
- **Live: FSEvents.** `notify`'s macOS FSEvents backend watches the existing source roots recursively; events are debounced (200 ms quiet, 2 s at most) and the changed files are processed. A directory event walks that subtree. If the event queue overflows the worker repeats the `stat` pass (never a content scan). A root that does not exist when indexing starts is not watched (`hutan index disable` + `enable`, or a daemon restart, picks it up once it exists).
- **Append-only reads.** A changed file is read from the stored byte `offset`, whole lines only (an unfinished last line waits; a last line that is already valid JSON is taken). Before trusting the offset the scanner re-reads the up-to-64 bytes in front of it and compares their hash with `tail_hash`.
- **Rewritten or truncated → re-index.** A file shorter than its offset, or whose bytes in front of the offset changed, or whose adapter format version changed, loses its rows and is read again from the start (`Metrics::reindexed`).
- **Malformed lines** (not JSON, wrong shape, invalid UTF-8, longer than 4 MiB) are skipped and counted in `sources.errors` (shown by `hutan index status`).
- **Atomic progress.** Rows and the new offset are committed in the same transaction, in batches of ≤ 1 000 lines / 8 MiB; the stamp (size/mtime) is only stored by the last batch, so a crash resumes from the offset and never skips or repeats a line.
- **Bounded CPU.** One dedicated thread at QoS *utility* (`pthread_set_qos_class_self_np`; `nice 10` off macOS), batched transactions, `yield_now` between batches and files. A panic on one transcript is logged and skipped.
- **History range.** With `range_days` > 0 files whose mtime is older, and rows whose timestamp is older, are not indexed; old rows are pruned at start.
- **Hutan feed.** The event bus has a bounded tap (1 024 events, drop when full) for `session.updated` and `terminal.block`. A block that reaches `finished` becomes a `command` row and (if non-empty) an `output` row keyed `b:<block id>:<started_at_ms>`; an agent report that changes state or message becomes a `tool` row keyed `a:<updated_at_ms>`. Records with the same key replace each other; records from before indexing was enabled are not backfilled; unfinished blocks are indexed when they finish.
- **Chat feed (M16-A).** The tap also carries the journaled (not streamed) events of chat sessions: user and assistant `message_end` become `user`/`assistant` rows, `tool_start` and the output of `tool_end` become `tool` rows, keyed `c:<seq>`, with the session's title and cwd. A chat session's id is its `session_key`.

## Operations

| Command | Effect |
|---|---|
| `hutan index status` | enabled/disabled, per source files, rows, last indexed, unreadable lines; database size |
| `hutan index enable` / `disable` | start / stop indexing (and record it in `daemon.toml`); disabling keeps the file |
| `hutan index clear` | delete `index.sqlite` (also while disabled) and, if enabled, start an empty one. Files are indexed again when they change, or on `rebuild`; Hutan rows are gone for good |
| `hutan index rebuild` | forget everything read from transcript files and read all of them again in the background; Hutan rows are kept (they cannot be derived again) |
| `hutan search "<query>" [--source …] [--since 7d] [--limit N] [--json]` | one hit per session: source, time, title, cwd, role, snippet, resume command |
| `hutan search … --resume N` | open hit N (1-based, as numbered by the same search) in a new terminal session in its cwd running its resume command (`session.create`); attaches when run from a terminal; a vanished cwd falls back to the current directory with a warning |

## Performance

`cargo run --release -p hutan-index --example perf -- --messages 100000` generates 100 000 synthetic Claude Code messages (1 000 files, 95.5 MiB; user and assistant text, tool calls and results of 0.2–3 KiB, Zipf-ish vocabulary, paths, flags, commands) in a temporary directory, indexes them with the real `Indexer` (QoS-utility thread + FSEvents watcher), searches, appends a line, and idles. Three runs on an Apple Silicon Mac under load (load average 6–8, other builds running), 2026-10-15:

| Measure | Result |
|---|---|
| Initial index of 100 000 messages | **3.6 – 7.8 s** wall (3.1 – 6.3 s CPU), 13 – 28 k messages/s; the spread is machine load, the fastest run is the quiet-machine figure |
| Rows / database size | 100 000 rows; **156.4 MiB** (164 % of the 95.5 MiB corpus: FTS5 keeps a copy of every text plus its postings, and the identifier column repeats paths and commands) |
| Bytes read | 95.5 MiB, each file opened once; 0 re-indexed |
| Search (8 queries, 20 sessions) | median **38 – 81 ms** over all queries (p95 70 – 131 ms); rare terms < 1 ms, phrases ~15 ms, queries on words that occur in most rows (`ka lo`) 65 – 90 ms because every matching row is scored |
| Search with a role filter | 23 – 31 ms |
| Append of one line | searchable after **237 – 368 ms** (200 ms debounce + FSEvents); 184 bytes read |
| **Idle CPU after indexing** | **0 ms over 10 s (0.000 % of a core)** in all three runs: no timers, no polling |

Unit-level numbers on real-format data: copies of 222 real transcript files (12 Claude Code, 60 Codex, 150 OMP; 43 700 rows, 283 MB read) in a temp directory indexed with 0 unreadable lines. The copies were deleted afterwards and nothing from them is in the repository.

## Known limits

- Redaction is a heuristic (see above). Transcripts the agents delete are not removed from the index until `clear`/`rebuild`.
- Rows are pruned to the history range only at daemon start / enable, not continuously.
- A source root that appears after indexing started is picked up on the next enable/restart.
- Hutan rows only cover blocks and reports seen while indexing was on; terminal output without shell integration (no OSC 133 marks) has no blocks.
- OpenCode, Cursor and Gemini adapters are not implemented (M17 roadmap lists them; OpenCode is deferred above).
- The desktop History panel and the ⌘K / ⌘J integration are in (`docs/design.md` § History panel); assistant `@search` is M16. Since 0.15 a hit names its origin: the transcript's `path` (the panel's **Reveal File**) and, for a Hutan block, `terminal_id` + `block_id` (**Open Block** jumps to it exactly). The id of a block is only meaningful while the daemon run that indexed it lives: after a daemon restart the session has another terminal and the panel says the block is gone.
- Chat journals (Codex, direct, assistant) are indexed as source `hutan` since M16-A (`docs/assistant.md` § Indexing of chats); `history.search` hits of Hutan sessions carry `session_id`. `IndexHandle::session_text` returns the rows of one session in order (used by the assistant's `agent_transcript` and `search` attachments); the assistant's `@search` is now the `search` attachment.
