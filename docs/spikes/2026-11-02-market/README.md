# Market research 2026-11-02 — supervising many coding agents, and who else does it

User request (2026-11-02): maximum research on Hutan's own niche (one assistant over many coding agents) and on new competitors, ahead of a demo. Five research streams ran in parallel on public sources (official docs and changelogs, GitHub repos and issue trackers, HN; Reddit, X and Product Hunt were mostly blocked, so those appear only as search snippets). Every claim in the stream files is linked and dated; `[INFERENCE]` marks a reading, not a source.

| File | Contents |
|---|---|
| [`new.md`](new.md) | ~45 products screened that the 2026-10-21 competitor survey did not cover; deep notes on 8 |
| [`incumbents.md`](incumbents.md) | The manager-of-agents features of Claude Code, Codex, Cursor, Copilot, Antigravity/Jules, Devin Desktop, Amp, Zed, Warp, JetBrains Air, compared with Hutan |
| [`oss.md`](oss.md) | 30+ open-source orchestrators: releases, and what their issue trackers ask for and complain about |
| [`voice.md`](voice.md) | What developers say about running agents in parallel: pains ranked, 50+ sources, verbatim quotes |
| [`demo.md`](demo.md) | How 13+ products demo supervision, what lands and what gets mocked, and a 2:30 Hutan demo script with a 40 s cut |

## 1. What changed in the market (2026-08 → 2026-11)

