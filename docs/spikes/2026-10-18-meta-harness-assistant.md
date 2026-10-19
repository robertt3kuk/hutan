# Hutan personal assistant as a local meta-harness

**Status:** research and proposed design; no implementation or product decision is implied.  
**Decision addressed:** direction §8 and **D-7: Advisor or Operator by default**.

## Executive recommendation

Build the assistant as **a persistent, daemon-owned workspace with a personal-assistant presentation**, not as another harness and not as a wrapper that reconstructs another agent’s tool loop.

Hutan should own:

- assistant workspaces, conversations and delegation relationships;
- curated memory, portable skills and explicit learning workflows;
- schedules, occurrence accounting and unattended capability grants;
- approvals, operation receipts, notifications and execution traces;
- backend-independent services exposed through the existing MCP/control surface.

OMP, Claude Code and Codex should retain their **native inference, compaction and tool execution**. Direct-provider models need a real, separately implemented gateway tool loop; until that exists, label them **Chat only**.

**Recommend D-7 = Advisor by default**, with a visible, user-controlled switch to **Operator — asks before acting**. Do not silently convert existing assistants into operators.

Three important qualifications:

1. **“Stored locally” does not mean “never sent to a model provider.”** Memory selected for a turn reaches the selected backend. The UI must disclose this.
2. **ACP is not a universal system-prompt API.** Stable ACP prompt input works everywhere; higher-priority instruction injection is adapter/version dependent.
3. **Today’s agent-origin marker is not a security boundary.** Unattended execution needs a restricted authenticated control endpoint and an OS/harness sandbox; filtering MCP tools alone cannot constrain a process that can access the ordinary user socket.

All architectural choices below are **[INFERENCE/design]** unless identified as observed behavior.

---

## 1. Evidence and current boundaries

### 1.1 Hutan already supplies the basic substrate

| Observed behavior | Evidence |
|---|---|
| Assistants are ordinary chat sessions with scope, history, restore, interrupt and model switching. Global assistants currently live in Home; scoped assistants live in the target workspace. | `docs/assistant.md:18-30`; `crates/protocol/src/assistant.rs:149-194` |
| The assistant’s direct-provider system prompt explicitly says it cannot run commands or change files. | `crates/daemon/src/assistant.rs:65-72` |
| OMP assistants launch with `--tools read,grep,glob` and an appended assistant prompt. | `crates/daemon/src/harness.rs:37-46,207-234` |
| Claude assistants request `plan`; Codex assistants use `ApprovalPolicy::Never` and `SandboxMode::ReadOnly`. | `crates/daemon/src/assistant.rs:606-622` |
| Hutan injects `hutan mcp` into ACP session setup and Codex process overrides without writing the user’s agent configuration. | `crates/daemon/src/mcp_wiring.rs:36-87` |
| MCP descriptors already declare schemas, read-only classification and daemon-request mappings once. | `crates/cli/src/mcp/table.rs:1-30,86-117` |
| Attachments are daemon-resolved, bounded and redacted; permission validation precedes reading. | `docs/assistant.md:76-110`; `crates/daemon/src/assistant.rs:8-14` |
| Harness assistants already sleep when idle and resume on demand; assistant workspaces need not imply permanently running harness processes. | `docs/assistant.md:66-74` |
| Direct gateway types currently contain only system/user/assistant roles, text/images, deltas, usage and completion—no tool calls or tool results. | `crates/gateway/src/types.rs:70-150` |

**Do not confuse native-tool restrictions with an end-to-end Advisor policy.** Hutan’s MCP wiring is independent of those restrictions, and `ControlService::gate` currently applies the global agent policy without an assistant-mode branch. A native read-only harness can still reach a mutating Hutan tool if that tool is exposed and daemon policy permits it. A real Advisor mode therefore needs daemon enforcement, not just the existing launch flags.

Evidence: `crates/daemon/src/mcp_wiring.rs:36-50`; `crates/daemon/src/control.rs:187-230`.

### 1.2 What Hermes demonstrates—and what not to copy

Hermes’ useful product loop is:

> work → remember stable facts → improve reusable procedures → recall previous work → begin the next task with accumulated knowledge.

Observed implementations:

- bounded `MEMORY.md` and `USER.md`, frozen at session creation;
- writes through real tools, optionally staged for approval;
- progressive skill disclosure;
- explicit `/learn` as a normal skill-authoring turn;
- durable scheduled occurrences and separate execution sessions;
- child agents with fresh context;
- memory, capabilities, automation and agent-management surfaces around chat.

Sources:

- `/Users/workie/proj/hermes-src/tools/memory_tool.py:1-5,64-102`
- `/Users/workie/proj/hermes-src/website/docs/user-guide/features/memory.md:13-33,57-88`
- `/Users/workie/proj/hermes-src/agent/learn_prompt.py:1-6,172-190`
- `/Users/workie/proj/hermes-src/cron/AGENTS.md:18-37`
- `docs/spikes/2026-10-18-hermes-meta-harness.md:19-35,70-82`

Hermes’ outbound ACP path retains a Hermes-owned loop by requesting textual tool calls. **Do not copy that bridge.** Hutan should expose services through real MCP and let external harnesses execute normally.

Evidence: `docs/spikes/2026-10-18-hermes-meta-harness.md:55-68`.

### 1.3 Source and verification scope

This investigation inspected Hutan and Hermes source and official upstream documentation/source. It did not execute models, harnesses, tests or benchmarks; did not inspect user agent configuration or secrets; and changed no repository files.

Official provenance:

