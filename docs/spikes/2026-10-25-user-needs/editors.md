# User needs from editor and IDE trackers (Zed, Helix, Lapce, Cursor)

Research for Hutan, 2026-10-25. Read-only.

## Method

- **Zed** (`zed-industries/zed`): REST label sweeps with `since=2025-10-01` (updated_at, so some older issues appear). Labels: `area:ai`, `area:ai/agent thread`, `area:ai/acp`, `area:integrations/git`, `area:collab`, `platform:remote`, `area:multi-buffer`, `area:project panel`. PRs excluded. Counts are current reaction totals. Bodies read with `gh api repos/zed-industries/zed/issues/<N>`.
- **Helix** (`helix-editor/helix`): `gh api repos/helix-editor/helix/issues?state=all&since=2025-10-01` (823 issues), ranked by `reactions.total_count`. `gh search` returned 502.
- **Lapce** (`lapce/lapce`): `gh search issues -R lapce/lapce --sort reactions` (100 issues), reactions summed.
- **Cursor**: `getcursor/cursor` has issues disabled. Used `forum.cursor.com` Ideas category (`/c/ideas/22`, `top.json?period=all|yearly`), ranked by likes; bodies via `forum.cursor.com/t/<id>.json`.
- **Hutan status**: keyword grep over `docs/`, ADRs and `CHANGELOG.md`, then reading matched sections. "Missing" means no doc mention, not a code audit.
- Prior research not repeated: `docs/spikes/2026-10-21-competitor-ux/zed.md`, `docs/spikes/2026-10-21-opinions/editor.md`.

## 1. Top 25 needs

Roughly ordered by reaction weight. `R` = reactions, `c` = comments, forum likes for Cursor.

| # | Need (user's words) | Sources | Category | Hutan status · evidence |
|---|---|---|---|---|
| 1 | "dev containers… locally with docker/podman or on a remote servers" | Zed #11473 (closed; R 930, c 163); #39695 remote files outside workspace (R 32, c 18); #44727 remote SSH hangs (R 21, c 21) | remote | **missing** · roadmap M9 "remote transport" is future |
| 2 | "Add support for jj SCM" | Zed #21538 (open; R 789, c 59); Helix #7967 (R 47, c 25) | git/review | **missing** · no jj in docs |
| 3 | "Staged and Unstaged diffs" | Zed #26560 (closed; R 897, c 119) | git/review | **partial** · design.md § Changes view: staged/unstaged pills and filter, read-only |
| 4 | "Commit partial changes / line-by-line staging" | Zed #45295 (open; R 150, c 12); #26430 multi-select in git panel (R 63) | git/review | **missing** · no stage action (decision D2) |
| 5 | "Git 3-way merge"; "side-by-side merge conflict resolution" | Zed #34813 (open; R 169); #58974 (open; R 97); Helix #5132 (R 86) | git/review | **missing** · E-6 "conflict bar" covers disk changes only |
| 6 | "Branch Merging" | Zed #57982 (open; R 159, c 14) | git/review | **has** · design.md § Changes view "Delivery" (Finish… merges a worktree); ADR 0020 |
| 7 | "Support history with external ACP agents" | Zed #37074 (closed; R 260, c 82) | agents | **has** · agents.md: "the Hutan journal is the history" |
| 8 | "Support for Agent Skills" | Zed #49057 (closed; R 192, c 20); #59088 (R 15) | agents | **missing** · no skills in agents.md or assistant.md |
| 9 | "file history viewing, local and Git"; "Git: Commit Log" | Zed #16827 (closed; R 261); #47402 (closed; R 62) | git/review | **missing** · no commit-log view in design.md or CHANGELOG |
| 10 | "Git blame and URL" | Helix #3035 (open; R 116); Zed #29686 (R 12) | git/review | **missing** · no blame in docs |
| 11 | "customize prompt for git commit message" | Zed #26823 (closed; R 169, c 25); #49222 (R 33, c 48) | git/review | **partial** [INFERENCE] · Finish commits and pushes (ADR 0020); message source undocumented |
| 12 | "Automatic Compaction" | Zed #35019 (closed; R 61, c 16) | agents | **partial** · context meter chip (agents.md); agents.md: "there is no compaction yet" |
| 13 | "Wait/retry when agent hits rate limits" | Zed #31531 (closed; R 55) | agents | **has** · design.md § Recoverable provider errors ("Retry now"); CHANGELOG 0.2.0 |
| 14 | "'Always allow' should apply only to the current tool" | Zed #30313 (closed; R 62) | security | **has** · status.md UX-CHAT-1: scope of "Always"; control.md "Always allow this tool in this workspace" (Mawas) |
| 15 | "agent can run rm -rf $HOME without warning" | Zed #37343 (closed; R 48, c 10) | security | **has** · agents.md § Approvals shows "where it would act"; UX-CHAT-1 cards |
| 16 | "Parallel Agent Execution" | Zed #51426 (closed; R 32, c 12); Cursor #139703 (28 likes) | agents | **has** · vision.md; design.md § Agent monitor |
| 17 | "Different model for subagents" | Zed #52042 (closed; R 35, c 15) | agents | **partial** [INFERENCE] · subagent tree shown; no per-subagent model documented |
| 18 | "Custom Modes missing in Cursor 2.1.0" | Cursor #143218 (327 likes, 97 posts); #144170 (80 likes) | agents | **missing** · agents.md: permission profiles fixed; only Mawas sets a mode |
| 19 | "How to open Agent Window and Editor Simultaneously" | Cursor #139304 (152 likes, 60 posts); #140035 (31) | layout/UI | **has** · design.md § Layout: split panes, movable panels, Agents overview ⌘⇧O |
| 20 | "Cursor is expensive"; "Usage Page $$ to Token Amount?" | Cursor #126446 (479 likes, 290 posts); #167153 (296 likes, 97 posts) | cost/usage | **partial** · Resources panel per-session cost (design.md § Activity monitor); no account-level usage found [INFERENCE] |
| 21 | "Cursor Android mobile app" | Cursor #165496 (193 likes, 84 posts) | mobile | **missing** · vision.md "mobile" later; roadmap M9 feasibility |
| 22 | "Add Code Folding" | Helix #1840 (open; R 508, c 49); Lapce #749 (R 29) | editor | **partial** · E-2 parses folds (status.md); no fold UI in editor.md [INFERENCE] |
| 23 | "Editor line wrap"; "Support soft wrap" | Lapce #945 (closed; R 85, c 27); Helix #136 (closed; R 109) | editor | **missing** · design.md § File tabs: "no wrapping" |
| 24 | "Debugging overhaul and redesign"; "Debugging support with DAP" | Helix #5950 (open; R 267); #505 (R 31); Lapce #812 (R 12) | editor | **missing** · no debugger in design.md, editor.md, CHANGELOG |
| 25 | "automatic reload when file changes externally" | Helix #1125 (open; R 356, c 82) | editor | **has** · design.md § File tabs "Changes on disk"; E-3 `fs.watch` |

