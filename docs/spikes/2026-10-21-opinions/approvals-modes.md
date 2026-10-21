# Community opinion: approvals, modes, queue vs steer, notifications

Research date: 2026-10-21. Topic: `approvals-modes`. Requested by Hutan (OpApprovals).

## Method and reproducibility

- **Hacker News (Algolia API)**, `tags=story`, `created_at_i>1704067200`, both `search_by_date` and `search`. Queries: "approval fatigue", "claude code permissions", "yolo mode agent", "dangerously-skip-permissions", "always allow agent", "permission prompts", "approval prompts", "sandbox agent", "bypass permissions", "agent asks permission", "Claude Code permissions".
- **HN comments**, `tags=comment`, `created_at_i>1735689600`. Queries: "approval fatigue", "permission prompts", "yolo mode claude", "always allow", "auto mode claude code", "bypass permissions", "queue messages while agent", "interrupt the agent", "steering agent mid-task", "type while claude is working", "message queue coding agent", "notification when claude needs input", "agent waiting for my input notification", "don't ask again permission", "allow for session claude", "steer codex". Most comment hits were noise; only the items below were used.
- **GitHub** (`gh search issues --sort reactions`, `gh issue view --json reactionGroups`). Repos: `anthropics/claude-code`, `openai/codex`. Queries: "permission mode changed", "mode reset", "accidentally approved", "accidentally allowed", "approval prompt", plus the queue/steer and always-allow titles. Reaction count = sum of `reactionGroups`. Comment count is the GitHub `comments` total.
- **Vendor / product docs** (read): code.claude.com/docs/en/permissions, /permission-modes, /interactive-mode; anthropic.com and claude.com posts; kiro.dev queue-steering docs; GitHub Copilot CLI steering docs; Codex CLI features docs.
- **Reddit: blocked from this environment** (www 403 / network block page, old.reddit block page, reader blocked). Only web_search titles were seen. They are listed below as **unread [INFERENCE]**, not used as evidence.
- Vendor-reported figures are labeled **(vendor)**. They are not independent evidence.

## (a) Show the harness's real permission mode; never change it silently

