# Community opinion: running several coding agents in parallel (orchestrators)

Scope: Hutan decisions (a) native lightweight Mac app vs Electron/web, (b) one "needs you" queue across agents, (c) terminal-faithful TUI agents vs chat-only UIs. Sources: Hacker News (primary), Reddit (snippet-level only, see Limits), vendor and dev-blog pages (labeled as such).

## Method and limits

- **HN search.** Algolia `search_by_date`, `tags=story` and `tags=comment`, `created_at_i > 1735689600` (2025-01-01 onward). Queries: Conductor, cmux, Superset, Vibe Kanban, Claude Squad, Crystal/Nimbalyst, Orca, "parallel coding agents", "running multiple Claude Code agents", "git worktrees agents", "agent needs attention notification", "Electron terminal app", "TUI vs chat UI agents", "tmux agents". Story pages read via `hn.algolia.com/api/v1/items/<id>` (about 60 threads, about 5,300 unique comments). Points and comment counts are from the item JSON. Relevance ranking of Algolia is weak, so thread selection was done by hand.
- **Reddit.** Direct access was blocked from this environment. `www.reddit.com/…json`, `old.reddit.com`, `api.reddit.com`, and page fetch returned HTTP 403 or "You've been blocked by network security" / "Prove your humanity". Reddit evidence therefore comes only from `web_search` with `site:reddit.com` plus topic terms. Those results give titles, dates, and short snippets. Vote and comment counts are shown only for a few threads. Treat every Reddit item as snippet-level and unverified in full context.
- **Subreddits in results:** r/ClaudeCode, r/ClaudeAI, r/ChatGPTCoding, r/cursor, r/AI_Agents, r/tmux, r/macapps, r/ExperiencedDevs, r/SideProject, r/LocalLLM.
- **Vendor and blog pages** (cmux README on GitHub, superset.sh and agentsroom.dev comparison pages, Product Hunt) are not community opinion. They are used only for product facts or to show what competitors claim.
- **Counts** are approximate, from hand-read threads. They are not exhaustive keyword counts. Keyword tallies were too noisy ("native", "terminal" match almost everything).
- **Thin spots:** Vibe Kanban's "Goodbye Bloop" post returned HTTP 500 and was not read. Claude Squad, Warp, and Zed have almost no community-opinion sources in this run; only vendor pages and comparison posts were found. Orca's stack is not confirmed (its README did not say Electron, so no claim is made).

## Findings

Link format: `https://news.ycombinator.com/item?id=N`. Points/comments are from the story item. Comment IDs are linked to the comment. "Voices" = distinct commenters in the threads read.

### (a) Native lightweight Mac app vs Electron/web

Praise for native/light (supports):
1. **cmux author:** "I tried a few coding orchestrators but most of them were Electron/Tauri apps and the performance bugged me." Vendor README by the author, not a community voice. https://github.com/manaflow-ai/cmux (README, "Native macOS app — Built with Swift and AppKit, not Electron").
2. **HN 49889214** (Cf, 2026-09-29): "5GB RAM gone on just an LLM harness doing nothing. At least Codex CLI in rust uses 80-100MB." https://news.ycombinator.com/item?id=49889214
3. **HN 49409366** ("Stop Making TUIs", 2026-08-23): "compared to hundreds of MB for a whole electron stack." https://news.ycombinator.com/item?id=49409366
4. **HN 49672151** (Rune, 2026-09-12): "First we got everything as HTML/Electron apps, now the trend is everything as TUIs." Argues real GUI apps are being abandoned. https://news.ycombinator.com/item?id=49672151
5. **HN 50151465** (whyho, thread 50148855, 168 pts / 127 comments): "I found orca way to cluttered and overkill. Cmux feels very lightweight." https://news.ycombinator.com/item?id=50151465
6. **HN 49800839** (joshstrange, JetBrains Air thread 49799287, 75 pts): "high ram/cpu usage when I'm not even 'using' the ide are so frustrating." Complaint about a heavy IDE. https://news.ycombinator.com/item?id=49800839
7. **Abralo author** (48832797, 37 pts / 31 comments): "Abralo has a Rust backend (0% CPU) and has one shared webview." Author claim in comment 48858502. https://news.ycombinator.com/item?id=48858502
8. **HN 49690202** (Sep 2026, parent thread not re-checked): "The Mac app is < 50mb." Praise for size. https://news.ycombinator.com/item?id=49690202

