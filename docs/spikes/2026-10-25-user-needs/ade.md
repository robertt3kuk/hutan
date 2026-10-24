# User needs from similar ADE repos (GitHub issues)

Scope: `stablyai/orca` (active, closest product), `BloopAI/vibe-kanban`, `stravu/crystal`, `smtg-ai/claude-squad`. Issues only, as asked.

**Staleness.** Vibe-kanban says in its README "Vibe Kanban is sunsetting" (https://www.vibekanban.com/blog/shutdown). Crystal says "Crystal Is Now Nimbalyst" and was deprecated in February 2026 (https://nimbalyst.com/). Claude-squad is low-activity (last push 2026-08-20; issue 250 asks "Project Seems Abandoned"). So only Orca is a live signal; vibe and crystal show historical demand.

**Method (reproducible).** `gh issue list -R <repo> --state all --search "sort:reactions-+1-desc" --limit 100 --json number,title,state,createdAt,reactionGroups,comments,labels`. Also `created:>2025-10-25`, `label:enhancement`, `--label "collecting upvote"` (orca: 49 items), and `reason:not-planned` (orca). `rx` = sum of `reactionGroups` counts; `cmt` = comments. Bodies were read for the top items (first ~700 characters); full threads were not read. Issue links: `https://github.com/<repo>/issues/<N>` with `<repo>` = `stablyai/orca` (O), `BloopAI/vibe-kanban` (V), `stravu/crystal` (C), `smtg-ai/claude-squad` (CS).

**Hutan evidence keys:** VIS = `docs/vision.md`; DES = `docs/design.md`; ORC = `docs/orchestration.md`; RM = `docs/roadmap.md`; AG = `docs/agents.md`; LSP = `docs/lsp.md`; CL = `CHANGELOG.md` (Unreleased/0.2.0); ADR20 = `docs/adr/0020-finish-commit-and-push.md`.

## (1) Top 25 needs

| # | Need (user's words) | Repos, issues, rx | Category | Hutan status · evidence |
|---|---|---|---|---|
| 1 | "Multi-repo workspaces… view diffs of several repos at once" | O#1099 (78 rx, 15 cmt); O#7568 (40); O#21118 (21); V#705 (7, closed) | git/review | **partial** · VIS: multiple repos per workspace; RM M7-B; ORC Changes lists the workspace's checkouts, no single cross-repo diff [INFERENCE] |
| 2 | Jujutsu workspaces "instead of worktrees" | O#1082 (47); CS#104 (4, closed not-planned) | git/review | **missing** · no `jj` in docs |
| 3 | "Go to References returns empty for named exports" | O#961 (41); O#14872 (bug, 16, "hover shows `any`") | editor | **partial** · LSP: diagnostics, hover, definition; no references (LSP, CL 0.2.0) |
| 4 | Embedded editor with LSP and Go to Reference | O#7754 (40); O#3035 LSP + codebase search (30) | editor | **partial** · editor + LSP shipped (CL 0.2.0, E-11b/E-12); no references, no codebase search |
| 5 | Git Graph view "similar to VS Code/Cursor" | O#4521 (31, 6 cmt) | git/review | **missing** · no graph in DES/ORC |
| 6 | Git blame in the editor: "the only workaround is `git blame` in the terminal" | O#9948 (28, 14 cmt) | git/review | **missing** |
| 7 | Multiple windows for multiple displays | O#6074 (39, 22 cmt) | layout/UI | **missing** · DES §Open questions 3: "Multi-window… (proposal: any)" |
| 8 | Chat UI / check in from mobile | O#3099 (28, 12 cmt); C#89 (6, mobile UI); O#15067 (1, 10 cmt, closed) | mobile | **missing** · VIS:71 "later… mobile"; RM M9 feasibility only |
| 9 | Headless server mode for the runtime | O#4280 (25, 3 cmt, closed; stateReason not checked) | remote/SSH | **partial** · daemon survives UI, CLI `hutan daemon` (RM M3); remote transport not built (RM M9, D-1) |
| 10 | Agents on remote machines / SSH | C#26 (8); O#13852 (1 rx, 12 cmt, closed: "Direct SSH update strands remote terminals"); O#9819 (2, closed) | remote/SSH | **missing** · RM D-1 "remote hutand (SSH→TLS→relay)" |
| 11 | "Multiple Claude Code subscriptions" via separate config dirs | O#11557 (25, 10 cmt) | cost/usage | **partial** · Accounts per harness, ADR 0015 (DES §Accounts; RM M8-A); no concurrent second account [INFERENCE] |
| 12 | "Pin an account to a worktree… hot-swap" | O#9496 (15, 6 cmt); O#5635 (1, 8 cmt, closed) | cost/usage | **missing** |
| 13 | Custom agent presets: `codex --profile`, `CLAUDE_CONFIG_DIR` | O#2284 (17, 20 cmt); O#14323 DeepSeek Harness (31, 17 cmt) | agents | **partial** · harness program/args in Settings (DES:483), "Track as" for unknown agents (DES:257); no named profiles or per-agent env [INFERENCE] |
| 14 | Intermediate permission modes (Claude "Auto", Codex "Approval") | O#13081 (22, closed); O#25559 (13, closed, bug) | security | **partial** · approval cards, control modes (DES; AG:46–56), mode chip (DES:261); no auto mode of its own |
| 15 | Sandboxed agents: "run AI agents safely in local sandboxes" | O#13665 (24, 5 cmt); C#21 (sandboxed agents) | security | **missing** · only Codex sandbox flags shown (DES:213) |
| 16 | Configurable editor theme (Monaco) | O#6308 (24, 6 cmt) | layout/UI | **has** · one theme for chrome/terminal/syntax (RM M11); theme packs TH-2/TH-3 (`docs/themes.md`, CL) |
| 17 | Custom task source: "generic API contract" / Gitea / Jira / Linear / GitHub issues | O#8376 (23); O#4703 (20); O#1310 Jira (11, closed done); V#1677 (9); V#2159 (7) | integrations | **missing** · tasks are native (ORC §Tasks); no import |
| 18 | Worktree env setup: copy `.env`, setup hook, port isolation | V#1947 (9, `.worktreeinclude`); CS#260 (8, 3 cmt); C#39 (1, 3 cmt) | git/review | **missing** · per-project `terminal.env` only (DES:356); `.worktreeinclude`-like hook deferred (`docs/spikes/2026-10-21-community-opinions.md`) |
| 19 | "Open a PR review": approve, request changes from the app | O#9512 (16, 1 cmt) | git/review | **missing** · ADR20 §3 "No pull requests"; Finish = commit+push (CL 0.2.0) |
| 20 | Auto-clean worktrees after Done/Cancel; delete branch after merge | V#765 (19, 13 cmt); V#2680 (6) | git/review | **partial** · `worktree.remove` refuses unless forced, `delete_branch` option (ORC §Worktrees); no automatic cleanup |
| 21 | Keybinding presets (Vim, Emacs) for the editor | O#9699 (16, 7 cmt) | editor | **partial** · Vim mode shipped, off by default (CL 0.2.0); no Emacs; keyboard-shortcut editor planned (RM ST-7) |
| 22 | Kanban board / bring back the old UI | V#2687 (33, 39 cmt, closed not-planned); V#2288 (17, 26 cmt); V#2509 (15) | layout/UI | **partial** · Tasks panel with dependency tree (RM M7-C), no board view [INFERENCE] |
| 23 | Auto-resume after rate limit with countdown (Claude Max) | C#18 (2 rx, open) | cost/usage | **partial** · *Retry available in N s* + *Retry now* (DES:259; CL 0.2.0); no auto-resume |
| 24 | Scheduled/recurring "heartbeat" or Ralph-loop prompts | V#1946 (20, 13 cmt); V#3039 (7) | agents | **missing** · DES:119 "Upcoming: No schedules yet" placeholder |
| 25 | Restore sessions after reboot ("resume after reboot") | CS#212 (3 rx, closed not-planned) | other | **has** · `restore_sessions = respawn` (AG:66); respawn at daemon start (RM M3 exit demo) |

Also seen (lower reactions): quick commands (O#8481, 9 rx, missing); "Add to .gitignore" in the explorer (O#9060, missing); configurable shell (O#5097, has `terminal.shell`, DES:356); session names (O#10250, has inline rename, CL 0.2.0); Explorer on the left (O#10153, has, Files moved right, DES:117).

Notifications (C#178, 3 rx): **has** (VIS attention model; DES notifications).

## (2) Five most important missing needs

1. **Cross-repo view (#1, O#1099 78 rx).** The highest-reacted live need. Hutan has multi-repo workspaces, but its Changes view works per checkout (ORC §Changes). Why: one agent task often spans repos (O#7568: "one task is scattered across the UI").
2. **Git history in the editor and Changes (#5–6, O#4521 31 rx, O#9948 28 rx, 14 cmt).** Hutan's diff review compares against the merge base only (ORC §Review and merge). Why: reviewing agent output needs "who changed this line and why". Blame and a graph are cheap given `git` is the source of truth (ORC Principles).
3. **Remote and mobile check-in (#8–10, O#3099 28 rx, O#4280 25 rx, C#26/C#89).** Missing, and the daemon-first design (VIS:16) is the base. Why: users want to unblock agents away from the desk; the spike `docs/spikes/2026-10-21-community-opinions.md` already ranks the "needs you" queue as the core, so a remote queue extends it.
4. **Sandboxed agent runs (#15, O#13665 24 rx, 5 cmt).** Missing. Why: O#13081 and O#25559 want auto/YOLO modes; containment is the missing part [INFERENCE]. Hutan's approval cards (AG §Approvals) cover prompts, not isolation.
5. **External task sources (#17, O#8376 23 rx, O#4703 20 rx, O#1310 closed done).** Missing. Why: Hutan tasks are native only, so work starts from tickets elsewhere. Lower urgency than 1–4 in reactions, but the source-of-truth gap is clear.

Honourable mention: worktree env setup (#18, V#1947, CS#260, 8–9 rx each). Cheap to build and repeated across three repos, but the reactions are low.

## (3) Gaps in evidence

- **Low counts.** Top Orca reactions are 78, most under 30; treat counts as directional. `rx` = sum of reaction groups; `cmt` = comments.
- **Threads not read.** Only the first ~700 characters of the top bodies were read; quotes come from those bodies.
- **Stale sources.** Vibe-kanban (sunsetting) and crystal (deprecated) are historical; claude-squad is low-activity.
- **Unverified.** Some closed state reasons (e.g. O#4280) were not checked. The Orca 27649–27680 batch (all not-planned, one comment each, filed 2026-10-11/12) is probably maintainer triage [INFERENCE].
- **Hutan status** comes from docs only (VIS, DES, ORC, RM, AG, LSP, CL, ADR20); no code or running app was checked.
- **Not covered:** Hacker News, Reddit, other trackers (GitHub issues only, as asked).
- **Scratch files:** I wrote `/tmp/e1`–`/tmp/e8` early, which may have overwritten other agents' files of those names. My data is in `/tmp/ade*`.
