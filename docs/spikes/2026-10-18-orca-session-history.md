# Orca agent session history study (read-only scout of stablyai/orca 1.4.214, MIT)

Source for the OL-* slices (2026-10-18). Paths in the Orca sections are relative to a checkout of github.com/stablyai/orca.

## Summary

Orca has **two separate layers**:

1. **History browsing:** discovers native CLI session stores, parses bounded row metadata/previews, caches parse results, applies workspace/project filters, renders grouped rows.
2. **Optional full-text search:** explicit opt-in; independently reconciles the same sources into a **SQLite FTS5 index containing transcript text**, not just row previews.

Do not conflate the history strip’s **“Scanned N files”** with the full-text index’s `filesIndexed`, or the browse list’s **“2 of 250 sessions”** with a machine-wide session total.

All paths below are relative to `/Users/workie/proj/orca-src-git`.

---

## 1. Agents, stores, formats, extracted fields

### Complete supported-agent inventory

`src/shared/ai-vault-types.ts:4-29` defines **25 agents**. Main file-source registry: `src/main/ai-vault/session-scanner-agent-sources.ts:35-365`.

Unless noted, discovery recursively selects matching extensions below the root; `**` below represents that recursive discovery, not necessarily a fixed upstream layout. Explicit scanner options can replace these roots; WSL homes add corresponding default roots.