Against or neutral (contradicts / mixed):
1. **HN 49397333** (Stop Making TUIs thread): "Just use Electron. Yes, performance blahblah, but thousands of Electron apps work on millions of machines just fine." https://news.ycombinator.com/item?id=49397333
2. **HN 49072376** (Prokop, 2026-10-13): "went with PWA… Electron would add extra overhead." Chose web, not native, for pragmatic reasons. https://news.ycombinator.com/item?id=49072376
3. **Market evidence:** Superset is Electron (its README has an Electron badge: github.com/superset-sh/superset) and is widely used. Orca (cross-platform) is praised in 50150507 ("since using Orca IDE it feels like a leap in usability and robustness"). Herdr (TUI, 404 pts, 48756578) and Pi 1.0 (TUI-first, 1,689 pts, 49926069) show engagement without native GUI.
4. **Non-mention:** Most threads never mention the app's runtime. Performance is a secondary concern, and the complaints that do appear come from people running many sessions.

Praise count: about 8 voices (5 HN comments, 1 Abralo author, 1 cmux author, 1 HN comment about size). Against or neutral: about 3 voices. [INFERENCE] native matters at scale (many sessions, high RAM), not as a purchase driver. Most native-praise voices are authors or users of native tools, so bias is likely.

### (b) One "needs you" queue across agents

Supports (users name the need):
1. **HN 50157097** (rosod, thread 50148855): "Visual way to verify which sessions are running still, which need my assistance, and which are actually done." https://news.ycombinator.com/item?id=50157097
2. **HN 50074942** (Muster, Mac menu-bar switcher, 2026-10-13): "it can be hard to keep track of which agent is waiting for me." Its answer is an ⌥Tab cycle to agents waiting for you first. https://news.ycombinator.com/item?id=50074942
3. **HN 49107750** (Agent-Manager tmux TUI, thread 49107749, 98 pts / 80 comments): "One of them is usually sitting on a permission prompt I never saw." https://news.ycombinator.com/item?id=49107750
4. **HN 49813032** (Gwae, 2026-09-23): "Option+g cycles you to whichever pane needs attention (failures first)." https://news.ycombinator.com/item?id=49813032
5. **HN 49945677** (eranation, Offrun thread 49942434, 78 pts / 68 comments): "show which one needs my input, which one is blocked." https://news.ycombinator.com/item?id=49945677
6. **HN 49655485** (2026-09-11): Claude Code "Notification hook fires when it's waiting on a permission prompt or sitting idle." https://news.ycombinator.com/item?id=49655485
7. **HN 49180542** (hboon, Agent-Manager, 2026-08-05): "icons and indicators for blockers, ready sessions that need attention." https://news.ycombinator.com/item?id=49180542
8. **HN 49225543** (andai, Sep-era Message-your-Claude-sessions discussion, story 49222824, 173 pts): wants a ping "when my Claude/Codex finishes cooking (or needs my attention)." https://news.ycombinator.com/item?id=49225543
9. **HN 49439704** (MulmoTerminal, 2026-08-25): status indicators that "change colors… based on the LLM's state." https://news.ycombinator.com/item?id=49439704
10. **Reddit r/ClaudeAI 1rzgmm9** (snippet, "I run 3-5 Claude Code agents in parallel. Here's the tool I built"): "Which agent finished? Which one is stuck and needs my input?" https://www.reddit.com/r/ClaudeAI/comments/1rzgmm9/i_run_35_claude_code_agents_in_parallel_heres_the (snippet only)
11. **cmux README** (vendor): "When an agent is waiting, its pane gets a blue ring and the tab lights up in the sidebar… Cmd+Shift+U jumps to the most recent unread." https://github.com/manaflow-ai/cmux
12. **Offrun tagline** (product, 49942434): "See who is working, who needs you…"

