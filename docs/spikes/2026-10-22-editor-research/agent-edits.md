# Agent-edit review in editors: evidence for Hutan E-10

Scope: how Zed, Cursor, Claude Code (VS Code and JetBrains), Copilot agent mode (VS Code), and Windsurf/Devin Desktop let users review agent edits in the editor. Also the top complaints: lost edits, conflicts with unsaved buffers, jumpy viewport, no per-hunk reject, stale buffers. Latest item seen is dated 2026-10-20.

Method (reproducible):
- GitHub counts: `gh api repos/<owner>/<repo>/issues/<n>` (reactions, comments, labels, state). `gh search issues` returned 403 secondary rate limits, so issue discovery used `gh api repos/<repo>/issues?state=all&sort=reactions` filtered by title/body, plus web_search.
- Forum counts: Discourse JSON header (likes, views, posts) on forum.cursor.com.
- HN: Algolia `hn.algolia.com/api/v1/search` with `tags=story` and `tags=comment`, `created_at_i > 2025-01-01`. Queries: "cursor accept reject diff agent", "agent edits review diff editor", "claude code diff review", "windsurf cascade reverted", "coding agent overwrote unsaved changes", "copilot edits keep undo", "cursor diff accept reject hunk", "claude code edits unsaved buffer", "agent overwrote my unsaved changes", "reject single hunk agent". Almost nothing relevant.
- Reddit: not accessed.
- Quotes below were checked against fetched text. Anything I could not check is labeled.

## 1. Vendor review models (primary docs)

**Zed**
- Point: native agent edits open a "Review Changes" multibuffer with per-hunk and whole-set accept/reject. `agent.single_file_review: true` gives inline per-file review. Follow is a crosshair icon. Each message has a "Restore Checkpoint" button.
- Link: https://zed.dev/docs/ai/agent-panel
- Hutan should: treat Zed's native review as the bar to meet for per-hunk review.

**Cursor**
- Point: edits are applied as the agent works. Review them in the diff view and reject what you don't want. Checkpoints are kept separate from Git and restore files only.
- Links: https://cursor.com/help/ai-features/agent and https://cursor.com/docs/agent/overview
- Quote: "Edits are applied as it works. Review them in the diff view and reject anything you don't want."
- Hutan should: direct write plus reject is Cursor's model. State in the UI that checkpoints don't undo terminal commands or external effects.

**VS Code (Copilot, Claude, and Codex harnesses vs. Local harness)**
- Point: Copilot/Claude/Codex harness edits save straight to disk with no pending state. Review via Chat, Agents window, or Source Control; revert via checkpoints or Git. Local-harness edits are saved and marked pending, with Keep or Undo per edit, per inline hunk, or for all. Pending state persists across restart. Staging in Source Control keeps; discarding reverts. `chat.editing.autoAcceptDelay` auto-keeps.
- Link: https://code.visualstudio.com/docs/agents/run/review-code-edits (the Copilot-specific URL resolved to this page in my fetch). The page also says the change-review experience for Copilot harness sessions "is under active development."
- Quote: "Pending means that you can keep or undo the saved edit. It doesn't mean that the edit is waiting to be written to disk."
- Hutan should: VS Code's "pending" means "already on disk, can be undone." Use that meaning for E-10 state, not a write gate.

**Claude Code (VS Code)**
- Point: in Manual mode, a side-by-side diff with accept/reject. Per-change "Accept this change" and "Reject this change" buttons need Claude Code v2.1.275+. A diff with more than 100 changes opens without per-change buttons. Auto and Edit-automatically modes write without asking.
- Link: https://code.claude.com/docs/en/vs-code (fetched as artifact)
- Quote: "If you edit the proposed content directly in the diff view before accepting, Claude is told that you modified it so it doesn't assume the file matches its original proposal."
- [INFERENCE] The docs say per-change buttons exist, but issue #33932 (open, updated 2026-10-03) still asks for them. The feature may be recent or surface-specific. Status unverified.
- Hutan should: when the user edits a hunk before accepting, tell the agent what changed, so its next view is correct.