Other strong items already covered: Helix #401 session persistence (R 393) → has (layout and file tabs persist); Helix #1976 integrated terminal (R 337) → has (terminal splits, `:term`); Lapce #700 Vim (R 46, c 71) → has (editor.md E-8); Helix #2177 open file in running editor (R 66) → has (`hutan open`); Helix #405 side-by-side diff (R 256) → partial (unified only).

## 2. Five most important missing needs

1. **Remote and container execution (row 1).** Largest signal: Zed #11473 (930 reactions, 163 comments), with remote bugs following it. Hutan's daemon model (vision.md principle 1) is the right base, but remote transport is still M9. [INFERENCE on fit]
2. **Git history and blame (rows 9–10).** Zed #16827 (261) and Helix #3035 (116). Read-only, daily, cheap to build. Changes shows only the diff against the merge base.
3. **Staging granularity and conflict resolution (rows 4–5).** Zed #34813 (169), #45295 (150), #58974 (97), Helix #5132 (86). Read-only Changes is decision D2, so revisit the decision with the user rather than treating it as an oversight.
4. **Agent skills and user-defined modes (rows 8, 18).** Zed #49057 (192), Cursor #143218 (327 likes). Hutan's ACP harnesses carry skills and modes that Hutan neither shows nor configures (agents.md). [INFERENCE] users expect Hutan to surface them.
5. **Usage and cost clarity (row 20).** Cursor #126446 (479 likes) and #167153 (296 likes) are the most-liked cost complaints. Hutan already tracks per-session cost and context, so account-level usage is the gap. [INFERENCE]

## 3. Evidence gaps

- **Helix**: the top list is dominated by 2021–22 issues; 2025–26 items top out near 55 reactions.
- **Lapce**: low activity. Its top 100 are all-time; the newest top items have 2 reactions. Not representative.
- **Zed**: reaction counts are current totals, not creation-window counts; label sweeps are not exhaustive; I read about 10 bodies, not 25.
- **Cursor**: the Ideas category mixes product feedback with model requests (Kimi, GLM, DeepSeek) and pricing complaints, so likes are directional. The most-liked threads are UI-churn complaints (#139648, 295 likes; #143002, 259 likes).
- **Hutan status** is a doc keyword grep. "Missing" rows (blame, commit log, debugger, skills) rest on no mention in `docs/`; verify in code before acting.
- Not checked: Helix Discussions, Lapce chat channels, Zed `area:ai` items after 2026-06.
