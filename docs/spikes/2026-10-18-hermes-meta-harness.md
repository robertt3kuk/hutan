# Hermes Agent as a meta-harness reference (NousResearch/hermes-agent, MIT)

Read-only scout report, 2026-10-18. Official sources only: stablyai/orca (onorca.dev, installed Orca's package.json), herdrdev/herdr (herdr.dev, Cargo.toml), NousResearch/hermes-agent (hermes-agent.nousresearch.com). Feeds docs/spikes/2026-10-18-direction.md.

Paths below are relative to **Hermes** `/Users/workie/proj/hermes-src`, except explicitly prefixed **Hutan**. Findings are source inspection only; no execution or changes.

## Summary

Hermes is **a persistent personal agent wrapped around its own agent runtime**, not merely a chat application. Its distinguishing loop is:

**work → save environment/user facts → author/improve reusable skills → recall previous sessions → start the next task with accumulated knowledge**, with scheduled and messaging entry points to the same core.

Most persistence and product surfaces can move into Hutan independently of the selected harness. The difficult boundary is **automatic learning and runtime control**: Hermes implements those inside its own turn lifecycle. Hutan should retain ownership of storage, context policy, schedules, MCP capabilities, and cross-session history, while letting the external harness own actual inference and native tools.

**Important discovery:** Hermes is **both an ACP agent and an ACP client**. Its outbound ACP client uses a textual tool-call bridge, however—not a clean harness-independent host architecture.

## 1. Product and “grows with you” loop

| Area | Concrete implementation |
|---|---|
| Product | Same core across CLI, messaging gateway, TUI, Electron desktop; persistent learning, scheduled jobs, delegation, terminal/browser tools. `AGENTS.md:10-25`; product positioning and advertised loop `README.md:19-29`. |
| Curated memory | Profile-local `~/.hermes/memories/MEMORY.md` for environment facts; `USER.md` for identity, preferences, communication/work style. Default caps **2,200 / 1,375 characters**. Markdown-ish multiline entries separated by **`§`**; capacity metadata appears in prompt. `website/docs/user-guide/features/memory.md:13-57`; `tools/memory_tool.py:39-61`. |
| Writing memory | Native `memory` tool: `add`, substring-span `patch`, `remove`, atomic batch operations; explicit target `memory` or `user`. No normal `read` action because content is preloaded. Capacity overflow returns an error and asks the model to consolidate, rather than silently compacting. Writes can be staged for approval; matched entries are pinned so approving an old request cannot mutate a different current entry. `website/docs/user-guide/features/memory.md:90-138,181-210`; `tools/memory_tool.py:64-121`. |
| Reading memory | Frozen **system-prompt snapshot** at session creation; writes persist immediately but do not mutate the running prefix. Tool responses expose live state. Long-running sessions therefore need meaningful boundaries to benefit from new facts. `tools/memory_tool.py:1-5`; `website/docs/user-guide/features/memory.md:38-67`; assembly `agent/system_prompt.py:522-547`. |
| Conversation recall | `session_search` searches SQLite history with FTS5 and returns actual messages: discovery, anchored scroll, session read, browse. Deduplicates by lineage, hides internal/subagent/tool sessions, demotes cron history relative to human conversations. **Current code explicitly makes no LLM calls**, despite README’s older “LLM summarization” wording. `tools/session_search_tool.py:1-51`. |
| Rich user modeling | External memory providers supplement built-in files and may inject their own prompt block/tools. Session-end ingestion is serialized with session switching to prevent cross-session misattribution. Honcho is advertised as dialectic user modeling, not the format of built-in `USER.md`. `README.md:26`; `agent/system_prompt.py:533-547`; `agent/memory_manager.py:606-637`. |
| Skills | User-visible source of truth `~/.hermes/skills/`; bundled skills are copied there; hub-installed and agent-created skills join them. Agent Skills-compatible packages: lean `SKILL.md`, optionally `references/`, `templates/`, `scripts/`. Progressive disclosure: index/routing context first, full skill via `skill_view` when relevant. `website/docs/user-guide/features/skills.md:7-13,120-153`; `agent/prompt_builder.py:1358-1376`. |
| Skill creation/improvement | Tools `skills_list`, `skill_view`, `skill_manage`; prompt instructs recording nontrivial workflows, patching faulty skills, updating missing steps before finishing. `/learn` turns arbitrary supplied sources into skill-authoring work in a **normal agent turn**, not a separate distillation engine. Existing targets require read-before-write. `agent/prompt_builder.py:257-262,1373-1376`; `agent/learn_prompt.py:1-6,171-186`; `agent/background_review.py:489-503`. |
| Automatic learning | Turn counters trigger background memory/skill review **after delivering the user’s answer**. Reviewer forks an agent on a transcript snapshot; uses bounded tool access; memory routing explicitly separates user attributes from environmental facts. Another curator maintains skill lifecycle/consolidation, with archival and rollback rather than destructive deletion. `agent/turn_context.py:758-767`; `agent/turn_finalizer.py:764-792`; `agent/background_review.py:367-378,1099-1115`; `agent/curator.py:1-7,364-435`. |
| SOUL/persona | Profile-home `SOUL.md` is primary system identity, replacing the default identity block. Current default is a concise style/persona instruction, not a knowledge store. SOUL has different trust treatment from checkout-supplied instructions; user-authored SOUL is warned-but-loaded, project instructions can be blocked by injection scanning. `SOUL.md:1`; `agent/system_prompt.py:550-556`; `agent/prompt_builder.py:83-96,1541-1561`. |
| Scheduled jobs | Per-profile `cron/jobs.json`; output archives `cron/output/{job_id}/{timestamp}.md`. Supports durations, recurring phrases, cron expressions, ISO one-shots; skill/model/provider/workdir overrides; pre-run scripts; script-only jobs; prior-job-output chaining; messaging delivery. `cron/jobs.py:1-3,78-90`; `cron/AGENTS.md:8-16`. |
| Scheduler execution | Durable occurrence accounting, per-home locking, gateway ownership, inactivity watchdog, catch-up policy. Job prompt loads explicit skills and bounded previous outputs; results can feed later runs. **Current code loads SOUL and built-in memory, disables background review**: `skip_memory=False`, `load_soul_identity=True`, `skip_background_review=True`. `cron/AGENTS.md:18-40`; `cron/scheduler_prompt.py:132-160,193-240,286-355`; `cron/scheduler.py:2520-2524`. |
| Messaging | Adapter-based gateway: Telegram, Discord, Slack, WhatsApp, Signal and others; authz, per-conversation sessions, commands, streaming delivery, media/voice, proactive completion events. Separate gateway can survive desktop exit. `README.md:25,147-159`; `gateway/AGENTS.md:6-14,20-26,30-66,79-85`. |
| Subagents | `delegate_task` creates child `AIAgent`s with fresh context, separate task IDs and scoped terminal/file state, inherited-but-filtered capabilities, focused goal/context prompts; parallel batch support; parent sees summary rather than full intermediate reasoning/tool traffic. This is context/runtime isolation, not necessarily an OS security sandbox. `tools/delegate_tool.py:1-10,25-58`. |
| Tools | Core includes web/search, files/patch, terminal/processes, browser automation, vision/image/audio, memory, skills, history, clarification/todos, Python RPC execution, delegation, schedules, kanban, connectors. Desktop sessions add pane/terminal/browser preview/focus/theme tools. Availability is platform/toolset/config gated. `toolsets.py:6-41,80-144`. |
| Providers/models | Registry of `ProviderProfile`s drives authentication, model listing, transport kwargs, routing, metadata, auxiliary models. Provider plugins supply protocol-specific preprocessing/body/kwargs hooks. This abstracts **inference providers**, not only ACP harnesses. `providers/README.md:3-24,28-52,63-75`. |

**Source drift to avoid copying:** `cron/AGENTS.md:92` says cron skips memory, but executable code says the opposite (`cron/scheduler.py:2522`). Likewise README’s summarized recall description is stale relative to `tools/session_search_tool.py:1-8`.

## 2. ACP: agent side and client side

### Inbound: Hermes as an ACP agent

`acp_adapter/server.py:1` explicitly describes an ACP agent server. It wraps synchronous Hermes `AIAgent` execution in asynchronous ACP, running inference/tool work off the event loop.

- **Advertised:** session load; image prompts; fork/list/resume capabilities. `acp_adapter/server.py:534-555`.
- **Lifecycle:** new/load/resume/cancel/fork/list; models and modes in session responses; available commands and usage updates. `acp_adapter/server.py:576-620,622-689`.
- **Persistence/replay:** loaded history is replayed as user/assistant text, thoughts, tool-start/tool-completion, todo/plan updates, compaction metadata. `acp_adapter/server.py:95-165,576-602`.
- **Prompts:** invokes Hermes itself, streams events, finalizes turn/history/usage. `acp_adapter/server.py:824-826,954-1012`.
- **Model switching:** real per-session model change. **Mode/config support is weaker than a policy engine:** mode setter describes persisting editor-requested mode; config setter acknowledges an absence of a typed ACP config surface. `acp_adapter/server.py:1028-1083`.
- **Client MCP servers:** accepts stdio/HTTP/SSE configs, registers them into the agent’s tool surface and refreshes tools. `acp_adapter/server.py:168-171,433-435,617-627`.
- **Permissions:** dangerous-command approvals bridge to ACP permission requests; allow-once→`once`, allow-always→`always`, rejection→`deny`; failures/timeouts deny. File edits also have ACP proposal/approval handling. `website/docs/developer-guide/acp-internals.md:105-127`; `acp_adapter/edit_approval.py:196-200`.
- **Tool presentation:** native Hermes tool executions become ACP tool kinds/content, including patch/write diffs, terminal text, previews, bounded results. ACP notifications are the **UI projection of execution**, not a way for a client to intercept arbitrary native tool dispatch. `website/docs/developer-guide/acp-internals.md:119-145`.

### Outbound: Hermes as an ACP client

`agent/copilot_acp_client.py:1-5,280-310` implements an OpenAI-compatible facade over an ACP subprocess; command/args are configurable. Each completion starts a short-lived ACP session and sends formatted conversation text.

Crucial mechanics:

- Hermes flattens system/user/assistant/tool context into a textual prompt.
- It serializes selected Hermes function schemas into prompt text.
- It requests `<tool_call>{OpenAI-shaped JSON}</tool_call>` responses and parses them back for Hermes’ own loop.
- It distinguishes autonomous agents with their own filesystem/execution tools from clients without native tools, forwarding only nonoverlapping Hermes capabilities where appropriate.

Evidence: `agent/copilot_acp_client.py:45-56,288-300,334-340`; `agent/acp_openai_bridge.py:1-8,25-29,111-121`.

**Conclusion:** both directions exist, but outbound ACP is a compatibility transport retaining a **Hermes-owned outer loop**. It is not equivalent to letting a durable external ACP agent run normally with host services exposed through MCP.

## 3. Desktop screens and principal UI

Current desktop is not a single “Home dashboard.” `/` is **new chat**; separate capabilities, automation, learning, management pages surround it.

| Screen | Main components / evidence |
|---|---|
| Home/new chat and session chat | Persona-sensitive introductory copy/wordmark; threaded conversation; tool/result UI; composer; model/reasoning selection; attachments/files/folders/images; voice hooks; cancel/steer; edit/reload/branch/restore; transcript loading/backfill; profile/session controls. `apps/desktop/src/components/chat/intro.tsx:18-44,109-130`; `apps/desktop/src/app/chat/index.tsx:1-96,98-123`. |
| Capabilities | Searchable, scoped master/detail page with tabs **Skills, Toolsets, Connectors, Plugins**; profile/connection selector; refresh and live counts. Hub browsing is embedded under Skills. `apps/desktop/src/app/capabilities/index.tsx:19-58,67-109,139-153`. |
| Skills management | Installed list, enable toggles, usage counts, sorting, bulk disable-unused; official catalog with install buttons; detail pane; code editor/save; archive confirmation. `apps/desktop/src/app/capabilities/skills/skills-tab.tsx:270-390`. |
| Memory/learning visualization | **Star Map**, not a standalone `/memory` route: graph of learned skills and memory entries, radial time axis, timeline/legend, imported/exported graph support. Nodes support edit and delete/archive dialogs. `apps/desktop/src/app/starmap/index.tsx:11-57`; `apps/desktop/src/app/starmap/node-context-menu.tsx:21-44,65-109`; data sources `agent/learning_graph.py:1-6,161-171`. |
| Memory settings | Persistent-memory enable/profile-enable, size caps/provider selection; context engine/compression controls; provider connection/configuration UI. `apps/desktop/src/app/settings/constants.ts:770-775`; `apps/desktop/src/app/settings/config-subpages.ts:63-67`; `apps/desktop/src/app/settings/memory/{connect,provider-config-modal,provider-config-panel}.tsx` (components located, not inspected in depth). |
| Settings | Subpages cover appearance/theme/typography/window/chat; main/auxiliary/MoA/fallback models; chat/attachments; workspace/shell/files; approvals/privacy/checkpoints; browser/network; memory/context; voice/transcription/speech; runtime/tools/terminal/delegation; gateway/devices/updates; keyboard; notifications; archives; credentials/vault. `apps/desktop/src/app/settings/subpages.ts:43-87`. |
| Other first-class screens | **Command Center, Session Import, Messaging, Webhooks, Artifacts, Cron, Profiles, Agents**, plus Settings/Capabilities/Star Map. Exact route enumeration `apps/desktop/src/app/routes.ts:11-24,63-77`. Those management screens’ internals were not individually inspected. |

## 4. Host-owned, harness-independent design for Hutan

The following is a proposed mapping **[INFERENCE]**, grounded in the boundaries above.

### What can reside entirely in `hutand`

1. **Memory/profile/persona storage and policy**
   - Scope by global/Home, workspace, optionally named persona.
   - Store bounded curated entries with IDs, revisions, provenance and approval state; Markdown export/import is optional, not a requirement to copy Hermes’ delimiter format.
   - Persona is user-controlled configuration; retrieved history, skill bodies, gateway payloads are not automatically equally trusted.
   - MCP: read/query memory plus add/patch/remove or proposal/approval operations.

2. **Skills service**
   - Own discovery/index, package files, enablement, versions, provenance, archival and read-before-write.
   - MCP: list/search/view/manage skills; host can resolve explicit skill invocation before submitting a turn.
   - Inject only compact routing metadata initially; load bodies on demand. Hermes’ current index-delta mechanism delivers changes as per-turn user notes rather than rebuilding the prefix: `agent/skills_index_delta.py:1-8`.

3. **Schedules**
   - Own schedule definitions, occurrence ledger, leases, execution sessions, job history, output chaining, delivery.
   - A due job asks the chosen harness to run a normal turn with explicit skill and previous-output context.
   - Durable scheduling must not depend on a GUI remaining open or a particular model process remaining alive.

4. **Cross-harness history**
   - Keep Hutan’s existing cross-source FTS5 index; expose bounded search/read/scroll through MCP.
   - Unlike Hermes, Hutan already indexes external harness transcripts, not only its own agent database.

5. **Hutan control tools**
   - Existing MCP covers sessions, chats, tasks, workspaces, history, UI. Extend that surface rather than adding a second tool protocol.

### How context actually reaches a harness

| Context | Hermes today | Hutan host equivalent |
|---|---|---|
| Persona | Primary system identity | Adapter-specific appended system instructions where supported; clearly scoped host context otherwise. Do not assume ACP standardizes arbitrary system-prompt replacement. |
| Curated memory/profile | Frozen system-prompt blocks | Snapshot assembled at session creation; bounded per-turn refresh/delta or explicit MCP read when stale. |
| Skill catalog | System routing/index guidance | Compact host-provided skill index and tool guidance; selected skill bodies via MCP or explicit attachment. |
| Explicit skill invocation | Skill text converted into invocation context | Host resolves command and attaches selected content to the submitted turn. |
| Cron context | Assembled job prompt with skills, script output, previous outputs | Host-generated job prompt sent to selected ACP/native harness session. |
| Historical recall | Native `session_search` | Existing `history_search` plus bounded session read/scroll extensions as needed. |
| Persistent writes | Hermes-native tool functions | MCP tools calling daemon-owned services with existing agent-origin authorization. |

**Existing Hutan constraint:** the Assistant is currently deliberately read-only; OMP is launched with restricted tools, Claude in plan mode, Codex read-only, direct provider without tools. Persona text even says “You cannot run commands or change files.” This cannot silently become an empowered personal agent without updating the product/policy boundary. **Hutan** `docs/assistant.md:54-62`; `crates/daemon/src/assistant.rs:65-72`.

## 5. Risks and approximations

| Risk / model-loop dependency | Host approximation **[INFERENCE]** |
|---|---|
| Hermes tracks internal tool iterations and injects continuation/stop instructions. ACP host observes the turn, not every inference decision or private state. `agent/turn_iteration_prep.py:165-167`; `agent/turn_stop_gates.py:1-6,135-145`. | Trigger learning from durable **turn completion/session boundaries**, not guessed internal model steps. Do not recreate an outer tool loop around every ACP response. |
| Background learning forks Hermes `AIAgent`, reuses schemas/cache assumptions and restricts dispatcher access. `agent/background_review.py:170-180,924-927,1099-1115`. | Run a separate bounded review session on the completed transcript, with only memory/skill proposal MCP tools. Keep user turn and maintenance turn distinct; require real tool-write receipts before reporting saved knowledge. |
| Skill extraction is a model behavior, not automatic persistence magic. Model may claim it saved facts without calling a tool. `website/docs/user-guide/features/memory.md:69-89`. | Host records writes as structured events; UI presents “saved” only on daemon success. Offer explicit “remember” and “learn from session” controls. |
| External harness has its own system prompt, tools, compaction, skills, permissions, subagents. Hermes text-tool bridge duplicates ownership. `agent/acp_openai_bridge.py:1-8`. | Prefer normal harness-native MCP calls. Do not ask the harness to print fake OpenAI calls for host parsing. Define which skills and tools are host-owned versus harness-native. |
| Frozen memory becomes stale in long-lived chats; refreshing the whole prefix destroys caching. `website/docs/user-guide/features/memory.md:55-67`. | Versioned snapshots, lightweight per-turn deltas, explicit fresh sessions; retain snapshot/version on the journal so context is explainable. |
| Host memory can cross workspace, persona or gateway boundaries; maintenance introduces concurrent writers. Hermes documents profile collisions and several scope bugs. `website/docs/user-guide/features/memory.md:22-24`; `cron/AGENTS.md:37-68`. | Daemon is the serialization authority; bind every read/write/job/tool request to stable scope and origin, never ambient UI selection. |
| Scheduled execution has no human present and may run powerful tools; skill/script/prior-output context is an injection path. `cron/scheduler_prompt.py:367-389`. | Explicit unattended capability policy, preauthorized operations, provenance-labelled data, approval/refusal semantics; never interpret user approval for a schedule as blanket future tool approval. |
| Direct provider backend presently has no tool loop. **Hutan** `docs/assistant.md:54-60`. | Either implement a direct-provider runtime with tools, or truthfully expose it as a reduced-capability backend. ACP plumbing alone does not give direct API models tools. |

## Concrete mapping: Hermes → Hutan

“New” means required for this design, not a claim that an exhaustive repository-wide absence audit was performed.

| Hermes feature | Hutan equivalent | Exists / new | Hutan refs |
|---|---|---|---|
| Personal-agent entry point | Global/Home Assistant with workspace counterparts | **Exists; evolve behavior/UI** | `docs/assistant.md:18-30`; `crates/daemon/src/assistant.rs:65-72,139-158` |
| Harness/model selection | OMP / Claude / Codex / provider per scope | **Exists** | `docs/assistant.md:28-62` |
| SOUL/persona | User-controlled, scoped identity appended through each backend | **New configurable identity; prompt seed exists** | `crates/daemon/src/assistant.rs:65-72` |
| MEMORY.md / USER.md | Daemon-owned curated environment facts and user profile | **New service/storage/tools** | Integrate with `crates/daemon/src/assistant.rs`; context assembly described in `docs/assistant.md:9-15` |
| Context injection | Redacted bounded host-context snapshot, explicit attachments and deltas | **Attachment machinery exists; persistent-context policy new** | `docs/assistant.md:76-110`; `crates/daemon/src/assistant.rs:8-14` |
| Skills catalog/view/manage | Host skill package service and MCP API; UI management | **New** | Extend MCP declaration pattern `crates/cli/src/mcp/table.rs:1-31,89-95` |
| Learn/review/curation loop | Turn-completion maintenance sessions producing audited proposals | **New** | Build above Assistant/harness sessions; no existing implementation asserted |
| Session recall | Cross-harness FTS5 history and MCP search | **Exists; richer bounded reading may need extension** | `crates/index/src/lib.rs:5-9`; `crates/index/src/search.rs:66-68`; `crates/cli/src/mcp/table.rs:726-734` |
| Cron/scheduled jobs | Daemon scheduler dispatching selected harness sessions | **New scheduler/occurrence ledger; reuse task/chat execution** | `crates/cli/src/mcp/table.rs:287-332,373-435` |
| Subagents/delegation | Child chat sessions or durable dependency tasks across harnesses | **Primitives exist; summarization/parent-child orchestration policy new** | `crates/cli/src/mcp/table.rs:287-332,373-435` |
| Native Hermes tools | Harness-native tools plus Hutan control MCP | **MCP foundation exists** | `crates/cli/src/mcp/table.rs:122-160,287-332,626-705,726-734` |
| ACP client MCP registration | Inject `hutan mcp` into ACP new/load; Codex native config | **Exists** | `crates/daemon/src/mcp_wiring.rs:1-7,36-59,70-72` |
| Permissions/write approvals | Agent-origin daemon requests and mutating MCP tool approvals | **Exists; extend to memory/skills/schedules** | `crates/cli/src/mcp/table.rs:24-31,39-47`; `crates/daemon/src/mcp_wiring.rs:6-7,38-45` |
| Messaging gateways | External channels routing to scoped Assistant sessions | **New integration surface** | Reuse Assistant scope/backend selection `docs/assistant.md:18-30` |
| Memory/skills/cron desktop pages | Home-adjacent management surfaces and persistent-state visibility | **New UI product scope** | Existing Assistant desktop contract referenced by `docs/assistant.md:1-3`; no additional desktop code refs inspected |

**Bottom line:** adopt Hermes’ **persistent-layer product contract**, not its runtime wholesale. Hutan already has the strongest prerequisite Hermes does not center on: daemon-owned, cross-harness sessions and control MCP. Add persistent knowledge, skill packages, a durable scheduler and an explicit learning workflow around those existing boundaries.