# Community opinion: meta-assistants / companions in dev tools (for Mawas)

Question: does the community want a meta-assistant (watches agents and sessions, has memory, acts with approval) as a headline feature, or as a quiet utility that is off by default? What do people expect from memory and privacy?

## Method and search log

**Hacker News (primary, read in depth).**
- Algolia search, `https://hn.algolia.com/api/v1/search` and `search_by_date`, `tags=story` and `tags=comment`, `numericFilters=created_at_i>1735689600` (from 2025-01-01), `hitsPerPage` 20–30.
- Queries, batch 1 (earlier in this task, about 40 queries): agent orchestrator, Claude Code session manager, agent dashboard/notifications, Recall, memory assistant, local-first memory, and related terms.
- Queries, batch 2 (stories and comments): `OpenClaw`, `Clawdbot`, `Moltbot`, `OpenClaw security`, `OpenClaw memory`, `Claude memory`, `ChatGPT memory`, `memory feature ChatGPT`, `AI agent with access to my computer security`, `personal AI agent always on`, `AI assistant remembers everything`, `Claude Code remote control phone`.
- Queries, batch 3 (comments): `approval fatigue agent`, `agent asks permission too often`, `proactive assistant annoying`, `AI assistant watching my screen`, `always listening assistant creepy`, `AI agent notifications interrupt me`, `human in the loop approve agent actions`, `agent memory deletion forget control`, `agent dashboard needs attention`, `AI agent undo rollback trust` (no hits printed).
- Thread trees read via `https://hn.algolia.com/api/v1/items/<id>`. Algolia's items payload does not include per-comment points, so "praise vs critical" counts below are my reading of visible top-level comments and their replies. They are [INFERENCE], not vote totals.

**Reddit (secondary, snippet-only).** `https://www.reddit.com/r/<sub>/search.json` returned HTTP 403. `old.reddit.com` returned 302. Direct thread reads returned a "Prove your humanity" challenge or a network block. Reddit evidence comes from `web_search` with `site:reddit.com` (queries: "OpenClaw experience memory security", "Claude memory feature opinion privacy", "AI agent approval notifications needs my input dashboard", "Screenpipe OR Microsoft Recall privacy"). **Scores and comment counts for Reddit are unavailable.** Reddit items below are titles or snippets only, so they cannot be used for agree/disagree counts.

**Primary and secondary sources checked.**
- Anthropic memory announcement (claude.com, Sep 11 2025; updated Oct 23 2025 for Pro/Max): "Memory is *fully optional*, with granular user controls".
- Microsoft Recall opt-in change: secondary only (PCMag "Microsoft Makes Controversial Recall Feature Opt-In"; TechTarget). Microsoft Learn returned 403.

## Findings: HN (scores are story points / comments from Algolia)

### A. Meta-assistant / always-on agent ("OpenClaw" wave, 2026)

1. **OpenClaw is a security/trust problem for personal machines.** Critical.
   - Quote: "You are not supposed to install OpenClaw on your personal computer" (title).
   - Link: https://news.ycombinator.com/item?id=47129647 — 237 pts / 184 cmts.
   - Voices: [INFERENCE] of visible top-level and reply comments, critical ~4 of 5, one "sane developers ... abandoning all of [security best practices] simply because it's AI".
2. **"Security nightmare" framing.**
   - Quote: "OpenClaw is a security nightmare dressed up as a daydream" (title); comment: "This problem is inherently unsolvable because LLMS are prone to hallucinations and prompt injection attacks."
   - Link: https://news.ycombinator.com/item?id=47479962 — 397 pts / 297 cmts.
   - Voices: [INFERENCE] mostly critical, with a minority offering mitigations (separate accounts, VMs).
3. **Sandboxing is seen as insufficient; approval is the crux.**
   - Quote (comment): "automation on the approval process but only with very strong guards in place… approval fatigue is an…"
   - Quote (comment): "It has access to your personal stuff (that's its main use case), access to the net, and it gets untrusted third party inputs. That's the unfixable trifecta right there."
   - Link: https://news.ycombinator.com/item?id=47154803 — 112 pts / 103 cmts.
   - Voices: [INFERENCE] ~1 approval-positive comment vs ~3 critical in the visible set.
4. **Memory reliability is the core complaint about the persona.**
   - Quote (title): "OpenClaw's memory is unreliable, and you don't know when it will break."
   - Quote (comment): "it is a very unreliable piece of software. I'm constantly having to fix it."
   - Link: https://news.ycombinator.com/item?id=47721955 — 168 pts / 177 cmts.
   - Voices: [INFERENCE] ~2 praise (uses it daily, pings and crons) vs ~2 critical (anterograde amnesia, unreliable).