**Supports (visibility and explicit control)**
- Docs: the status bar shows the active mode (`⏸ manual mode on`, `⏵⏵ auto mode on`). Shift+Tab cycles modes. The Remote Control dropdown shows the actual local mode. Source: https://code.claude.com/docs/en/permission-modes
- Users want explicit control of defaults: #98159 "let users set a default permission mode, incl. 'Skip all approvals'", 21 reactions, 10 comments, open (2026-09-29). Request, not praise.
- HN 49214994 (Aug 14 auto-default thread, 23 pts, 9 comments), AussieWog93: months of use with no catastrophe under auto. Praise voice: 1.
- Vendor: "users approve 97%" of prompts; auto-mode default for new Pro/Max/Team sessions Aug 14 (https://claude.com/resources/articles/auto-mode-default-in-claude-code). (vendor)

**Contradicts (silent or surprising mode changes)**
- Aug 14 auto-default flip, HN 49214994 (https://news.ycombinator.com/item?id=49214994): kelnos "I absolutely don't want it changing code without my permission"; dexterlagan "I felt a bit uneasy. Can we trust them now?"; graceful6800: auto mode broke an SSH config. Three voices against, one for.
- Mode reset: #27380 "Permission bypass mode resets when switching between concurrent sessions", 10 comments, 8 reactions, closed NOT_PLANNED (2026-02-21).
- Mode label does not match behavior: #39523 "Bypass permissions mode is fundamentally broken", 37 comments, 19 reactions, open (2026-03-26). Says `bypassPermissions` still prompts.
- Mode indicator bug: #80716 "Auto-mode classifier incorrectly detects permission mode change in plan mode", 7 comments, open.
- Anthropic's own docs: a project `.claude/settings.json` cannot set `auto` or `bypassPermissions` as the starting mode. If the user's `defaultMode` is not auto, sessions keep it, and the user is asked once on Pro/Max/Team. The first auto session shows a one-time notice. These are protections, but they show that default-mode changes were made to users (docs, permission-modes).

**Verdict (a): MIXED, leaning CONTRADICTS for silent change.** Evidence: 3 HN voices plus 3 issues (#27380, #39523, #80716) against silent or mismatched modes. 2 supporting voices or requests (AussieWog93; #98159). Hutan's rule (show the real mode, never change it silently) matches what users complain about.

## (b) Messages typed while the agent works are queued (visible, editable), not steered

**Supports queueing as the visible default**
- Claude Code ships queue-by-default. Docs: "Type a message and press Enter while Claude is working. Claude Code queues the message instead of interrupting the turn." Queued entries are shown in gray, and "Take back what you queued" with Up returns them to the input, editable (https://code.claude.com/docs/en/interactive-mode). Shipped behavior, not vendor marketing.
- #50246 "Message queue mode: queue messages instead of interrupting active tasks", 254 reactions, 77 comments, closed COMPLETED 2026-08-17 (https://github.com/anthropics/claude-code/issues/50246). Largest engagement on this topic.
- Queue-side requests: #30677 (VS Code, queue instead of interrupt, 22 comments, open); #33323 (task queue, 28 comments, open); #34599 Codex "Messages sent during active work can be dropped", 4 reactions, 4 comments, open (users want them kept, not lost).
- Anthropic staff reply on #64624 (2026-08-17, bcherny): "a message you send while Claude is working shows as queued, but it's delivered to Claude at the next tool-call boundary within the current turn." (https://github.com/anthropics/claude-code/issues/64624)
- Kiro and GitHub Copilot CLI document mid-turn steering as the optional mode (https://kiro.dev/docs/cli/chat/queue-steering/, https://docs.github.com/en/enterprise-cloud@latest/copilot/how-tos/copilot-cli/use-copilot-cli/steer-agents). These are vendor docs.

**Supports steering as the escape hatch (demand that queue-only does not meet)**
- #64624 "Real-time steering: send message mid-generation without queueing", 18 reactions, 12 comments, closed COMPLETED 2026-08-17. Commenters: happymontana ("works exceptionally well on OpenAI Codex"), FedeCuci, deep-lyra ("interrupting and typing context again is a pain"), natha909 ("my only mid-task option is the stop button, which discards in-progress work"), Zombi3h3art (reports it now steers, asks for a toggle). Five-plus voices for steering.
- #30492 "Real-time steering: priority message channel", 36 comments, closed. Says Escape is destructive.
- Codex: #13044 "Swap TAB / ENTER for Steering and Queuing", 7 reactions, 7 comments, closed COMPLETED. #13595 "Restore immediate send on 'enter' and queueing via 'tab'", 5 comments, closed. #42610 "Enter to immediately steer a queued message", 1 reaction, 1 comment, open. #23615 "Queue, Steer, (new) Stash", 2 comments, open.
- Cancel is destructive: radialmonster (#64624, 2026-06-14): "that risks stopping it in the middle of work and leaving files in a bad state."

**Confusion about queue semantics (both sides)**
- #36326 "Docs say Enter interrupts mid-task, but it only queues the message", 2 reactions, 4 comments, closed NOT_PLANNED (2026-03-19).
- #16905 "Esc key interrupt disabled when queued messages or background tasks exist", 14 reactions, 7 comments, open.
- #62349 "/cancel to clear queued messages without interrupting", 2 reactions, 4 comments, closed.

**Verdict (b): MIXED, leaning SUPPORTS queue as the default.** Queue-side evidence: 4 items (#50246 with the most reactions, #30677, #33323, #34599) plus Anthropic's shipped behavior and docs. Steer-side evidence: 6 items (#64624, #30492, #13044, #13595, #42610, #23615), with the sharpest complaints about lost in-progress work. Queue as the default, with an explicit, labeled "send now / steer" action, is supported. Hutan's "visible, editable" queue matches the editable part of Anthropic's design ("Take back what you queued").

## (c) Approval cards default to the safe choice and state the scope of "always"

**Supports stating the scope**
- Anthropic docs: "Yes, and don't ask again" scope: Bash = "Permanently per repository and command"; file edits = "Until session end"; WebFetch = "per repository and domain". "Claude Code offers those options only when the prompt can show you everything they would allow, so a rule you save from a prompt covers only what its option named." (https://code.claude.com/docs/en/permissions). Vendor doc, but it is the design principle Hutan wants.
- Scope confusion produces complaints (these are the failure cases the rule prevents):
  - #11380 "Claude continually asks for permission, even after selecting yes, always allow", 64 reactions, 82 comments, closed COMPLETED (2025-11-10).
  - #30519 "Permissions matching is fundamentally broken", 81 reactions, 31 comments, open. "Always Allow" saves the verbatim string (e.g. `git commit -m "fix typo"`) as a dead rule.
  - #31523 "Permission system UX: ... rule accumulation", 19 reactions, 5 comments, open. 150+ narrow rules after 3 months.
  - #74715 "'Always allow' for Claude-in-Chrome site permissions is always persisted as duration:'once'", 11 reactions, 20 comments, open (2026-07-06).
  - #29400 "'Don't ask again' does not persist", 3 comments, closed.
- HN 47343927 Show HN permission guard (127 pts, 35 comments): niobe "The current permissions solution is unbelievably poor." (complaint).

**Supports a safe default (accidental approvals are the harm)**
- #70622 "Add option to disable clickable Yes/No prompts in terminal", 85 reactions, 23 comments, closed COMPLETED (2026-06-24). "constantly accidentally cancelling things, or more dangerously approving things."
- #9765 "IntelliJ Plugin: Diff tab automatically steals focus, causing accidental approvals", 18 reactions, 21 comments, closed NOT_PLANNED (2025-10-17). Keystrokes go to the diff approval.
- Auto mode: vendor says approvals were 93% (Mar) and 97% (Aug) of prompts, which means most prompts are clicked through (vendor). Simon Willison: "Confirmation fatigue is real" (https://simonwillison.net/2026/aug/8/auto-mode). Independent voice.
- Shift+Tab "with no field open, it selects the option that allows the action for the rest of the session, when the prompt offers that option" (interactive-mode docs). One key can grant session-scope approval; this is a scope trap.

**Counter-evidence (always as the primary action)**
- #36258 Cowork: "'Always allow' is the primary action button, and 'Allow once' requires clicking a small dropdown", 5 reactions, closed NOT_PLANNED (2026-03-19). Only item against.

**Verdict (c): SUPPORTS.** Scope-stating: 6 items for (#11380, #30519, #31523, #74715, #70622, #9765), 1 against (#36258). Evidence supports "Allow once" as the safe default and an explicit statement of scope on "always". Note: the docs' Yes-first ordering was observed; whether "Yes" is pre-focused was not verified [INFERENCE].

## Notifications when input is needed (supporting context)

- Demand is high: openai/codex #3962 "Play a sound when Codex finishes a prompt / task", 222 reactions, 56 comments, open (2025-09-20). Claude Code remote push: HN 47148454 "Claude Code Remote Control", 544 pts, 65 comments; HN 46491486 "Claude Code On-the-Go", 543 pts, 69 comments. HN 47033722 Show HN Claude Relay (1 pt): "coming back to find Claude Code waiting for approval."
- Counter: HN 47391803 "LLMs can be exhausting", 348 pts, 50 comments: "I've disabled the in-your-face agent-is-done/need-input notifications." HN 46903558 "My AI Adoption Journey", 984 pts, 62 comments: "Don't let the agent notify you."
- Suppress-when-focused is praised: HN 47209148 Show HN Ccnotifs (2 pts): suppresses when the pane is focused. Commenter: "that's the kind of thing that separates useful from annoying."

**Verdict (notifications): MIXED.** Demand for a "needs you" alert is strong (large reactions/points). Nagging is the complaint. Focus-aware suppression is the praised pattern.

## Thin or weak sources (flagged)

- HN 50127073 (1 pt, 0 comments). HN 48069567 (5 pts). GitHub #92279 "Auto mode: let a classifier block fall back to a permission prompt instead of a hard deny" (3 comments, open; example: `git push origin develop v0.42.0` was blocked and the model told the user to run it by hand). #42610 (1 reaction). #23615 (2 comments). #36258 (5 reactions).
- Reddit (unread, titles only, [INFERENCE] relevance): 1vjqcvf "Anthropic Flips Claude Code to Auto Mode by Default Aug 14"; 1vn7unx "Auto mode goes default tomorrow (Aug 14)"; 1ldi19c "Claude Code ignores my 'ask permission before git commit' setting"; 1rhtfpz "Claude code permissions discussion".

## Five takeaways for Hutan

1. **Queue is the expected default; steer must be a labeled second action.** Claude Code ships queue-by-default (#50246: 254 reactions). Steering demand is real (#64624, #13044) but comes with a warning that cancel-to-steer loses work. A "send now at next tool boundary" action matches what Anthropic says it does.
2. **Show the scope of "always" in the card, and preview what it covers.** Users got dead exact-string rules (#30519, 81 reactions) and 150+ narrow rules (#31523). Name the scope: repo plus command, domain, or session.
3. **Make "Allow once" the focused default; never let mouse clicks or focus moves approve.** #70622 (85 reactions) and #9765 show accidental approvals. #36258 shows "Always" as primary drew the same complaint.
4. **Never change mode silently; make the mode a per-session, visible value.** The Aug 14 auto default drew "I absolutely don't want it changing code without my permission" (HN 49214994). Mode reset across sessions (#27380) and bypass that still prompts (#39523, 37 comments) break trust.
5. **Notify on "needs you," with focus-aware suppression and a quiet mode.** Sound/push demand is large (#3962: 222 reactions; HN 47148454: 544 pts). Nagging is the counter-complaint (HN 47391803: 348 pts; HN 46903558: 984 pts).

## Verdicts per decision

- **(a) Show the real permission mode; never change it silently: MIXED, leaning CONTRADICTS on silent change.** 3 HN voices + 3 issues against (#27380, #39523, #80716); 2 supports (AussieWog93; #98159 request).
- **(b) Messages typed while working are queued (visible, editable), not steered: MIXED, leaning SUPPORTS queue as default.** Queue side 4 items (#50246 254 reactions, #30677, #33323, #34599); steer side 6 items (#64624, #30492, #13044, #13595, #42610, #23615). Shipped Claude Code behavior is queue-by-default.
- **(c) Approval cards default to safe choice and state the scope of "always": SUPPORTS.** 6 items for (#11380, #30519, #31523, #74715, #70622, #9765); 1 against (#36258).
