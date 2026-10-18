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
…
)
```
…

…

…
Choose **Advisor by default**, make Operator a deliberate capability change, keep persistent writes receipt-backed, and treat backend differences and unattended security as real engineering boundaries—not details a system prompt can solve.

[Backing file: ~/.omp/agent/sessions/-proj-heeler/2026-10-15T13-22-59-393Z_01a13fba-f4c1-715f-8096-a302709d439a/ResearchAssistant.md (65.2KB). Use agent://ResearchAssistant:raw:1-3000 for bounded verbatim chunks, agent://ResearchAssistant:1-3000 for numbered exploration, and the backing file path for search/copy workflows.]

[Showing lines 1-300 of 1352. Use :301 to continue]