5. **Praise: "changing my life" and daily use.**
   - Quote (title): "OpenClaw is changing my life."
   - Link: https://news.ycombinator.com/item?id=46931805 — 340 pts / 513 cmts.
   - Counter (top comment): "There's an odd trend with these sorts of posts where the author claims to have had some transformative change... but also seemingly has nothing to show for it."
   - Voices: [INFERENCE] mixed; praise is common, but the most-replied comment is a skeptic.
6. **"Who is using it?" yields real use cases, plus skepticism about fake praise.**
   - Quote: "I still use it and find it helpful. My OpenClaw instance uses an Obsidian project as its memory." (memory in user-owned, version-controlled files)
   - Link: https://news.ycombinator.com/item?id=47783940 — 342 pts / 383 cmts.
   - Counter quote (comment): "My immediate reaction... is 'That's great, but it would have taken the same effort to ask your LLM to write a script'."
   - Voices: [INFERENCE] roughly even between users and skeptics.
7. **Productive use cases (praise, team-level).**
   - Quote: "We run an OpenClaw agent for our entire team... Runs our standups, checks in with everybody EOD on blockers."
   - Link: https://news.ycombinator.com/item?id=47147183 — 106 pts / 87 cmts.
   - Voices: [INFERENCE] praise-heavy; examples are concrete, but the thread also has cost complaints ("~$75/week").
8. **Acts with money/accounts draws objection.**
   - Quote (comment): "I just don't see a reason to allow OpenClaw to make purchases for you, it doesn't feel like something that a LLM should have access to."
   - Link: https://news.ycombinator.com/item?id=46872465 — "A sane but bull case" 303 pts / 481 cmts.
   - Voices: [INFERENCE] the "bull case" is praised, but access-to-money comments are critical.
9. **"Bigger than an assistant" is seen as hype.**
   - Quote (title): "OpenClaw isn't fooling me. I remember MS-DOS."
   - Link: https://news.ycombinator.com/item?id=47831437 — 307 pts / 333 cmts.
   - Voices: [INFERENCE] critical.

### B. Memory expectations (Claude, ChatGPT, local-first)

10. **Claude's memory is praised as natural, ChatGPT's is seen as creepy.**
    - Quote: "Claude memory weaves it's memories of you into chats in a natural way, while ChatGPT feels like a salesman."
    - Link: https://news.ycombinator.com/item?id=45684134 — "Claude Memory" 559 pts / 312 cmts.
    - Counter quote: "For every time that I'd get a better answer if the LLM had a bit more context on me... there seems to be a multiple of that where the 'memory' was either actually confounding."
    - Voices: [INFERENCE] praise and critical roughly split; the critical comments are about bleed-through, not about the existence of memory.
11. **Cross-context bleed.**
    - Quote: "I love Claude's memory implementation, but I turned memory off in ChatGPT... it was weird when it was making associations across things that aren't actually associated in my life."
    - Link: https://news.ycombinator.com/item?id=45214908 — "Claude's memory architecture is the opposite of ChatGPT's" 448 pts / 236 cmts.
    - Voices: [INFERENCE] mostly praise for Claude's scoped approach; a few comments about ads and business models in the ChatGPT case.
12. **Memory should be a file you own.**
    - Quote (comment): "instead of the memory being locked away with a specific vendor, it's stored in version control."
    - Link: https://news.ycombinator.com/item?id=47783940 (see item 6).
    - Also: "Why would you not use context files in form of .md?" (19 replies) — https://news.ycombinator.com/item?id=45516584 — "Recall: Give Claude memory with Redis-backed persistent context" 171 pts / 93 cmts. Critical of the Redis design.
    - Voices: [INFERENCE] ~3 for file-based / user-owned vs ~1 for "a hosted store is fine".
13. **Memory accretion / needs an opinionated default.**
    - Quote: "Initial read suggests the memory accretion continues indefinitely... Any plans for a more opinionated way to handle memory (but still within user's control)?"
    - Link: https://news.ycombinator.com/item?id=48819808 — "Rowboat – local-first alternative to Claude Desktop" 219 pts / 99 cmts (2026-07-07).
    - Voices: [INFERENCE] one clear request; not contested in the visible set.
