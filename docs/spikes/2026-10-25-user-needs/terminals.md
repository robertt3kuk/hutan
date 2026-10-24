# Terminals slice: what users of similar tools ask for (2026-10-25)

Scope: terminal-first agent tools. Sources: `manaflow-ai/cmux`, `wavetermdev/waveterm`, `warpdotdev/Warp`, `ghostty-org/ghostty`. Counts are `reactions/comments` on the issue (all-time, as shown by GitHub). `GhD#n` = Ghostty discussion, whose count is its vote total.

Links: issues are `https://github.com/<repo>/issues/<n>` with repo `manaflow-ai/cmux`, `wavetermdev/waveterm`, `warpdotdev/Warp`, `ghostty-org/ghostty`. Discussions are `https://github.com/ghostty-org/ghostty/discussions/<n>`.

## 1. Top 25 needs

Hutan status: `has` = documented as built; `partial` = some of it documented; `missing` = not in docs. Paths are under `/Users/homie/proj/hutan/docs/` (status from docs only; no code read).

| # | Need (user's words) | Evidence (repo#, reactions/comments, state) | Category | Hutan status (doc) |
|---|---|---|---|---|
| 1 | Run it on Linux and Windows | cmux#330 221/57 open; cmux#1012 52/19 open; waveterm#76 74/21 closed 2023 | other | **missing**: macOS-first; roadmap.md:193 lists Linux/Windows as later direction |
| 2 | Use local models and any OpenAI-compatible endpoint (Ollama, LM Studio, localhost, custom headers) | Warp#4339 1447/145 reopened; Warp#9303 47/23 closed; Warp#11589 43/15 open; Warp#12142 42/8 open; Warp#15781 25/8 open (headers) | integrations | **has** for endpoints (gateway.md:26–27, assistant.md:39); custom request headers **missing** |
| 3 | BYOK without a paid plan | Warp#9288 150/25 closed "BYOK still requires a Pro plan" | cost/usage | **has** (provider keys in gateway.md:22–35; no plan gate in docs [INFERENCE]) |
| 4 | Drive other agents over ACP, not only the vendor's CLI | Warp#7326 87/34 reopened, open | agents | **has**: ACP harness chats (agents.md; not line-checked) |
| 5 | Terminals survive app quit and reattach (tmux/zellij-like) | cmux#1663 27/10 open; Ghostty#1935 tmux control mode 755/22 open; Warp#3935 zellij 169/10 open (2023) | terminal | **has**: daemon owns PTYs, closing UI detaches (design.md:20, 798) |
| 6 | Restore tabs, panes and layouts after restart | cmux#480 50/16 closed; waveterm#2057 12/3 open; waveterm#2072 3/0 open | layout/UI | **has**: Sessions & restore page (design.md:473) |
| 7 | Quick/dropdown terminal on a global hotkey | Ghostty#2384 107/55 closed; Ghostty#4624 40/20 closed (Linux); cmux#2758 26/8 closed; cmux#2411 25/5 open; waveterm#2128 8/2 closed | terminal | **partial**: global shortcut only for Mawas (design.md:849) |
| 8 | SSH/remote workspaces with persistence | cmux#1664 38/5 open; Warp#6831 72/6 open; Warp#6744 79/23 open (WSL); cmux#559 16/15 closed; waveterm#707 25/9 closed | remote/SSH | **missing**: roadmap.md:110–111 (M9 not started); shell-integration.md:47 (ssh out of scope) |
| 9 | Paste images into a terminal, including over SSH | cmux#3435 22/21 closed; cmux#3639 15/14 closed; waveterm#2261 6/1 open; GhD#10517 14 votes, 3 comments | terminal | **missing**: design.md:646 refuses; images go to chat only |
| 10 | Find text in terminal scrollback | Ghostty#189 1221/110 closed; GhD#10476 1 vote (answered: shipped in 1.3) | terminal | **missing**: only file ⌘F (design.md:673), no terminal find found |
| 11 | Vim-style scrollback navigation; open scrollback in an editor | GhD#3708 75 votes, 52 👍, 10 comments (answered with key tables); Ghostty#2504 54/13 open | terminal | **partial**: wheel and ⇧PageUp history (design.md:614); no keyboard nav or open-in-editor found |
| 12 | Unlimited scrollback; keep alternate-screen output | Warp#6957 58/0 open; cmux#2334 20/6 open | terminal | **partial**: paged history `terminal.history` (design.md:614); cap not checked |
| 13 | Clickable file paths and links that open in an editor | Ghostty#1972 230/71 closed; cmux#1544 24/6 closed; cmux#1778 23/2 closed | terminal | **has**: design.md:631–641 |
| 14 | Notifications when an agent finishes or needs input; not flaky or late | cmux#1027 44/8 open; cmux#2322 22/2 open; Warp#7896 OSC 9 22/5 closed; Ghostty#5634 OSC 99 32/1 open | notifications | **has** for agent attention (design.md:226–255); OSC 9/99 parsed (protocol.md:202), user-visible mapping not confirmed [INFERENCE] |
| 15 | Merge a tab into a split; detach a tab to a window; split a tab | Warp#8959 23/14 open; Warp#9030 22/13 open; cmux#3297 25/8 closed; cmux#1362 19/6 open; Ghostty#10092 52/2 open | layout/UI | **partial**: "Move tab to split" by drag (design.md:53); per-tab splits (design.md:651–657); tab-to-window not found |
| 16 | Rename and colour tabs | Ghostty#2509 76/44 closed; Warp#8642 24/11 open | layout/UI | **partial**: rename (design.md:216, 535); tab colour not found |
| 17 | Hide or move sidebar and chrome (top sidebar, hide tab bar, icons) | Warp#7288 72/35 closed; cmux#2919 28/5 open; waveterm#1022 8/4 open | layout/UI | **partial**: docks left/right/bottom (design.md:41), panel checklist (design.md:70); top sidebar not found |
| 18 | Customize keybindings and remap | Ghostty#5160 106/17 closed; cmux#135 31/15 closed; cmux#1756 20/4 closed; waveterm#1943 12/9 open | terminal | **has**: keymap.toml (design.md:350, 376, 465) |
| 19 | Review agent diffs: stage, commit selected, side-by-side | Warp#8542 44/9 open; Warp#7043 28/10 open; Warp#9017 23/9 open; cmux#609 16/6 open | git/review | **partial**: Changes read-only, unified diff (design.md:606–608); Finish commit+push (orchestration.md:149); no staging (decision D2) |
| 20 | Undo or checkpoint agent edits | Warp#6802 27/9 open | agents | **has**: CK-1…CK-5 (design.md:886) |
| 21 | Token and cost counter | Warp#5993 46/21 closed | cost/usage | **has**: usage labels (design.md:571) |
| 22 | Control agent permissions; no silent bypass | cmux#3547 27/20 closed; cmux#4005 21/6 closed | security/permissions | **has**: Agent control & security page (design.md:473) |
| 23 | Memory, CPU and latency fixes | cmux#4529 22/25 closed; cmux#2962 17/10 open; Warp#6736 28/34 closed; cmux#2408 15/2 open | performance | **has** budgets (perf.md:20; B2) [INFERENCE: coverage not checked] |
| 24 | Rich output in the terminal (markdown tables, inline images) | Warp#8299 22/12 closed (dup); roadmap.md:159 planned | layout/UI | **missing**: terminal images not done (roadmap.md:159, vision.md:66); chat markdown only (design.md:269) |
| 25 | Amazon Bedrock and cloud-hosted model providers | Warp#8506 68/11 open; waveterm#2160 6/0 open | integrations | **missing**: no Bedrock kind in gateway.md; Codex Bedrock account state only (agents.md:268) |

