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
4. **Modes.** **Advisor** by default (reads and proposes; mutating Hutan tools refused **by the daemon** for Mawas sessions, not only by the harness's read-only flags). **Operator — asks before acting** is a visible, deliberate switch; every mutating call goes through `[control]` approvals with per-tool grants. Unattended (scheduled) runs need a restricted authenticated control endpoint + harness sandbox before they may mutate anything.
5. **Local-first honesty.** Everything is stored on the daemon's host. What is selected into a turn's context is sent to the backend's model — the UI says so.
6. **Staging and protocol.** Additive services and the Home page ship as protocol minors now. Moving Mawas into its own workspace kind (`kind = assistant`) is a semantic workspace change; it ships together with the task-workspace cutover (direction D-1) as **one protocol major**, not as a projection hack.

## Slices
MW-1 Home page + Advisor enforcement + naming · MW-2 memory service + MCP + context snapshot per backend · MW-3 explicit learning (remember / learn from session) · MW-4 skills · MW-5 delegation into task workspaces + monitoring · MW-6 secrets vault (ADR) · MW-7 traces · MW-8 direct-provider tool loop · MW-9 schedules · MW-10 Operator mode + grants · later: assistant workspace kind with the D-1 major, voice.

## Consequences
- `docs/assistant.md` becomes the Mawas document; the old "left-only panel" rule (OL-2) is replaced by the Home page.
- Every service is testable without a model (mocked inference only; real-backend runs are opt-in, never in the gate).
- Performance budget: Home and its rail do no polling while hidden; Today updates from daemon events; idle Mawas harness processes sleep (AS-1).