Against or commoditized:
1. **HN 49106307** (Hissigh, Superlogical thread 49098965, 796 pts): "And I don't really care about notifications. I just switch back to the relevant workspace and check on things when I want to." https://news.ycombinator.com/item?id=49106307
2. **HN 49110237** (phito, 2026-07-30): "It's very easy to add hooks to your favorite harness to display a notification in tmux when it's done working." Says the queue is a DIY feature, which weakens it as a moat. https://news.ycombinator.com/item?id=49110237
3. **HN 49229197** (liamwire, 2026-08-09): "Does your terminal emulator not support bells/notifications?" Native terminal notifications may be enough. https://news.ycombinator.com/item?id=49229197

Counts: about 11 supporting voices (threads read), 3 against or commoditizing. Verdict signal strong for need, weaker for "key differentiator". [INFERENCE] the queue is a need users recognize but can build with hooks, so Hutan needs more than the queue itself (for example, review/merge flow).

### (c) Terminal-faithful TUI agents vs chat-only UIs

Supports terminal fidelity / TUI:
1. **HN 49932946** (jitl, Pi 1.0 thread 49926069, 1,689 pts): "i love that tui agents fit right in among my shells and vims." https://news.ycombinator.com/item?id=49932946
2. **HN 49944564** (Offrun thread 49942434): "It looks like crap. Why would I want the terminal coding agents inside of a GUI app? The idea is terrible." https://news.ycombinator.com/item?id=49944564
3. **HN 49397811** (yeodev, Stop Making TUIs thread): "I'd take TUI apps over electron apps every time as long as the UX is good." https://news.ycombinator.com/item?id=49397811
4. **HN 50150606** (frangonf, tmux thread 50148855): "if you have control over your setup, I would use a capable terminal and ditch in-band multiplexers." https://news.ycombinator.com/item?id=50150606
5. **HN 49989527** (Neptune, 2026): "couldn't find a terminal I liked for working with AI agents. cmux didn't run well on my Ubuntu setup." Shows demand for a real cross-platform terminal. https://news.ycombinator.com/item?id=49989527
6. **HN 48756578** (Herdr, 404 pts / comments on TUI multiplexer): TUI multiplexer with status (idle, blocked, working) in a left tab bar. https://news.ycombinator.com/item?id=48823618
7. **Reddit r/ClaudeCode 1rxwkoz** (snippet, "My thoughts on Conductor vs Superset vs cmux vs vanilla Claude Code", Mar 2026): "not having full native cli support is annoying." Complaint about a chat-style wrapper losing CLI fidelity. https://www.reddit.com/r/ClaudeCode/comments/1rxwkoz (snippet only)

Supports GUI / chat / hybrid:
1. **HN 49932225** (rcarmo, Pi 1.0): "I do use the TUI on occasion, but need a remote web UI for 99% of my stuff." https://news.ycombinator.com/item?id=49932225
2. **HN 49926278** (jacobgold, Pi 1.0): "you'd be even better off using a GUI agent multiplexer of some kind rather than juggling terminal tabs." https://news.ycombinator.com/item?id=49926278
3. **HN 49926269** (pizzafeelsright, Pi 1.0): "I ended up deploying a GUI wrapper because TUI isn't as friendly to new adopters." https://news.ycombinator.com/item?id=49926269
4. **HN 49926341** (w-ll, Pi 1.0): "What do you suggest for a GUI agent multiplexer?" Demand signal. https://news.ycombinator.com/item?id=49926341
5. **HN 50153453** (klodolph, tmux thread): "I can do terminal multiplexing better in the GUI—tabs, virtual desktops, or in the browser with xterm.js." https://news.ycombinator.com/item?id=50153453
6. **HN 50156930** (artemave, tmux thread): "I also wanted gui apps being part of the session, not just terminals." https://news.ycombinator.com/item?id=50156930
7. **HN 50152750** (Scea91, tmux thread): "Now I am 100% in the ChatGPT desktop app, because it felt I am doing way too much tinkering and maintenance of my setup." https://news.ycombinator.com/item?id=50152750
8. **HN 50078887** (TeMPOraL, Offrun thread): "I can't be arsed to deal with TUI at home anymore." https://news.ycombinator.com/item?id=50078887
9. **HN 49672151** (Rune): TUI trend criticized in favor of real GUI apps (see (a)).
10. **HN 49945576** (smokeydoe, Offrun thread): defends a GUI for managing many agents "difficult to manage with enough of them." https://news.ycombinator.com/item?id=49945576
11. **Reddit r/AI_Agents 1tgre44** (snippet): "CLI/TUI is somewhat limiting and complex workflows seem better expressed in a GUI." https://www.reddit.com/r/AI_Agents/comments/1tgre44 (snippet only)

