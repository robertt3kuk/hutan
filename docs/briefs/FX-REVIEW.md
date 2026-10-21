# FX-REVIEW — "Needs you · review" that never clears, and Finish taking the wrong message

Agent: implementer-opus (Claude Opus 5.5)   Depends on: CK-3 merged   Branch: `slice/FX-REVIEW`

**Contract ownership:** delegated for `crates/core/src/task.rs` delivery transitions only if needed; **no protocol change** (if one seems needed, stop and report).

## Context
The end-to-end pass `docs/spikes/2026-10-23-e2e-usertest.md` (top issues 2 and 8, B1, friction "Mawas Home Today") found:
- After the first turn of a chat ends, the session shows `review · awaiting review`, the sidebar grows a *Needs you* group and the status bar counts it. It **stays** through later turns (while the agent works, during *Stopping…*), while the user is looking at the chat, after the user replies, and after **Finish** committed and pushed everything. A first chat that changed nothing ("Look around the project…") also awaits review.
- Finish pre-fills the commit message with the title of that first chat, which changed nothing, instead of the chat that made the edits.
- Mawas Home *Today* shows nothing under *Needs you* while two agents need the user; the Agents rail lists Mawas itself as "State unavailable".

Where it lives: `DeliveryState` in `crates/core/src/task.rs` (`none → awaiting_review → accepted | changes_requested → merged | discarded`), the daemon's move to `AwaitingReview` in `crates/daemon/src/tasks.rs` (~1168, incl. "the main checkout, but an agent did the work: it awaits review"), the desktop's `awaits_review` (`sessions/tasks.rs:84`) feeding `sessions/presentation.rs` (reducer precedence) and `sessions/store.rs:1032` (the `review` pill), the Finish card in the Changes view (`worktree.finish`, ADR 0020), Mawas Today (`wb/panels/assistant_rail.rs`), the Agents rail (`wb/panels/agents.rs`).

## Rules to implement (product decision, from the spike's quiet-attention principle)
1. **Working beats review.** While a turn of the session runs (or is stopping), the session is *Working*; a review reason never shows over it.
2. **Only a turn that changed files awaits review.** A turn whose checkpoint (CK-2) or `FileDiff` events show no change leaves the delivery as it was (`none` for a fresh task). No checkpoint (non-git, off) → fall back to `FileDiff` events of the turn.
3. **A new turn re-opens the work:** when the user sends again, the delivery leaves `awaiting_review` (back to `none` with a history record by the user) and is set again only by the next changing turn.
4. **Looking is not accepting, but it is seeing.** Once the user has seen the finished turn (the chat or its Changes view focused after the turn ended — use the existing unread/seen mechanism for completions), the session leaves *Needs you* and shows a quiet `Ready for review` state word without counting in the status bar; the Tasks panel keeps the delivery pill. Unseen → counts as today.
5. **Finish closes the loop:** a successful Finish (commit + push) in a checkout moves every open delivery (`awaiting_review`, `accepted`, `changes_requested`) of tasks whose sessions ran in that checkout to `merged` with the commit id, actor = the user's client; the session leaves *Needs you*.
6. **Finish message:** pre-fill from the most recent turn that changed files in that checkout (its task/chat title, or the first line of its prompt), never from a chat that changed nothing; with several, the newest one and a one-line hint listing the others.
7. **Mawas Today** lists the same *Needs you* items as the Agents page (shared reducer, not a second rule set); Mawas itself is not an agent row in the Agents rail.

## Acceptance
Daemon tests: a read-only turn leaves delivery `none`; a changing turn → `awaiting_review`; a new user turn → `none` (history record), the next changing turn → `awaiting_review` again; Finish moves open deliveries in the checkout to `merged` with the commit. Desktop GPUI tests (state-waiting): a running second turn shows *Working* not review; after the user focuses the finished chat the status bar count drops and the row says `Ready for review`; after Finish the *Needs you* group is gone; the Finish message comes from the chat that edited files; Today's *Needs you* matches the Agents page; no Mawas row in the Agents rail. Update existing tests that pin the old behaviour, keeping what they prove. Cua screenshots: chat header during turn 2, sidebar after Finish. `just check` green; commit per step, messages end `(FX-REVIEW)`. Fresh `HUTAN_HOME`; no `hutand` left.

## Parity
CLI `hutan task ls` shows the same delivery; the MCP task table text unchanged except values.

## Out of scope
Partial commits, PRs; `docs/status.md`, `docs/roadmap.md`.
