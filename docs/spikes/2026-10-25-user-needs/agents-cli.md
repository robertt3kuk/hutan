# User needs from agent CLIs and their GUIs (Codex, Claude Code, OpenCode, Crush)

Date: 2026-10-25. Scope: desktop, app and IDE-facing needs. Model quality and provider billing are out of scope.

## Method

- **Repos (live, none archived):** `openai/codex`, `anthropics/claude-code`, `anomalyco/opencode` (`sst/opencode` redirects here), `charmbracelet/crush`.
- **Queries.** Crush: `gh issue list -R charmbracelet/crush --state all --search "sort:reactions-+1-desc" --limit 100 --json number,title,state,createdAt,reactionGroups,comments,labels,url`. The other three returned HTTP 504 on that search, so I used REST: `gh api -X GET search/issues -f q="repo:<repo> is:issue sort:reactions-+1-desc" -f per_page=100 -f page=1|2` (200 items per repo). The REST search limit (30/min) was hit once; I waited about 60 s.
- **Counts.** `reactions.total_count` across all reaction types (directional). † = created before 2025-10-25.
- **Not done:** no `enhancement` / `feature request` label search; no full comment threads; no Reddit or forum data.
- **Hutan evidence:** grep of `docs/*.md` and `CHANGELOG.md` in this tree. Line numbers approximate.

## 1. Top 25 needs