Hybrid (both camps accept it): **HN 49926854** (trefoiled, Pi 1.0): "Orca is a GUI agent multiplexer that works with dozens of harnesses, including Pi." The Pi 1.0 thread (610 comments fetched, 124 mention TUI/GUI/terminal/desktop) shows the top voices favor a GUI multiplexer around real terminal sessions.

Counts: about 7 terminal/TUI-leaning voices, about 11 GUI/chat/hybrid voices (threads read). Verdict mixed. [INFERENCE] the most-supported shape is real terminal fidelity inside a GUI shell, not chat-only and not raw TUI.

### Competitive and market signals (not decisions)
- **Anthropic native Agent View** in Claude Code (code.claude.com/docs/en/agent-view; Reddit r/ClaudeCode 1tba6qk, snippet "Anthropic just shipped Agent View… managing 5+ parallel"). Direct threat to a needs-you queue as a standalone feature.
- **Conductor:** Mac, native per agentastic.dev; $22m Series A (job posting 47602101, 2026-04-01, secondary); cloud workspaces (HN 49118273, 2026-07-31, 4 pts). Reddit r/macapps weekly thread (snippet): "Was using it but stopped when the developer told me it would be US $10 per month." Pricing complaint.
- **Orca:** cross-platform (macOS, Windows, Linux per GitHub); HN 47549197 "Open Source 'Conductor + Ghostty'" (21 pts / 10 comments); "cmux is Mac only" (47549379); cluttered vs light (50151465). Mixed.
- **cmux:** macOS only "for now" (GitHub); lightweight praise (50151465); "I didn't like the way it did tabs" (47549379).
- **Superset:** Electron, source-available under Elastic License 2.0 (superset.sh/llms.txt); ~10.5k GitHub stars (search snippet). Positioned as "Code Editor for the AI Agents Era".
- **Herdr:** HN 48756578 (404 pts) "One terminal to rule them all"; joined YC (49201003, 281 pts), mixed reaction ("YC means VC means commercialization means enshitification", 49201599).
- **Vibe Kanban:** HN 44533004 (195 pts / 132 comments, 2025-07-11). Parallel agents "clash while editing files simultaneously" (44533339); "spent tens of thousands on Opus tokens and worktrees - it's just not that great" (44538697). Later community-run after Bloop shut down (agentsroom.dev, secondary).
- **Worktree conflicts:** Foremerge HN 49789356 (45 pts / 20 comments): "Git only notices if the resulting patches happen to touch the same lines." Reddit r/ClaudeAI 1wpixl9 (snippet): "intent conflicts" when two agents run in parallel.
- **Zed / Warp:** vendor pages only (zed.dev/blog/parallel-agents, April 22, 2026; warp.dev docs). No community-opinion source found in this run. Flag as thin.
- **Claude Squad:** comparison posts only (superset.sh, vibecodinghub.org). No direct community thread found in this run. Flag as thin.

