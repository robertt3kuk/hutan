# Session history index (roadmap M17)

A local, opt-in full-text index over what you did and what your agents did: Hutan terminal blocks and agent state changes, and the transcripts of Claude Code, Codex and OMP. `hutan search "<query>"` finds the session, `--resume N` opens it again. The same reading also builds a **session catalog** (one row per session: title, first prompt, latest turns, counts, model, project) that `hutan history sessions` and the desktop's Agent Session History browse (§ The session catalog).

Code: `crates/index` (`hutan-index`: adapters, scanner, store, search, session summaries, redaction, indexer thread), `crates/daemon/src/history.rs` (lifecycle, Hutan feed, requests), `crates/daemon/src/history_catalog.rs` (the session catalog), `crates/protocol/src/history.rs` (DTOs), `crates/cli/src/commands/history.rs`. Wire: `docs/protocol.md` § Session history search and § Session catalog.

## Privacy

- **Opt-in.** `[history] enabled = false` is the default (the desktop's History panel and the setup wizard never pre-tick a source: you choose which of the three third-party stores to read, `~/.claude/projects`, `~/.codex/sessions`, `~/.omp/agent/sessions`, and Hutan's own records). While disabled, no transcript directory is opened, listed or watched, no Hutan record is tapped, and `index.sqlite` is not created. Enabling is `hutan index enable`, **Enable indexing** in the History panel or the On/Off in Settings → History (the daemon records it in `daemon.toml`), or editing the file; the panel's **Disable** and the same switch turn it off again.
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
sessions  one per transcript file or Hutan session: source, key, title, cwd, and the catalog summary (ptitle, first_prompt, latest, messages, model, branch, created_ms, updated_ms, modified_ms, subagent)
entries   one per chunk: session, role, ts_ms, ext_key (Hutan records), terminal_id + block_id (Hutan block rows, schema 2)
fts       FTS5(body, idents; tokenize = 'unicode61 remove_diacritics 2'), rowid = entries.id
provider_titles  titles an agent keeps outside its transcripts (Codex `session_index.jsonl`): source, key, title
meta      small facts
```

- **One row per chunk.** Texts are redacted, then split into pieces of at most 4 KiB at line breaks (or character boundaries inside an overlong line). User and assistant messages keep at most 16 chunks, tool calls/results and Hutan output 4, and a session keeps at most 2 000 tool rows. Per-row metadata lives in `entries`/`sessions` (not as UNINDEXED FTS columns) so that deleting a file's rows is an indexed lookup, not a table scan.
- **Identifiers get their own column.** `idents` holds paths (`crates/daemon/src/lib.rs`), flags (`--locked`, `-p`), Rust paths (`crate::paths`), symbols (`parse_line`, `FooBar`, `run()`) and backticked spans of the chunk; for a `command` row, the whole command line. The tokenizer splits on punctuation, so `cargo test -p daemon` is the phrase-free word set `cargo`, `test`, `p`, `daemon` in both columns.
- **Ranking.** `bm25(fts, 1.0, 2.0)` (identifier column counts double) multiplied by a role weight in SQL: command 1.4, user 1.3, assistant 1.0, tool 0.7, output 0.6. Then one hit per session: the best row, with the number of matching rows (`ROW_NUMBER`/`COUNT` window over the session), best session first; the snippet comes from FTS5 `snippet()` with `«»` around matches.
- **Query language.** Words and `"quoted phrases"`, all must match; no operators (everything is quoted before it reaches FTS5), so punctuation can never raise a syntax error. Filters: sources, roles, project (cwd prefix), `since_ms`/`until_ms`.
- **Where a hit comes from** (schema 2, protocol 0.15). A hit of a transcript source carries the absolute `path` of the file (`sources.path` of its session). A `hutan` command or output row stores the `terminal_id` and `block_id` of the block it was indexed from (`HutanRecord.block`), and its hit carries them. `terminal_id`s belong to one daemon run, `block_id`s to one terminal: a client compares the hit's terminal with the session's current one before it jumps to the block. Agent-report and chat rows have none, and neither have Hutan rows written before schema 2.
- The database is a cache: a schema version (`PRAGMA user_version`) that is neither 1, 2 nor the build's (3) drops it and starts empty. **Schemas 1 and 2 are upgraded in place**: (1 → 2, protocol 0.15) `terminal_id` and `block_id` are added to `entries`; (→ 3, protocol 0.29) the catalog summary columns and `provider_titles` are added to `sessions`; the rows of the transcript files and their `sources` rows are dropped so that the first stat pass finds every file new and reads it again (their hits then carry `path`, their sessions get a summary), and the Hutan rows stay (nothing can derive them again; they have no block, and their summaries are rebuilt from their rows). A crash in the middle leaves the old schema and the upgrade runs again.

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
| `hutan history sessions [--scope all\|workspace\|project] [--ws ID\|NAME] [--filter TEXT] [--sort updated\|created] [--source …] [--limit N] [--refresh] [--json]` | the session catalog as a table (agent, title, msgs, age, model, project) and a footer (`2 of 250 sessions · scanned 986 files 3 min ago`); text from transcripts has escape sequences and control characters stripped before it reaches the terminal (`--json` carries it as it is, escaped by JSON). `--ws` alone means `--scope workspace`; `--refresh` looks at the files again first |
| `hutan search … --resume N` | open hit N (1-based, as numbered by the same search) in a new terminal session in its cwd running its resume command (`session.create`); attaches when run from a terminal; a vanished cwd falls back to the current directory with a warning |

## The session catalog

A Hutan session that `session.close` removed keeps its row in the catalog: `HistorySessionRow.closed` is true (protocol 0.30), with the title, folder and project it had, so a client shows **Reopen** (`session.reopen`) instead of Open.

`history.sessions` (protocol 0.29, `docs/protocol.md` § Session catalog) answers "what sessions are there on this machine": one row per session of the enabled sources, summarised while the indexer reads, so listing a thousand sessions takes milliseconds and never opens a transcript.

**Fields.** Each row has the source and the agent's session key; `title`; `cwd`, `branch`, `model`; `messages`; `created_ms`/`updated_ms`; `first_prompt` (at most 2 000 characters); `latest` (the last 3 turns, oldest first: role, one line of at most 400 characters, time); the transcript `path`; `resume_argv`; for a Hutan session its `session_id`; and the project it belongs to (`workspace_id`, § Scopes). Every text goes through the redaction before it is kept (a prompt with a key in it shows `[REDACTED]`), and nothing but the first prompt and the last three turns is stored: the catalog is not a second copy of the transcript.

**How the summary is folded.** The summary lives in the adapter's `ParseState` (persisted in `sources.state`) and is copied into the `sessions` row at every batch commit. An appended file resumes from the stored byte offset and updates the summary from the new lines only; a rewritten or truncated file is dropped and folded again from the start; OMP's title line, rewritten in place, is read again and takes only the title from it. A text part of a message is not a message: the text parts of one line (one role) are one message; tool calls and results are not counted. Records longer than 4 MiB are skipped like for the index (`MAX_LINE_BYTES`, stricter than Orca's 10 MiB), at most 3 turns are in memory per session, and a message is looked at only for its first 64 KiB.

**What counts.** `messages` = user and assistant messages. For a Hutan terminal it counts the commands (terminals have no other conversation), for a Hutan chat the user and assistant messages; the latest turns of a terminal are its last commands (role `command`). Claude Code's injected turns (`<system-reminder>`, `<local-command-stdout>`, slash-command envelopes, interruption and compaction notices, the tags Orca lists as harness machinery) are not messages and never become the first prompt; a prompt that merely starts with some other `<tag>` is one. Sub-agent transcripts (Claude `<session>/subagents/…`, OMP `<ts>_<id>/…`, a Codex thread started by another thread, a session with `parentSession`) are not rows of their own. A session with neither a message nor a title the agent gave is not listed (`loaded` does not count it); a Hutan session needs at least one command or chat message.

**Title precedence** (as Orca): Claude Code — the title you gave (`custom-title`) > the generated one (`ai-title`, the latest) > the first line of the first prompt; Codex — the thread name in `<codex home>/session_index.jsonl` (written lazily by Codex; read when the file changes, on every scan and refresh) > `thread_name` in `session_meta` > the first prompt; OMP — the `title` / `title_change` records > the first prompt; Hutan — the session's title. The first-prompt fallback is its first line, 80 characters. Model: the last turn that names one (Claude and OMP assistant lines, `turn_context.model` for Codex, OMP `model_change`). Branch: Claude's `gitBranch`, Codex's `session_meta.git.branch`; nothing is invented through `git` (OMP records none).

**Scan state.** `files` per source = transcript files below the roots (inside the history range), looked at by the last stat pass (startup, `refresh`, an overflowed event queue) and, between passes, the number of files read (`sources` rows); `done` counts up during a pass; `finished_ms` is when the last pass or the last live read finished. `refresh: true` queues a rescan of changed files (the same stat pass as at startup: it opens only files whose size or mtime changed) and answers when it has run, waiting at most 10 s; a longer wait answers with `scanning: true` and the rescan still pending. Live changes arrive through FSEvents as before; a refresh is for the button.

**Scopes and attribution** (`crates/daemon/src/history_catalog.rs`). The index knows nothing of projects; the daemon maps each `cwd` to the project (or Home, or workspace) with the longest root containing it, a project beating Home beating a workspace on equal roots. A `cwd` that no root contains but that sits in a linked git worktree of a project's repository (found through the checkout's git common directory, as when a session is created) belongs to that project; so does everything below a symlinked root (both sides are resolved). `workspace` scope = inside a root of the workspace or one of its member projects (or a worktree of one of them); `project` scope = attributed to the id. A live Hutan session belongs to the workspace it runs in. Attribution is computed per request from the daemon's current workspaces (cached per distinct `cwd`), so it follows a project being added or moved without reindexing.

**Filter** is a case-insensitive substring match, every term required, over title, first prompt, latest turns, agent name, model, branch, `cwd` and session key; `repo:` matches the attributed project's name or its folder's name, `path:` the `cwd` or the transcript path. Quoted values (`repo:"my repo"`, `"two words"`). Over 2 KiB: no rows. This is Orca's legacy row filter; full-text search (`history.search`) stays the ranked, ungrouped mode for typed queries.

**Order and counts.** `updated` (the last message; else when the transcript was last read) or `created` (the first message; else the same fallbacks), newest first, ties by session key. Two files that claim one session id (a forked Codex rollout copies its parent's header) are one session, the most recently updated. `matched` = rows after scope, sources and filter, `sessions` = the first `limit` (default 250, at most 1 000), `loaded` = rows of the enabled sources before all of that, which is what "2 of 250 sessions" shows.

**Performance.** On the synthetic corpus of the next section (1 000 sessions) a catalog read takes about 2 ms (median of five, release build); the database grew from 156 to 162 MiB (the summaries). The daemon adds attribution and filtering over the rows in memory: bounded by the number of sessions, no file access except one `canonicalize` or git-worktree lookup per distinct `cwd` that no root contains.

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

The catalog read is measured by the same example (`catalog: 1000 sessions, median 2.1 ms`). Unit-level numbers on real-format data: copies of 222 real transcript files (12 Claude Code, 60 Codex, 150 OMP; 43 700 rows, 283 MB read) in a temp directory indexed with 0 unreadable lines. The copies were deleted afterwards and nothing from them is in the repository.

## Known limits

- Redaction is a heuristic (see above). Transcripts the agents delete are not removed from the index until `clear`/`rebuild`.
- Rows are pruned to the history range only at daemon start / enable, not continuously.
- A source root that appears after indexing started is picked up on the next enable/restart.
- Hutan rows only cover blocks and reports seen while indexing was on; terminal output without shell integration (no OSC 133 marks) has no blocks.
- OpenCode, Cursor and Gemini adapters are not implemented (M17 roadmap lists them; OpenCode is deferred above).
- The desktop History panel and the ⌘K / ⌘J integration are in (`docs/design.md` § Agent Session History panel, and § Desktop below); assistant `@search` is M16. Since 0.15 a hit names its origin: the transcript's `path` (the panel's **Reveal Log**) and, for a Hutan block, `terminal_id` + `block_id` (**Open Session** jumps to it exactly). The id of a block is only meaningful while the daemon run that indexed it lives: after a daemon restart the session has another terminal and the panel says the block is gone.
- The catalog's Hutan rows only cover sessions seen while indexing was on (like the index); the file count of a scan is the number of transcript files found by the last stat pass or read since, not a re-count of the directory after every live change; a Codex title written after the last scan or file change appears at the next one.
- Chat journals (Codex, direct, assistant) are indexed as source `hutan` since M16-A (`docs/assistant.md` § Indexing of chats); `history.search` hits of Hutan sessions carry `session_id`. `IndexHandle::session_text` returns the rows of one session in order (used by the assistant's `agent_transcript` and `search` attachments); the assistant's `@search` is now the `search` attachment.

## Desktop: the Agent Session History panel

The desktop's History panel (`docs/design.md` § Agent Session History panel) is a client of three requests and nothing else: `history.sessions` for the browsable catalog (protocol 0.29), `history.search` for ranked full-text results, and `history.status` / `history.set_enabled` / `daemon.settings.set` for the index state and the off-state card. It never reads a transcript itself.

- **Off**: no request but `history.status` (and `daemon.settings.get`, so that Enable writes only what differs). The panel is a short card (*Off until you choose sources.*) and a **Set up history…** button; the form behind it has the four sources **unticked** whatever `daemon.toml` has (consent is never pre-given), each with the folder it reads, and **Enable** stays off until one is ticked. **Enable** writes the sources (`history.hutan|claude|codex|omp`; all four while the daemon's values are unknown), then turns indexing on. The first-run setup has an optional History step with the same four unticked sources; leaving it with none ticked changes nothing.
- **Catalog**: one `history.sessions` per change of the scope (Workspace / Project / All, with the shown workspace or project as `workspace_id`), the filter text (live, every keystroke replaces the request in flight), the sort (*Last updated* / *Created*), the sources menu and *Show more* (`limit` 250, +250 per click, 1000 at most); then again every 30 s while the panel is visible, every 0.7 s while the answer says `scan.scanning`, and after `history.status` reports another row count. The refresh button sets `refresh: true`. Rows are grouped client-side (project = the daemon's `workspace_id` attribution, folder, agent); the count line is `matched` of `loaded`.
- **Full text**: a typed query that has no `repo:` / `path:` word also goes to `history.search` (250 ms after the last key, or on Return), with the `history` settings' roles and range, the sources menu and — for Workspace and Project — the first folder of the shown workspace or project. Its ranked sessions replace the catalog list until the query changes.
- **Resume**, **Open Log**, **Open Working Directory** and the rest are workbench actions (`ResumeHistoryHit`, `OpenHistoryFile`, `OpenHistoryDirectory`, `RevealHistoryFile`, `OpenHistoryHit`); the panel hands them the row's `resume_argv`, `cwd`, `path` and `session_id`. A daemon without `history.sessions` gets the full-text search alone, with a note.
- **Tests**: `crates/desktop/tests/it/history_catalog_gpui.rs` runs the panel against a real in-process daemon whose history roots point at synthetic transcripts in a temporary directory (rows, groups, scopes, filter, `repo:` / `path:`, ranked results, sort, expand, menu and every action, refresh and the timer, the off card and Enable, an older daemon); `history_gpui.rs` and `history_panel_gpui.rs` cover the search, filters, Resume, Clear and Rebuild.
