# Vibe Kanban: UX/UI pain points (for Hutan)

Repo verified: `BloopAI/vibe-kanban` (28,409 stars at search time; `gh repo view`).
Snapshot: GitHub issues as of the 2026-10-21 search. Issues cited as `#N` are in that repo.

## Method (reproducible)
- Labels: `gh label list -R BloopAI/vibe-kanban`. Labels are only `bug, enhancement, feature, onboarding, coding agents, codex, triage, question, help wanted, duplicate, invalid, wontfix, documentation, good first issue`. There is **no UX/design label**, so UX was found by search.
- Search: `gh search issues --repo BloopAI/vibe-kanban --sort reactions --limit 20-50 '<term>'` for `ux`, `ui`, `confusing`, `keyboard`, `focus`, `notification`, `review`, `diff`, `settings`, `theme`, `scroll`, `sidebar`, `approval`, `mobile`, `login`, `workspace`, `onboarding`, `worktree cleanup`, `slow lag`, `where hidden`, `vim`. Some calls failed transiently and were retried with backoff. `vim` and `where hidden` returned 0 hits. `layout` and `panel` were not re-run after transient failures, so they are not covered.
- `gh search --json` does not expose reactions. Reactions and comment bodies come from `gh api repos/BloopAI/vibe-kanban/issues/N` (`reactions.total_count`) and `.../issues/N/comments`.
- Reaction counts below: 👍 = `+1`; ❤ = `heart`. "c" = comment count.
- Docs checked: `https://vibekanban.com/docs/cloud/getting-started`.

Thin areas: keyboard/vim, theming, accessibility and onboarding have almost no UX-specific issues. Keyboard hits are #2276 (next/prev task, 0👍, 2c) and #2804 (inline-code shortcut, 0👍, 1c). Vim has no hits. Those areas need a different source, such as Discussions or Reddit.

---