## Praise vs complaints (short)
- Praise: worktree isolation and status dashboards (Herdr, Agent-Manager, Muster, cmux); lightweight native (cmux, Abralo); "which needs my input" indicators.
- Complaints: heavy RAM/CPU (JetBrains Air 49800839; JS harness 49889214); parallel edit clashes (Vibe Kanban 44533339); cost (Conductor $10/mo, Reddit snippet; Vibe Kanban token spend 44538697); notifications not valued by some (49106307); missing non-git VCS support (Herdr discussion #522); MCP auth broke inside herdr (49203412); "4 agent limit is a no for me" (Abralo 48851909); "code navigation is terrible" in Conductor (47626874).

## Verdicts for Hutan decisions

| Decision | Verdict | Evidence (approximate, hand-read threads) |
|---|---|---|
| (a) Native lightweight Mac app beats Electron/web | **MIXED** (leans supports on performance; not a purchase driver) | Supports: about 8 voices (HN 49889214, 49409366, 49672151, 50151465, 49800839, 49690202, Abralo 48858502, cmux README). Contradicts or neutral: about 2 explicit (HN 49397333, 49072376); Superset (Electron) and Orca are not penalized in threads read. |
| (b) One "needs you" queue is the key feature | **SUPPORTS** (need is strong; "key" is contested) | Supports: about 11 voices (HN 50157097, 50074942, 49107750, 49813032, 49945677, 49655485, 49180542, 49225543, 49439704; Reddit 1rzgmm9 snippet; cmux README). Against or commoditized: about 3 (HN 49106307, 49110237, 49229197). |
| (c) Users want terminal-faithful TUI agents, not only chat UIs | **MIXED** (terminal fidelity wanted; GUI multiplexer around it preferred by many) | TUI/terminal: about 7 voices (HN 49932946, 49944564, 49397811, 50150606, 49989527; Herdr 404 pts; Reddit 1rxwkoz snippet). GUI/chat/hybrid: about 11 voices (HN 49932225, 49926278, 49926269, 49926341, 50153453, 50156930, 50152750, 50078887, 49945576; Reddit 1tgre44 snippet). |

## Takeaways for Hutan
1. **Queue is the proven pain, but not a moat.** Many users ask "which needs me" (about 11 voices), yet DIY hooks and tmux colors already cover it (HN 49110237). The queue must come with review and merge flow, not stand alone. [INFERENCE]
2. **Keep real terminal fidelity inside a GUI shell.** The stated demand is "terminal-faithful but GUI-managed" (Pi 1.0 thread, tmux thread, Herdr vs Orca). Real PTYs for TUI agents plus a native multiplexer fit this better than chat-only panels or raw tmux. Chat-only panels get the most pushback (49944564, 49926278).
3. **Native performance is a credible differentiator, not a headline.** Memory complaints (49889214, 49800839) and "lightweight" praise (50151465) support a native app. Electron is not rejected (49397333), so the case must be made with measured RAM per agent, not as a tagline. [INFERENCE]
4. **Anthropic's native Agent View is a direct threat** to the queue as a standalone feature (Reddit 1tba6qk snippet; code.claude.com/docs/en/agent-view). Differentiation must come from cross-agent (Claude, Codex, OMP over ACP) queueing and review, not from showing sessions.
5. **Worktree isolation is table stakes, and conflicts are the next pain.** Users praise worktrees (Reddit 1vtfawm, 1ss7uh6 snippets) and complain about clashing edits (44533339) and "intent conflicts" (HN 49789356, Reddit 1wpixl9). A conflict/intent layer above worktrees is an open niche. [INFERENCE]

## Sources (primary links)
- HN items: 49942434, 48756578, 49201003, 49098965, 50148855, 49937540, 44533004, 48832797, 49789356, 47549197, 49107749, 49233448, 49799287, 47626598, 49926069, 49222824, 49118273, 50074942, 49813032, 49655485, 49180542, 49439704, 49110237, 49229197, 49889214, 49409366, 49397333, 49397811, 49672151, 49800839, 49072376, 49932946, 49932225, 49926278, 49926269, 49926341, 50153453, 50156930, 50152750, 50078887, 50150606, 50151465, 49989527, 49926854, 49945576, 49106307, 50157097, 49107750, 49945677, 49225543. Comment IDs are linked inline above.
- GitHub: https://github.com/manaflow-ai/cmux (README); https://github.com/stablyai/orca ; https://github.com/superset-sh/superset (README: Electron badge).
- Docs/vendor: https://code.claude.com/docs/en/agent-view ; https://superset.sh/llms.txt ; https://superset.sh/compare/claude-squad-alternative ; https://agentsroom.dev/blog/best-multi-agent-coding-tools ; https://zed.dev/blog/parallel-agents ; https://www.warp.dev/blog/universal-agent-support-level-up-coding-agent-warp.
- Reddit (snippet-level, via web_search): r/ClaudeCode 1rxwkoz, 1tba6qk, 1ryq4t7, 1vtfawm, 1ss7uh6; r/ClaudeAI 1rzgmm9, 1wpixl9; r/AI_Agents 1tgre44; r/macapps Conductor pricing (weekly thread, Jul 2025).