14. **"Remember everything" is valued in principle, feared in practice.**
    - Praise quote: "the ability to remember and recall your activity is a fantastic feature for almost all users."
    - Link: https://news.ycombinator.com/item?id=43660914 — "Recall going back into Windows" 110 pts / 123 cmts (2025-04-12).
    - Counter: "Who, exactly, is clamoring for Recall in the first place?" https://news.ycombinator.com/item?id=44657291 — "Brave blocks Microsoft Recall by default" 290 pts / 296 cmts (2025-07-23; older than the 2025 window but the strongest privacy anchor).
    - Voices: [INFERENCE] praise is a small minority compared with the privacy-critical replies.

### C. Passive recording and privacy (default-on backlash)

15. **Screen-recording memory is treated as surveillance.**
    - Quote: "I dont want any agent or any llm to know all the time what i have been doing on my system, it would be privacy nightmare."
    - Link: https://news.ycombinator.com/item?id=49024620 — "Launch HN: Screenpipe – Record how you work and turn that into agents" 88 pts / 67 cmts (2026-07-23).
    - Counter: "This sounds so ripe for abuse and so terrifying if it were that I don't even want to experiment with it."
    - Voices: [INFERENCE] ~1 positive (uses it for "AI buddies") vs ~4 critical (privacy, cloud, abuse).
16. **Always-on recording devices are a privacy issue, even if local.**
    - Quote: "Even if the model runs locally, there's still a serious privacy issue: Unwitting victims of something recording everything they said."
    - Link: https://news.ycombinator.com/item?id=47094977 — comment in "Every company building your AI assistant is now an ad company" https://news.ycombinator.com/item?id=47092203 (315 pts / 165 cmts, 2026-02-20).
    - Voices: [INFERENCE] predominantly critical in the visible set.
17. **Default-on data collection is a trust breaker.**
    - Quote: "This bait-and-switch with privacy is what annoys me."
    - Link: https://news.ycombinator.com/item?id=48737226 — "Installing Cursor on iOS irreversibly changes your privacy settings" 249 pts / 34 cmts (2026-06-30).
    - Voices: [INFERENCE] ~8 critical, 0 defenders in the visible set.
18. **Local-first alternatives are a stated preference.**
    - Quote (comment): "zero chance im trusting any cloud or third party SaaS if this doesn't run fully local its a no go for enterprise."
    - Link: https://news.ycombinator.com/item?id=49024620 (item 15).
    - Voices: [INFERENCE] ~2 local-first demands vs ~1 "local is fine".

### D. Agent manager / notifications / approval (the "needs you" queue)

19. **Notifications when agents need you (demand).**
    - Quote (comment): "Can I make the notifications work with Gemini CLI and Kiro CLI too?"
    - Link: https://news.ycombinator.com/item?id=47079718 — "cmux – Ghostty-based terminal with vertical tabs and notifications" 198 pts / 77 cmts (2026-02-19).
    - Companion: "Omnara – Run Claude Code from anywhere" https://news.ycombinator.com/item?id=44878650 — 310 pts / 168 cmts (2025-08-12). Praise: "using Claude Code from your phone is kind of great." Critical: "So you've reinvented SSH+screen except slower and much less flexible?"
    - Voices: [INFERENCE] ~2 praise vs ~2 critical; the notification request is the strongest recurring signal.