| Agent | Disk source and overrides | Parsed format / important companions |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl`; additionally `$CLAUDE_CONFIG_DIR/projects`, **retaining default root**, installed Claude account-profile roots | Claude JSONL records. Prunes `subagents/` from top-level results; counts/loads them beneath parent |
| Qoder | `~/.qoder/projects/**/*.jsonl` | Claude-like JSONL, with Qoder workspace/runtime records; prunes `subagents/` |
| CodeBuddy | `~/.codebuddy/projects/**/*.jsonl` | Its JSONL roles/content, summaries/AI title; prunes `subagents/` |
| Codex | `${CODEX_HOME:-~/.codex}/sessions/**/*.jsonl`; additional configured Codex homes; WSL current and retired Orca runtime homes | Rollout JSONL; `session_index.jsonl` in Codex home supplies renamed/generated thread titles |
| Gemini | `~/.gemini/tmp/**/*.{json,jsonl}` | Legacy JSON `messages[]` or JSONL messages/`$set`; **no cwd recorded by parser**, so not workspace/project-attributable |
| GitHub Copilot | `${COPILOT_HOME:-~/.copilot}/session-state/**/*.jsonl` | Event JSONL: `session.start`, `session.info`, `session.model_change`, `user.message`, `assistant.message`, shutdown |
| Cursor | `~/.cursor/projects/**/agent-transcripts/**/*.jsonl` | Role/message JSONL; associated chat `meta.json` supplies enrichment |
| Grok | `${GROK_HOME:-~/.grok}/sessions/**/summary.json` | Summary JSON plus sibling `chat_history.jsonl` |
| Devin | `${DEVIN_HOME}/transcripts/**/*.json` and sibling `agent_logs/**/*.json`; default home is `${XDG_DATA_HOME:-~/.local/share}/devin/cli`, Windows `%APPDATA%/devin/cli` | ATIF JSON `steps[]`; adjacent `sessions.db` enriches title/location/visibility |
| Hermes | `~/.hermes/sessions/**/session_*.json` | Whole JSON `messages[]` |
| Jcode | `${JCODE_HOME:-~/.jcode}/sessions/**/session_*.json` | Durable JSON `messages[]`; intentionally ignores live `.journal.jsonl` and backups |
| Kiro | `${KIRO_HOME:-~/.kiro}/sessions/cli/*.json` | UUID metadata JSON plus matching `.jsonl` turns; no descent into background-task directories |
| Rovo Dev | `~/.rovodev/sessions/**/metadata.json` | Metadata plus `session_context.json`, supporting `messages` and `message_history` |
| Pi | `~/.pi/agent/sessions/**/*.jsonl`; `$PI_CODING_AGENT_DIR` accepted with compatibility normalization | Pi message-graph JSONL |
| OMP | Default `~/.omp/agent/sessions/**/*.jsonl`; more complex resolution below | Pi-derived message-graph JSONL; OMP title records; prunes same-named task artifact directories |
| Prime Agent | `~/.prime/agent/sessions/**/*.jsonl`; `$PRIME_AGENT_SESSION_DIR`, then legacy `$PRIME_AGENT_CODING_AGENT_SESSION_DIR`, then `$PRIME_AGENT_CODING_AGENT_DIR/sessions` | Pi-derived message-graph JSONL |
| OpenClaw | `${OPENCLAW_STATE_DIR:-~/.openclaw}/agents/<agent>/sessions/**/*.jsonl` and legacy `~/.clawdbot/agents/<agent>/sessions/**` | Message-graph JSONL; only enters session subtrees, not embedded other-agent homes |
| Droid | `~/.factory/sessions/**/*.jsonl` **and** `~/.factory/projects/**/*.jsonl` | `session_start`, system, message/completion JSONL |
| Cline | `${CLINE_SESSION_DATA_DIR:-~/.cline/data/sessions}`; one manifest beneath each session directory | Session metadata JSON plus `.messages.json` |
| Kimi Code | `${KIMI_CODE_HOME:-~/.kimi-code}/sessions/wd_*/session_*/state.json` | State JSON; home `session_index.jsonl` maps session to work directory; `agents/main/wire.jsonl` or selected primary-agent wire supplies messages |
| Muse | `${XDG_DATA_HOME:-~/.local/share}/muse/sessions/YYYY/MM/DD/<uuid>/session.jsonl` | Durable event JSONL including nested retained-frame records; no special upstream sessions env override |
| OpenCode | `${XDG_DATA_HOME:-~/.local/share}/opencode/storage`; SQLite in parent data directory | Legacy `storage/session/**.json`, associated `message/<session>/…` and parts; SQLite session/message/part stores |
| OpenCode 2 | Same OpenCode database discovery | v2 tables distinguished by worker/schema inspection; current releases may share `opencode.db` |
| ZCode | `~/.zcode/cli/db/db.sqlite` | Separate OpenCode-shaped SQLite history |
| Antigravity | `~/.gemini/antigravity-cli/brain/<conversation>/.system_generated/logs/{transcript,transcript_full}.jsonl` | Transcript JSONL plus sibling-to-brain `history.jsonl`; optionally includes `antigravity-ide` and `antigravity` roots |

**Root references**

- Claude/Codex/account/WSL roots: `src/main/ai-vault/session-scanner-roots.ts:15-106`.
- OpenCode roots and v1/v2 dispatch: `src/main/ai-vault/session-scanner-opencode-sources.ts:16-118`.
- OpenCode honors `OPENCODE_DB`: absolute path used directly; relative name under data directory; `:memory:` disables disk database selection. Otherwise enumerates `opencode.db` and `opencode-*.db`: `src/main/opencode/opencode-data-directory.ts:4-30`, `src/main/opencode-usage/opencode-database-discovery.ts:16-82`.
- OpenClaw: `src/main/ai-vault/openclaw-session-layout.ts:8-35`.
- Kimi: `src/main/ai-vault/session-scanner-kimi-paths.ts:16-41`.
- Muse: `src/shared/muse-session-log.ts:11-24`.
- Prime normalization: `src/main/ai-vault/session-scanner-values.ts:146-202`.
- Antigravity: `src/main/ai-vault/session-scanner-antigravity-sources.ts:14-57`, `session-scanner-antigravity-paths.ts:3-40`; origin names `src/shared/antigravity-session-origin.ts:1-5`.
- ZCode: `src/main/ai-vault/session-scanner-zcode-sources.ts:12-45`.

**OMP resolution must be ported deliberately**, not simplified to `~/.omp`:

`src/main/ai-vault/omp-session-root.ts:31-91`:

```text
explicit scanner sessionsDir → safe root, no fallback
else OMP_CODING_AGENT_DIR → compatibility-normalized root
else:
    profile = OMP_PROFILE if set, otherwise PI_PROFILE
    validate profile; invalid → no source
    base = HOME / (PI_CONFIG_DIR or ".omp")
    config = base/profiles/profile if nondefault profile else base
    defaultAgent = config/agent
    PI_CODING_AGENT_DIR may override only in allowed nonprofile case
    on Unix, if using defaultAgent and XDG_DATA_HOME/omp[/profiles/profile] exists:
        sessions = that data root/sessions
    otherwise sessions = agentDir/sessions
```

Root-like/empty unsafe overrides are rejected rather than accidentally scanning cwd.

### Unified row fields and title semantics

`src/shared/ai-vault-types.ts:104-156` defines:

```text
id, executionHostId, executionHostPlatform?,
agent, sessionId, title,
cwd?, branch?, model?,
filePath, codexHome?,
createdAt?, updatedAt?, modifiedAt,
messageCount, totalTokens,
previewMessages[{role,text,timestamp}],
previewMessagesTruncated?,
firstUserPrompt? (on demand only),
lastUserPrompt?,
queuedMessageCount, subagentTranscriptCount,
resumeCommand, subagent?, structuredSession?
```

**The title is not universally the first user prompt.** First user prompt is a fallback; provider/user-generated titles often win:

- **Claude:** custom title > latest `ai-title` > first non-injected user prompt > metadata/injected fallback. Assistant model and usage extracted; user/assistant records count as messages. `session-scanner-primary-parsers.ts:81-194`.
- **Codex:** first canonical `session_meta` supplies ID/cwd/git branch; `turn_context` updates cwd/model; response/event variants supply messages; home `session_index.jsonl` title outranks raw prompt. Non-user-origin worker sessions are excluded early. `session-scanner-codex-parser.ts:112-233`.
- **OMP:** OMP title-fold records win; first user prompt fallback. Session header gives ID/cwd; `model_change.modelId` or `.model`, then assistant model; user/assistant message count. Pi/OpenClaw/Prime share fold. `session-scanner-graph-parsers.ts:177-297`.
- **Gemini:** `sessionId`, `startTime`, `lastUpdated`, per-message timestamps; first user content; `type:"gemini"` yields assistant/model/tokens. **cwd/branch remain null**. `session-scanner-gemini-parsers.ts:64-185`.
- **OpenCode legacy:** explicit session title/directory/time; messages add count, model, tokens, actual text; user summary only title fallback, not first-prompt identity. `session-scanner-opencode-parser.ts:28-43,96-136`.
- **Grok:** `info.id/cwd`, `generated_title` then `session_summary`, `current_model_id`, `head_branch`, metadata message count and timestamps; history gives previews/first prompt. `session-scanner-grok-parser.ts:35-59,75-97`.
- **Copilot:** session start ID/time; trusted-folder session-info text for cwd; model-change/shutdown model; first user content/transformed content title. `session-scanner-copilot-parser.ts:44-93`.
- **Hermes:** `session_id`, `cwd`, `model`, `session_start`, `last_updated`, messages; metadata count fallback. `session-scanner-hermes-parser.ts:60-93`.
- **Devin:** ATIF session identity, `working_directory`, agent/generation model, step timestamps and user/assistant text; DB enriches metadata. `session-scanner-devin-parser.ts:60-76,130-174`.
- **Jcode:** custom_title > title > first user; `working_dir`/`working_directory`, model, created/updated, durable message timestamps. Unlike most parsers it counts durable messages across mapped roles. `session-scanner-jcode-parser.ts:90-137`.
- **Kiro:** basename UUID, metadata cwd/title, nested `session_state.rts_model_state.model_info.model_id`, created/updated; user/assistant transcript events count. `session-scanner-kiro-parser.ts:76-125`.
- **Cline:** `session_id` or session directory; cwd/workspace_root; metadata model or assistant modelInfo.id; started_at/updated_at/message ts; first user fallback. `session-scanner-cline-parser.ts:83-130,158-185`.
- **Kimi:** session directory ID **retains `session_` prefix**; cwd from index; state title/lastPrompt; state timestamps; wire model config/usage/messages. `session-scanner-kimi-parser.ts:54-74,100-149`.
- **Muse:** parent directory UUID; first metadata workspace_root retained against later cwd drift; branch observation, configured model, accepted user intent and assistant output events. `session-scanner-muse-parser.ts:128-199,245-255`.
- **Droid:** start ID/title/cwd; system cwd/model; message/completion content. `session-scanner-droid-parser.ts:50-83,110-125`.
- **CodeBuddy:** summary > AI title > first real user; injected provider turns demoted; location/model extraction. `session-scanner-codebuddy-parser.ts:90-162`.
- **Qoder:** Claude fold plus workspace-directories fallback and runtime-config model. `session-scanner-qoder-parser.ts:35-56`.
- **Cursor:** first user text fallback, role messages and timestamps; chat metadata enrichment is separate. `session-scanner-cursor-parser.ts:60-94`.
- **Rovo:** metadata title/name/summary and multiple workspace-key aliases, context messages/history. `session-scanner-graph-parsers.ts:44-145`.
- **Antigravity:** user request / planner response events; history enriches opening-prompt/workspace attribution. `session-scanner-antigravity-parser.ts:98-128`, `session-scanner-candidate-parse.ts:133-140`.

**Missing fields stay null**, not synthesized through git subprocesses. Common timeline fold takes earliest/latest valid timestamps; `modifiedAt` is filesystem time and sort fallback. Global row identity is:

```text
executionHostId + ":" + agent + ":" + sessionId + ":" + filePath
```

See `session-scanner-accumulator.ts:123-208`. Preview is **latest five messages**, not complete history: `:36,210-255`. Full first prompt is extracted only on demand, avoiding permanent list payload growth: `:297-313`, `src/shared/ai-vault-types.ts:130-136`.

### Workspace/project/worktree attribution

There are two related mappings:

1. **Project attribution:** normalized cwd inside known worktree/setup/repo roots, with execution-host and WSL namespace discrimination.
2. **Workspace resume/display owner:** matches worktrees plus folder workspaces, including prior paths; reports current/active/archived/unavailable.

Project algorithm:

```text
candidates = known worktree paths + project host setups + fallback repo roots
matches = candidates whose normalized root owns session cwd in its execution namespace
restrict matches to session execution host
if host mismatch or ambiguous untagged multihost path:
    fallback to cwd folder project
choose longest root; equal path favors worktree over setup
key = project identity if available, otherwise repo identity
no cwd → unknown
no matching identity → normalized cwd folder group
```

Refs: `src/renderer/src/components/right-sidebar/ai-vault-session-projects.ts:79-116,161-244,263-326`.

Workspace owner/display: `ai-vault-session-worktree.ts:56-145`; no match yields unavailable with cwd path. Project attribution deliberately does **not** depend on currently selected project.

Scope discovery avoids losing old in-scope sessions behind global recency caps:

- Claude/Pi/CodeBuddy: cwd bucket directories.
- Codex/OMP/Prime/OpenClaw/Droid/Qoder: opening records; read **8 KiB then 64 KiB**, Codex starts at 64 KiB.
- Other agents: cached/full parse.
- Gemini: no cwd, no scoped discovery.

`src/main/ai-vault/session-scope-cwd.ts:14-132`.

---

## 2. Scanning, caching, persistence, performance

### History browsing scan

`src/main/ai-vault/session-scanner.ts:44-152`:

```text
limit = requested depth, default 1000; unlimited → infinity
per-source recency limit = default 2 * limit
discover sources concurrently
candidates = merge discoveries, order newest first
parse at most first 2 * limit candidates for global list
deduplicate; sort descending session updated/time fallback; take limit
separately discover/parse in-scope sessions past global recency cap
union global and scoped rows by identity, alias-deduplicate, sort again
return sessions, issues, scannedAt
```

Thus **1000 is not a hard maximum number of files statted**, nor necessarily final rows after adding scope guarantees. Current source defines default 1000, not 250: `src/shared/ai-vault-session-depth.ts:4-54`. The screenshot’s 250 may be chosen depth/loaded result count.

Performance:
- Native discovery batches **16**, concurrency **4**, shared native semaphore **8**: `session-scanner-discovery.ts:21-23,123-149`.
- Parsing concurrency **8**; yield event loop every **256** candidates; early stop when older candidates cannot outrank retained sessions: `session-scanner-candidate-parse.ts:23-115`.
- Newest-file collector avoids retaining every discovered candidate.
- Same-path parse lane prevents concurrent consumers corrupting shared cursors: `session-scanner-parse-cache.ts:132-148`.
- Transcript reader feeds both list fold and registered search-index consumers, avoiding a second independent decode pipeline: `session-transcript-reader.ts:103-151`.

### Incremental algorithm

`src/main/ai-vault/session-scanner-parse-cache.ts:42-107,186-265` and `session-transcript-reader.ts:65-160`:

```text
if cached platform + mtime + size match:
    if dependency/sidecar stat matches:
        reuse row without transcript read
    if dependency only enriches metadata:
        remerge sidecar onto cached raw fold
else if agent has resumable JSONL fold:
    can_append =
        size >= previous byte offset
        and mtime >= previous mtime
        and (same mtime or actual file growth)
        and previous offset is zero or byte before it is newline
    clone prior fold if can_append, otherwise create fresh fold
    stream complete records from offset or zero
    save byte offset after last complete newline
    final unterminated record may appear in display-only cloned fold
    but does not advance resumable state
else:
    parse whole rewritten document / database-backed session
```

Append folds: Claude, Qoder, CodeBuddy, Codex, Cursor, Copilot, Droid, Pi/OMP/Prime/OpenClaw, Gemini JSONL, Antigravity.

Whole-document/other sources only get unchanged reuse: Grok, Rovo, Devin, Hermes, Jcode, Gemini JSON, Kimi, OpenCode/v2/ZCode, Cline, Kiro, Muse.

**No generic per-file tail-only scan or maximum file-byte cap exists in this core reader.** New JSONL files stream from beginning; incremental reads start at consumed offset. **10 MiB per-record bound**, not per-file: oversize records are discarded through newline, reported as issues, and remaining records still parsed. `session-scanner-jsonl-reader.ts:18-138`, `session-transcript-record-budget.ts:1-18`.

Head-only scope probes above are a separate optimization, not final metadata parsing. Some agent-specific readers have their own limits; do not impose one generic “read last N bytes” behavior in Rust.

### Browse persistence

A separate **`session-parse-cache.sqlite`** holds parsed row checkpoints; legacy `session-parse-cache.json` is migrated. SQLite is placed beside configured legacy cache path:

- `session-parse-cache-persistence.ts:18-31,95-104`.
- Write debounce **1.5 s**, max wait **15 s**, batches **500**.
- Old native rows are checked for missing transcript after **30 days**, up to **1000** checks per helper start; WSL rows expire after **180 days** instead of booting/stalling distros to stat them.
- This is **not** the transcript FTS database.

### Full-text index scheduling / storage

Index path: **`<dataRoot>/ai-vault/session-search.sqlite`**: `src/main/ai-vault-search/session-search-database-path.ts:11-12`.

Schema: `session-search-schema.ts:23-96`:
- `sessions`: identity, title, cwd/key, branch, created/updated, count, resume command, content-prefix hash.
- `files`: source path, device/inode identity, byte offset, mtime/size, current/due/failed state, failure stat/count.
- `messages`: session/role/time.
- `messages_fts`: user text, assistant text, tool text, identifier shadow terms.

**Polling, not a filesystem watcher.** Indexer explicitly has “no queue, no watch set”; durable store state is the work ledger: `session-search-indexer.ts:45-98`.

Defaults `session-search-indexer-options.ts:5-31`:
- Initial full-machine sweep.
- Reconcile every **20 s after previous pass finishes**.
- Recent cycles consider newest **12 per agent root**.
- Full sweep every **15 cycles**; nominal five-minute cadence, plus pass duration.
- Pass deadline default **5 s**; checked **between files**, so not a hard deadline interrupting one giant file.
- Full sweep refreshes resolved roots.

Per-file decisions `session-search-read-decision.ts:51-124`:

```text
outside retention → skip
new source → require some read
failed >=3 times at unchanged mtime → skip until mtime changes
state due / changed file identity / missing valid cursor / truncated → whole read
failed but eligible retry → some read
same mtime+size → skip
otherwise → some read, letting parser append where safe
```

Deadline remainder is still owed by source/store facts, not an in-memory queue: `session-search-index-pass.ts:46-109`. Source deletions are reconciled, with inaccessible/missing mounts distinguished from confirmed deletion.

### Refresh and “Scanned N files”

- Browse refresh button: `refresh({force:true})`.
- While full-text searching: **reissues search**, not the browse forced scan.
- Exact wiring: `src/renderer/src/components/right-sidebar/AiVaultPanel.tsx:248-269`.
- Browse mount reuses renderer/main caches; refocus and newly observed provider session IDs trigger refresh. Automatic forced rescans share **30 s** throttle across panel remounts; explicit user refresh bypasses budget. `ai-vault-session-refresh.ts:36-50,156-190,303-357`.
- **“Scanned N files” = sum over per-agent discovered files + databases**, not successfully parsed sessions or FTS indexed rows. During reading it shows `done / total`; finished age is relative to scan settlement. Strip expands to per-agent breakdown. `AiVaultScanProgressStrip.tsx:55-118,146-183`.

---


## 3. Search, scopes, ordering, grouping, counts

### Legacy filter versus FTS

When local indexing is not consented/enabled, the search box remains a **legacy row filter**. Remote hosts reporting disabled/no-service also fall back to that mode. FTS requests debounce **250 ms**.

`src/renderer/src/components/right-sidebar/use-ai-vault-search.ts:115-204`, debounce `:100-108`.

Legacy filter:
- Case-folded substring terms, **all terms required**.
- Searches title, session ID, agent, branch, model, cwd, transcript filepath, preview text.
- `repo:` targets resolved project label/folder fallback.
- `path:` targets cwd **and** transcript filepath.
- Supports quoted operator values.
- Query > **2 KiB** yields no results.
- No relevance ranking; uses browse timestamp order.

`src/shared/ai-vault-session-filters.ts:64-128,190-277`.

FTS:
- Searches **decoded user/assistant/tool transcript text and identifier shadow terms**; not the same metadata-string concatenation as legacy filter.
- `repo:`/`path:` still act as qualifiers.
- Identifier shadow terms split `resolveTerminalPath` and `src/main/foo-bar.ts`.
- FTS tokenizer retains `_ . - / +` inside tokens.
- Query cap **512 UTF-16 code units**, preserving surrogate integrity.
- Retrieval ladder: **exact phrase → AND → typo-repaired phrase/AND → OR**.
- BM25 role weights **user 3, assistant 2, tool 1, identifiers 1**.
- “Conversation” content scope restricts to user/assistant columns and zeroes other weights; this content scope is distinct from Workspace/Project/All location scope.
- One best matching message represents each session.
- Ranking subtracts `0.02 * ln(1 + message_count)`.
- Forks with same eligible opening-content prefix hash collapse; newest copy wins, duplicate count retained.
- Tie: newer updated time, then stable numeric row ID.
- Cursor binds query identity + database incarnation + generation; stale cursor restarts first page in renderer.
- Snippets are fetched only for final page.

Refs:
`session-search-schema.ts:16-21,88-96`;
`session-search-retrieval.ts:17-27,91-137`;
`session-search-hit-ranking.ts:5-114`;
`session-search-engine.ts:95-163`;
`use-ai-vault-search.ts:75-88`.

### Workspace / Project / All

Browse filter, `src/shared/ai-vault-session-filters.ts:81-116`:

```text
Workspace:
    session cwd owned by selected workspace scope paths
Project:
    session resolved project key equals selected project key
    no selected project → no matches
All:
    no cwd/project restriction
```

FTS sends an identity (`within`) which host resolves into its workspace/project path catalog, rather than trusting renderer invented project names: `use-ai-vault-search.ts:143-189`, `src/main/ai-vault-search/session-search-scope-resolution.ts`.

### Sort/group/count

- Browse sort: **Last updated**, **Created** (`updated` / `created`), descending, fallback modifiedAt.
- Search sort: **Relevance**, **Newest**.
- Groups: **project**, folder, or agent. Default project.
- Group insertion order follows already sorted sessions; no separate alphabetical group re-sort.
- Folder label is last two normalized path segments; unknown cwd → unknown location.
- Search results are **ungrouped**, retaining host ranking.
- Group header counts are group’s filtered rows.
- **“2 of 250 sessions”** means `filteredSessions.length` versus **loaded browse rows**. It is not “two matches out of the entire FTS corpus.”
- Searching shows `N results` and paginates; no claimed machine-total count.

Refs: `src/shared/ai-vault-types.ts:48-55`;
`ai-vault-session-filters.ts:132-164,279-318`;
`src/renderer/src/components/right-sidebar/ai-vault-session-filters.ts:59-104`;
`ai-vault-view-defaults.ts:12-15`;
`AiVaultSessionListBar.tsx:14-35`;
`AiVaultPanel.tsx:284-307`.

---

## 4. Row UI, expansion, actions, resume commands

### Row behavior

`src/renderer/src/components/right-sidebar/AiVaultSessionRow.tsx:158-256`:

- **Click row toggles details**, not automatic resume.
- Title is draggable to resume into tab/drop targets; detail text remains selectable.
- Collapsed row: title, latest conversation turn labeled **You / Agent / Tool / System**, metadata.
- Latest snippet is not guaranteed assistant-only; role label follows actual latest turn.
- Metadata: agent icon/name, live attention indicator when appropriate, `N msgs`, optional subagent count / recoverable “Not saved”, age, model, workspace badge/status.
- During FTS search, evidence snippet replaces collapsed latest-preview text.
- Chevron and explicit Resume are separate affordances; `⋯` and right-click share menu construction.

Metadata exact rendering: `ai-vault-session-row-display.tsx:34-139,177-199`.

### Expanded content

`AiVaultSessionDetails.tsx:76-88,96-254`:
- Resume in session workspace / new tab buttons.
- Optional resume in native chat / new CLI.
- Hand Off to Another Agent.
- View Log.
- **First prompt card**, with full prompt on-demand support.
- **Latest three conversation turns**, not complete transcript.
- Subagents section loaded beneath parent where supported.
- Workspace/location receipt and status.
- Zero-turn session shows “not saved” explanation rather than pretending resume works.

### Menu inventory

`AiVaultSessionActionMenuItems.tsx:100-234`:

- Jump to Original Pane.
- Jump to Worktree / Folder.
- Resume (contextual label).
- Resume in New Native Chat / New CLI where supported.
- Copy Resume Command.
- Hand Off to Another Agent.
- Open Log.
- Reveal Log.
- Open Working Directory.
- Copy Session ID.
- Copy Log Path.
- Delete where permitted.

Availability depends on local source access, supported ownership, recoverable content, structured-session status, etc. Structured native-chat rows do not get transcript deletion callback.

Delete trashes **validated transcript and companions**, companion-first. It checks agent-known source shapes, leaf type, realpath containment/symlink escape; WSL uses separate validated removal rather than OS trash. SQLite/IDE sources are not indiscriminately unlinked.

`src/main/ai-vault/session-delete.ts:15-37,43-110`; common discoverability and deletion registry relationship: `session-scanner-agent-sources.ts:78-112`.

### Resume invocation matrix

Commands are built with shell-specific quoting and normally:

```text
cd <quoted cwd> && <agent resume invocation>
```

Codex may prefix/pin `CODEX_HOME`; real-home resume explicitly removes inherited `CODEX_HOME` and `ORCA_CODEX_HOME`. Settings can override base agent command.

`src/shared/ai-vault-resume-command.ts:20-63,65-176,183-270`.

| Agent(s) | Invocation after configured/default base |
|---|---|
| Codex | `codex resume <id>` |
| Claude, Qoder, CodeBuddy, Gemini, Grok, Hermes, Devin, Droid, Jcode, OpenClaw, ZCode | `<base> --resume <id>` |
| Cursor | `cursor-agent --resume <id>` |
| OMP | `omp --resume <absolute transcript path>`; ID only if no path |
| Prime Agent | `<base> --resume <absolute transcript path>` |
| Pi | `pi --session <id>` |
| OpenCode / OpenCode 2 | `<base> --session <id>` |
| Kimi | `kimi --session <session_… id>`; cwd must match |
| Muse | `muse resume <uuid>`; workspace-scoped |
| Copilot | `<base> --resume=<id>` |
| Cline | `<base> --id <id>` |
| Rovo Dev | full launch base + `--restore <id>` |
| Kiro | full launch base + `--resume-id <id>` |
| Antigravity CLI native | `<base> --conversation <id>` |
| Antigravity IDE/reference history | **new** CLI conversation: `--prompt-interactive <transcript-reference prompt>`, not fake native resume |

OpenCode2/Rovo/Kiro use full launch command, preserving mode/subcommands; most others use detection command. Native structured-session ownership may activate existing chat rather than spawn equivalent terminal command.

---

## 5. Privacy and CLI formatting

### Consent / retention / text storage

- **FTS is off by default**: `{enabled:false, historyDays:null}`.
- Before opt-in, no indexer construction, index DB opening, or transcript reads **for that index**.
- This does **not** mean history browsing itself is opt-in; browse still reads native sessions for rows/previews.
- Retention: all history or positive days, normalized/clamped to **3650**.
- Policy changes close/reconstruct immutable indexer.
- Index stores transcript text **as written, without secret redaction**. Explicit design comment says it is a second plaintext copy of local transcript content.

Refs: `src/shared/ai-vault-search-settings.ts:3-28,31-59`;
`src/main/ai-vault-search/session-search-policy.ts:8-22`;
`session-search-schema.ts:8-13`.

Transport redaction is narrower than content sanitization:
- Relay strips resume command and source location details, retaining source presence.
- Relay status withholds degraded-root paths and native error strings.
- **Transcript snippets are not generally secret-redacted by this helper.**

`src/shared/ai-vault-search-transport.ts:5-32`.

### Required CLI module

`src/cli/agent-session-search-format.ts:21-173` is a presentation layer, not scanner/index logic:

- `terminalSafe`: strip ANSI escape sequences and terminal controls, preventing OSC52/cursor escapes embedded in transcript text from executing.
- One-line formatting removes CR/LF.
- Hit: agent, updated timestamp, title, optional host; evidence role/snippet; resume command only if provided by transport contract.
- Keeps engine `[[match]]` markers, emits no ANSI highlighting.
- Reports result count/page duration, cursor continuation, candidate/query/snippet truncation, freshness wait timeout, optional planner debug.
- Distinguishes disabled, not-ready, no-service, scope-unknown, unsupported-agent, stale cursor.
- Status prints enabled/phase/filesIndexed/filesDue/filesFailed/reconcile/sweep/generation/degraded-root count.
- Terminal escape stripping is **output safety**, not credential redaction.

---

## Rust port architecture and key algorithms

Recommended direct conceptual split—not a verbatim TypeScript port:

```text
ProviderSourceRegistry
    roots(env, account profiles, execution host)
    discover predicate / pruned subtrees / companion dependencies
    parser + scope-cwd strategy + resume strategy

TranscriptReader
    per-path serialization
    byte-accurate JSONL continuation
    append/replace message stream
    oversized-record reporting
    cancellation

SessionFold / BrowseCache
    immutable published row
    latest 5 previews
    provider title precedence
    first prompt separately on demand
    mtime+size+dependency signatures

SearchIndex (opt-in)
    SQLite files + sessions + messages + FTS5
    consumes TranscriptReader events
    initial/full/recent reconciliation
    durable due/current/failed states and retention

ScopeCatalog
    host-aware normalized path ownership
    longest-prefix project/workspace resolution

HistoryView
    browse filters/grouping/counts
    separate debounced ranked search
    click-expand, explicit resume, contextual menu
```

Core reader pseudocode:

```text
lock(source_path)
candidate = stat(source, companions)
cached = browse_cache.lookup(source)

if unchanged(candidate, cached):
    return enrich_or_reuse(cached)

if resumable(candidate.provider) and safe_append(candidate, cached):
    mode = append
    state = clone(cached.fold)
    offset = cached.complete_line_offset
else:
    mode = replace
    state = new_fold(provider)
    offset = 0

broadcast begin(mode, identity, offset)
for complete_record in stream(source, offset):
    if record_size > 10 MiB:
        skip_to_newline_and_record_issue()
    else:
        provider.consume(record, state, message_sink)
publish display snapshot
commit cache/index only for complete accepted read
preserve incomplete trailing record for next continuation
```

Search pseudocode:

```text
resolve host-owned workspace/project identity to cwd constraints
split repo:/path: operators from text
plan bounded text query
retrieve phrase; else AND; else typo repair; else OR
respect agent/time/cwd filters during retrieval
apply repo:/path: predicates
keep each session's best BM25 message
score -= 0.02 * ln(1 + message_count)
collapse eligible same-prefix forks, newest representative wins
sort relevance/newest with deterministic ties
validate generation-bound cursor
slice page
fetch snippets/source presence only for page
```

**Most important fidelity decisions for Hutan:** retain the separate browse/FTS distinction, provider-specific title precedence, host-aware longest-root attribution, in-scope discovery beyond recency cap, explicit click-expand behavior, absolute-path OMP resume, and “shown of loaded” count semantics.

Read-only investigation; no files changed and no builds/tests executed.