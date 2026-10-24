# User needs: multi-agent and parallel-agent tools (GitHub issues)
Scope: parallel and multi-agent dev tools. Date of research: 2026-10-25.

## Method (reproducible)

- Repos (top by stars, live, not archived): `superset-sh/superset` (15.5k stars, pushed 2026-10-24), `OrchestratorInc/agent-orchestrator` (13.6k, pushed 2026-10-24, formerly `ComposioHQ/`), `BloopAI/vibe-kanban` (28.4k, pushed 2026-09-19, **sunsetting**: https://github.com/BloopAI/vibe-kanban and https://www.vibekanban.com/blog/shutdown, dated 2026-04-10), `smtg-ai/claude-squad` (8.7k, pushed 2026-08-20). `conductor-oss/conductor` was excluded: it is a workflow engine, not a coding tool. Conductor (the Mac app) has no public repo or feedback board that I found.
- Query per repo: `gh issue list -R <repo> --state all --search "sort:reactions-+1-desc" --limit 100 --json number,title,state,createdAt,reactionGroups,comments,labels,url`. Also `--label enhancement`. A "not planned" filter (`reason:"not planned"`) returned unreliable results, so I used the state field instead.
- Reactions = sum of `reactionGroups`. c = comments. Bodies read for the top items.
- Superset UX (naming, sidebar, input lag, shortcuts) is covered by `docs/spikes/2026-10-21-competitor-ux/superset.md` and is not repeated.

Issue URLs: `https://github.com/<repo>/issues/<n>`. Short names: **S** = superset-sh/superset, **V** = BloopAI/vibe-kanban, **AO** = OrchestratorInc/agent-orchestrator, **CS** = smtg-ai/claude-squad.

## 1. Top 25 needs

| # | Need (user's words) | Repos, issues (reactions, c) | Category | Hutan status (evidence) |
|---|---|---|---|---|
| 1 | "use superset in ssh remote server" | S #1513 (34, c12, closed duplicate); AO #3853 (3, c2) | remote/SSH | missing: SSH is design only (`docs/spikes/2026-10-18-remote.md`, D-3; `roadmap.md` M9) |
| 2 | "Make login non-required" | S #2685 (23, c7, closed duplicate); V #2952 (10) | security/permissions | has: no Hutan login; local daemon (`docs/vision.md`); harness auth is the harness's own (`docs/agents.md` § Accounts) |
| 3 | "Linux support" | S #405 (23, c12, open) | other | missing by design: macOS-first; headless Linux is a later direction (`roadmap.md` D-1) |
| 4 | Several repos in one project | S #2364 (21, c6, not planned); CS #56 (open, c~4); CS #299 (4) | git/review | has: a workspace holds several repositories (`docs/orchestration.md` § Worktrees) |
| 5 | Keep the old UI and data after an upgrade | V #2687 (33, c39, not planned); V #2288 (17) | other | partial: protocol versions exist; live daemon upgrade is M9 (`docs/adr/0009-process-survival-policy.md`) |
| 6 | GitLab / self-hosted git, and PR creation | V #1697 (26, c8, reopened); S #2714 (11, c5); S #6445 (4); AO #251 (3, c2) | git/review | partial: Finish commits and pushes (`docs/orchestration.md` § Review and merge, ADR 0020); no PR and no forge API |
| 7 | Automatic worktree cleanup after a retention period | V #765 (19, c13) | git/review | partial: `worktree.remove` refuses live sessions (`docs/orchestration.md` § Worktrees); no retention policy |
| 8 | Choose the worktree and branch name at creation | S #7318 (11, c7); S #6398 (in 2026-10-21 spike) | git/review | has: `worktree.create {branch}`, `hutan task add --worktree=BRANCH` (`docs/orchestration.md` §§ Worktrees, Tasks) |
| 9 | Run setup (`npm ci`) when a worktree is made | CS #181 (4, closed) | git/review | missing: no setup or bootstrap hook in any doc (grep for setup, bootstrap, on_create) |
| 10 | Per-project account choice, and switching accounts | AO #6696 (0, c0); S #8104 (0, new) | cost/usage | partial: one account list per harness (`docs/agents.md` § Accounts); no per-project choice |
| 11 | Fall back to another agent when the usage limit hits | AO #7160 (c2); AO #6947 (c0, role pool with quota failover) | cost/usage | missing: only a retry countdown for a rate-limited turn (`docs/design.md` § UX-RETRY) |
| 12 | See usage and tokens per session | AO #5026 (c6, closed) | cost/usage | has: usage per session (`roadmap.md` M13-C); context meter (`docs/agents.md` ~l.301) |
| 13 | Sandboxed or container runs, without skip-permissions | S #3957 (7, c7); AO #6663 (0, c0, agents can read `~/.ssh`); S #2419 (13, c~12, `--dangerously-skip-permissions` hardcoded) | security/permissions | partial: Codex sandbox flags (`docs/agents.md` l.138); no container and no per-session permission default |
| 14 | An orchestrator reads a worker's output | AO #5597 (c4) | agents | has: agent control surface, CLI and MCP, `agents = ask` policy (`docs/adr/0016-agent-control-surface.md`) |
| 15 | Per-role setup (MCP servers, plugins, system prompt) | AO #2195 (c5) | agents | partial: `[mcp.*]` is shared by every harness session (`docs/agents.md` l.247); no per-role profile |
| 16 | Sessions survive a restart | AO #6639 (2, open, Windows); CS #212 (3) | agents | has: `restore_sessions = respawn` plus ACP `session/load` (`docs/agents.md` § Restore); recovery card (`docs/design.md` l.660) |
| 17 | Agent stuck in "working" or a stale session lock | AO #5050 (c1); AO #4280 (c5, open) | agents | partial: state detection is a roadmap source (`roadmap.md` M13); no stop hook yet |
| 18 | Loop until the self-tests pass ("Ralph mode") | V #1946 (20, c13) | agents | partial: `task start --auto` (`docs/orchestration.md` § Tasks); no retry or test loop [INFERENCE: none in docs] |
| 19 | A kanban board of tasks | V #2509 (15, c7); V #2730 (15); V #2288 (17) | layout/UI | partial: a task list with dependencies (`docs/orchestration.md` § Tasks; `docs/design.md` Tasks panel); no board |
| 20 | Tracker integration (Linear, Jira, Trello) | V #2159 (7) | integrations | missing by decision: the tracker is not the source of truth (`docs/process.md` table) |
| 21 | Export and self-hosted storage for project data | V #3396 (19, c9, open) | other | partial [INFERENCE]: local SQLite (`docs/adr/0006-storage-sqlite.md`); export not found |
| 22 | Stop or restart the lead orchestrator from the desktop | AO #7114 (c1); AO #6836 (0, slot-limit prompt) | agents | has: stop and kill, interrupt (`docs/design.md` l.585); `hutan restart <id>` (`docs/agents.md`) |
| 23 | Warn when two agents write to one checkout | community spike (HN 49789356, 44533339) | agents/git | has: `shared_with` in placement (`docs/orchestration.md` § Placement) |
| 24 | One "needs you" queue across agents | community spike (~11 voices) | notifications | has: Requests group (`docs/design.md` l.238) |
| 25 | Quiet hours and focus-aware notification suppression | community spike (Codex #3962) | notifications | partial: mute per session (`CHANGELOG.md` l.44); notify.* settings have no quiet hours (`docs/design.md` l.483) |

Left out as low-signal: S #2861 and #4601 (covered by the 2026-10-21 spikes), AO #3154, AO #1059.

## 2. Five most important missing needs

1. **Remote execution over SSH** (row 1). S #1513 has 34 reactions and was closed as a duplicate, which shows the need. AO #3853 asks for the same thing: a thin laptop client on a remote daemon. The decision is already made (SSH first, D-3). Hutan has no implementation. A laptop-only tool cannot use a bigger remote machine or a cloud VM.
2. **Container or sandboxed unattended runs** (row 13). S #2419 (13 reactions) reports that Claude Code always runs with a hardcoded `--dangerously-skip-permissions`, and S #3957 and AO #6663 ask for an isolated container. Parallel agents in worktrees need a safe way to run unattended. Hutan has only Codex sandbox flags. [INFERENCE: the volume is low, but the trend in agent tools is clear.]
3. **Fall back on usage limit** (row 11). AO #7160 and #6947 are open, filed in October 2026, and ask for a per-project fallback agent and a quota pool. Engagement is low (0–2 reactions), so the weight is inferred from the problem, not from votes. Parallel agents hit limits at the same time, and a retry countdown alone does not keep a task moving.
4. **Worktree bootstrap and retention** (rows 7, 9). V #765 (19 reactions) asks for cleanup after a retention period. CS #181 asks to run `npm ci` in new worktrees. Hutan can create and remove worktrees but has no setup hook and no retention policy. Stale worktrees and missing dependencies slow down every parallel task.
5. **Forge integration and PR creation** (row 6). V #1697 (26 reactions, reopened) asks for self-hosted GitLab. S #2714 (11) asks for GitLab. Hutan's Finish pushes the branch but does not open a review request, and it has no forge API. The community spike shows Finish-to-main is contested, so PR support may matter more than a direct push [INFERENCE].

## 3. Evidence gaps

- **agent-orchestrator is low-engagement.** The top 100 issues total about 50 reactions, and most are filed by its maintainers and agents. Its ranking reflects internal planning more than user demand. I used comment counts and labels as the weak signal.
- **Superset totals are inflated.** S #1513 and #2685 are closed duplicates. Their reaction counts are real but spread across merged threads.
- **vibe-kanban is sunsetting.** Its shutdown post is dated 2026-04-10, and its top items are historical. The enhancement-label pull hit a GitHub secondary rate limit (HTTP 403); the top-100 pull succeeded, and the rate limit cleared after a wait.
- **claude-squad is effectively unmaintained.** Its top issue is "Project Seems Abandoned" (CS #250, 11 reactions, closed 2026-02-12). Its totals are low (189 reactions over 100 issues).
- **Conductor** is not in the set. It is closed source, and I found no public repo or feedback board.
- **Overlap.** NeedsADE covers vibe-kanban and claude-squad from the ADE side (`docs/spikes/2026-10-25-user-needs/ade.md`). The two reports should be read together.
- **Not planned filter.** `reason:"not planned"` was unreliable, so "not planned" status comes from the issue state only.
- **Not checked:** Hutan's own feedback; the agents' own changelogs for multi-agent features; superset docs beyond `docs.superset.sh/remote-workspaces`.