**Claude Code (JetBrains)**
- Point: diffs open in the IDE diff viewer. `/config` "Diff tool" switches `auto` (IDE) or `terminal`. The docs describe whole-diff viewing. They don't mention per-hunk reject.
- Link: https://code.claude.com/docs/en/jetbrains
- Hutan should: a diff hand-off is not a review workflow. Don't count it as parity.

**Windsurf / Devin Desktop (Next builds)**
- Point: v3.11.1108 made agent diff zones off by default, so agent edits apply directly to files. "Auto-open edited files" is off by default. Agent edits skip format-on-save unless "Run save actions on agent edits" is on. A "Since last seen" scope was added to the Changes tab. v3.9.1018 removed Cascade from Next builds.
- Link: https://docs.devin.ai/desktop/changelog-next
- Quote: "Agent diff zones are off by default, so agent edits are applied directly to files."
- [INFERENCE] Windsurf chose direct writes plus a "since last seen" view over inline gates. Stable still ships the older flow, so this is moving.
- Hutan should: have a "since last seen" view and a per-session ledger. Decide explicitly whether agent saves run format-on-save.

**Copilot (agent mode, VS Code)**: covered by the VS Code section above. No separate Copilot-only page was read.

## 2. Complaints with counts (primary sources)

### 2a. Lost or unreviewable edits; per-hunk reject missing

- **Claude Code #33932**: "[FEATURE] VS Code Extension: Diff review UI similar to GitHub Copilot Edits Review". Open. Created 2026-03-13. Updated 2026-10-03. 290 reactions (+1 209, heart 51, eyes 24). 41 comments. Labels: enhancement, area:ide, platform:vscode. Asks for a pending list, inline diffs, per-change and per-file accept/reject, and Accept All/Reject All.
  - Quote (ImmortalWing, 2026-04-25): "the changes are written directly to disk without going through VS Code's `TextEditorEdit` API … No gutter decorations".
  - Quote (egelev, 2026-06-09): "the lack of an inline 'Edits Review' UI is the single biggest point of friction in adopting Claude Code right now."
  - Workarounds named in thread: `molon/hunkwise`, `EngageLabsIt/vscode-ai-plan-reviewer`.
  - Hutan should: ship gutter decorations and per-hunk reject in the first slice. This is the top ask.
- **Claude Code #42448**: hunk-level diff review request. Closed as a duplicate of #33932 on 2026-04-05. 0 reactions, 3 comments.
  - Quote: "the user can only accept or reject the entire change. There is no way to: Review individual hunks and accept/reject them separately."
  - Hutan should: test per-hunk reject, not just accept-all/reject-all.
- **Zed discussion #56048**: "Accepting/rejecting edits made by external agents". Created 2026-05-07 by wimpal. The vote widget shows 21 votes. 4 comments, 5 participants.
  - Quote (Flamintus, 2026-06-16): "Per-hunk accept/reject of agent edits is honestly one of my main reasons for staying on Cursor."
  - Maintainer benbrandt (2026-06-19): the ACP filesystem APIs "had several issues … for ACP v2 we've actually removed these apis and will explore different avenues … I can't give a timeline on it at the moment." Reply reactions: 👍 2, 😕 5, 👀 6.
  - Quote (rahbari, 2026-09-07): "it doesn't even list the changes done by external agents like Claude Code."
  - Hutan should: the editor, not a protocol surface, should own the diff state for each hunk.
- **Zed #59157**: "Add PR-style review comments in Git Diff UI that feed into the active agent thread". Open. Created 2026-06-11. Updated 2026-10-07. 150 reactions (+1 118, heart 13, eyes 12). 11 comments.
  - Hutan should: a comment on a hunk that feeds the agent is in demand. It is cheap if E-10 has per-hunk state.
