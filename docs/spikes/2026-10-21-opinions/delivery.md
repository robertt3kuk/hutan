# Delivery opinions: commit/push to main, worktrees, checkpoints

Scope: how people want agent work delivered. Three decisions are judged: (a) finish = commit and push straight to main, no PR, for a solo developer; (b) default to the main checkout, suggest a worktree only when another agent is active or the tree is dirty; (c) checkpoints exist but are off by default.

## Method and limits

- **Hacker News (primary):** Algolia search at `hn.algolia.com/api/v1/search` (`tags=story` and `tags=comment`, `numericFilters=created_at_i>1735689600` i.e. 2025+, `hitsPerPage` 8–30; `search_by_date` not separately needed). Queries: "claude code worktree", "git worktree agent", "worktrees parallel agents", "claude code rewind", "claude code /rewind", "codex undo", "cursor checkpoints", "agent auto commit", "commit directly to main agent", "commit directly to main", "trunk based development agent", "coding agent pull request review", "agent checkpoints undo edits", "branch per agent pull request", "ai agent commit push main". Full trees pulled via `hn.algolia.com/api/v1/items/<id>`. Points and comment counts are HN values at fetch time.
- **Reddit: not usable.** `www.reddit.com/.../search.json` returned 403; `old.reddit.com` returned 404; reader fetches returned a bot-check page. Reddit findings below are web_search snippets only and are marked **[snippet, unverified]**. Treat Reddit as absent evidence.
- **Vendor docs and issue trackers** were read directly (Claude Code, Cursor, GitHub Copilot CLI, Codex issue reactions via `gh api` GET).
- **Thin spots:** most "Show HN" worktree tools have 0–10 points. Solo-developer-specific opinions on push-to-main are almost absent. Vote counts for HN comments are not visible in the Algolia payload, so agreement counts below are approximate hand counts of the fetched threads and labeled "~".

## Correction to earlier notes

