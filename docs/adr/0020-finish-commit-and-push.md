# 0020. Finish: the user commits and pushes; agents never push

- Status: accepted (2026-10-21, with the user)
- Date: 2026-10-21
- Amends: `docs/orchestration.md` § Review and merge ("Hutan never fetches or pushes"; commit refused in the main checkout)
- Evidence: `docs/spikes/2026-10-21-product.md` §2 (the default path cannot finish: row 6′), `docs/spikes/2026-10-21-community-opinions.md` (Finish row), `docs/spikes/2026-10-21-opinions/delivery.md`

## Context
An agent started in the main checkout (the default placement, kept by the user on 2026-10-21) can be reviewed but not committed or merged in Hutan: `worktree.commit` is refused for the main checkout and *Merge into …* needs a worktree. The user wants the loop to end with "commit and push to master, no PR, locally for now". Community evidence supports humans pushing to trunk and strongly objects to **agents** committing or pushing to main without review ("the tool call itself needs to be gated").

## Decision
1. **One Finish action** in the Changes view, for both placements:
   - main checkout: commit all reviewed changes (message prefilled from the session or task title, editable), then push the current branch to its upstream;
   - worktree: commit, merge into the target branch in the main checkout (existing merge modes), then push the target branch; offer the existing worktree cleanup afterwards.
   Each step reports its result; a failed push leaves the commit in place and says why (no upstream, rejected non-fast-forward, auth): Hutan never force-pushes, never rebases on its own, never stores credentials (git uses the user's own credential helper and SSH agent from the login environment).
2. **Finish is user-only**: the daemon request is classified `UserOnly` (ADR 0016); an agent-originated connection gets `forbidden` whatever `[control]` says. Agents keep their own tools (a harness may still run `git push` in its shell if the user allows it in that harness; Hutan does not do it for them and shows such commands like any other approval).
3. **No pull requests** for now; `fetch` only as needed to report "behind upstream" before pushing (read-only).
4. Settings: `[finish] push = true` (default), so a user who wants commit-only keeps it local.
5. Protocol: additive minor with capability `finish` (`worktree.finish {checkout, message, push}` → per-step results); `hutan finish` CLI.

## Consequences
- The default path closes inside Hutan; "Hutan never fetches or pushes" becomes "Hutan pushes only when the user presses Finish".
- New failure modes surface in the UI (auth prompts from git's helper, rejected pushes); they must be explained, not hidden.
- Revisit for PR creation (D-P1 option b) when a user asks for it.

## Amendment 2026-10-26 — staging and partial commits (STAGING, protocol 0.53)
The user answered yes to partial commits (revising decision D2, "Changes is read-only; no stage, unstage or commit-selected"; evidence: `docs/spikes/2026-10-25-user-needs/README.md` need #8 — Zed #34813, #45295, Helix #5132).
- **Staging is the user's, like Finish.** `worktree.stage`, `worktree.unstage`, `worktree.discard` and `worktree.commit` / `worktree.finish` with `staged_only` are `UserOnly`: an agent-originated connection gets `forbidden` whatever `[control]` says. `worktree.hunks` (the ids) is a read. Agents still never push, and Finish stays user-only.
- Stage and unstage change git's index only (`git add -A`, `git reset`, `git apply --cached` of a generated hunk patch, `--reverse` to unstage); the working tree is never written. A hunk id is a digest, so a stale one answers `conflict` and nothing changes.
- **Discard is destructive and undoable**: the daemon holds the checkout, takes a checkpoint (ADR 0019, kind `restore`) before and after, refuses while an agent turn runs in the checkout or checkpoints are off, and returns the checkpoint; restoring it is Undo (files only; the index is not restored).
- *Commit all* and Finish's default are unchanged; *Commit staged* and Finish's *Commit staged only* commit exactly the index (no `git add`), and the main checkout may be committed this way as Finish does.
