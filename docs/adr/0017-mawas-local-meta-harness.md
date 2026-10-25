# 0017. Mawas — the local personal assistant as a meta-harness

- Status: accepted (2026-10-19, with the user: name "Mawas", build it; staged)
- Milestone: MW
- Evidence: `docs/spikes/2026-10-18-meta-harness-assistant.md` (design, slices PA-0…PA-12), `docs/spikes/2026-10-18-hermes-meta-harness.md` (Hermes Agent, NousResearch/hermes-agent, MIT), `docs/spikes/2026-10-18-vault-traces.md`, `docs/spikes/2026-10-18-direction.md` §8, §12.

## Context
The user wants a personal assistant — "a local Jarvis" — that is harness-independent (OMP, Claude Code, Codex over ACP, or a direct provider), keeps everything important locally, protects secrets, can drive all of Hutan with tools, and has traces. Hermes Agent shows the value is the persistent layer (curated memory, skills, recall, schedules, delegation) around a model loop. Hutan already owns sessions, cross-harness history, MCP control tools and approvals in the daemon (ADR 0001, 0016). Today's assistant (M16, M8-B) is a read-only chat per scope.

## Decision
1. **Name and presentation.** The assistant is **Mawas** (Malay "orangutan"). Left nav item "Mawas" opens the **Mawas Home page in the center** (conversation + a collapsible *Today* rail: needs you, working agents, upcoming schedules; entry points to Memory, Skills, Schedules, Traces). Not a narrow dock panel anymore.
2. **Meta-harness, not a second agent runtime.** Each Mawas turn runs on the chosen backend's own loop (OMP / Claude Code / Codex via ACP / app-server). Hutan supplies services through the daemon and the existing `hutan mcp` tool surface, plus a per-session context snapshot. We do **not** copy Hermes' text tool-call bridge. Direct providers are labelled **Chat only** until a gateway tool loop exists (slice MW-8).
3. **Daemon-owned services** (each its own capability, additive minors): memory (curated, revisioned entries with provenance; proposals and receipts — nothing shows "saved" without a daemon receipt), skills (Agent Skills format; read-only discovery of the user's agent skill dirs, never writing them without consent), schedules (ledger, misfire/overlap policies, explicit unattended capability grants), delegation (Mawas starts agents in **task workspaces** and watches them), traces (local bounded spans, OTel ids/GenAI conventions, content stays in journals), secrets vault (`secret://name`, resolved only at egress, ADR to follow with MW-6).
4. **Mawas acts, with safeguards (decided with the user 2026-10-19).** Mawas makes tool calls: its harness's native tools and Hutan's MCP tools. Default mode **"Ask before acting"**:
   - reading (files, history, sessions, status) runs freely;
   - every **mutating** call (Hutan control/MCP: create/close/kill sessions, worktrees, tasks, settings; harness native: edit files, run commands) raises an approval in Hutan — in the chat and in the Today rail — with **Allow once / Allow for this chat / Always allow this tool here / Deny**; grants are stored by the daemon, scoped (tool × workspace × chat), listed and revocable in Settings → Mawas;
   - **always ask**, whatever the grants: destructive or irreversible actions (kill/stop busy sessions, delete/remove, merge/push, anything touching secrets, settings that widen permissions) — Mawas can never widen its own permissions or approve its own requests;
   - limits per turn (max mutating calls, then pause and ask), a visible **Stop Mawas** kill switch, and an audit trail of every call with its approval (feeds traces, MW-7);
   - harness launch follows the mode: OMP with its full tools but permission prompts routed to Hutan's approval UI (ACP `session/request_permission`), Claude Code in its default permission mode (not `plan`), Codex `workspace-write` + `on-request`;
   - the enforcement lives in the **daemon** (origin attribution of Mawas sessions, `ControlService`), not only in harness flags. Origin attribution is cooperative (ADR 0016); a stronger restricted endpoint + sandbox comes with unattended schedules (MW-9/MW-10).
   **Advisor** (read and propose only) remains as a stricter mode in Settings → Mawas.
5. **Local-first honesty.** Everything is stored on the daemon's host. What is selected into a turn's context is sent to the backend's model — the UI says so.
6. **Staging and protocol.** Additive services and the Home page ship as protocol minors now. Moving Mawas into its own workspace kind (`kind = assistant`) is a semantic workspace change; it ships together with the task-workspace cutover (direction D-1) as **one protocol major**, not as a projection hack.

## Slices
MW-1 Home page + "Ask before acting" safeguards + naming · MW-2 memory service + MCP + context snapshot per backend · MW-3 explicit learning (remember / learn from session) · MW-4 skills · MW-5 delegation into task workspaces + monitoring · MW-6 secrets vault (ADR) · MW-7 traces · MW-8 direct-provider tool loop · MW-9 schedules · MW-10 restricted endpoint + sandbox for unattended runs · later: assistant workspace kind with the D-1 major, voice.

## Consequences
- `docs/assistant.md` becomes the Mawas document; the old "left-only panel" rule (OL-2) is replaced by the Home page.
- Every service is testable without a model (mocked inference only; real-backend runs are opt-in, never in the gate).
- Performance budget: Home and its rail do no polling while hidden; Today updates from daemon events; idle Mawas harness processes sleep (AS-1).

## Amendment 2026-10-25 — memory files (MEMORY-FILES, protocol 0.46)
Decision 3 keeps its rule — curated, revisioned entries with provenance, proposals and daemon receipts, **SQLite as the source of truth** — and adds a **mirror**: the daemon writes each scope's memory as Markdown (`<Home folder>/.mawas/memory/`; with the user's opt-in *Keep memory in the project*, `<project>/.hutan/memory/`), one file per category with a stable id comment per entry. The files are a view the user can read, review and version in git, not a second store: an edit of one is read by the daemon and becomes a **pending proposal** (origin `file`) that goes through the same approval flow as an agent's — never applied silently, secrets refused as ever, problems reported without the text — and `memory.import` makes proposals from an exported file the same way. Why the files are not primary: revisions, `expected_revision` conflicts, idempotent receipts, the 200-entry and 2 KiB bounds and provenance need one writer, and a file that two machines edit gives no receipt and no conflict rule. Consequences: a project's `.hutan/memory/` is in the user's repository only when they ask; a teammate's files arrive as proposals; `docs/assistant.md` § Memory files is the reference.