The HN story 48947776 "Claude Code: Anatomy of a Misfeature" (2026-07-17, 142 pts, 121 comments; https://news.ycombinator.com/item?id=48947776, source essay https://www.olafalders.com/2026/07/17/claude-code-anatomy-of-a-misfeature/) is about Claude Code's **unattended behavior when it stops to ask a question** (it keeps going). It is **not primarily about checkpoints or commits**. The auto-commit remark (maxloh, 48948235) is a side point. Quotes from that thread are used below only as analogy for default-on behavior, and marked [INFERENCE].

## Findings

### 1. Commit/push straight to main vs branch + PR

**Praise for direct-to-main (human workflows, not agents)**
- Trunk-based thread, https://news.ycombinator.com/item?id=47098252 (2026-02-21, 73 pts, 9 comments), "Trunk Based Development":
  - fjfaase: "Working without branches, except for releases, is the most effective way of working, using rebase instead of merge to get a single line of commits." (agree)
  - 4pkjai: "I worked in a team of four ... this way. I really enjoyed it. After that I joined a company that worked with PRs. Felt like such a waste of time." (agree)
  - 47099495 (games team): "we do commit directly to main ... pre submit review, post submit checks." (agree with a gating caveat)
  - Disagree/caveat: jascha_eng: "I've rarely seen ... people actually commit directly to main. Except in very early stage projects. But it does always feel the fastest if you only review code 'on-demand' in PRs." (~2 voices, both caveated).
- Solo-dev direct commits are only jokes or one-liners in this data (44799527, 2025-08-05: "Good thing I always commit directly to the main branch."). Solo-specific evidence: **thin**.

**Complaints / gating for agents pushing to main**
- SDL/AI commits thread, https://news.ycombinator.com/item?id=47790791 (2026-04-16, 132 pts, 15 comments), reply 47797526: "It can't merge, doesn't have admin rights, etc. This after I started catching it commit directly to upstream main without PR." (one voice, agent-specific).
- Cursor force-push, https://news.ycombinator.com/item?id=46728766 (2026-01-23, 7 pts, 9 comments): hombre_fatal: "Prompt instructions are never sufficient... The tool call itself needs to be gated." apothegm: "I prohibit agents from accessing version control at all." compressedgas: instructions are "advisory." (~3 voices agree that rules are not enforcement).
- GitHub Copilot coding agent, https://news.ycombinator.com/item?id=44031432 (2025-05-19, 564 pts): timrogers: "Copilot pushes its work to a branch and creates a pull request... Copilot literally can't push directly to the default branch." r0ckarong (sarcastic): "Check in unreviewed slop straight into the codebase. Awesome." (product design choice = no direct push; ~1 praise, 1 complaint).
- "If AI writes code, should the session be part of the commit?", https://news.ycombinator.com/item?id=47212355 (2026-03-02, 497 pts, 392 comments): latexr: "A better solution would be to read and understand the code before committing it." xigoi: committing AI code without prompts is "like committing compiler-generated machine code." (~2 voices for review-before-commit; the thread is mainly about attribution, not push policy.)
- Ask HN teams using AI, https://news.ycombinator.com/item?id=42701745 (2025-01-14, 40 pts, 32 comments): consensus is normal code review; "AI does not commit code; people do." (~3 agree on human review; anotherpaulg (aider) reviews via `/diff` and branch diffs.)
- Claude 3.7 launch thread, https://news.ycombinator.com/item?id=43163011 (2025-02-24, 2127 pts, 135 comments): eschluntz used "pushing to master" as the example of a command that needs gating (single example).

**Tally for (a):** supporting voices (trunk/direct-main, human) ~3–4; contradicting/gating voices (agent-specific) ~5–6. The gating complaints are about agents, not solo humans. [INFERENCE] The community objection is "agent actions need a gate", not "solo developers must open PRs."

### 2. Auto-commit per turn / commit-after-success

- Claude Code docs: checkpointing is separate from git, and Claude Code does not commit. [docs]
- Positive: tfrancisl, https://news.ycombinator.com/item?id=48063548 (2026-05-08, Git for AI Agents, 129 pts, 67 comments): "Just use git... skills and hooks". ozim: "commit after successful build". (~2 agree.)
- Negative: boombapoom (same thread): "I think of git more like a defense and quality control against AI slop than something that should be automated." (~1 voice.)
- Aider auto-commits (HN 43168124 on the Claude 3.7 thread): a user was scared by auto-commits; another said a throwaway branch is "a nice way to work." (mixed, ~1 each.)
- Session-in-commit debate (47212355): maxloh-style and xigoi-style voices want commits that are reviewed, not auto-generated. [INFERENCE] auto-commit per turn is seen as noise in history unless squashed; no strong praise found.

### 3. Git worktrees per agent

**Love / in use (parallel work)**
- Claude Code official docs, https://code.claude.com/docs/en/worktrees: `claude --worktree <name>` creates `.claude/worktrees/<name>/` on a branch; subagents accept `isolation: worktree`. (Vendor first-party, not community opinion; shows direction.)
- Twill.ai comment, https://news.ycombinator.com/item?id=47722434 (2026-04-10): "Claude code can kick off parallel agents working in their own worktrees natively now."
- https://news.ycombinator.com/item?id=47495347 (2026-03-23): "I do parallel agents in worktrees... come back tomorrow." (1 voice).
- https://news.ycombinator.com/item?id=47601726 (2026-04-01), Baton: "running parallel agents on different worktrees ... for at least a month." (1 voice).
- https://news.ycombinator.com/item?id=47037246 (2026-02-16): "three agents in parallel in three separate work trees." (1 voice).
- Direnv/worktrees, https://news.ycombinator.com/item?id=47372855 (2026-03-14, 30 pts, 8 comments): users add WorktreeCreate hooks to copy `.env`; Roocode uses `.worktreeinclude`. (~3 voices on setup scripting.)
- Ask HN, https://news.ycombinator.com/item?id=46292682 (2025-12-16, 70 pts, 66 comments): weeksie: "Claude Code + worktrees (manual via small shell script)." (1 voice.)
- Zed/Cursor comment, https://news.ycombinator.com/item?id=47867228: "everyone is going towards parallel agents and worktrees" (1 voice).
- Reddit [snippet, unverified]: r/ClaudeCode/comments/1ss7uh6 "Git worktrees are the move - git worktree add ../feature-x feature-x lets separate Claude Code sessions work the same repo without stomping each"; r/ClaudeCode/comments/1wkb413: "Worktrees keep agents from editing the same working directory, but they don't guarantee that a later release includes everything..." (a caution about integration).

**Complaints / caution**
- Agentastic, https://news.ycombinator.com/item?id=46501758 (2026-01-05, 11 pts, 22 comments): avree: "Every time I've seen people use Git worktrees with agents, it's incredibly wasteful... Each one needs to build its own context." Rebuttal frio/justanothermatt: "developer capacity is the bottleneck." ahmadyan: per-worktree setup.sh. (~1 complaint vs ~2 rebuttals.)
- Docker/containers, https://news.ycombinator.com/item?id=44533044 (2025-07-11, 265 pts, 29 comments): sothatsit: copying per agent; "harder to review... tedious to manage [worktrees]." nojs keeps permanent numbered worktrees. (~1 complaint, ~1 alternative.)
- Sculptor, https://news.ycombinator.com/item?id=45427697 (2025-09-30, 176 pts, 85 comments): uses containers, not worktrees; nvader: "Sculptor does not use work trees, but that is an implementation detail." thejash: snapshot container each turn for rollback. (~2 choose containers/snapshots over worktrees.)
- Claude docs caveat (vendor): worktree is "a fresh checkout, so initialize your development environment there"; worktrees share `.git`. Also `baseRef` defaults to the default branch ("fresh"); uncommitted main-checkout changes are not carried over unless `head` is chosen.
- GitButler alternative, https://news.ycombinator.com/item?id=44799194 (2025-08-05, 8 pts): mojombo: "With Claude Code's new lifecycle hooks, GitButler auto-sorts simultaneous AI coding into separate branches." (1 voice; "without worktrees" framing.)
- "How Claude Code Broke My Git Worktree", https://news.ycombinator.com/item?id=48571146 (2026-06-17, 3 pts, 0 comments): title only; no community signal.

**Tally for (b) worktree use:** pro-worktree / parallel voices ~6 (mostly one-liners); caution voices ~3 (avree, sothatsit, Reddit integration caution). Setup-cost complaints recur (~4 mentions). Vendor direction: worktree is now an explicit flag, not the default.

### 4. Checkpoints, undo, rewind

**Vendor defaults (on, not opt-in)**
- Claude Code, https://code.claude.com/docs/en/checkpointing: "Claude Code automatically tracks Claude's file edits as you work." Keeps 100 most recent checkpoints; snapshots swept about 30 days after last save (`cleanupPeriodDays`). Limitations: Bash `rm/mv/cp` not tracked; subagent edits mostly not restored; external and concurrent-session edits not tracked; symlinks/hard links skipped. Docs: "Checkpoints are designed for quick, session-level recovery... continue using version control, such as Git."
- Cursor, https://cursor.com/docs/agent/overview (Checkpoints section): "Agent automatically creates them before making significant changes"; "Checkpoints are stored locally and separate from Git. Only use them for undoing Agent changes; use Git for permanent version control."
- GitHub Copilot CLI, https://docs.github.com/en/copilot/how-tos/copilot-cli/use-copilot-cli/roll-back-changes: double-Esc or `/undo`/`/rewind`; "Conversation only" or "Conversation + files"; limits: files over 10 MB, more than 500 changed files per turn, and files edited by the user are skipped. Sessions started before file tracking was enabled cannot restore files.

**Demand signal**
- Codex issue, https://github.com/openai/codex/issues/11626 (opened 2026-02-12, open, 49 comments, 229 reactions: 228 +1 and 1 -1 per GitHub API): "CLI: Add /rewind checkpoint restore that reverts both chat context and Codex-applied code edits." Strong demand for code+conversation rewind in Codex.
- Third-party: git-rewind npm https://www.npmjs.com/package/git-rewind; agent-rollback https://github.com/Nainish-Rai/agent-rollback (created 2026-06-09).

**Community comments**
- Show HN Git for AI Agents, https://news.ycombinator.com/item?id=48063548 (2026-05-08, 129 pts, 67 comments): Zambyte: "may culminate in a single commit. It may be useful to track or undo things between commits." (agree with checkpoints.) doshay: found "/rewind functionality in claude very lean" (1 voice, mild complaint about feature depth.)
- Reddit [snippet, unverified]: r/ClaudeAI/comments/1u8cgpc "Claude Code's rewind isn't an undo button": "/rewind restores Claude's edits and the conversation, not bash side effects or outside changes. Treat it as local undo, keep git as the..."
- Claude Code misfeature thread (see correction above): enraged_camel, https://news.ycombinator.com/item?id=48948524 (2026-07-17): "should be gated behind a toggle that is off by default, and designed to be enabled per session via prompt." redhale, https://news.ycombinator.com/item?id=48951085: "I like this feature as an OPTION that is DOCUMENTED... not that's how it was rolled out." (2 voices for opt-in on an autonomy feature; [INFERENCE] transferable to checkpoints as a default-setting question, not directly about checkpoints.)
- Undo-loss complaint: maxloh, https://news.ycombinator.com/item?id=48948235 (2026-07-17): "Claude Code doesn't auto-commit when stopping for an answer. There might be possible data loss if an uncommitted file is edited." hombre_fatal (same): "relying on Claude Code's internal version control puts you at the same mercy of their product decisions." wgd: file state recoverable from conversation history (1 voice).

**Tally for (c):** vendors ship checkpoints on by default (3 of 3 checked: Claude, Cursor, Copilot CLI). Demand for rewind is high (Codex 229 reactions; 2+ HN voices). Default-off advocacy: ~2 voices (enraged_camel, redhale), about a different autonomy feature. No direct evidence found that users want checkpoints disabled by default.

## Verdicts

Verdict = community and vendor evidence in this file. Counts are approximate.

- **(a) Finish = commit and push straight to main, no PR, for a solo developer: MIXED, leaning CONTRADICTS for agent-driven pushes.** Supporting ~3–4 (trunk-based/direct-main humans: fjfaase, 4pkjai, games-team comment, trunk thread). Contradicting ~5–6 (gate agent pushes: 47797526, Cursor force-push 46728766 thread, Copilot design 44031432, boombapoom, eschluntz). Solo-specific evidence is thin. [INFERENCE] The objection is to unreviewed agent pushes, not to solo trunk workflows; a gate (pre-push check, protected branch, or diff review) is the common ask.
- **(b) Default to main checkout; worktree only when another agent is active or tree is dirty: MIXED, leaning SUPPORTS.** Supporting ~6 (parallel worktree users; Claude makes worktrees an explicit flag, not default). Contradicting ~3 (setup cost, wastefulness, tedious; avree, sothatsit). Caveat: Claude's worktree starts from the default branch, so a "dirty" trigger must not silently drop uncommitted work [INFERENCE]; the dirty trigger is a Hutan design choice with no direct community evidence found.
- **(c) Checkpoints exist but are off by default: CONTRADICTS (on default is the norm).** Vendor evidence 3 of 3 default-on (Claude "automatically tracks", Cursor "automatically creates", Copilot CLI rewind). Demand evidence: Codex 229 reactions, Zambyte/doshay comments. Default-off support ~2 voices (enraged_camel, redhale), about a different feature. Opt-in framing could still be defended for storage cost, but no source here argues for it on checkpoints.

## Five takeaways for Hutan

1. Gate agent pushes, not solo humans: the strongest community objection is an agent committing to main without review (47797526, 46728766). Require the agent to finish on a branch or behind a push gate enforced in the tool, not in prompts (46728766: "The tool call itself needs to be gated").
2. Checkpoints should be on by default, with clear limits: vendors all ship them on, and users want code+conversation rewind (Codex 11626: 229 reactions). Document that bash edits and external edits are not tracked, as Claude's docs do.
3. Keep git as the permanent record: Cursor and Claude both state checkpoints are separate from git. Hutan's rewind should not replace commits.
4. Worktrees: offer them, do not force them. Setup cost (`.env`, deps, context) is the repeated complaint; Claude's `--worktree` flag and `.worktreeinclude` show the fix. Consider a hook for setup.
5. Reddit could not be read. A follow-up with an alternate method (e.g., a logged-in browser or the Reddit API) is needed before any solo-developer claim is treated as settled.