- OMP’s [official site](https://omp.sh/) identifies `can1357/oh-my-pi` as its `codeRepository`; the [published package](https://www.npmjs.com/package/@oh-my-pi/pi-coding-agent) identifies the same repository.
- The [ACP agent directory](https://agentclientprotocol.com/get-started/agents) links Claude’s adapter. The historical Zed repository URL redirects to [`agentclientprotocol/claude-agent-acp`](https://github.com/agentclientprotocol/claude-agent-acp).
- OpenAI’s [app-server documentation](https://developers.openai.com/codex/app-server) links [`openai/codex`](https://github.com/openai/codex).
- [Anthropic’s skills repository](https://github.com/anthropics/skills) identifies [agentskills.io](https://agentskills.io/specification) as the standard.

Upstream `main` findings establish available source contracts, **not support in every installed release**.

---

## 2. Architectural options

| Option | Advantages | Disadvantages |
|---|---|---|
| Keep today’s attachment-only assistant | Smallest change; predictable, cheap | Does not meet persistence, scheduling, learning or control requirements |
| Embed Hermes as the assistant runtime | Many features already implemented | Introduces a second runtime, storage/policy conventions and lifecycle; harness independence becomes weaker |
| Build a universal Hutan inference/tool loop around ACP | Superficially uniform control | Duplicates harness ownership; weak textual bridging; compaction, cancellation and native-tool behavior become fragile |
| **Daemon services + native harnesses through MCP** | Reuses Hutan’s source of truth, ordinary sessions, approvals and cross-harness history | Requires honest backend capability reporting and adapter-specific context injection |
| Daemon services + Hutan’s direct-provider runtime | Full control over tool availability and receipts; works with local models | Requires a real tool loop, provider-specific continuation handling and additional testing |

**Recommendation:** the fourth option as the principal architecture, with the fifth as a supported backend—not a replacement for native harnesses.

---

## 3. Assistant workspace and Home UX

### 3.1 Domain model

An assistant workspace is an ordinary workspace with:

```text
kind = assistant
assistant_for_workspace_id = null | task/project workspace ID
```

- Exactly one global assistant workspace per daemon.
- At most one assistant workspace for each target workspace.
- Multiple conversations per assistant workspace.
- Ordinary terminal/chat tabs and the ordinary layout machinery remain usable underneath.
- Delegated execution lives in **task workspaces**, not hidden inside the assistant’s conversation.
- Global assistant scope does not automatically grant every repository’s files or memory.
- A per-workspace assistant resolves context against its **target workspace**, not whichever workspace is currently selected in the desktop.
- `parent_id` remains the existing project-grouping relationship; do not overload it with assistant ownership.

Use a private daemon-managed cwd for the global assistant rather than requiring an arbitrary repository or the user’s entire home as its root. This requires changing today’s creation path, which rejects a workspace without a first root:

`crates/daemon/src/assistant.rs:569-583`.

Separate:

1. **session membership**—which assistant workspace owns the chat;
2. **context scope**—which target workspace’s approved knowledge it may receive;
3. **filesystem permissions**—what the daemon and native harness can actually read/write.

Those are not interchangeable.

### 3.2 Home is a center page, not a narrow chat sidebar

The direction asks for a Home page. The present assistant is documented as a left-only panel (`docs/assistant.md:143-153`). That restriction should be intentionally replaced, not accidentally bypassed.

Recommended shell:

```text
┌─────────────────┬──────────────────────────────────────────┬─────────────────────┐
│ Navigation      │ Hutan                 Local ▾            │ Today           ×   │
│ Search          │ Global ▾  OMP · model ▾  Advisor ▾        │ Needs you       2   │
│ Tasks           ├──────────────────────────────────────────┤ [approval]           │
│ Agents          │ Conversation title ▾          New chat  │ [question]           │
│ Hutan           │                                          │                     │
│                 │ User / assistant transcript              │ Working          3  │
│ Workspaces      │ Tool cards with real execution status    │ api-fix · Claude    │
│  API migration  │ Memory/skill proposals                   │ tests · Codex       │
│  Release        │ Delegation cards → task workspace        │                     │
│                 │                                          │ Upcoming            │
│ Sessions        │                                          │ 09:00 Daily brief   │
│  zsh ~/src/api  ├──────────────────────────────────────────┤                     │
│                 │ [+ Attach] [Skills]                       │ Memory · Skills     │
│                 │ Ask Hutan…                      [Send]    │ Schedules · Traces  │
└─────────────────┴──────────────────────────────────────────┴─────────────────────┘
```

Guidelines:

- Keep the normal workspace/session navigation visible.
- Center chat is primary; Today is a collapsible supporting rail.
- Do not put Memory, Skills, Schedules and Traces into five simultaneously expanded mini-panels.
- Preserve the task workbench’s tabs/splits while Home is selected.
- Scope, host, backend and mode remain visible; none may be inferred solely from sidebar selection.
- New chat creates a fresh conversation, not a new assistant workspace.
- A backend with no tool loop shows **Chat only**, not an unusable Operator control.
- Initial Home shows useful actions: **Ask a question**, **Review what needs you**, **Learn from a session**. No invented “good morning” summary that silently starts inference.
- Refreshing Today is a daemon query/event update, not a paid model turn.

The global-page/workbench distinction is consistent with Orca’s observed shell, rather than copying a nonexistent Orca personal-assistant page:

`docs/spikes/2026-10-18-orca-layout.md:7-45,198`.

### 3.3 Management pages

Memory, Skills and Schedules use a shared **searchable master/detail** pattern:

```text
┌──────────────────────────────┬───────────────────────────────────────────────┐
│ Memory             [+ Add]   │ Prefer concise answers                       │
│ [Search…]                    │ Profile · Global · revision 4                │
│ Scope: Global ▾              │                                               │
│ Active | Proposed | Archived │ User prefers concise, concrete responses.    │
│                              │                                               │
│ ● Response style             │ Source: chat “Working preferences”, turn 8   │
│ ● API deployment location    │ Approved by you · 18 Oct                      │
│ ◇ Proposed: test command      │ Last injected: conversation …                 │
│                              │                                               │
│                              │ [Edit] [Archive] [View revisions]              │
└──────────────────────────────┴───────────────────────────────────────────────┘
```


[Showing lines 1-200 of 1352. Use :201 to continue]
Memory detail shows scope, provenance, approval, revision and injection history. Skills detail additionally shows source root, package hash, compatibility, files and whether Hutan owns the package. Schedule detail shows next occurrences, timezone, policy and previous runs.

An optional memory/skill graph may be interesting later; **a graph is not the primary editor**.

### 3.4 Interaction and accessibility

- One primary action per page: Send, Add memory, Create skill, Create schedule.
- Keyboard focus order: navigation → page header → content → composer → supporting rail.
- Escape closes the nearest overlay without losing draft/chips.
- Focused approval cards expose keyboard-operable Allow once / Deny; no accidental “approve on Enter” when focus is ambiguous.
- Long paths wrap or reveal their full value; status is not color-only.
- Narrow windows collapse Today into a drawer.
- Virtualize transcripts, conversation lists and run history.
- Hidden management pages perform no periodic inference or render work.
- “Running”, “waiting for you”, “unreachable” and “finished, not reviewed” remain distinct.

---

## 4. Memory service

### 4.1 Store curated knowledge, not an uncontrolled transcript summary

Recommended entry categories:

- **profile**: communication preferences and user-approved personal facts;
- **fact**: stable environment/project facts;
- **note**: workspace-specific decisions and working notes.

Persona/system identity is separate user-controlled configuration. A memory entry cannot grant tools, widen a scope or override policy.

Recommended scopes:

| Scope | Meaning |
|---|---|
| Global | This daemon’s approved cross-workspace knowledge |
| Workspace | Knowledge for one explicit target workspace |
| Session | Working notes for one conversation; not automatically promoted |
| Host | Implicit in the owning daemon; no automatic cross-host sharing |

Global profile entries may be inherited by assistant conversations after the user enables that behavior. Workspace facts never become global merely because a maintenance model suggests they are broadly useful.

A different assistant backend must not silently change the scope or transmission consent of an existing entry.

### 4.2 Schema

Use the existing SQLite/storage crate, with new migrations and repositories. A suitable logical schema:

```text
knowledge_scopes(
  id PK, kind CHECK(global|workspace|session),
  workspace_id FK?, session_id FK?,
  revision NOT NULL,
  UNIQUE appropriate-to-kind
)

memory_entries(
  id PK, scope_id FK,
  category CHECK(profile|fact|note),
  current_revision,
  state CHECK(active|archived),
  created_at_ms, updated_at_ms
)

memory_revisions(
  entry_id FK, revision,
  body, content_hash,
  provenance_json,
  author_kind CHECK(user|agent|import),
  approval_id FK?,
  operation_id UNIQUE,
  created_at_ms,
  PRIMARY KEY(entry_id, revision)
)

knowledge_proposals(
  id PK, scope_id FK,
  target_kind CHECK(memory|skill),
  operation CHECK(create|update|archive|restore),
  target_id?,
  expected_revision?,
  proposed_payload,
  provenance_json,
  state CHECK(pending|applied|rejected|conflict|cancelled),
  origin_session_id?, origin_turn_id?,
  approval_id FK?,
  created_at_ms, resolved_at_ms?
)

assistant_context_snapshots(
  id PK, session_id FK,
  backend, adapter_version?,
  scope_revisions_json,
  manifest_json,
  rendered_context,
  content_hash,
  created_at_ms
)

assistant_context_deliveries(
  session_id FK, turn_id,
  snapshot_id FK,
  delta_json,
  transport,
  state CHECK(prepared|submitted|failed|unknown),
  created_at_ms,
  PRIMARY KEY(session_id, turn_id)
)
```

Rules:

- Revisions are immutable; archive/restore does not overwrite history.
- `expected_revision` is mandatory for updates, including approval-time application.
- One transaction applies a proposal, writes its receipt and increments the affected scope revision.
- Duplicate `operation_id` returns the original receipt.
- Rejected proposals are never injected.
- Provenance identifies the actual source: session/turn/journal sequence range, user-entered text, or imported file/package hash. The daemon stamps origin identity; the model cannot invent authoritative provenance.
- A model inference is labeled **inferred**, not converted into a user-confirmed fact.
- Contradictory entries produce a visible conflict; no silent merge.
- Use FTS for curated entries if needed; embeddings are not a prerequisite.
- Redaction happens before persistence and injection. Likely credentials are refused with a link to the vault flow, not quietly stored as ordinary memory.
- Memory is not a secret store.

Hermes demonstrates why approval must bind the reviewed object, not re-run a substring match later:

`/Users/workie/proj/hermes-src/tools/memory_tool.py:64-78`.

### 4.3 Approval and write semantics

Memory tools return one of:

```json
{
  "status": "proposed",
  "proposal_id": "…",
  "applied": false
}
```

or, after a committed transaction:

```json
{
  "status": "applied",
  "operation_id": "…",
  "entry_id": "…",
  "revision": 3,
  "scope_revision": 18
}
```

Never call `proposed` “saved to memory”.

User actions can save a user-entered entry directly. Model-authored writes default to proposals. Optional automatic application is a user-only setting with explicit scope/category restrictions; it must not imply that model-generated facts are user-confirmed.

Do not inherit Hermes’ fail-open behavior if an approval module is unavailable: its source explicitly permits that fallback (`tools/memory_tool.py:84-90`). Hutan should fail closed.

### 4.4 Bounded snapshots and deltas

At conversation creation:

1. Resolve principal, host, target scope and transmission policy.
2. Select active approved entries.
3. Produce deterministic ordering and a bounded rendered snapshot.
4. Store the manifest and exact delivered bytes.
5. Inject through the best supported backend route.

The snapshot includes entry IDs/revisions, not just prose.

At later turn boundaries:

- Freeze that turn’s context before submitting it.
- Compare current scope revisions against the conversation snapshot.
- Send a compact delta with additions, replacements and tombstones.
- Identify the base snapshot and resulting revision set.
- Journal the delta as **Hutan-added context**, not text the user supposedly typed.
- Do not rewrite old transcript messages or a live system prefix.
- Bound total active memory and delta size. If a delta cannot fit, submit a complete bounded current memory view or require a fresh conversation; never silently omit a deletion.
- On resume, use the recorded snapshot and delivery state rather than assuming a restarted process has refreshed its context.
- When compaction visibility is unavailable, do not claim a delta remains in the model’s context. Reassert a compact authoritative view at a safe boundary or start a fresh conversation.

A deletion cannot remove bytes already delivered to a provider or retained by a harness. The UI should distinguish **archived for future use** from **removed from local records**, and explain retained transcripts/snapshots before erasure.

Hermes’ frozen prefix and session-boundary tradeoff is explicit:

`website/docs/user-guide/features/memory.md:57-65`.

Its skill-delta mechanism is a useful precedent, but explicitly skips its Codex app-server mode; it is not evidence that the same injection route works everywhere:

`/Users/workie/proj/hermes-src/agent/skills_index_delta.py:1-14,100-109`.

---

## 5. What each backend actually supports

### 5.1 Capability matrix

| Backend | Verified source contract | Recommended Hutan route |
|---|---|---|
| **OMP ACP** | `--append-system-prompt <text-or-file>` appends to the generated system prompt; retains native instructions. Explicit append supersedes discovered `APPEND_SYSTEM.md`. The CLI reads prompt input at launch; changing a file does not make it a live context channel. | Create a daemon-owned immutable prompt file per snapshot; launch with its path. Preserve that snapshot on process wake. Deliver subsequent deltas through ordinary ACP prompt content. |

[Showing lines 201-400 of 1352. Use :401 to continue]
| **Claude Agent ACP** | Current official adapter source supports the **unstable** `systemPrompt` field on new/load/resume/fork, with append/override modes. Append preserves the Claude Code preset. On resume, changed system instructions may take effect only at the next compaction. | Negotiate and version-test this feature. Prefer append when supported; otherwise use a labeled first-turn snapshot via ACP text/embedded resource. Always use prompt-content deltas for immediate updates. |
| **Codex app-server** | Official **rust-v0.162.1** `ThreadStartParams` and `ThreadResumeParams` expose `baseInstructions` and `developerInstructions`; these fields are not individually marked experimental. | Add typed `developerInstructions` to Hutan’s start/resume DTOs. Preserve native base instructions; do not replace them. Use ordinary `turn/start.input` text for immediate deltas. |
| **Direct provider** | Gateway sends system messages today, but has no tool-call vocabulary or loop. | Frozen system snapshot plus subsequent context messages; memory lookup/write tools only after implementing the real tool loop. Otherwise Chat only. |

Sources:

- [OMP system-prompt customization](https://github.com/can1357/oh-my-pi/blob/main/docs/system-prompt-customization.md)
- [OMP CLI reference](https://github.com/can1357/oh-my-pi/blob/main/docs/cli-reference.md)
- [Claude adapter client-system-prompt implementation](https://github.com/agentclientprotocol/claude-agent-acp/blob/main/src/client-system-prompt.ts)
- [Codex rust-v0.162.1 thread schema](https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/app-server-protocol/src/protocol/v2/thread.rs)
- [ACP stable session setup](https://agentclientprotocol.com/protocol/v1/session-setup)
- [ACP stable prompt turn](https://agentclientprotocol.com/protocol/v1/prompt-turn)

### 5.2 Important implementation details

**OMP**

Do not put personal memory directly into process argv. Use a private daemon-owned prompt file under the host data directory, with restrictive permissions. It is an instruction artifact, not a user configuration edit.

OMP’s append option can displace the user’s existing append source. Hutan already supplies an explicit append flag today, but expanded memory makes this interaction more important. Surface it in backend diagnostics; do not promise that Hutan’s prompt is simply concatenated with every user append file.

OMP task subagents do not automatically inherit the main session’s append prompt. A Hutan-delegated agent therefore needs its own explicit context preparation.

Source: [OMP system-prompt customization, inputs by session type](https://github.com/can1357/oh-my-pi/blob/main/docs/system-prompt-customization.md).

**Claude**

Do not impersonate JetBrains AIR just to use its private `customInstructions` metadata. Use the advertised system-prompt extension when supported, or ordinary ACP prompt content.

The official adapter marks AIR extensions experimental and client-specific:

[Claude adapter AIR extension contract](https://github.com/agentclientprotocol/claude-agent-acp/blob/main/docs/air-extensions.md).

**Codex**

Hutan’s current DTOs do **not** send instruction fields:

`crates/agents/src/codex/wire.rs:39-69`.

Add `developerInstructions`; leave `baseInstructions` absent by default. Do not use collaboration-mode developer instructions as a generic memory-update mechanism: upstream documents special replacement/default semantics for those mode settings.

**ACP generally**

Current Hutan session setup sends cwd and MCP servers, not an arbitrary system prompt:

`crates/agents/src/acp/mod.rs:315-397`.

Stable ACP prompt content is a user-turn channel. Label a fallback snapshot as host-provided knowledge and explicitly record its lower-priority transport; do not call it “system injection”.

### 5.3 Suggested external wire shapes

Capability-gated Claude setup:

```json
{
  "method": "session/new",
  "params": {
    "cwd": "/host/private-assistant-cwd",
    "mcpServers": [],
    "systemPrompt": {
      "mode": "append",
      "content": [
        {"type": "text", "text": "Hutan assistant policy and snapshot …"}
      ]
    }
  }
}
```

Codex setup:

```json
{
  "method": "thread/start",
  "params": {
    "cwd": "/host/private-assistant-cwd",
    "sandbox": "read-only",
    "approvalPolicy": "never",
    "developerInstructions": "Hutan assistant policy and snapshot …"
  }
}
```

Portable ACP fallback/delta:

```json
{
  "method": "session/prompt",
  "params": {
    "sessionId": "…",
    "prompt": [
      {"type": "text", "text": "The user's actual request"},
      {
        "type": "resource",
        "resource": {
          "uri": "hutan://context/snapshot-id/delta/turn-id",
          "mimeType": "text/plain",
          "text": "Hutan-added knowledge delta; base snapshot …"
        }
      }
    ]
  }
}
```

Use embedded resources only when advertised; otherwise send clearly delimited text. Capability absence must not cause a silent unsupported request.

---

## 6. Skills service and sharing

### 6.1 Use Agent Skills, not a Hutan-specific package format

The [official specification](https://agentskills.io/specification) defines:

```text
skill-name/
  SKILL.md
  scripts/       # optional
  references/    # optional
  assets/        # optional
```

`SKILL.md` has YAML frontmatter with required `name` and `description`. Optional standard fields include `license`, `compatibility`, string-valued `metadata` and experimental `allowed-tools`.

Progressive disclosure:

1. name/description at routing time;
2. body when activated;
3. supporting files on demand.

The specification recommends keeping the main skill below 500 lines and instructions below approximately 5,000 tokens.

**`allowed-tools` is not a capability grant.** Hutan’s policy remains authoritative. Claude-specific dynamic execution syntax must not be executed merely because Hutan reads the skill.

Anthropic’s repository is useful examples, not uniformly reusable open-source content: it explicitly distinguishes Apache-2.0 examples from source-available document skills.

Source: [anthropics/skills README](https://github.com/anthropics/skills).

### 6.2 Ownership and discovery

Use two source classes:

- **Hutan-owned**: packages in the daemon data directory; revisioned and editable through proposals.
- **External**: read-only discovery roots selected by the user.

Supported external-root adapters should include:

- Claude personal and workspace `.claude/skills`;
- OMP’s active native agent skills root;
- explicit extra roots.

Claude documents personal skills at `~/.claude/skills/<name>/SKILL.md`:

[Claude Code skills locations](https://code.claude.com/docs/en/skills#where-skills-live).

OMP documents native user skills at `~/.omp/agent/skills` and project skills under ancestor `.omp/skills`, but profiles and environment overrides alter the native root:

- [OMP skills](https://github.com/can1357/oh-my-pi/blob/main/docs/skills.md)
- [OMP configuration discovery](https://github.com/can1357/oh-my-pi/blob/main/docs/config-usage.md)

Therefore, do not hardcode `~/.omp/skills`, and do not search all agent configuration recursively. Resolve only documented skill roots, using the selected harness profile/environment or an explicit path.

Discovery does not:

- write configuration;
- install plugins;
- run scripts;
- evaluate dynamic skill substitutions;
- enable newly found packages automatically for unattended jobs.

### 6.3 Sharing strategy

Recommended default:

> **Read existing Claude/OMP skills in place; expose enabled Hutan skills through MCP.**

This provides shared usage without editing agent configurations.

For native harness loading outside Hutan, offer an explicit **Share with Claude/OMP…** action:

1. preview exact destination/config changes;
2. show whether the backend supports a custom directory or needs a package copy;
3. require user consent;
4. refuse collisions;
5. write only marked Hutan-managed artifacts;
6. retain a receipt and undo path.

Do not silently symlink all of Hutan’s evolving skills into `~/.claude/skills`. Such a link would turn a later Hutan skill update into a change in unrelated native-agent sessions.

Read-only external packages remain externally owned. **Edit in Hutan** means fork into Hutan’s owned store unless the user explicitly approves an external write.

### 6.4 Index and revision storage

```text
skill_sources(
  id PK, kind CHECK(hutan|claude|omp|directory),
  root_path, scope_id FK?,
  discovery_enabled, default_enable,
  read_only,

[Showing lines 401-600 of 1352. Use :601 to continue]
  created_at_ms
)

skills(
  id PK, source_id FK,
  package_path, declared_name, description,
  enabled, validation_state,
  current_revision,
  UNIQUE(source_id, package_path)
)

skill_revisions(
  skill_id FK, revision,
  package_hash,
  manifest_json,
  immutable_package_path,
  provenance_json,
  approval_id FK?,
  created_at_ms,
  PRIMARY KEY(skill_id, revision)
)
```

- Deduplicate identical packages by canonical identity/hash.
- Preserve differing same-name skills under source-qualified IDs; never silently pick an arbitrary file.
- Keep declared package names standard-compliant; UI qualification need not alter the package’s `name`.
- Record unsupported extension fields as compatibility warnings rather than silently executing them.
- Bound file counts/sizes; reject traversal, devices, sockets and escaping symlinks.
- Revalidate package hash when applying an approved update.
- Schedules pin skill revisions/hashes. A changed external package requires review before the next unattended run.
- `skill_view`/`skill_read_file` return provenance and revision alongside content.
- Loading a script is not running it; execution needs a separately authorized tool.

---

## 7. Advisor, Operator and approvals

### 7.1 Recommended meaning

| Capability | Advisor | Operator |
|---|---|---|
| Answer using permitted context | Yes | Yes |
| Read allowed Hutan state/memory/skills | Yes | Yes |
| Produce a plan or proposed knowledge change | Yes | Yes |
| Publish a memory/skill proposal into active knowledge | User approval/action only | User approval by default |
| Create/send/stop sessions and tasks | No | Policy-gated |
| Change schedules or external configuration | No | Policy-gated; grants/settings remain user-only |
| Approve its own request or widen its policy | Never | Never |
| Use a direct backend without tools | Chat only | Unsupported until tool loop exists |

Advisor may stage a **proposal**; it cannot apply it. This is not a hidden Operator action. The UI must distinguish draft/proposed state from active state.

Mode changes are user-only, reflected in the header and journal. Apply reductions immediately to daemon calls; rebuild a native harness session at a safe boundary when its tool surface cannot be narrowed live.

### 7.2 Effective policy is an intersection

```text
effective capabilities =
    daemon maximum
  ∩ assistant mode
  ∩ session grant
  ∩ schedule grant, when unattended
  ∩ delegated-child grant
  ∩ host/resource restrictions
  ∩ backend/native sandbox capability
```

A permissive `[control] agents = "allow"` must not override Advisor or a schedule’s narrower grant.

Keep the current exhaustive request classification, but enrich authorization with:

- allowed methods/tools;
- allowed workspace/session IDs and roots;
- allowed UI actions/settings namespaces;
- allowed secret references and purposes;
- outbound connector/destination restrictions;
- execution/token/cost/concurrency limits;
- expiry and policy revision.

Show approvals with the complete meaningful action, not only the current 400-character parameter summary. Bind approval to a normalized request hash, target resources, expected revisions and expiry.

Existing global policy and audit behavior:

`docs/control.md:49-78`.

### 7.3 Security prerequisite for unattended work

Today the ordinary socket trusts same-user connections; origin can be dropped or altered, and an invalid claimed origin falls back to user treatment:

`docs/control.md:33-47`.

A token added to the ordinary endpoint alone does not solve that: a process could reconnect without it.

Recommended design:

- provide a separate daemon-owned **restricted agent endpoint**;
- authenticate it with a per-session/run capability passed through an inherited channel or private bridge;
- make identity immutable for that connection;
- deny expired/invalid identity rather than treating it as user;
- sandbox the harness so it cannot reach the unrestricted user endpoint, credential stores or broader filesystem/network;
- route direct-provider tools in-process with the same restricted principal.

If a backend/platform cannot provide that boundary, report **cooperative policy only** and do not offer strong unattended execution guarantees. “Read-only sandbox” also does not mean “cannot read secrets”; read roots matter.

---

## 8. MCP surface: existing tools and proposed additions

### 8.1 Current inventory

The current table contains these tools:

| Group | Tools |
|---|---|
| Sessions/processes | `session_list`, `session_create`, `session_send_text`, `session_output`, `session_stop`, `session_restart`, `session_rename`, `session_kill`, `process_signal`, `agent_interrupt` |
| Chats | `chat_create`, `chat_send`, `chat_log` |
| Resources/approvals | `resources_list`, `approval_list` |
| Tasks | `task_add`, `task_list`, `task_start`, `task_review`, `task_done`, `task_cancel`, `task_reset` |
| Worktrees | `worktree_new`, `worktree_list`, `worktree_diff`, `worktree_file_diff`, `worktree_merge`, `worktree_commit`, `worktree_rm` |
| Workspaces/projects | `workspace_list`, `project_recent`, `workspace_create`, `workspace_assign`, `workspace_delete`, `workspace_open` |
| Notifications/history | `notification_list`, `history_search` |
| Desktop | `ui_actions`, `ui_layout`, `ui_run`, `ui_set_setting` |

Source: `crates/cli/src/mcp/table.rs:122-818`.

Current `workspace_assign` moves a **project**, not an individual session:

`crates/cli/src/mcp/table.rs:668-684`.

There is deliberately no approval-response tool.

### 8.2 Additions

| Proposed MCP tool | Daemon method | Purpose |
|---|---|---|
| `assistant_status` | `assistant.status` | Scope, backend capabilities, effective mode, snapshot, budgets and current work |
| `memory_list`, `memory_search`, `memory_get` | `memory.list/search/get` | Bounded curated-memory reads |
| `memory_propose` | `memory.propose` | Create/update/archive proposal with expected revision and provenance |
| `knowledge_proposal_list`, `knowledge_proposal_get` | `knowledge.proposal.list/get` | Inspect proposal state and eventual receipt |
| `skills_list`, `skill_view`, `skill_read_file` | `skills.list/view/read_file` | Progressive skill access |
| `skill_propose` | `skills.propose` | Validated package create/update/archive proposal |
| `schedule_list`, `schedule_get`, `schedule_runs` | `schedule.list/get/runs` | Inspect definitions, previews and results |
| `schedule_create`, `schedule_update`, `schedule_pause`, `schedule_run`, `schedule_remove` | corresponding `schedule.*` | Policy-gated schedule control; cannot create its own unattended grant |
| `session_assign` | `session.assign` | Move a session into/out of a task workspace |
| `history_session_read` | `history.session.read` | Bounded, cursor-based reading of an indexed external conversation |
| `delegation_start`, `delegation_status`, `delegation_cancel` | `delegation.*` | Durable parent/child execution and monitoring |
| `trace_list`, `trace_get` | `trace.list/get` | Redacted execution records, not private model reasoning |
| `secret_refs_list` | `secret.refs.list` | Only references authorized for this principal; never secret values |

Do not add a generic “execute arbitrary daemon request” tool. Explicit descriptors preserve discoverability, schemas and policy classification.

Host selection should be explicit in the MCP bridge’s connection/principal. An agent must not change the desktop’s active host and thereby redirect later requests. Cross-host delegation requires a specific authorized destination and visible transfer of context.

User-only methods—proposal decisions, capability grants, mode changes, source-root registration, credential configuration and sharing consent—remain absent from model tools.

### 8.3 Monitoring without polling storms

Delegation tools return IDs and a bounded status summary. The daemon subscribes to child session/task events and maintains durable status.

An optional bounded `delegation.wait {ids, after_revision, timeout_ms}` can reduce repeated polling. Completion notifications go to the UI immediately; prompting the parent model again is a separate, auditable, budgeted continuation.

A task’s native turn ending is not equivalent to tested/accepted work. Preserve the existing review/delivery state.

---

## 9. Schedules

### 9.1 Product behavior

Each schedule runs a **normal assistant execution session** using an explicit backend, context scope and capability policy.

The human-facing assistant conversation is not reused concurrently. Home gets a run card linking to the execution transcript.

Recommended initial supported forms:

- timezone-aware five-field cron;
- a UTC one-shot instant;
- fixed interval with explicit anchoring.

Natural-language entry may help fill the form, but the user approves the normalized expression, timezone and next five occurrences.

Do not add Hermes’ pre-run scripts, script-only jobs, output chaining and messaging gateways merely because Hermes has them. They expand the capability surface substantially. The schedule design should support ordinary prompts and pinned skills completely first.

### 9.2 Ledger and schema

```text
schedules(
  id PK, assistant_workspace_id FK, context_scope_id FK,
  name, prompt,
  schedule_kind, expression, timezone,
  backend_json,
  skill_pins_json,
  capability_grant_id FK,
  revision, enabled,
  next_due_at_ms,
  misfire_policy CHECK(skip|latest),
  overlap_policy CHECK(skip|queue_one),
  max_runtime_secs, max_tokens?, max_cost_micros?,
  created_at_ms, updated_at_ms
)

[Showing lines 601-800 of 1352. Use :801 to continue]

schedule_runs(
  id PK, schedule_id FK,
  scheduled_at_ms, schedule_revision,
  policy_revision, skill_pins_json, snapshot_id FK?,
  state CHECK(queued|dispatching|running|completed|failed|
              blocked_policy|interrupted|unknown|skipped|cancelled),
  session_id FK?,
  attempt, retry_of_run_id FK?,
  lease_owner?, lease_expires_at_ms?,
  started_at_ms?, ended_at_ms?,
  reason?, result_ref?,
  UNIQUE(schedule_id, scheduled_at_ms, attempt)
)
```

Claim due occurrences and advance `next_due_at_ms` in one transaction. The first attempt is uniquely keyed to the scheduled instant.

**Do not promise exactly-once external effects.** A daemon can crash after a model/tool request was accepted but before its result was recorded. Mark ambiguous dispatches `unknown`; do not blindly repeat them. Explicit retry creates a linked attempt. Daemon-owned mutating tools should use durable operation IDs to deduplicate retries.

Hermes’ ledger, catch-up and dead-owner accounting are valuable evidence for the failure modes:

`/Users/workie/proj/hermes-src/cron/AGENTS.md:24-37`.

### 9.3 Unattended policy

A schedule’s creation approval is not blanket approval for every future action.

Default unattended policy:

- reads only within explicit scope;
- no shell/write/native arbitrary tools;
- no new delegation unless explicitly granted;
- no memory/skill publication;
- no UI dependence;
- no arbitrary outbound delivery;
- only named secret references for named connector purposes;
- bounded runtime, tokens, cost and concurrency;
- no “allow always” learning from native permission prompts.

An action outside the grant returns a structured policy denial and ends or blocks the run according to its policy. Notify the user; do not leave an unattended process waiting ten minutes for an approval that cannot arrive.

Changed skill content, changed target roots, widened tool access or changed delivery destinations invalidates the grant and requires review.

### 9.4 Time, lifecycle and remote semantics

- Persist instants in UTC; keep an IANA timezone for cron interpretation.
- Define DST gaps as skipped, repeated local times as one selected occurrence; show the exact UTC/local preview.
- Never move a schedule to another host silently.
- Losing the desktop connection does not stop a remote host’s schedule.
- A stopped daemon does not execute jobs. If close mode stops the daemon, show **Schedules paused while Hutan is closed**.
- Do not silently enable login/background services when the user creates a schedule.
- On restart, apply the selected misfire policy; default `skip` avoids a burst of old work.
- If the vault/backend is unavailable, report the concrete blocked state; do not weaken policy or switch to an unrelated backend automatically.

---

## 10. Direct-provider backend: real tools or honest chat

### 10.1 Current limitation

The gateway is deliberately chat-only:

`docs/gateway.md:5-14,31-37`; `crates/gateway/src/types.rs:70-150`.

Registering an MCP server for an ACP agent does not give a direct-provider chat any tools.

### 10.2 Recommended implementation

Keep HTTP/provider translation in `hutan-gateway`; keep execution, state and policy in the daemon.

Extend gateway vocabulary with:

- tool definitions;
- structured assistant tool-call parts;
- tool-call argument streaming/assembly;
- tool-result messages with stable call IDs;
- provider-required opaque continuation data where necessary;
- explicit finish reason: final answer versus tool calls.

Daemon loop:

1. Assemble frozen context and permitted tool descriptors.
2. Stream one provider response.
3. Finish and validate complete tool calls; partial argument JSON is never executable.
4. Journal tool intent with stable call/operation IDs.
5. Authorize under the same `ControlService` principal as MCP.
6. Execute through shared daemon handlers.
7. Journal bounded/redacted results.
8. Send correctly correlated results to the provider.
9. Continue until final answer, cancellation, failure or an explicit budget limit.

Start with Hutan’s service/control tools, not an additional implementation of shell, browser and filesystem tools already supplied by native harnesses.

Use native provider protocols—OpenAI function calls, Anthropic tool-use/tool-result blocks, Ollama tools—not textual `<tool_call>` parsing.

Handle:

- multiple and parallel tool calls;
- cancellation while a tool waits for approval;
- interrupted/error tool results;
- provider-specific required continuation blocks;
- restart with outstanding calls;
- usage across all internal requests;
- non-idempotent dispatch uncertainty;
- safe context truncation that never separates a tool result from its call.

A provider/model’s tool capability is **unknown** until documented/configured or tested by an explicit capability fixture. An `openai-compatible` endpoint must not be assumed tool-capable merely because it streams text.

### 10.3 Avoid a duplicated tool registry

Move reusable descriptor/schema data out of the CLI-only table into `crates/protocol/src/tool_catalog.rs` or a similarly dependency-light module.

- CLI MCP projects that catalog.
- The direct runtime projects the same catalog.
- The daemon owns execution and authorization.
- Composite tools such as `session_output` use a shared daemon-facing implementation.
- Do not make the daemon depend on the CLI or start `hutan mcp` as a subprocess for every direct tool call.

---

## 11. Learning loop: explicit, scoped and receipt-backed

### 11.1 “Remember”

Provide a message/selection action:

```text
Remember…
  Scope: Global / This workspace / This conversation
  Type: Preference / Fact / Note
  [editable text]
  Source: selected message / manual
  [Save]
```

This operation can save user-authored text without a model call.

For natural-language “remember this”, the model may create a proposal. The transcript displays:

- **Proposed**, awaiting review;
- **Saved to Workspace memory · revision 3**, only after commit;
- **Not saved**, with denial/conflict/storage failure.

Hermes explicitly documents the common false-save problem:

`website/docs/user-guide/features/memory.md:67-88`.

### 11.2 “Learn from session”

A separate maintenance run receives:

- a fixed transcript boundary/hash;
- the user’s goal and selected scope;
- existing relevant memory and skill metadata;
- bounded source-reading access;
- only knowledge proposal tools.

It may:

- propose durable facts/preferences;
- create or improve a reusable skill;
- cite source turns and observed successful steps;
- identify uncertainty or failed approaches.

It may not:

- infer permission to publish;
- execute extracted commands merely to “learn”;
- copy secrets into a skill;
- treat a task’s unverified final claim as a successful procedure;
- recursively schedule another learning run;
- widen its own tool access.

Show a review batch with diffs and per-item approval. Apply each reviewed revision safely; conflicting items remain unapplied rather than overwriting newer knowledge.

Store:

```text
learning_runs(
  id PK, source_session_id FK,
  through_seq, transcript_hash,
  target_scope_id FK,
  backend_json, execution_session_id FK?,
  state, created_at_ms, ended_at_ms?
)
```

**Automatic learning is optional and off initially.** If later enabled, it should run after durable turn/session boundaries, consume explicit budgets and still produce proposals by default. ACP does not expose enough information to reproduce Hermes’ internal iteration-based review exactly.

---

## 12. What makes it a local “Jarvis”

The useful distinction is not a voice or an animated persona. It is **persistent knowledge + safe action + awareness of ongoing work**.

### 12.1 Deliver first

1. **Existing global hotkey** opens/focuses Home’s composer.
2. **Today** gathers pending approvals/questions, running delegations, failed schedules and completions.

[Showing lines 801-1000 of 1352. Use :1001 to continue]
3. **Delegate this task** creates/selects a task workspace, starts a chosen harness with explicit scope and a bounded brief, then links its status back.
4. **Monitor and intervene**: view transcript, inspect changes/resources, send a follow-up, interrupt or cancel—with policy.
5. **Recall**: search and read real prior sessions; attach evidence.
6. **Remember/learn**: reviewable knowledge with receipts.
7. **Notifications**: actionable, redacted and quiet when the relevant conversation is visible.
8. **Traces**: explain what ran, why it was permitted, what context was sent and what it cost.

Existing global-shortcut behavior is documented at `docs/assistant.md:145,153`; its current implementation is macOS-specific, with no equivalent non-mac implementation in the surveyed platform layer:

`docs/spikes/2026-10-18-hutan-model-lifecycle-transport.md:73-79`.

### 12.2 Voice is optional

Recommend:

- push-to-talk dictation, off by default;
- local speech recognition where available;
- editable transcript before sending;
- explicit remote-transcription consent;
- opt-in local speech playback;
- visible recording state and immediate stop.

Do not begin with a wake word or always-listening service. Voice must not become an approval bypass: destructive operations still require an explicit structured confirmation.

### 12.3 Traces and privacy

Record observable execution, not fabricated chain-of-thought:

```text
trace_id, span_id, parent_span_id,
host_id, workspace_id, session_id, turn_id,
operation/tool/backend,
start/end, status,
approval_id?, policy_revision?,
context_snapshot_id?,
tokens?, cost_micros?,
redacted attributes/result references
```

Span kinds: turn, context assembly, MCP call, authorization, tool execution, delegation, schedule occurrence and learning run.

Store locally with bounded retention. No OTLP export without explicit consent. Do not put secret values, full environment blocks or unbounded tool payloads in attributes.

The vault design is a separate prerequisite. Assistant tools receive `secret://…` references only; the daemon resolves them for an approved purpose. Encryption does not protect against every same-user malicious process while unlocked, nor prevent a model from exfiltrating ordinary readable personal data through an allowed network tool.

---

## 13. Concrete module and settings layout

### 13.1 Reuse existing crates

```text
crates/daemon/src/
  assistant.rs                 # scope/open/configure facade
  assistant_workspace.rs       # assistant workspace identity and migration
  assistant_context.rs         # snapshots, manifests, deltas, transmission policy
  memory.rs                    # memory/proposal service
  skills.rs                    # discovery, validation, versions and proposals
  schedules.rs                 # due queue, occurrence ledger and dispatch
  learning.rs                  # bounded maintenance runs
  delegation.rs                # parent/task/session relationships and status
  direct.rs                    # existing direct chat
  direct_tools.rs              # direct-provider tool loop
  control.rs                   # authoritative policy/grants
  mcp_wiring.rs                # restricted principal-aware bridge
  traces.rs                    # observable execution spans

crates/storage/src/
  memory.rs, skills.rs, schedules.rs, assistant_context.rs
  learning.rs, delegation.rs   # repositories; no harness behavior

crates/protocol/src/
  assistant.rs                 # status/mode/context DTOs
  memory.rs, skills.rs, schedule.rs, delegation.rs
  tool_catalog.rs              # shared descriptors

crates/agents/src/
  adapter.rs                   # typed opening context/capabilities
  acp/{mod,wire}.rs             # negotiated prompt extension/fallback
  codex/{mod,wire}.rs           # developerInstructions

crates/gateway/src/
  types.rs                     # structured calls/results/continuation
  openai.rs, anthropic.rs, ollama.rs

crates/desktop/src/workbench/
  home/                        # Home page and supporting rail
  knowledge/                   # memory/skill master-detail
  schedules/                   # editor and run history
```

Do not extract a new umbrella assistant crate until dependency boundaries actually require it.

### 13.2 Suggested settings

Retain current backend/model/context/idle keys. Add settings with clear daemon versus desktop ownership:

```toml
# daemon.toml
[assistant]
mode = "advisor"                  # user-only change
idle_stop_secs = 600              # existing
local_inference_only = false      # only offer enforceable configurations

[assistant.memory]
enabled = true
inherit_global_profile = true
share_with_task_agents = false
write_policy = "propose"          # user-only: propose | scoped_auto
profile_budget_chars = 2000
facts_budget_chars = 4000
workspace_budget_chars = 4000
delta_budget_chars = 8000

[assistant.skills]
enabled = true
external_discovery = false        # setup can enable selected roots
catalog_budget_chars = 12000

[assistant.learning]
automatic = false
max_concurrent = 1
max_turns = 8

[schedules]
enabled = false
max_parallel = 1
default_misfire = "skip"
default_overlap = "skip"

[assistant.runtime]
max_tool_rounds = 16
max_parallel_tool_calls = 4
```

These budget values are proposed defaults, not measured optima.

Store sensitive per-schedule grants and root registrations as typed database records, not giant settings allowlists. Per-backend memory transmission consent must be explicit and inspectable.

Desktop settings:

```toml
[assistant]
enabled = false                   # preserve current enable/disable semantics
global_shortcut = ""              # existing
home_rail = true
voice_input = false
voice_output = false
```

Do not change the current enablement default merely as a side effect of implementing Home. Direction decisions should separately approve any first-run default change.

A strict local-inference mode should initially accept only a direct backend with an explicitly local endpoint and suitable configuration. Do not claim that an external harness is local-only without knowing/enforcing its provider routing.

---

## 14. Hutan protocol additions and compatibility

### 14.1 Additive service capabilities

Allocate minors from the actual protocol baseline at implementation time; do not reserve a speculative exact version here.

Suggested capabilities:

```text
assistant.status
assistant.context
memory
skills
schedule
delegation
traces
assistant.operator
```

Example requests:

```json
{"id":1,"method":"assistant.status","params":{"session_id":"…"}}
```

```json
{
  "id":2,
  "method":"memory.propose",
  "params":{
    "scope":{"kind":"workspace","workspace_id":"…"},
    "operation":"update",
    "entry_id":"…",
    "expected_revision":2,
    "body":"…",
    "source":{"session_id":"…","from_seq":20,"through_seq":28},
    "operation_id":"…"
  }
}
```

```json
{
  "id":3,

[Showing lines 1001-1200 of 1352. Use :1201 to continue]
  "method":"schedule.create",
  "params":{
    "assistant_workspace_id":"…",
    "name":"Daily brief",
    "schedule":{"kind":"cron","expression":"0 9 * * 1-5","timezone":"Europe/London"},
    "prompt":"Summarize the authorized workspace's pending work.",
    "backend":{"kind":"omp","model":"…"},
    "capability_grant_id":"…",
    "skill_pins":[],
    "misfire_policy":"skip",
    "overlap_policy":"skip",
    "operation_id":"…"
  }
}
```

User-only methods:

```text
assistant.mode.set
knowledge.proposal.decide
capability_grant.create/update/revoke
skills.source.add/remove
skills.share
```

Events:

```text
memory.changed
knowledge.proposal.updated
skills.changed
schedule.updated
schedule.run.updated
delegation.updated
assistant.context.updated
```

Use the existing `epoch`/`seq` envelope. List/detail snapshots carry the event watermark; clients subscribe before snapshot and apply only later events. Events should carry identifiers/revisions and small summaries, not full skill packages or transcripts.

ADR 0002 requires capability-gated additive features, bounded frames/queues and replay/snapshot semantics:

`docs/adr/0002-ipc-transport-and-framing.md:20-39`.

### 14.2 `kind = assistant` is not automatically minor-compatible

`WorkspaceKind` is a **closed serde enum**:

`crates/protocol/src/messages.rs:474-481`.

An older client can fail to deserialize a new `"assistant"` variant. `SessionInfo.workspace_id` is required, and existing assistant scope derives from Home versus non-Home:

`crates/protocol/src/messages.rs:829-836`; `crates/protocol/src/assistant.rs:4-6`.

Two valid choices:

| Choice | Compatibility consequence |
|---|---|
| Minor with negotiated `workspace.assistant` projection | Every response, event and replay sent to an old client must project assistant workspaces into understood legacy shapes, preserving consistent IDs and scope behavior. Requires comprehensive dual-wire tests. |
| **Major for the workspace semantic cutover** | Simpler and honest: old clients reject during handshake; upgrade CLI/desktop together. No long-lived legacy workspace projection. |

**Recommendation:** service APIs can be additive minors; use a **major** for the clean assistant-workspace/scope cutover if the project does not already implement a general capability-aware workspace projection for the broader task-workspace redesign.

This is an exception to the direction document’s optimistic “all personal-assistant changes are minors”, not a framing redesign. The frame codec and transport remain unchanged.

Adding optional DTO fields alone is minor-compatible. Changing global assistant membership, introducing a closed enum variant or altering old scope semantics without projection is not.

---

## 15. Migration

1. Add storage migrations and back up before the workspace cutover.
2. Rebuild the workspace table’s kind constraint to include `assistant`, following the existing table-rebuild pattern:
   `crates/storage/migrations/0008_projects.sql:8-10,16-41`.
3. Create the global assistant workspace and any required scoped assistant workspaces.
4. Move **only assistant sessions**; keep session IDs, journals, native conversation IDs, creation order, drafts and model choices.
5. Keep ordinary Home/unassigned sessions untouched.
6. Preserve the scoped target in an explicit relation, not by looking at current UI selection.
7. Preserve existing backend configuration and context restrictions.
8. Set all migrated assistants to Advisor; do not grant schedules, skill sharing or automatic learning.
9. Preserve existing native conversations with their actual old prompt state. Mark them as legacy-context conversations until a fresh conversation establishes a new snapshot; do not claim retroactive prompt replacement.
10. Redirect existing OpenAssistant, history hits, Send to Assistant and global-hotkey routes to Home while preserving their target scope.
11. Migrate saved panel placements intentionally; remove obsolete left-only Assistant placement behavior and update affected layout fixtures/docs.
12. Do not import native harness memory automatically. Offer scoped import with provenance and preview.
13. Do not modify Claude/OMP configuration during migration.
14. Older daemons expose today’s assistant; newer clients hide unavailable capabilities rather than fabricating empty successful pages.
15. Document downgrade behavior: an older daemon must refuse an unsupported storage schema, not rewrite it.

The existing requirement to preserve default-assistant ordering for equal timestamps remains important:

`docs/assistant.md:22-24`.

---

## 16. Risks and mitigations

| Risk | Required mitigation |
|---|---|
| Persistent prompt injection through memory/skills | Provenance, proposal review, typed scopes, bounded content, no policy elevation from text |
| Local storage mistaken for local inference | Per-backend disclosure; transmission controls; honest enforceability of local-only mode |
| Native harness bypasses MCP policy | Restricted endpoint plus actual sandbox; cooperative-only label otherwise |
| Advisor silently acts through existing MCP | Enforce mode in daemon authorization and filter catalog as defense in depth |
| Approval applies to changed content | Expected revisions, content hashes and normalized request binding |
| “Saved” is only model prose | UI receipts generated from committed storage events |
| Resumed conversation has stale memory | Recorded snapshots/deliveries, explicit deltas, fresh-context action |
| External skill update changes scheduled behavior | Pinned package hash/revision; reapproval on change |
| Cron duplicates an external effect | Occurrence ledger, operation IDs, explicit unknown state; no exactly-once claim |
| App closes and schedules stop unexpectedly | Show lifecycle dependency; no silent background-service enablement |
| Cloud/provider failure causes unexpected backend switch | Pin unattended backend; require policy-compatible explicit fallback |
| Excess context and idle resource use | Bounded catalogs/memory, progressive disclosure, existing assistant sleep |
| Same-user malware steals unlocked data | Honest threat model; vault references; minimal process read/network access |
| Observability leaks private content | Redaction before storage, bounded attributes, opt-in export, retention controls |

---

## 17. Slice breakdown and acceptance tests

These are implementation slices, not claims that tests were run.

| Slice | Deliverable | Acceptance criteria |
|---|---|---|
| **PA-0: ADRs and contracts** | Workspace/scope, policy principal, context injection and schedule semantics | Compatibility decision documented; exact backend capability matrix; no implied universal ACP system support |
| **PA-1: Assistant workspace** | Storage/migration, global/scoped identity and ordinary session membership | Repeated open creates one workspace/default; equal-timestamp ordering survives restart; non-assistant sessions untouched; per-workspace context cannot cross target |
| **PA-2: Home page** | Center chat, Today rail, scope/backend/mode controls, navigation migration | Keyboard-only send/attach/review; drafts survive scope/page switching; narrow layout usable; hotkey/history/send-to-agent reach correct scope; hidden pages do no polling work |
| **PA-3: Restricted principal and modes** | Advisor/Operator daemon enforcement, grants and restricted MCP wiring | Advisor mutation denied even under global allow; forged/expired run identity never becomes user; agent cannot approve/widen grant; sandboxed backend cannot reach unrestricted socket; cooperative-only configurations labeled |
| **PA-4: Memory service** | Revisioned entries, proposals, receipts, read/search UI/MCP | Concurrent update conflicts; stale approval cannot overwrite; storage failure emits no saved receipt; rejected entries never inject; secret-shaped input refused; export/import preserves provenance |
| **PA-5: Context adapters** | Snapshot/delta assembly and OMP/Claude/Codex routes | OMP receives file-based append without personal argv text; Claude extension negotiated or fallback used; Codex sends developer instructions; deletions represented; wake/resume keeps correct snapshot; unsupported fields never sent blindly |
| **PA-6: Skills service** | Agent Skills validation, read-only discovery, versions/proposals | Standard valid packages load; malformed/escaping packages refuse; same-name variants visible; no scripts execute during discovery; external files/config unchanged; package hash change invalidates scheduled pin |
| **PA-7: Explicit learning** | Remember and bounded learn-from-session review | Remember works without model; maintenance has only permitted tools; source transcript boundary fixed; fake model “saved” text creates no badge; accepted batch yields real revisions; conflict leaves old data intact |
| **PA-8: Delegation and monitoring** | Durable parent/task/session links and event-driven status | Child runs in selected task workspace with bounded context; child grant cannot exceed parent; restart preserves links; completion is not acceptance; cancel behavior explicit; no polling storm |
| **PA-9: Schedules** | Cron preview, ledger, policies and run history | Fake-clock tests cover timezone/DST, missed runs and overlap; crash at claim/dispatch/result boundaries produces no duplicate automatic effect; unexpected tool denied unattended; daemon-off state honest; revoked grant stops future runs |
| **PA-10: Direct-provider tools** | Structured provider calls/results and shared dispatcher | Mock OpenAI/Anthropic/Ollama fixtures cover fragmented args, multiple calls, failures, cancellation and continuation; no call executes before valid completion; usage sums correctly; chat-only models remain labeled |
| **PA-11: Traces and notifications** | Linked execution spans, redaction and actionable notices | Trace follows parent→delegation→tool→approval; no secret values in DB/log/index/export fixtures; retention works; reconnect does not duplicate notifications; no desktop does not block headless execution |
| **PA-12: Optional voice** | Push-to-talk and opt-in playback | No recording before consent; recording state visible; cancel deletes temporary audio; remote transcription requires explicit choice; voice cannot approve destructive requests |

Cross-cutting tests:

- protocol golden fixtures and unknown-field handling;
- old/new client compatibility or explicit major mismatch;
- subscribe/snapshot/replay at every new service boundary;
- authorization equivalence across desktop, CLI, MCP and direct tools;
- all test data under isolated temporary homes;
- mocked inference only for automated tests;
- real-backend conformance runs only by explicit opt-in, never paid calls in routine tests.

## Bottom line

Hutan becomes a convincing local personal assistant when it can **remember with evidence, act with bounded authority, delegate into visible task workspaces, watch the work and explain what happened**.

The right foundation is already present: daemon-owned sessions, cross-harness history, MCP wiring and approvals. Extend that foundation rather than importing another runtime.

Choose **Advisor by default**, make Operator a deliberate capability change, keep persistent writes receipt-backed, and treat backend differences and unattended security as real engineering boundaries—not details a system prompt can solve.