- **Zed #34108**: "AI: 'Reject All' option will revert more than necessary". Closed 2025-08-05 (completed). 4 comments.
  - Quote (OP): "Rejecting all AI editor changes reverts excessively, sometimes deleting entire files created in prior steps."
  - Hutan should: reject must be scoped to the hunks the agent wrote in this session, not "restore file from HEAD."
- **Cursor forum 152099**: "Only some diffs are shown when agent makes changes". Created 2026-02-17 by Jovan. 236 posts, 387 likes, 8726 views. Version 2.5.14 regression. Staff (Sanjeed, 2026-02-17) acknowledged; staff (Dean Rie, 2026-05-17) calls it "the canonical thread."
  - Quote (Jovan, OP): "the diff … is shown only about the 1st change … you see the changes i the chat, and when you inspect the files you see many more changes!"
  - Quote (Jovan, post 6): "the changes are applied, but they are not highlighted."
  - Hutan should: the hunks decorated and listed must match the hunks the agent wrote. Test multi-function edits in one turn.
- **Cursor forum 160856**: "Bring back per-change Apply + inline diff review". Created 2026-05-17 by a long-time user. 282 views, 3 likes. Staff (Dean Rie, 2026-05-17): "per-change Accept or Reject didn't go away, but in the new Agent Window the default flow shifted toward session-level Review." Staff suggests Auto-Run off so the agent asks before applying. Staff also says "I can't give an ETA" for restoring the old flow as default.
  - Quote (user, 2026-05-22, topic reply): "There are so many times when the agent does 90% of the code correct but 1 or 2 changes are wrong and need to be rejected."
  - Hutan should: a session-level summary is not per-hunk review. Keep per-hunk review as the default.