20. **Approval fatigue.**
    - Quote (HN "IAXT" launch, 1 pt, https://news.ycombinator.com/item?id=48830666, maker comment, truncated in the source): "First is approval fatigue: on a long Claude Code or Cursor run you…"; the design choice is to record and review after the session "instead of trying to catch it live".
    - Quote (comment): "managing multiple agents leads to approval fatigue and human-in-the-loop design goes for a toss" (vendor comment, https://news.ycombinator.com/item?id=49947124, 2026-10-03; 0 points).
    - Voices: [INFERENCE] thin (2 comments); they agree on the pain. The IAXT launch itself had 1 point, so it is weak evidence.
21. **Parallel-agent chaos: "clash while editing files" and "lose mental context".**
    - Quote: "whenever i try to parallelize, they clash while editing files simultaneously, i lose mental context of what's going on, they rewrite tests etc. It's chaos."
    - Link: https://news.ycombinator.com/item?id=44533004 — "Vibe Kanban – Kanban board to manage your AI coding agents" 195 pts / 132 cmts (2025-07-11).
    - Voices: [INFERENCE] praise ("excellent") in ~2 comments vs ~3 pain points.
22. **Crowded market: "convergent ideas".**
    - Quote: "what's really fascinating isn't that multiple people have built the same thing is just how convergent all the ideas are."
    - Link: https://news.ycombinator.com/item?id=46588905 — "Agent-of-empires session manager" 118 pts / 44 cmts (2026-01-12).
    - Also: "Ask HN: agent orchestrator" https://news.ycombinator.com/item?id=46993479 — 41 pts / 60 cmts (2026-02-12), with "No point. Claude Code with skills and subagents is plenty."
    - Voices: [INFERENCE] skeptical about separate orchestrators; the signal is "subagents are enough".
23. **Inbox-style async agents: mixed.**
    - Quote: "why not an actual inbox? set up a gmail and have the agent email you?"
    - Link: https://news.ycombinator.com/item?id=49713894 — "Pizza Bot – An inbox for AI agents that work in the background" 61 pts / 37 cmts (2026-09-15).
    - Voices: [INFERENCE] ~3 critical of the form factor vs ~1 praise.
24. **Platform/vendor risk (adjacent).**
    - Quote (title): "Claude Code refuses requests or charges extra if your commits mention 'OpenClaw'" — https://news.ycombinator.com/item?id=47963204 — 1349 pts / 720 cmts (2026-04-30). Also "Anthropic no longer allowing Claude Code subscriptions to use OpenClaw" https://news.ycombinator.com/item?id=47633396 — 1099 pts / 827 cmts.
    - Relevance: not about Mawas directly, but it is a strong signal that assistants built on third-party harnesses carry vendor-policy risk. [INFERENCE] for Hutan.

## Findings: Reddit (titles and snippets only; no vote/comment counts available)

25. **Memory: consent and ownership.**
    - Title: "Anthropic injected Claude's memory without consent" — https://www.reddit.com/r/claudexplorers/comments/1rpcyag/anthropic_injected_claudes_memory_without_consent (r/claudexplorers, snippet dated Mar 9 2026).
    - Title: "Claude knows a lot about me. None of it is mine" — https://www.reddit.com/r/ClaudeAI/comments/1rgypoa/claude_knows_a_lot_about_me_none_of_it_is_mine (date not in snippet). A third-party memory layer is offered as a fix.
    - Complaint, snippet-only, [INFERENCE] from titles.
26. **Memory: praise.**
    - Title: "Completely taken off guard by the memory feature" — https://www.reddit.com/r/ClaudeAI/comments/1psrrg8/completely_taken_off_guard_by_the_memory_feature. Snippet: "Anthropic implemented memory into Claude is really good. If you use Projects properly to keep chats together by topic you can really leverage [it]."
    - Praise, snippet-only.
27. **Memory: edit and control.**
    - Snippet from "Claude memory - experience so far": "Memory edits don't 'fold in overnight' - they're permanent and immediate. When you add an edit, it's there. Remove it, it's gone."
    - Link: https://www.reddit.com/r/ClaudeAI/comments/1ooxqza/claude_memory_experience_so_far
    - Supports inspectable, editable memory.
28. **Waiting-for-input notifications (demand).**
    - Title: "No notification when a Claude Code session is waiting for input, any..." — https://www.reddit.com/r/ClaudeCode/comments/1qxdmcw/no_notification_when_a_claude_code_session_is (r/ClaudeCode, date not shown). Snippet: "killing my productivity when running multiple Claude Code sessions in parallel."
    - Also: "Stop babysitting your AI agents – Get a push notification when..." https://www.reddit.com/r/ClaudeCode/comments/1snfone/stop_babysitting_your_ai_agents_get_a_push
    - Also: "I built a free dashboard for managing parallel Claude Code sessions" (snippet dated May 19 2026): "auto-detecting whether each one is waiting for input, actively working, idle, or errored" — https://www.reddit.com/r/ClaudeCode/comments/1thuzpc/i_built_a_free_dashboard_for_managing_parallel
    - Demand is present across three threads. Vote and comment counts are unavailable.
29. **OpenClaw security (Reddit).**
    - Titles: "How are you securing your OpenClaw's credentials?" https://www.reddit.com/r/openclaw/comments/1svp6pp/how_are_you_securing_your_openclaws_credentials ; "Major Security Concerns? : r/openclaw" https://www.reddit.com/r/openclaw/comments/1slbrnb/major_security_concerns ; "OpenClaw security is worse than I expected and I'm not sure what to do about it" https://www.reddit.com/r/AI_Agents/comments/1r3u98p/openclaw_security_is_worse_than_i_expected_and_im
    - Consistent with the HN security framing. Snippet-only.
30. **OpenClaw memory (Reddit).**
    - "Why doesn't OpenClaw have memory by default?" https://www.reddit.com/r/openclaw/comments/1s9yduu/why_doesnt_openclaw_have_memory_by_default ; "What do you use for memory?" https://www.reddit.com/r/openclaw/comments/1s607qe/what_do_you_use_for_memory — snippet: "I've been hearing from people that the default memory configuration is not good enough. But, personally, I feel it's okay for my day-to-day use cases."
    - Split: users who want better memory vs users satisfied with the default. Snippet-only.
31. **Passive recording (Reddit).**
    - "Microsoft Recall takes screenshots of your screen every few seconds - it's a privacy nightmare!" https://www.reddit.com/r/microsoft/comments/1mdtu53/microsoft_recall_takes_screenshots_of_your_screen ; "Microsoft's new Windows 11 Recall is a privacy nightmare" https://www.reddit.com/r/privacy/comments/1cy795p/microsofts_new_windows_11_recall_is_a_privacy (2024, older).
    - Screenpipe appears on Reddit as "An open-source AI that remembers everything you do on your computer" (r/DiscoverProjects, https://www.reddit.com/r/DiscoverProjects/comments/1usy57c/...), with no visible reception. Thin.

## Verdicts

**(a) Mawas as a headline feature with its own page: MIXED, leaning contradicts.**
- Supports (interest in assistant personas): OpenClaw threads 340/513, 342/383, 303/481, 106/87; Moltis 131/52 (memory assistant). Demand exists.
- Contradicts (trust and reliability dominate the big threads): 397/297 security; 237/184 "not on your personal computer"; 307/333 "isn't fooling me"; 168/177 memory unreliable; 315/165 ad-company / always-listening. About 5 big threads are critical vs about 3 praise.
- Reading: the persona draws attention, but its most-replied comments are about security, reliability, and privacy. A headline page would put Hutan into the OpenClaw trust debate. The narrower job ("tell me when an agent needs me") has more consistent demand (see item 19 and Reddit 28).

**(b) Quiet utility, off by default: SUPPORTS.**
- Evidence: default-on recording and memory draws the sharpest backlash: Brave 290/296; Recall returns 110/123; Screenpipe 88/67 (4 critical vs 1 positive in visible replies); Cursor iOS privacy 249/34 (~8 critical, 0 defenders); always-listening 315/165 (critical).
- Anthropic's own memory design is opt-in ("fully optional") with per-project scope and incognito; Recall was reported as changed to opt-in after backlash (secondary: PCMag, TechTarget).
- Counter: proactive, always-there assistants are wanted by some (Omnara praise, OpenClaw daily users). Defaults off are not contradicted; the demand for proactivity is for opt-in use.

**Memory / privacy expectations (cross-cutting):**
- Inspectable, editable, deletable memory; per-project or per-context scoping; a clean-slate or incognito option (45214908; 45684134; Reddit 27; Anthropic memory doc).
- Memory as a user-owned file (Obsidian, git, .md) is preferred over a vendor store (47783940; 45516584 comment, 19 replies).
- Biggest complaint about memory is reliability and bloat, not its existence (47721955; 48819808).
- Privacy: local-first is a stated requirement for many; passive capture is distrusted even when local (49024620; 47094977).

## Takeaways for Hutan

1. The assistant persona has the largest HN engagement, but its top-replied comments are security, reliability, and privacy. Do not lead with "assistant"; lead with the "needs you" queue.
2. The strongest validated job is "tell me when an agent needs me" (cmux 198/77, Omnara 310/168, Vibe Kanban 195/132; Reddit notification threads). Mawas should start as the notifications/approvals layer.
3. Default-on observation or memory is the most common cause of backlash (Recall, Screenpipe, Cursor iOS). Keep Mawas memory and observation off by default, with a visible, editable memory and per-project scope.
4. Memory expectations: inspectable, editable, deletable, portable (markdown/git), and a clean-slate/incognito mode. Fix bleed-through and bloat before adding features.
5. Approvals: "approval fatigue" is the named risk. Batch approvals, show concrete diffs, and allow only narrow, reversible auto-approvals. [INFERENCE] from items 3 and 20; the evidence base for approval design is thin (2 comments).

## Limits
- HN scores are story points from Algolia. Per-comment points were unavailable, so "agree vs disagree" counts are my reading of visible comments [INFERENCE].
- Reddit: no scores or comment counts were retrievable (403/challenge). Reddit evidence is titles and snippets only.
- Recall's opt-in change rests on secondary sources (PCMag and TechTarget snippets); Microsoft pages returned 403.
- The approval-design evidence is thin (about two comments and one vendor comment).
- Much of the assistant discussion is about OpenClaw specifically, and may not transfer to Mawas's narrower scope [INFERENCE].
