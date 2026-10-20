# UX pain points: Crystal → Nimbalyst

## Method and caveats

- **Subject:** `nimbalyst/nimbalyst` is the successor product and the primary source. `stravu/crystal` is the predecessor, described as "(Crystal is now Nimbalyst)". The maintainer confirmed on 2026-02-13 that Crystal "is dead but its functionality has been moved to nimbalyst.com" (https://github.com/stravu/crystal/issues/235). Crystal is used as historical evidence.
- **Data:** the full issue list was pulled with `gh issue list -R <repo> --state all --limit 2000 --json number,title,state,createdAt,comments,reactionGroups,labels,body`. This gave 1354 Nimbalyst issues (719 open, 635 closed) and 138 Crystal issues. Filtering was local with jq regex on titles and bodies, sorted by comment count, then bodies and comments were read for the selected issues.
- **Search note:** `gh search issues` worked for the first queries (`ux`, `ui`, `confusing`, `keyboard`). It then returned HTTP 403 (rate limit) for the remaining queries (focus, layout, notification, diff review, approval, onboarding, settings, scroll, queue, stop/interrupt). The local dump covers the same ground, so no coverage was lost. Reproduce with the `gh issue list` command above.
- **Engagement caveat:** reactions are almost zero. Nimbalyst's highest are #49 and #95 (12 thumbs-up each) and Crystal's are #177 (11) and #26 (8), but these are platform requests, not UX defects. Comment count is used as the engagement proxy throughout. Entries show `R=reactions / C=comments` at the time of the dump (2026-10-21).
- **Labels:** Nimbalyst has `area:*` labels (e.g. area:ui, area:ai, area:workspace, area:app-shell), but most UX issues are unlabeled. Crystal has only default GitHub labels.
- **Thin-source warning:** Crystal's UX-specific issues are mostly one-off reports with 0–3 comments. The Nimbalyst signal is stronger for approval, stop/queue, and scroll issues. Anything not directly observed is marked `[INFERENCE]`.

## Top pain points

Format: problem · issues (R/C, state, date) · quote · principle · Hutan lesson.

### 1. "Stop" does not stop, and queued prompts keep running
- Problem: Stop cancels the current turn, but queued prompts start immediately. The "⚡ Interrupt and send now" control behaves exactly like a hard abort, and the queued message is returned to the input box.
- Issues: #337 (R0/C17, closed, 2026-05-16, https://github.com/nimbalyst/nimbalyst/issues/337); #1744 (R0/C0, open, 2026-10-12, https://github.com/nimbalyst/nimbalyst/issues/1744); #1691 (R0/C1, open, 2026-10-10, https://github.com/nimbalyst/nimbalyst/issues/1691); #962 (R0/C7, closed, queued MCP prompts stranded, https://github.com/nimbalyst/nimbalyst/issues/962).
- Quote (#337): "behaves identically to clicking the red X / stop button." Quote (#1744): "there is no way to actually halt a session that has a queue."
- Principle: user control and freedom; visibility of system status (Nielsen 1, 3).
- Lesson: Hutan should keep stop, cancel-and-send, and queue as three distinct verbs. Stop must also clear the queue unless the user explicitly keeps it, and the UI must show "stopping…" until the provider confirms.

### 2. Approval cards do not show what is being approved
- Problem: the "Allow this tool?" card shows fragments or bare labels. For PowerShell it shows only the word "PowerShell". For compound Bash it shows one piece at a time. For sub-agents it shows only "Agent".
- Issues: #1791 (R0/C0, open, 2026-10-17, https://github.com/nimbalyst/nimbalyst/issues/1791); #1654 (R0/C0, open, 2026-10-04, https://github.com/nimbalyst/nimbalyst/issues/1654); #1181 (R0/C4, open, 2026-08-06, https://github.com/nimbalyst/nimbalyst/issues/1181).
- Quote (#1791): "I'm approving something I can't judge from the prompt." Quote (#1654): one click on Session or Always "approves every PowerShell command".
- Principle: informed consent and error prevention; recognition over recall.
- Lesson: an approval card must show the full command, the working directory, and the exact scope of "Always". If the scope is broader than what the card displays, the UI must say so.

### 3. Approval state lies: prompts appear for calls that were never gated, or stay pending
- Problem: Allow hooks and auto-mode approvals still show a pending "Allow this tool?" card, and the attention count includes them.
- Issues: #408 (R0/C3, open, 2026-05-22, https://github.com/nimbalyst/nimbalyst/issues/408); #426 (R0/C7, closed, 2026-05-24, https://github.com/nimbalyst/nimbalyst/issues/426); #1656 (R0/C0, open, 2026-10-04, https://github.com/nimbalyst/nimbalyst/issues/1656); #1561 (R0/C1, open, 2026-09-21, https://github.com/nimbalyst/nimbalyst/issues/1561).
- Quote (#408): the chat "renders an 'Allow this tool?' card as if the gate had fired." Quote (#1656): an approved call "stays pending forever" and is counted in "N prompts need review".
- Principle: consistency and honest system status; trust.
- Lesson: derive every attention item from the real gate decision. Hutan should never show a card or count for a call that the policy engine already resolved.

### 4. Question and form widgets discard user input
- Problem: typed answers are wiped when a turn completes. Submitted forms vanish after a restart. Interrupted cards stay pending forever. Hook-denied questions show as "Submitted".
- Issues: #1218 (R0/C9, open, 2026-08-11, https://github.com/nimbalyst/nimbalyst/issues/1218); #1647 (R0/C0, closed, 2026-10-02, https://github.com/nimbalyst/nimbalyst/issues/1647); #1617 (R0/C0, open, 2026-09-29, https://github.com/nimbalyst/nimbalyst/issues/1617); #1577 (R0/C1, open, 2026-09-23, https://github.com/nimbalyst/nimbalyst/issues/1577); #1549 (R0/C0, open, 2026-09-18, https://github.com/nimbalyst/nimbalyst/issues/1549).
- Quote (#1218): "Text typed into a PromptForUserInput field is silently cleared when the agent's turn completes." Quote (#1647): "You spend 30 minutes filling out a prompt form, hit Submit, and your answers vanish."
- Principle: error prevention; user control; data loss is the worst failure in a form.
- Lesson: persist drafts of every agent-issued form the moment they are typed, and show the saved state. A form that fails must keep its input and say why.

### 5. Ambiguous and stale status indicators ("Thinking…", "?" badge)
- Problem: a yellow "?" badge appears on sessions that are not waiting for input. "Thinking…" stays on finished sessions for hours. Mobile shows "Waiting" for queued messages.
- Issues: #634 (R0/C1, closed, 2026-06-14, https://github.com/nimbalyst/nimbalyst/issues/634); #1000 (R0/C1, closed) and #1010 (R0/C1, open), both titled "unclear icon meaning, unreliable state" (https://github.com/nimbalyst/nimbalyst/issues/1010); #1648 (R0/C0, open, 2026-10-03, https://github.com/nimbalyst/nimbalyst/issues/1648); #1622 (R0/C0, open, 2026-09-29, https://github.com/nimbalyst/nimbalyst/issues/1622); #1693 (R0/C0, open, 2026-10-10, https://github.com/nimbalyst/nimbalyst/issues/1693).
- Quote (#634): the "?" badge "is the same icon users read as 'waiting for user input', so finished/idle sessions look blocked on the user."
- Principle: visibility of system status; consistent iconography.
- Lesson: define a small state set (running, waiting-for-you, queued, done, error). Each state gets one icon and one meaning, derived from durable state, and it must clear when the state ends.

### 6. Transcript jumps, pinned banners, and duplicated prompts
- Problem: an idle transcript jumps every 1–2 seconds. Scrolling to the bottom snaps back. The last user prompt is repainted at the bottom of later turns. Error and usage banners stay pinned after recovery.
- Issues: #1762 (R0/C0, open, 2026-10-14, https://github.com/nimbalyst/nimbalyst/issues/1762); #1783 (R0/C0, open, 2026-10-16, https://github.com/nimbalyst/nimbalyst/issues/1783); #1608 (R0/C2, open, 2026-09-28, https://github.com/nimbalyst/nimbalyst/issues/1608); #1588 (R0/C0, open, 2026-09-25, https://github.com/nimbalyst/nimbalyst/issues/1588, "Urgent: makes the chat unusable"); #1673 (R0/C0, open, 2026-10-07, https://github.com/nimbalyst/nimbalyst/issues/1673); #1743 (R0/C0, open, 2026-10-12, https://github.com/nimbalyst/nimbalyst/issues/1743).
- Quote (#1762): the transcript "jumps between scroll positions every 1-2 seconds without any user input." Quote (#1588): "a second copy is pinned at the bottom of the chat, under the agent's latest reply."
- Principle: user control; consistency; the main reading surface must be stable.
- Lesson: pin scroll position to the user's reading anchor, not the bottom. Banners must be scoped to a specific turn and cleared when the state they describe ends.

### 7. Notifications and navigation land in the wrong context
- Problem: clicking a notification switches to the right project but the wrong session loads. The list and the main view show different sessions.
- Issues: #1794 (R0/C0, open, 2026-10-17, https://github.com/nimbalyst/nimbalyst/issues/1794); #402 (R0/C4, closed, 2026-05-22, https://github.com/nimbalyst/nimbalyst/issues/402); #155 (R1/C6, closed, 2026-05-04, https://github.com/nimbalyst/nimbalyst/issues/155, multi-project switching "without juggling windows"). Crystal: #77 (notification explosion on open, closed, https://github.com/stravu/crystal/issues/77), #58 and #56 (notification settings not persisted or no visual feedback, https://github.com/stravu/crystal/issues/58, https://github.com/stravu/crystal/issues/56), #109 (sound toggle broken, open, R2, https://github.com/stravu/crystal/issues/109).
- Quote (#1794): "the session list highlights the notification's session, while the main view opens a different one." Quote (#402): the session is "unreachable."
- Principle: consistency; match between system and user model.
- Lesson: a notification click must set project and session in one atomic navigation. Settings toggles must apply immediately and show their state.

### 8. Keyboard shortcuts that silently fail, and drafts lost to Escape
- Problem: ⌘⌥W never fires on macOS because Option rewrites the key. ⌘⌥→/← do nothing in Agent mode. Up arrow in the prompt box opens a "Save to user memory" view and deletes the typed draft. Shortcuts cannot be rebound.
- Issues: #1415 (R0/C1, closed, 2026-09-01, https://github.com/nimbalyst/nimbalyst/issues/1415); #1671 (R0/C0, open, 2026-10-07, https://github.com/nimbalyst/nimbalyst/issues/1671); #1584 (R0/C0, open, 2026-09-25, https://github.com/nimbalyst/nimbalyst/issues/1584); #1677 (R0/C0, open, https://github.com/nimbalyst/nimbalyst/issues/1677) and #1186 (R0/C0, open, https://github.com/nimbalyst/nimbalyst/issues/1186) for rebinding; #173 (R0/C2, open, 2026-05-06, https://github.com/nimbalyst/nimbalyst/issues/173) for submit/newline. Crystal: #183 (R2/C0, open, https://github.com/stravu/crystal/issues/183).
- Quote (#1415): the handler matches `e.key === "w"`, which "Option rewrites" on macOS. Quote (#183, a vim user): "it'll close the prompt dialogue and lose the prompt i wrote."
- Principle: error prevention; discoverability; freedom for vim and power users.
- Lesson: match shortcuts by physical key code on macOS, make every binding rebindable, and never let Escape or arrows destroy a draft. Hutan's vim mode must not inherit this.

### 9. Hover tooltips that block the controls under them
- Problem: a context-meter tooltip opens on hover and covers the queued-prompt buttons, so the user cannot act on them.
- Issues: #429 (R0/C12, closed, 2026-05-24, https://github.com/nimbalyst/nimbalyst/issues/429). The maintainer's comments show the design debate: "This is a button UI - so you can norm it to be click for open panel", and the reply that "we need both" tooltips and click behavior.
- Quote (#429): "Tooltip remains visible, blocking clicks."
- Principle: consistency with control semantics; Fitts's law (targets must stay reachable).
- Lesson: make anything that looks like a button open on click. Reserve hover for plain labels, and let overlays close when the pointer moves to a control under them.

### 10. Session list has no manual organization or clear archive feedback
- Problem: sessions sort only by recency. Users cannot group or pin them, and the tree styling suggests the wrong parent. Archive from the context menu does nothing.
- Issues: #767 (R0/C2, open, 2026-07-03, https://github.com/nimbalyst/nimbalyst/issues/767); #1201 (R0/C2, open, 2026-08-09, https://github.com/nimbalyst/nimbalyst/issues/1201); #1457 (R0/C1, open, 2026-09-04, https://github.com/nimbalyst/nimbalyst/issues/1457); #282 (R0/C7, open, 2026-05-14, https://github.com/nimbalyst/nimbalyst/issues/282); #1686 (R0/C0, open, https://github.com/nimbalyst/nimbalyst/issues/1686). Crystal: #73 (archive sessions, closed, https://github.com/stravu/crystal/issues/73), #99 (no way to clear a conversation, closed, https://github.com/stravu/crystal/issues/99).
- Quote (#282): "Right-clicking a session … 'Archive' does nothing — no feedback, no state change."
- Principle: feedback; recognition; flexibility.
- Lesson: every action must produce visible feedback. Let users pin, group, and reorder sessions, and make the hierarchy visually match the data model.

### 11. Multi-project work: window juggling and lost layout
- Problem: each project needs its own window. Multi-project state is lost on restart. Projects cannot be reordered. Project names are tied to folder names.
- Issues: #155 (see #7); #1572 (R0/C1, closed, 2026-09-23, https://github.com/nimbalyst/nimbalyst/issues/1572); #1684 (R0/C0, open, 2026-10-09, https://github.com/nimbalyst/nimbalyst/issues/1684); #1674 (R0/C1, open, https://github.com/nimbalyst/nimbalyst/issues/1674) and #1763 (closed, https://github.com/nimbalyst/nimbalyst/issues/1763) for custom display names; #349 (R0/C1, closed, https://github.com/nimbalyst/nimbalyst/issues/349) for tray groups not persisting.
- Quote (#1572): "the state is not stored when restarting the application and we have to add each project again."
- Principle: consistency; user control; persistence of user-built arrangements.
- Lesson: persist workspace layout across restarts, make the project list reorderable, and let display names differ from disk paths.

### 12. Diff and Markdown review can silently rewrite the document
- Problem: accepting an AI diff leaves old and new rows, or rewrites unrelated text. Opening a long Markdown file freezes the window for ~25 seconds. External file edits are not detected.
- Issues: #1829 (R0/C0, open, 2026-10-20, https://github.com/nimbalyst/nimbalyst/issues/1829); #1734 (R0/C0, closed, 2026-10-12, https://github.com/nimbalyst/nimbalyst/issues/1734); #1606 (R0/C0, closed, 2026-09-28, https://github.com/nimbalyst/nimbalyst/issues/1606); #1543 (R0/C0, open, 2026-09-17, https://github.com/nimbalyst/nimbalyst/issues/1543); #1820 (R0/C0, open, 2026-10-19, https://github.com/nimbalyst/nimbalyst/issues/1820, cursor and scroll lost on Source/Rich switch). Crystal: #96 (collapse unchanged lines, closed, R0/C2, https://github.com/stravu/crystal/issues/96); #97 (diff view doesn't auto-update, open, https://github.com/stravu/crystal/issues/97).
- Quote (#1734): "Keep All" rewrites "the entire file from the editor's own model," which breaks math and YAML. Quote (#1829): "accepting … leaves the file … with BOTH the old and the new row."
- Principle: user control; trust in review; consistency.
- Lesson: "Keep" must apply only the reviewed hunk and never regenerate the file. Diffs must refresh when the file changes on disk. Mode switches must keep cursor and scroll.

### 13. Final answers hidden inside collapsed reasoning, and transcript noise
- Problem: the agent's final answer is folded into a collapsed "thinking" block. System notes trigger extra turns that say nothing new.
- Issues: #1801 (R0/C4, open, 2026-10-17, https://github.com/nimbalyst/nimbalyst/issues/1801). The maintainer replied that the text comes from the API's progress updates, not from a Nimbalyst drop, and users have written workarounds in CLAUDE.md. #1555 (R0/C0, open, 2026-09-20, https://github.com/nimbalyst/nimbalyst/issues/1555); #823 (R0/C4, open, 2026-07-11, https://github.com/nimbalyst/nimbalyst/issues/823).
- Quote (#1801): "the agent's entire final response is hidden and only a summary is … inside a collapsed thinking block."
- Principle: visibility; minimal surprise. [INFERENCE] The root cause is upstream, but the UI decision to hide the text is local.
- Lesson: the final answer must always render in the main flow, never inside a collapsed block. Only reasoning and tool detail should collapse.

### 14. Mobile and sync confusion
- Problem: the iOS app shows "Couldn't load sessions" for every project. Sync keys disagree after re-pairing. Mobile status does not match desktop.
- Issues: #1733 (R2/C7, open, 2026-10-12, https://github.com/nimbalyst/nimbalyst/issues/1733); #1782 (R0/C0, open, 2026-10-15, https://github.com/nimbalyst/nimbalyst/issues/1782); #337 (iOS queue "not delivered" comment, see #1). Platform-demand signal: Android #95 (R12/C5, open, https://github.com/nimbalyst/nimbalyst/issues/95); remote SSH #49 (R12/C9, open, https://github.com/nimbalyst/nimbalyst/issues/49); headless server #416 (R6/C9, open, https://github.com/nimbalyst/nimbalyst/issues/416). Crystal: #89 server mode (R6/C3, open, https://github.com/stravu/crystal/issues/89).
- Quote (#1733): iOS shows "Couldn't load sessions" and "pull-to-refresh does nothing."
- Principle: visibility of status; error recovery.
- Lesson: sync errors must name the cause and offer one recovery action. Remote and mobile access are the most-requested directions, so the remote session model needs a clear design.

### 15. Settings and onboarding are hard to discover
- Problem: Settings has about 30 pages with no search. A key setting is hidden in a page users don't expect. Crystal users could not find "Show Thinking", and some did not discover drag-and-drop for images.
- Issues: #1574 (R0/C0, open, 2026-09-23, https://github.com/nimbalyst/nimbalyst/issues/1574). Crystal: #237 (R0/C3, open, 2026-02-22, https://github.com/stravu/crystal/issues/237, "I can't find it in any place of settings"); #24 (R0/C2, open, https://github.com/stravu/crystal/issues/24, "I didn't realise I could drag and drop images").
- Quote (#1574): "You have to guess which page a setting is on."
- Principle: discoverability; recognition over recall.
- Lesson: ship a settings search from day one and index every toggle. Show a first-run tour that names the drag-and-drop and shortcut affordances.

## The 3 most important lessons

1. **Make control verbs honest and atomic.** Stop must stop, including the queue. "Send now" is a separate action. The UI state must follow the real process state and must not show "Thinking…" or "waiting" after the process has ended (#337, #1744, #1691, #1648, #634).
2. **Approval and input cards must show exactly what is being decided, and must never discard input.** Show full commands and the real scope of "Always", derive attention counts from the gate decision, and persist drafts of every form (#1791, #1654, #1181, #408, #1656, #1218, #1647).
3. **Keep the user's place and never rewrite their work silently.** Anchor scroll to the reading position, scope banners to their turn, route notifications to the right project and session in one step, and apply accepted diffs as minimal hunks (#1762, #1783, #1608, #1794, #1829, #1734).