## 2. Five most important missing needs

1. **Remote and SSH workspaces with persistence.** Most-reacted terminal cluster after Linux: Warp#6831 (72/6), Warp#6744 (79/23), cmux#1664 (38/5), cmux#559 (16/15), GhD#10517. Hutan has the daemon/client split that makes it possible, but M9 remote transport is not started (roadmap.md:110–111) and SSH is explicitly out of scope for shell integration (shell-integration.md:47). Expected users: anyone running agents on a remote box.
2. **Quick terminal on a global hotkey.** Ghostty#2384 (107/55) and #4624 (40/20), cmux#2758 (26/8) and #2411 (25/5). Hutan's only global shortcut is for Mawas (design.md:849), so the Carbon hot-key path already exists and could be reused for a terminal window. Cheap, visible, high-engagement.
3. **Paste images into a terminal.** cmux#3435 (22/21) and #3639 (15/14), GhD#10517 (14 votes, 3 comments; the maintainer says the feature belongs in the core). Hutan refuses (design.md:646). Agent users paste screenshots constantly; the chat accepts them, the terminal does not.
4. **Find and scrollback navigation in terminal output.** Ghostty#189 (1221/110) is the most-engaged terminal feature in this set; GhD#3708 has 75 votes and 52 👍; #2504 (54/13). Ghostty's answer on #10476 says search shipped in 1.3. Hutan has only file find (design.md:673).
5. **Linux and Windows.** cmux#330 (221/57), cmux#1012 (52/19), waveterm#76 (74/21). Largest demand signal, but this is a strategic macOS-first choice (roadmap.md:193), not a quick fix. Listed for the decision record, not as a feature to build now.

## 3. Gaps in evidence

- Warp `gh issue list` returned 504; used `gh search issues -R warpdotdev/Warp "created:>2025-03-01" --sort reactions --order desc --limit 100` and `gh search issues ... "label:enhancement" "created:>2025-06-01"`, then `gh issue view <n> --json reactionGroups,comments,stateReason` per issue. Warp#7123 (Copilot) counts were not fetched (rate limit).
- Ghostty `gh issue list` also 502/504; same search fallback. Ghostty feature asks mostly go to Discussions, which `gh` cannot search, so votes come from `read` of each discussion page (logged-out vote count); five discussions were read (#10476, #3708, #9912, #10517, #12064); others are cited from search snippets without votes. Issue counts understate Ghostty demand.
- cmux and waveterm via `gh issue list -R <repo> --state all --search "sort:reactions-+1-desc" --limit 100 --json number,title,state,createdAt,reactionGroups,comments,labels,url`. Many top items are 2023–2024; recent 2026 items are fewer in number.
- Waveterm has low engagement (most items under 10 reactions) and is mostly 2023–2025; treat as weak signal.
- Warp's recent top items are dominated by BYOK, AI and agent features; few are terminal-specific. Warp is a commercial AI terminal, so it is a weaker proxy for Hutan's terminal-first positioning.
- Hutan status comes from docs only (no code read, no build). `has` means documented; I did not verify the behaviour. Lines were checked by `grep` and `read`; a few cites (agents.md ACP, perf.md coverage) are not line-checked.
- Reaction and comment counts are all-time; older items (Warp#4339, Ghostty#189) dominate the top of the list.
- No search covered Ghostty or Warp for 2026 items beyond the top-100 per query, so recent but lower-reacted needs may be missing.