| # | Need (user's words, paraphrased) | Sources (reactions) | Category | Hutan status · evidence |
|---|---|---|---|---|
| 1 | Shared project instructions across agents ("AGENTS.md support") | claude-code [#6235](https://github.com/anthropics/claude-code/issues/6235) (6,688, closed, †); [#31005](https://github.com/anthropics/claude-code/issues/31005) (572) | memory/context | **partial.** Each harness reads its own files; Hutan never reads or writes `AGENTS.md`/`CLAUDE.md` (no hits). Mawas memory `docs/assistant.md` l.82–84. |
| 2 | Native app on Linux, Windows, Visual Studio | codex [#11023](https://github.com/openai/codex/issues/11023) (1,464, closed); claude-code [#65697](https://github.com/anthropics/claude-code/issues/65697) (655, closed); [#15942](https://github.com/anthropics/claude-code/issues/15942) (597, open) | other | **missing by design.** `docs/vision.md` non-goals: Windows/Linux UI. |
| 3 | Control agents from a phone or a remote dev box | codex [#10450](https://github.com/openai/codex/issues/10450) (878, closed); [#9224](https://github.com/openai/codex/issues/9224) (544, closed); opencode [#7790](https://github.com/anomalyco/opencode/issues/7790) (116, SSH desktop); [#10288](https://github.com/anomalyco/opencode/issues/10288) (104, mobile) | remote/SSH, mobile | **missing (planned M9).** `docs/architecture.md` l.257 no TCP listener until an ADR; l.332 remote TLS/WebSocket (M9). |
| 4 | See quota or the 5-hour limit; stop burning tokens | codex [#28879](https://github.com/openai/codex/issues/28879) (561, closed); [#14593](https://github.com/openai/codex/issues/14593) (308); [#34035](https://github.com/openai/codex/issues/34035) (279, open); claude-code [#16157](https://github.com/anthropics/claude-code/issues/16157) (726, open); [#38335](https://github.com/anthropics/claude-code/issues/38335) (544, open) | cost/usage | **partial.** Context meter `docs/agents.md` l.301; cost parsed from `usage_update` l.220; retry countdown `docs/design.md` l.259. No quota or spend meter. |
| 5 | Several accounts; switch profiles | claude-code [#18435](https://github.com/anthropics/claude-code/issues/18435) (1,183, open); [#36151](https://github.com/anthropics/claude-code/issues/36151) (1,065, open); [#27302](https://github.com/anthropics/claude-code/issues/27302) (616); codex [#4432](https://github.com/openai/codex/issues/4432) (154, `--auth-profile`) | security/accounts | **partial.** `hutan account ls\|login\|logout` `docs/agents.md` l.145; `crates/daemon/src/accounts.rs`. Several accounts per harness not evidenced. |
| 6 | Hooks so tools can react to agent events | codex [#2109](https://github.com/openai/codex/issues/2109) (688, closed, †) | integrations | **partial.** Hooks feed attention only (`docs/architecture.md` l.104; `docs/control.md` l.47). User event hooks planned M9 (`docs/architecture.md` l.331). |
| 7 | Agent can use the language server | codex [#8745](https://github.com/openai/codex/issues/8745) (609, open) | integrations | **has (partial reach).** LSP in daemon (`CHANGELOG.md` 0.2.0; `docs/lsp.md`); control `lsp` and `doc.*` (`docs/control.md` l.134). Per-agent use not verified. |
| 8 | Undo and rewind agent edits, with file restore | codex [#9203](https://github.com/openai/codex/issues/9203) (540, open); [#11626](https://github.com/openai/codex/issues/11626) (230); claude-code [#8763](https://github.com/anthropics/claude-code/issues/8763) (277, closed) | git/review | **has.** Per-turn checkpoints (`docs/agents.md` l.70; ADR 0019); Rewind l.117–121; CK-5 `docs/design.md` l.886. |
| 9 | Plan first, approve before edits | codex [#2101](https://github.com/openai/codex/issues/2101) (506, closed, †); crush [#1734](https://github.com/charmbracelet/crush/issues/1734) (62, open) | agents | **partial.** Plan events (`docs/agents.md` l.39); plan is a Request (`docs/design.md` l.242). No plan-mode toggle evidenced. |
| 10 | Keep secrets out of agent reach | codex [#2847](https://github.com/openai/codex/issues/2847) (473, closed, †) | security | **partial.** Checkpoint exclude `.env*`, `*.pem`… `docs/agents.md` l.81; sandbox passthrough l.138. No agent read-deny. |
| 11 | Subagents and a view of parallel agents | codex [#2604](https://github.com/openai/codex/issues/2604) (413, closed, †); opencode [#5887](https://github.com/anomalyco/opencode/issues/5887) (99); [#12661](https://github.com/anomalyco/opencode/issues/12661) (190, not planned); crush [#431](https://github.com/charmbracelet/crush/issues/431) (51) | agents | **partial.** Agent tree `docs/agents.md` l.226; monitor M13-B `docs/design.md` l.563. Hutan monitors subagents, does not spawn them. |
| 12 | Quick side question without a session ("/btw") | opencode [#16992](https://github.com/anomalyco/opencode/issues/16992) (410, closed); claude-code [#37323](https://github.com/anthropics/claude-code/issues/37323) (225, closed) | agents | **has (different shape).** Mawas quick assistant `docs/vision.md` l.54; `docs/assistant.md`. No literal `/btw`. |
| 13 | Vim motions in the prompt box | opencode [#1764](https://github.com/anomalyco/opencode/issues/1764) (198, not planned); crush [#1199](https://github.com/charmbracelet/crush/issues/1199) (42) | editor | **has.** Vim mode covers the composer `docs/design.md` l.856 (V-1). |
| 14 | Session goals that persist across turns | opencode [#27167](https://github.com/anomalyco/opencode/issues/27167) (217, open) | agents | **partial.** Tasks with dependencies `docs/vision.md`; `docs/architecture.md` l.240. No session-goal concept. |
| 15 | Large pasted text: preview and edit | opencode [#8501](https://github.com/anomalyco/opencode/issues/8501) (328, closed); claude-code [#3412](https://github.com/anthropics/claude-code/issues/3412) (314, closed, †) | input | **partial.** Pasted text attaches to any chat `docs/agents.md` l.293; paste confirm `docs/design.md` l.395. No preview/edit of attachments. |
| 16 | Queue messages while the agent works; steer or unqueue | claude-code [#50246](https://github.com/anthropics/claude-code/issues/50246) (254, closed); opencode [#4821](https://github.com/anomalyco/opencode/issues/4821) (126) | agents | **has.** Queue and ⌃↩ send now (`CHANGELOG.md` 0.2.0); `docs/design.md` l.882; `chat.queue.*` `docs/protocol.md` l.15. |
| 17 | Questions must not time out or auto-answer | claude-code [#73125](https://github.com/anthropics/claude-code/issues/73125) (414, closed); codex [#28969](https://github.com/openai/codex/issues/28969) (221, open) | approvals | **has.** Question cards and Requests group (`docs/agents.md` l.333; `docs/design.md` l.242). No timer found [INFERENCE]. |
| 18 | Approval rules that match and persist | codex [#3085](https://github.com/openai/codex/issues/3085) (125, closed); claude-code [#30519](https://github.com/anthropics/claude-code/issues/30519) (81, open); opencode [#8463](https://github.com/anomalyco/opencode/issues/8463) (94, closed) | security | **partial.** One primary Allow once (`CHANGELOG.md` 0.2.0); Mawas asks before acting `docs/assistant.md` l.70. Options come from the agent (`docs/agents.md` l.305); no Hutan rule store. |
| 19 | Review agent diffs in the editor or IDE | claude-code [#33932](https://github.com/anthropics/claude-code/issues/33932) (306, open); codex [#2998](https://github.com/openai/codex/issues/2998) (240, open) | git/review | **has.** Changes view `docs/design.md` l.600; editor Accept/Reject, ⌘⌥Z (`CHANGELOG.md` 0.2.0). |
| 20 | Terminal does not jump, flicker, or lose scroll | claude-code [#3648](https://github.com/anthropics/claude-code/issues/3648) (836, closed, †); [#826](https://github.com/anthropics/claude-code/issues/826) (822, open); [#769](https://github.com/anthropics/claude-code/issues/769) (336) | terminal | **has, to verify.** Own PTY; jump-to-latest `docs/design.md` l.612; resize l.620. Live check needed. |
| 21 | Copy without broken indentation | claude-code [#18170](https://github.com/anthropics/claude-code/issues/18170) (301, open); opencode [#4283](https://github.com/anomalyco/opencode/issues/4283) (135) | terminal | **partial.** Block copy `docs/design.md` l.719. Copy from wrapped agent TUI not evidenced. |
| 22 | Context breakdown and manual compact | codex [#23794](https://github.com/openai/codex/issues/23794) (253, closed); [#11325](https://github.com/openai/codex/issues/11325) (156, closed); opencode [#6152](https://github.com/anomalyco/opencode/issues/6152) (154) | memory/context | **partial.** Context meter `docs/agents.md` l.301. No breakdown or compact action. |
| 23 | Customizable status line | codex [#17827](https://github.com/openai/codex/issues/17827) (220, open) | layout/UI | **partial.** Arrangeable status bar (`CHANGELOG.md` 0.2.0). No per-chat status line. |
| 24 | Several windows on one workspace | claude-code [#30154](https://github.com/anthropics/claude-code/issues/30154) (265, open); codex [#2909](https://github.com/openai/codex/issues/2909) (153, closed, †) | layout/UI | **undecided.** Open question `docs/design.md` l.1004. |
| 25 | Voice input for prompts | codex [#3000](https://github.com/openai/codex/issues/3000) (300, open); opencode [#4695](https://github.com/anomalyco/opencode/issues/4695) (208, not planned) | other | **missing.** No voice hits in `docs/`. |

Alternates: model fallback, opencode [#7602](https://github.com/anomalyco/opencode/issues/7602) (122): missing (only retry, `docs/design.md` l.259). Sound on finish, codex [#3962](https://github.com/openai/codex/issues/3962) (223): Hutan chose no sound (`docs/design.md` l.242–248).

## 2. Five most important missing needs

1. **Remote and mobile control of agents** (rows 3). Codex #10450 (878) and #9224 (544). The remote path is M9 and blocked on a transport ADR (`docs/architecture.md` l.257, l.332). Largest gap for a daemon-first design.
2. **Quota and spend visibility** (row 4). Claude #16157 (726), #38335 (544); Codex #28879 (561), #34035 (279). Hutan parses cost but shows only context. A limit and spend strip could build on existing `usage_update` events.
3. **Event hooks for user automation** (row 6). Codex #2109 (688, †). Hutan hooks serve its own attention state only; user plugins are M9.
4. **Several accounts per harness** (row 5). Claude #18435 (1,183) and #36151 (1,065). Hutan has one login per harness (`docs/agents.md` l.145); switching needs account-scoped harness environments in `crates/daemon/src/harness.rs`.
5. **Voice input** (row 25). Codex #3000 (300) open; OpenCode #4695 (208) not planned. Low cost, no design work in Hutan.

Close seconds: a project instruction file (row 1; Hutan relies on each harness) and a multi-window decision (row 24).

## 3. Gaps in the evidence

- REST search returns the top 200 per repo by reactions; lower-reaction and label-only items are missing.
- Crush is mostly 2025 and TUI-focused; weak signal for desktop.
- Bodies come from search payloads, not full threads. Reactions include non-requests ("+1" on bugs); ranking is directional.
- `state_reason` (not planned) was sampled, not checked for all items.
- Hutan status is from docs and CHANGELOG, not a running app. "has, to verify" rows (terminal, LSP reach, question timers) need a live check.
- GitHub only; no Reddit or Discord.