- **VS Code #262495**: "Remove the 'Keep' button in Chat Agent mode and use standard save behavior". Open. Created 2025-08-20. 31 reactions (+1 29, heart 2). 11 comments. Labels: feature-request, ux.
  - Quote (connor4312, VS Code, 2026-04-03, #304539): "Long term I think keep/undo are going away."
  - [INFERENCE] VS Code is moving toward save-to-disk with checkpoints. Don't copy the Keep button without a reason.

### 2b. Conflicts with unsaved buffers; stale buffers

- **Claude Code #70662**: "VSCode extension: apply edits to buffer via workspace.applyEdit() instead of writing to disk". Created 2026-06-24 by misha-sunborn. Closed as not_planned 2026-08-09 by the stale bot. Locked. 4 comments, 0 reactions. Bot marked it a duplicate of #62900, #66326, #53220.
  - Quote (OP): "When the buffer is dirty (has unsaved changes), VSCode won't auto-reload from disk, so Claude's edits are silently invisible until the user manually resolves the conflict."
  - Hutan should: this is the core E-10 failure mode. An agent write to a dirty buffer must become a conflict-bar state, not silent drift on disk.
- **Claude Code #53220**: "[FEATURE] VS Code Extension should warn when reading/writing unsaved files or use unsaved version". Created 2026-04-25. Closed as not_planned. 2 comments, 0 reactions.
  - Quote (OP): "if I forget to save my changes he doesn't see the latest content and I either lose changes (either mine or his) or I get conflicts to resolve."
- **Cursor forum 160123**: "Agent edits should sync with the open editor buffer". Created 2026-05-08 by a Booking.com senior PM. 63 views, 0 likes. Staff (Mohit, 2026-05-13): "The buffer/disk race you've identified is a known pain point and something our team is actively working on improving … No timeline to share yet."
  - Quote (OP): "The Agent's edit tools write only to disk … User's next save (manual or auto) overwrites the agent's edit with the stale buffer."
  - Proposals in order: route agent edits through the open buffer (as Cmd+K does); auto-save dirty buffers before agent writes; warn on dirty files; default `files.autoSave = onFocusChange`.
  - Hutan should: pick the buffer-route option. It is the only one that removes the race. Auto-save before write is the fallback.
- **Zed #49162**: "ACP readTextFile returns stale buffer content after terminal commands modify files on disk". Open. Created 2026-02-14. Updated 2026-08-21. 6 reactions (+1 6), 6 comments. Labels: state:needs repro, area:ai/acp, never stale, reach:many users, severity:S2.
  - Quote: "There is no buffer invalidation or refresh mechanism between terminal execution and subsequent file reads."
  - Hutan should: after any agent write, shell command, or read, reconcile disk with the rope buffer. Terminal writes count.
- **Zed #44060**: "Switching LLM Provider clears pending Agent edits/Multibuffer review view". Closed. Created 2025-12-03. 1 comment, 0 reactions. Labels: area:ai, reach:some users, severity:S2.
  - Point: changing the model while review is pending clears the multibuffer and loses the pending edits.
  - Hutan should: pending hunks must survive model switches, chat switches, and restart. VS Code's Local harness already does this.
- **Cursor forum 151264**: "Undo deletes file created by Agent after multiple edits". Created 2026-02-09. 4 likes, 322 views. Staff (Dean Rie, 2026-02-09): "Undo on files created by the agent deletes the entire file instead of undoing the last change." Colin (2026-02-20): "This will be fixed in the next update of 2.5."
  - Hutan should: Ctrl-Z on an agent-written file must undo one hunk, never delete the file.
- **Cursor forum 152336**: "Agent edits are not showing in file preview". Created 2026-02-20. 183 views. Staff (Dean Rie): "The agent only shows a diff for the first change in a file, and the rest get applied without highlighting." Same root cause as 152099.
- **Cursor forum 81507**: "File changes made by the agent are not reflected". Created 2025-04-20. Open tab showed no change until "Reopen Editor." Auto-closed after 30 days.
  - Hutan should: test that the open buffer refreshes on agent writes. Reopen must never be the fix.
- **VS Code #304539**: "Copilot Chat Keep/Discard creates unsafe state for vibe-coding workflows". Open. Created 2026-03-24. 8 comments, 0 reactions. Labels: info-needed.
  - Quote (sonnyandersson, 2026-03-26): "The Keep function has caused several lost edits if you hit it retroactively."
  - Staff (connor4312, 2026-03-25): clicking Keep "just saves the file + triggers save participants. It does not rewrite the file."
  - Hutan should: a pending hunk left unresolved in session A must not revert session B's later edits when undone. Anchor each hunk, and undo only that hunk.
- **VS Code copilot-release #4911**: "Agent made file changes before approval". Closed as completed on 2025-02-17. 5 comments. Labels: feature-request, verified, verification-needed, under-discussion, on-release-notes, cross-file-editing.
  - Quote (OP): "the agent had already modified these files before I explicitly approved any changes."
  - Hutan should: decide and show the write policy. Writes before approval are a recurring complaint.

### 2c. Jumpy viewport and performance

- **Zed #65894**: request for an option to disable the live diff/edit view (apply edits without rendering) to avoid main-thread freezes on large or minified files. Open. Created 2026-10-20. 0 reactions, 0 comments. Label: state:needs triage.
  - Quote (OP): "the live diff rendering wedges the main thread in a parse/highlight/layout loop, pinning a core at 100%+ and freezing the whole editor for minutes at a time."
  - Hutan should: throttle live diff, handle large or minified files, and offer an off switch.
- **Cursor 152099 (second part)**: the edited-files tally shows "+1 or +2 line edits" when far more lines changed (MichaelX, 2026-02-18). This is a count mismatch, not a viewport bug.
- Viewport jumping specifically: no primary-source issue with reactions turned up. Treat it as unverified.

## 3. Summary by tool

| Tool | Review model | Per-hunk reject | Live follow | Main open problem (primary source) |
|---|---|---|---|---|
| Zed (native agent) | Review Changes multibuffer; single-file review | Yes | Follow icon | Model switch clears pending review (#44060); Reject All over-reverts (#34108). External ACP agents: no per-hunk review (#56048; maintainer says ACP fs APIs removed) |
| Cursor | Applied as it works; session "Review"; checkpoints | Unreliable (152099, 387 likes) | Diffs unreliable | Only first diff shows; buffer/disk race (160123); undo deletes file (151264) |
| VS Code (Local harness) | Pending Keep/Undo per hunk; persists across restart | Yes | Not verified | Keep gate is being retired (#304539, #262495) |
| VS Code (Copilot/Claude/Codex harness) | Direct to disk; review via Chat/Agents/Source Control | Via checkpoints/Git | Not verified | No pending state by design |
| Claude Code (VS Code) | Manual-mode diff accept/reject; per-change buttons for v2.1.275+ | Documented; conflicts with #33932 | Not verified | No gutter, no modified dot (#33932, 290 reactions); buffer conflicts (#70662, #53220) |
| Claude Code (JetBrains) | IDE diff viewer, whole diff | Not documented | No | Same as VS Code |
| Windsurf / Devin Desktop (Next) | Direct writes; diff zones off by default; "Since last seen" | Not documented in changelog | Auto-open off by default | Cascade removed in Next; stable still on the older flow (moving target) |

## 4. Gaps and limits

- HN: thin. No story or comment about in-editor agent-edit review. Tangential Show HNs: Revdiff (17 pts, 4 comments, 2026-04-12, https://news.ycombinator.com/item?id=47742437), Perspica semantic diff (15 pts, 7 comments, 2026-09-30, https://news.ycombinator.com/item?id=49914005), Nimbalyst (8 pts, 4 comments, 2026-04-30, https://news.ycombinator.com/item?id=47962230). They show demand for external review tools. [INFERENCE] they are not evidence for in-editor UX.
- Reddit: not accessed.
- GitHub search: rate-limited. Coverage comes from repo issue lists sorted by reactions plus web_search. More Zed and Claude Code review issues probably exist.
- Cursor: many duplicate threads. Canonical: 152099 (diffs), 160123 (buffer sync), 160856 (per-change review). Seen but not read: 134528 "Agent edits sometimes not saving file to disk."
- Windsurf: no issues with reactions found. The Cascade-era file-level accept/revert model is from a third-party summary, not verified.

## 5. Five recommendations for E-10

1. **Route agent writes through the buffer when the file is open and dirty.** Don't write silently to disk under a dirty buffer. Show the conflict bar, or apply the change to the rope buffer as a pending hunk. Evidence: Claude Code #70662 (dirty buffer hides agent edits), Cursor 160123 (save overwrites agent edit), VS Code #304539. Recheck disk mtime/hash on every save.
2. **Ship per-hunk reject scoped to this session's hunks**, not "restore file." Test multi-function edits in one turn. Evidence: Cursor 152099 (387 likes, first hunk only), Zed #34108 (reject-all deletes files), Claude Code #42448 and #33932 (per-change accept/reject is the top ask).
3. **Keep pending hunks as state that survives model switch, chat switch, and restart**, and make Undo undo one hunk, never a file. Evidence: Zed #44060, VS Code Local-harness pending state, Cursor 151264 (undo deletes file).
4. **Make the ledger match disk.** Decorate every agent hunk from the actual write, reconcile after terminal commands, and throttle live diff for large or minified files with an off switch. Evidence: Zed #49162 (stale after terminal writes), Zed #65894 (freezes on large files), Cursor 152336 and 152099 (count mismatch).
5. **Default to direct writes with a pending ledger, not an approval gate; make approve-before-write opt-in.** Evidence: Windsurf Next (diff zones off by default), VS Code (Copilot/Claude harness writes directly; Keep is being retired per #304539 and #262495). Cursor users push back on writes before approval (160856). Say the write policy in the UI.