- **"Projects" became the incumbents' category.** A coordinator chat, an overview grouped by state, shared memory and schedules: Cursor Projects (2026-09-10), Claude Code Projects (2026-09-17), Copilot projects (preview 2026-10-28); Devin Desktop's Agent Command Center, Amp's Puck and the Antigravity app are the same shape. All are cloud-first and single-vendor at the core (`incumbents.md` §1).
- **Claude Code is converging on Hutan's primitives**: agent view with `--json` "for another Claude session that supervises", cross-session messaging that "can't approve anything" with a per-session `accept | hold | refuse`, a suggested reply in the peek panel that never answers a permission prompt (`incumbents.md` §2.1). Agent view's row text is written by a model and billed; an HN thread (586 points) complained about the spend (`demo.md` §2).
- **Second-model approvers are the incumbents' answer to approval fatigue**: Codex auto-review (a reviewer agent with a circuit breaker), Claude auto mode; open source goes further — Maestro's Pianola and Agent Deck's conductors *answer* agents' prompts when "confident" (`oss.md` §2.6–2.7). Meanwhile Claude Code's auto mode was hijacked 60–80 % of the time in a public test, and the CSA recommends treating classifier controls as "advisory rather than determinative" (`voice.md` §2.4).
- **New open-source rivals with traction**: Orchestrator.inc (14k★, per-project orchestrator agent, Kanban with "Needs you", CI failures routed back to the worker; two high-severity advisories: unauthenticated loopback RCE), Omnigent by Databricks (11k★, "meta-harness", policy engine, cross-vendor reviewer routing), Paseo (22.8k★, mobile), Orca Orchestrator V3 (coordinators woken when a worker reports) (`new.md` §2, `oss.md` §0).
- **Money and exits**: Superset → Paradev with an $11.5M seed (and source-available ELv2, not OSI), Herdr $6M seed and a native app, Conductor a reported $22M Series A; Google's Jules shuts down (read-only 2026-11-11, gone 2026-12-31); Vibe Kanban sunsetting, Terragon shut (`oss.md` §0, `voice.md` §9).
- **A shared standard for agent state**: OSC 7501, the Program Status Protocol, published 2026-10-06 by Mitchell Hashimoto, implemented in libghostty the same day, read by Orca since v1.4.232, with proof-of-concept patches for Claude Code and Codex ([post](https://mitchellh.com/writing/program-status-osc7501)). Hutan parses it (`crates/terminal/src/agent_status.rs`) but does not answer the spec's feature-detection query and ignores record ids and `progress` (no reply code found in `crates/`, checked 2026-11-02).
- **Agent-to-agent trust is the live security story**: OpenAI's 2026-08-26 incident report names agent side channels and "agents adopting goals from one another"; a poisoned-PR chain between two Google agents; a 2026-10-05 paper finds CaMeL's guarantees do not compose in hierarchical multi-agent systems — exactly a supervisor that reads its workers' output (`voice.md` §6, `new.md` §1e).

## 2. What developers say (from `voice.md`, ranked by how many sources raise it)

1. **Attention: "who needs me?"** Anthropic's own study (2026-10-20) calls human attention "the bottleneck for agent output" with sessions "around 80% idly waiting for human input"; people cope with 2–5 parallel streams.
2. **Review capacity and not knowing what an agent did.**
3. **False "done"** — "“Done, all tests pass.” Except they don’t." The fixes people trust take verification away from the agent (read-only tests, a revert test).
4. **Approval babysitting.**
5. **Cost and tokens.**
6. **Semantic conflicts between parallel agents** ("Git only notices if the patches touch the same lines").
7. **The supervisor as a new black box** — "How do you prevent the orchestrator itself from becoming the new black box?"

People pay for tokens, not glue; they prefer one-time fees and distrust VC-funded tools (`voice.md` §4).

## 3. Where Hutan stands

**Ahead, and confirmed by at least two streams** (do not say "only" in public copy; the streams found no counter-example, which is not proof):

| Hutan (0.3.5) | Market |
|---|---|
| One assistant over every agent in **every** workspace, with a Focus | Orchestrators and Projects are per-project (Orca, Orchestrator.inc, Pane, Cursor/Claude/Copilot Projects) |
| Board from daemon state, labelled *exact* / *state only*, "not recorded" instead of zero, no model tokens | Agent view rows are model-written and billed; state detection is the top complaint in cmux, herdr, Orca trackers |
| *Mawas suggests, you answer* — Use is the user's own click | Pianola, Agent Deck, Codex auto-review answer or approve on the user's behalf |
| Messages to agents: per target agent, full text, through the queue, never into a waiting agent or the user's draft | Orca's tracker: agents typing into the user's terminals; Claude Code's cross-session messaging is the closest peer |
| Web fetch and search off for the supervisor; agent text treated as data | No incumbent documents this for a manager that reads other agents' transcripts |
| Terminal agents unmodified in a PTY, beside Codex, Claude Code and OMP chats | Devin Desktop, Zed, Warp, JetBrains Air host several vendors, mostly over ACP |

**Behind**: phone or remote approval (all six cloud-capable incumbents have it; Hutan's remote hosts are experimental), independent verification of "tests pass", quota awareness, a jump-to-next-waiting key, the OSC 7501 feature query.

## 4. Recommendations, in order

Cheap, deterministic, and each one strengthens the demo:

1. **Next waiting agent** — a key and ⌘K command that jumps to the next agent waiting on the user and, once handled, to the one after (Orca #12577, cmux RFC #15266).
2. **State accuracy fixtures from competitors' bug trackers** — a background subagent still running is not idle (herdr #4511, #5004); a permission or question dialog is blocked (herdr #4573, #3871). Add them to the terminal-agent recogniser tests and fix what fails.
3. **OSC 7501, the rest of the spec** — answer the feature query so patched Claude Code and Codex detect Hutan, read record `id`s (subagents) and `progress`, keep the spec's size limit. Strategic: it is becoming the standard and Hutan already speaks most of it.
4. **Verification signs and a gated re-run** — add review-packet signs users describe (test files edited in the same turn, `skip`/`xfail` added, assertions removed, snapshots changed) and a *Run the tests yourself* action that runs the project's test command in the agent's worktree as a normal gated request and records the exit code. This answers pain #3 with Hutan's evidence, not the agent's word.
5. **Allowed targets with revoke, and a circuit breaker** — list the per-agent grants Mawas holds with *Revoke* (Cursor's allowed list); after N denials in a turn Mawas stops and Recent says so (Codex's breaker).
6. **Cross-worktree overlap warning** — "agents A and B both changed `payment.rs` since 10:00" from the changed-file sets Hutan already has.
7. **Memory provenance** — `decided_by: user | agent | mawas` on Mawas memory entries (Cursor Projects field report: decisions lost, "defaults accepted" that nobody accepted).

Medium:

8. **Quota awareness** — "this agent hits its limit around 14:10"; queue the next turn until the reset; Mawas *suggests* switching agent, the user clicks.
9. **Review by another harness** — *Review Last Turn with Mawas* can use a different harness or model than the author and says which; order the packet's files so the ones that explain the change come first (Amp).
10. **CI failure → the agent that wrote it** — Mawas proposes the follow-up message to the owning agent through the existing gate.

Large, strategic: **remote approval** (start with actionable macOS notifications, then a read-only remote view, then approvals). **Avoid**: a default auto-approver, model-written board rows, per-agent notification storms, limits written only as prompt prose, unauthenticated local control endpoints (Orchestrator.inc's RCE — worth a short audit of the daemon socket and `hutan mcp` before any remote work).

## 5. The demo

`demo.md` §4 has the full script, **"Three agents, one that waits for you"** (2:30): the board across three public repos with mixed agents (Codex chat in a worktree, Claude Code chat with *Ask*, OMP in a terminal) → "what needs me first?" answered with sources named → *Mawas suggests* → **Use** → the review packet's warning signs → a per-agent gated nudge (*from Mawas · sent with your approval*) → Recent and Flow with no model call. A 40 s cut keeps the question, the suggestion and the nudge.

What lands in this market: leaving and coming back to a ready list, and the human catching a real agent mistake. What gets mocked: "factory", "swarm", "fleet", forcing agent-first workflows, supervisors that quietly spend tokens, launch-day bugs. Prerequisites before recording: one or two days of real history for Recent and Flow, the exact build dry-run on a copy of the data folder (0.3.5's schema change is forward-only), real agents only, jump cuts labelled. Recommendations 1–4 above would each add a scene or make one more reliable.