## 1. Redesign replaced the classic board; the old path was removed
- Problem: the "Workspaces (beta)" UI became the default. Users lost the one-board-per-project kanban they relied on, and could not go back.
- Links: #2687 "Bring back old UI" (33👍, 39c, closed) · #2288 "Is the workspaces (beta) UI intended to eventually replace the current (classic) UI?" (17👍, 26c, closed) · #2730 "Personal Kanban View with Legacy UX" (15👍, 0c, open) · #2923 (8👍, 2c, closed) · #2646 "How to switch back to kanban mode" (7👍, 3c, closed).
- Quotes: #2687: "Previous UI was more UX friendly for me and was more 'kanban' than new one." #2288: "it completely does away with the concept of a 'task' … in favor of a 'workspace'." #2730: "completing the same workflow now takes 2 steps."
- Workaround users posted: downgrade with `npx vibe-kanban@0.1.8` (#2687 comment, 2026-02-11). Escape hatch: `/local-projects` (#2646 comment by iamriajul). Neither is in the UI.
- Maintainer reply (#2288, ggordonhall): "the workspaces view is **not** going to completely replace the kanban board."
- UX principle violated: user control and freedom; preserving the user's mental model during change; progressive disclosure.
- Hutan should: keep the board as the home surface, add workspaces/sessions as a view beneath it, and never remove the working path. Any new mode should be opt-in with a visible switch back.

## 2. Two-step interaction replaces single-step drag
- Problem: moving a card between columns needs a second step in the new UI.
- Link: #2730 (15👍, 0c, open). Same complaint in #2509 "Beta UI feedback: please keep the Kanban board" (15👍, 3c, open): "that 'win' feels buried or visually muted." It also says "the feedback button is broken."
- UX principle violated: efficiency of common actions; visible feedback for completion.
- Hutan should: keep drag-to-column as the one-gesture status change. Give completion clear positive feedback (state change, not a muted badge).

## 3. Login and cloud sync gate core local features
- Problem: local projects and the board were moved behind sign-in and cloud sync. Users could not tell what was uploaded.
- Links: #2952 "Why do I need to log in to use features such as Kanban?" (10👍, 1c, closed) · #2687 comments (mehusm001-lab: "Why can't users use kanban board offline(not logined)?", 32👍 on that comment; byczasty: "Don't force users to log in just to access the core features.", 21👍) · #2812 (10👍, 4c, closed) · #2724 "what data is shared with new UI?" (7👍, 1c, open) · #3354 "Can we get back the local only projects?" (5👍, 10c, open) · #3396 "Support for self-hosted projects and better export" (19👍, 9c, open).
- Quotes: #2812: "I was **forced to migrate my local projects to the cloud**, which I do not want." #2724: "I can't sync my company's private IP up into your servers." #3396 (body): "0.1.44 has now forced the shutdown of Projects – they are export-only and cannot be accessed."
- Docs contradict this: the Cloud getting-started page says "Cloud uses the same interface you're familiar with," and its table lists local projects as "No sign-in required … Works offline" (vibekanban.com/docs/cloud/getting-started). The users' experience is different.
- Context: #3396 body says Bloop is "shutting down as a startup." A secondary source (superset.sh comparison page) dates the shutdown announcement to 2026-04-10 [INFERENCE: not verified against a primary source].
- UX principle violated: consent and transparency about data flow; no-account local-first mode; never remove the user's local data.
- Hutan should: keep local mode fully working offline with no account. Show a per-project "stays local / synced" indicator, and preview exactly what will be uploaded before any sync.

## 4. Migration hides or loses user data without a clear explanation
- Problem: after an upgrade, projects and tasks disappeared, or old data was shown in a different place.
- Links: #2687 (body: "after upgrade ALL old tasks and PROJECTS are gone") · #2696 "after installing 0.1.9 I can't see my projects anymore" (3c, closed) · #3329 "Workspaces disappear from kanban cards when local workspace is cleaned up" (0👍, 0c, open).
- UX principle violated: visibility of system status; error prevention and recovery.
- Hutan should: run migrations with a pre-flight summary ("12 projects, 40 tasks will move"), keep the old data visible until the user confirms, and never make items vanish silently when a backing workspace is cleaned up. Show a "data archived" state instead.

## 5. Diff and review are hidden by default; the main action is hard to find
- Problem: the side-by-side diff, which users call the core value, is off by default. Common actions are hard to locate.
- Link: #2300 "New UI, good look and feel, worst UX" (5👍, 9c, open). Body quotes: "side-by-side diff interface, toggled off by default and i need to turn it on every time when i open agent session." Other lines: "Where is the word-wrap button?", "where is 'CREATE PR' and 'Rebase' buttons?", "How to return from task to kanban board?" Summary: "The current implementation is REALLY counterintuitive."
- Same thread: "kanban board show me agent 'in progress' but … the agent session in diff mode … shows the agent is complete." The board and the session disagree.
- UX principle violated: visibility of system status (card state must match session state); discoverability; default to the primary task.
- Hutan should: default to the review surface (diff) when an agent finishes. Keep PR/rebase/merge actions on the same surface as the diff, and make sure board state and session state come from one source.

## 6. Users get trapped in a view; review disappears after completion
- Problem: after a task completes, the review step is missing and there is no route back to the board.
- Links: #2409 "Code review not available after completing task" (0👍, 0c, open). Body: "there's no way to get back to Kanban page after task is completed. (You're locked in workspaces, unless you change the URL manually)." Also: "shows 0 / 0 and doesn't allow to go to review."
- Related layout bug: #2549 "Attempts section hidden at bottom of page when task description is long" (1👍, 1c, open). The attempt cannot be clicked because it is below the fold.
- UX principle violated: user control and freedom (escape hatch); visibility of system status; layout hierarchy.
- Hutan should: always provide a one-click path back to the board. Put the attempts/sessions list above long descriptions, or make it a collapsible tab.

## 7. Heavy or inconsistent flows: task creation and programming-only options
- Problem: the create flow is a modal that navigates away after submit. The task menu is full of programming items for a non-programming use case.
- Links: #1815 "Task creation flow inconsistency (Dialog → Sidebar transition)" (0👍, 1c, closed) · #1816 "Too many programming-specific options in task dropdown menu" (0👍, 1c, closed). These are maintainer-closed; the reporter's use case was knowledge management.
- UX principle violated: consistency; progressive disclosure; audience-appropriate language.
- Hutan should: use one creation pattern (inline or modal, not both) and stay on the current surface after submit. Group rarely used actions under "more" and show only actions relevant to the current state.

## 8. Technical noise in the conversation transcript
- Problem: hook responses, npm warnings and model-init lines show up as chat messages.
- Link: #1817 "Technical system messages visible to users in conversation view" (0👍, 1c, closed). Quote: "These are implementation details that confuse non-technical users."
- Related: #1411 "Parts of message being dropped – not visible within UI" (1👍, 8c, open). Agent text such as "present the question to you" appears in the log but not in the UI.
- Related: #809 "Pre-execution error from npm exec is not clearly surfaced in the UI" (0👍, 1c, open).
- UX principle violated: aesthetic and minimalist design; visibility of system status; error messages that help users recover.
- Hutan should: keep the transcript to agent and user content. Put system events in a collapsible log. Never hide model output silently, and show pre-run failures as a visible state on the session.

## 9. Latency and memory make switching feel slow
- Problem: switching between agent chats and large diffs is slow, and the UI can hang on large sessions.
- Links: #1140 "Large conversations take up a lot of memory and slow down the interface" (1👍, 5c, open) · #3373 "Kanban/workspace loading can trigger runaway memory usage and eventual hangs" (0👍, 1c, closed) · #3218 "Opening a workspace/session can OOM the backend during historical normalized-log replay" (0👍, 2c, open).
- Quotes: #1140 user: "switching between agent chats should be near-instant ( less than 500ms )". Maintainer: "switching between workspaces with large diffs … can be solved by virtualising the diffs panel."
- UX principle violated: perceived performance; responsiveness.
- Hutan should: virtualize diffs and logs from the start. Load the last N messages first, then backfill. Target switching under 500 ms.

## 10. Background refresh resets scroll, focus and input state
- Problem: the UI reloads or re-renders on its own, which resets review position and typing.
- Links: #1361 "web version … refreshes roughly every 30 seconds" (1👍, 16c, open). Comment (novacoole): "Does make it quite difficult to review diffs." Maintainer asked whether the file watcher was re-rendering the diffs tab; the reporter said it refreshes "even if the page is static." · #2108 "Beta – Mobile view is practically un-navigable" (3👍, 2c, closed). Body: "the UI flashes every time the pr scan completes … lose carat position … the review scroll is reset, and the review comment I was working on goes away."
- UX principle violated: user control; state preservation; avoid disruptive updates.
- Hutan should: update state in place (diff, not full re-render). Never reset scroll, caret or an in-progress comment on a background event.

## 11. Mobile and small screens: review is unusable
- Problem: the beta is hard to use on phones and small windows.
- Links: #2108 (above) · #1461 "Cannot see review panel on mobile" (3c, closed) · #1359 "Can it provide better support for mobile use?" (3👍, 3c, closed) · #1523 "Enable diff and preview panels on smaller screens" (0👍, 0c, closed). Maintainer later: "Mobile should be much improved following the launch of Remote Access" (#2300 comment, 2026-02-28).
- UX principle violated: responsive design; consistency of features across viewports.
- Hutan should: define the review flow for small viewports before shipping it (for example, a single-column review with comments), not after.

## 12. Hidden lifecycle: worktrees pile up or vanish; cleanup is buried in settings
- Problem: worktrees and their disk use are invisible. Cleanup is automatic and surprising, and the setting is hard to find.
- Link: #765 "[Feature Request] Configurable automatic cleanup of worktrees" (19👍 incl. 11❤, 14c, open, the highest-reacted UX thread in the search). Quotes: user with "26 GB" after two days; "I was surprised to learn that isn't the case." Maintainer: "The issue with adding a setting is that 99% of users won't find it, so if we can do this smarter …" Suggestion from a user: prompt at the moment a task moves to Done/Cancelled.
- UX principle violated: visibility of system status; recognition over recall; the user's control over resources.
- Hutan should: show each workspace's disk use and its cleanup policy on the card and in the Done/Cancelled flow, where the user acts. Don't leave it only in settings.

## 13. AI-generated commit and merge text is not reviewable
- Problem: commit messages come from task or conversation content. They can leak secrets and are too long.
- Links: #2301 "Unable to customize or allow AI to generate merge messages" (3👍, 3c, open). Quote: "This exposes a lot of private information, even the API key if mentioned in the conversation." · #1624 "Need to make commit message more concise" (3👍, 8c, open). Title: "Right now it's too much unnecessary info."
- UX principle violated: user control over generated output; error prevention; privacy.
- Hutan should: show an editable preview of commit and PR text before anything is committed. Source it from the diff, not the chat, and offer a short default.

## 14. Attention and approval: blocked agents are easy to miss, and navigation locks
- Problem: when an agent waits for plan approval or a question, the session switcher is disabled and there is no global attention cue.
- Links: #3247 "Session switching is unavailable in plan approval and AskUserQuestion modes" (0👍, 0c, open). Body: "the session switcher disappears or becomes unavailable, so it is not possible to switch to another session until the current approval/question flow is completed or stopped." · #1459 "[Feature Request] Notification Sound for Plan Approval Requests" (0👍, 1c, closed; maintainer linked PR #1463) · #879 "[Feature Request] Waiting Approval column" (1👍, 1c, open) · #2543 "Linux: notification sounds silently fail when paplay/aplay are not installed" (0👍, 0c, closed).
- UX principle violated: visibility of system status; user control; multi-task attention management.
- Hutan should: keep navigation available while any session is blocked. Show a global count of "needs you" sessions, plus a waiting-approval state on the board. Give notifications a visible fallback when audio is unavailable.

---

## Notes on the kanban/board model (for product direction)
- #1866 "Custom Kanban Status/Column" (8👍, 3c, closed) and #1106 "Master Kanban View – See All Tasks Across Active Projects" (7👍, 3c, closed) show two structural asks: custom columns and a cross-project board. Maintainer (#2288): "The Kanban view is repo agnostic, so you can have a single board for your agents working on unrelated tasks."
- Sorting: #3116 "Always move finished tickets to the top of the 'Done' column" (1👍, 0c, open). Quote: "it sticks it somewhere in the middle which is confusing." #2319 "Configurable Project Sorting" (0👍, 0c, open).
- Filtering: #3183 "Add toggle to hide blocked issues in board view" (0👍, open).

## The 3 most important lessons for Hutan
1. **Never make the working path disappear in a redesign.** Keep the board as the home surface, make new views opt-in, and always give a visible way back (#2687 33👍/39c, #2288 17👍/26c, #2646, #2409).
2. **Local-first must mean local-first.** No account is needed for local use, data flow is shown before any sync, and migrations preview and keep data visible (#2952 10👍, #2812 10👍, #2724 7👍, #3354, #3396 19👍).
3. **Review, attention and state preservation are first-class.** Diffs are visible by default, board and session state come from one source, navigation stays open while an agent is blocked, and background updates never reset scroll or input (#2300, #3247, #1361 16c, #2108, #765 19👍).
