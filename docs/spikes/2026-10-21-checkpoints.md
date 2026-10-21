# Per-turn checkpoints of agent edits (`UX-CHECKPOINT`, decision D3) — design, measurements and slice plan

Date: 2026-10-21 · Author: `CheckpointSpike` · Status: **proposal** (research only; nothing in the repo was changed besides this file and `docs/adr/0019-agent-checkpoints.md`)

**Evidence boundary.** Hutan facts are read from the main checkout (no build, no test, no app run). The cost numbers in §4 are **measured** with plain `git` 2.54.0 (Apple Git-157) on a throw-away repository under `$TMPDIR` (Apple M1 Pro, APFS, warm page cache, one run unless a count is given); nothing was run in the Hutan repository. Facts about Claude Code, Codex and ACP come from the linked documents and sources, read on 2026-10-21. OMP facts come from the strings of the installed `omp` 18.12.3 binary (never run with a model). `codex` and `claude` are not installed on this machine, so Codex 0.160–0.169 behaviour is **[UNVERIFIED]** where marked. `[INFERENCE]` marks reasoning that no source or experiment confirms.

---

## 0. Summary

**Design in one paragraph.** For every turn of a chat session whose working directory is inside a git checkout, the daemon snapshots the checkout twice — before the prompt reaches the agent and when the turn ends — into one hidden ref `refs/hutan/checkpoints/<session>/<turn>` (the *end* commit's parent is the *start* commit). A snapshot is made with git plumbing against a **private index** (`GIT_INDEX_FILE`, seeded once from the user's index): `ls-files -o --exclude-standard` + `diff-files` find what changed, the daemon `lstat`s the candidates (size cap, secret-name denylist, LFS/filter attributes, symlinks, gitlinks), `hash-object -w --no-filters` stores the bytes, `update-index --index-info`, `write-tree`, `commit-tree`, `update-ref`. The user's index, HEAD, stash, refs and working tree are never touched (verified, §4.3); ignored files are never read (verified). Restore is **per path, not per tree**: the files the agent's turn(s) changed are compared with the files *now*; a file that still equals the end-of-turn version is rewritten to its start-of-turn version, a file that differs (you or another agent edited it) is a **conflict** that stays untouched unless acknowledged. Before writing, the daemon snapshots the present state (a `restore` checkpoint), so *Undo restore* is the same operation run on that checkpoint. Restore is **files only**: no harness Hutan drives can rewind its conversation over ACP / the Codex app-server (§5.4), so the dialog says "<harness> still remembers this turn" and offers to send it a note.

**Numbers (measured, §4).** 10 021 tracked files (20 of them 1 MiB binaries) + 5 000 ignored files: a snapshot of an unchanged tree takes **≈ 95–120 ms** (`git status` alone: 35–38 ms); 3 edited + 1 new + 1 deleted file **125 ms, +52 KiB**; ≈ 250 changed files (1.1 MB of new content) **326 ms, +1.1 MiB**; one 5 MiB binary **404 ms, +5 MiB**; one 120 MiB file **3.5 s, +129 MiB** (hence a per-file cap); the very first snapshot of a repository with no index and 10 k untracked files **6.7 s**; every file stat-dirty (formatter run) **0.5–1.8 s once**, then ≈ 95 ms with a persistent private index. 20 000 loose refs make `git branch` take **0.71 s** (17 ms packed or absent) and cost 80 MB of blocks, so refs are capped (≈ 1 000 per repository) and packed. `git push --mirror` **does** push `refs/hutan/*` (measured) — a visible downside of keeping them in the user's repository.

**First slice: `CK-1`** — the snapshot engine as a daemon module with no protocol and no UI, tested on temporary repositories (invariants of §4.3, caps, skips, concurrency). Seven slices in §7; three decisions for you in §9 (default on/off, retention numbers, where the refs live).

---

## 1. What Hutan already has (and what it lacks)

| Need | Present | Gap |
|---|---|---|
| **Decision and scope** | `UX-CHECKPOINT` card #23 and theme T6 (`docs/spikes/2026-10-21-ux-plan.md:154-168,639-645`); D3 answered "ADR + spike first" (`docs/status.md:16`); invariant "hard-to-reverse decision → ADR first" (`AGENTS.md`, Invariants) | no code, no roadmap slice |
| **Turn model** | `AgentEventKind::{TurnStarted, TurnCompleted, TurnFailed, TurnCancelled}` + `ends_turn()` (`crates/core/src/agent.rs:284,420-429,490-494`); `FileDiff { changes: Vec<FileChange{path, kind, move_path, patch, before, after}> }` (`agent.rs:164-183,362`); the daemon's pump handles `TurnStarted` / turn end at `crates/daemon/src/chat.rs:2035-2043`; `ChatManager::send` sets `busy` and calls the adapter at `chat.rs:1258-1300` | no hook before the prompt leaves; `FileDiff` is only what the harness reports (not Bash edits) |
| **Checkouts and git plumbing** | `git_command` / `run_git` with `GIT_TERMINAL_PROMPT=0`, `GIT_OPTIONAL_LOCKS=0`, closed stdin, timeout, `kill_on_drop`, `GIT_*` location variables removed (`crates/daemon/src/worktrees.rs:566-630`); checkout derivation without spawning git (`placement.rs:112,154`); `shared_with` (`placement.rs:201`); Home is never a project (`projects.rs:79-90`, `docs/orchestration.md:73`) | `git_command` removes `GIT_INDEX_FILE` — the engine needs its own variant; no `core.hooksPath` override |
| **Review/merge of worktrees** | `worktree.diff` / `file_diff` bounded to 2 MiB per file, 20 MiB and 5 000 files, `--no-ext-diff --no-textconv` (`docs/orchestration.md:142`); "Hutan never fetches or pushes" (`orchestration.md:178`) | diff is vs the merge base, not vs a checkpoint |
| **Who is asking** | `ClientInfo.origin`, `[control]` policy, `Access::{Read, Mutate, UserOnly}` (`docs/adr/0016-agent-control-surface.md:13-14`, `crates/daemon/src/router.rs:615`) | a restore is a new `UserOnly` request |
| **File I/O through the daemon** | ADR 0012 amendment §3: `fs.*`, `fs.changed` with `origin {self, agent, external}`, agent-originated `fs.write` through `[control]` approval (`docs/adr/0012-embedded-editor.md:39`); E-3 running | restore must announce its writes so open editor buffers reload (`origin` gains `restore`) |
| **Resumable history** | the journal is Hutan's own history; ACP `session/load` / `session/resume`, Codex `thread/resume` (`docs/agents.md:24,60-66,149,201`); Hutan offers the harness no `fs` and no `terminal` (`agents.md:148`) | Hutan cannot rewind the harness's own context (§5.4) |
| **Storage** | SQLite migrations up to `0013_memory.sql` (`crates/storage/migrations`) | a `checkpoints` table |

## 2. Evidence: users, harnesses, and what went wrong at Codex

**Demand** (from the UX plan and the competitor spikes): Codex [#9203](https://github.com/openai/codex/issues/9203) (+538, a maintainer: the removed `/undo` "will need to be redesigned") and [#11626](https://github.com/openai/codex/issues/11626) (+229); Claude Code [#64615](https://github.com/anthropics/claude-code/issues/64615) (rewind's destructive default applies on Enter, no preview) and [#8763](https://github.com/anthropics/claude-code/issues/8763) (R277: recovery depended on rewind); Zed [#28676](https://github.com/zed-industries/zed/issues/28676) (R17, restore is irreversible). Requirements that follow: preview first, non-destructive default, restore itself undoable (`docs/spikes/2026-10-21-competitor-ux/claude-code.md:102-107`, `codex.md:18-26`).

**What Claude Code does** ([checkpointing](https://code.claude.com/docs/en/checkpointing)): a checkpoint per prompt that starts a turn; snapshots for the **100 most recent**, kept about **30 days**; options *restore code and conversation / conversation / code*; it tracks only its file-editing tools — **Bash changes, subagent edits, edits you make outside, and symlinked / hard-linked paths are not restored**; "not a replacement for version control". The SDK's `rewindFiles` is [files only](https://code.claude.com/docs/en/agent-sdk/file-checkpointing). Hutan's design differs in tracking the *result* (tree delta) so Bash edits are covered, and in sharing one mechanism for all three harnesses.

**What Codex learned the hard way.** Codex shipped ghost-commit snapshots built the same way (temporary `GIT_INDEX_FILE` + `git add` + `write-tree`) and the failure reports are the best risk list this design has: [#19588](https://github.com/openai/codex/issues/19588) (`git add -A` over a trusted **home directory**: >200 GB of `tmp_pack_*`, app unresponsive; `ghost_snapshot = false` did not stop jobs in flight), [#50920](https://github.com/openai/codex/issues/50920) (canceled turn-diff captures left **774 GiB** of `tmp_pack_*` — the temp index was isolated but the object directory was not, and the size limit applied after the writes; related #28750, #43158, #53874), and a VS Code report in #19588 of 20–30 GiB recurring in an ordinary repository. Lessons adopted in §3.5: refuse Home and roots; bound bytes **before** writing; never cancel a git write in flight; stop retrying after failures; surface "snapshots off because …"; a documented per-workspace switch that takes effect at once.

## 3. Design

### 3.1 Where it applies
- **Chat sessions** with a harness (Codex, OMP, Claude Code) — the sessions that have turns and edit files. Terminal agents (turn boundaries only inferred from hook state, `docs/integrations.md`) and direct model chats (no file tools) are out of v1 (`CK-7` may revisit terminals).
- The checkout is `placement::checkout(session cwd)`: the nearest ancestor with `.git`; a linked worktree is its own checkout (own private index, shared ref store and object store).
- **Unsupported, and said so** (`checkpoints: {state: "unsupported", reason}` on the chat): folder not in git (no object store — the feature does not invent one); `git` missing; checkout is `$HOME`, `/` or a volume root; bare/odd repositories; a repository with more than `max_untracked_files` (default 20 000) untracked files at seeding. A non-git folder gets the line "Checkpoints need a git repository", never a silent no-op.

### 3.2 Snapshot algorithm (plumbing; never the user's index)
Environment of every call: Hutan's existing hygiene (`GIT_TERMINAL_PROMPT=0`, `GIT_OPTIONAL_LOCKS=0`, `LC_ALL=C`, closed stdin) plus `GIT_INDEX_FILE=<HUTAN_HOME>/checkpoints/<repo-id>/<worktree-id>.index`, `GIT_NO_LAZY_FETCH=1` / `--no-lazy-fetch` (a partial clone must never fetch; git 2.54 accepts the flag — the variable is its documented equivalent **[UNVERIFIED here]**), `-c core.hooksPath=/dev/null` (measured: a user `reference-transaction` hook runs on `update-ref`), `-c core.fsmonitor=false`, fixed author/committer `Hutan <hutan@localhost>` (`commit-tree` ignores `commit.gpgsign` — measured with a failing `gpg.program`).

1. **Seed** (first use, and again when a `.gitignore` / `.git/info/exclude` changes): copy `<gitdir>/index`; if it has conflict stages or does not exist, `read-tree HEAD` (or start empty on an unborn branch). Copying does not lock or modify the user's index.
2. `update-index -q --refresh` on the private index (hashes stat-dirty files once).
3. **Candidates**: `ls-files -z -o --exclude-standard` (untracked and not ignored; directories with a trailing `/` are nested repositories → skipped) and `diff-files --raw -z --no-renames` (modified/deleted against the private index, i.e. against the previous snapshot).
4. **Filter in Rust with `lstat` only** (no content read yet): skip with a recorded reason files above `max_file_bytes`, past `snapshot_budget_bytes` / `max_files` in total, secret-name denylist (§3.8), paths with a `filter=` attribute (LFS; checked with `check-attr --stdin -z` only when a `.gitattributes` mentions `filter=`), non-regular files other than symlinks, gitlinks. Symlinks are stored as links (`120000`, `readlink` text — `hash-object` would follow them).
5. `hash-object -w --no-filters --stdin-paths` for the remainder (**raw bytes**: no CRLF/ident/LFS filter ever runs), then `update-index -z --index-info` with modes `100644`/`100755`/`120000` and deletions.
6. `write-tree`; if the tree equals this checkout's previous snapshot tree **and** is an *end* checkpoint of a turn that changed nothing, no ref is written ("no file changes" is recorded in SQLite). Otherwise `commit-tree` (trailers `Hutan-Session`, `Hutan-Turn`, `Hutan-Kind: start|end|restore-pre|restore-post`) and `update-ref`.

Serialised per checkout (one mutex per private index). A snapshot is **never cancelled once `hash-object` started** (Codex #50920); a start snapshot has a 5 s budget after which the prompt is sent anyway and the turn is marked `checkpoint: late` (restore of that turn is refused — its start state is unknown). To keep the budget out of the user's way the first snapshot of a checkout is taken in the background when the chat opens (`CK-2`).

### 3.3 Layout and metadata
- Git side: `refs/hutan/checkpoints/<session-id>/<turn-id>` → end commit (parent = start commit, no parent otherwise); `refs/hutan/checkpoints/<session-id>/<turn-id>-r<n>` → restore checkpoints (pre-restore commit, parent of the post-restore one). One ref per turn, not two, because ref count matters (§4.4). Tree objects hold the whole worktree state (excluding ignored/skipped), so a checkpoint stays meaningful after the user switches branch.
- SQLite `checkpoints` (next free migration): id, session_id, turn_id, kind, checkout_root, common_dir, ref, commit, tree, parent_id, created_ms, files_changed, bytes_added, skipped_json, state (`ok|late|failed`). Rebuildable from the refs' commit trailers at daemon start (reconcile: rows without refs are dropped, refs without rows are adopted or removed per retention).

### 3.4 What is and is not captured

| Case | Behaviour | Evidence |
|---|---|---|
| tracked, modified/deleted; untracked non-ignored | captured | §4.3 |
| **ignored** (`node_modules/`, `target/`, `.env`) | never read, never in a tree | measured: 0 matches in 10 k-file tree; `.env`, `*.log`, `target/` absent in the small repo |
| file above `max_file_bytes` (default 10 MiB) | skipped, listed in the turn footer ("2 files not captured: too large"); a conflicting overwrite of such a file is refused because it could not be undone | 120 MiB file cost 3.5 s and 129 MiB (§4.2) |
| binary ≤ cap | captured as is (random 5 MiB → +5.0 MiB; no delta compression before gc) | §4.2 |
| **submodule / nested repo** | the gitlink is neither captured nor restored; edits *inside* it are not captured | `git add` of a nested repo stores only a gitlink and warns (measured); `ls-files -o` prints `emb/` |
| **LFS** (`filter=lfs`) | the path is skipped with reason "LFS"; restore never writes a blob that starts with `version https://git-lfs.github.com/spec/v1` | no `git-lfs` here to test [INFERENCE]; raw-byte hashing avoids the clean filter, the attribute check avoids mixing pointers and content |
| symlinks | captured as links; **restore skips symlinks** (reported), like Claude Code | [Claude Code docs](https://code.claude.com/docs/en/checkpointing) |
| files with CRLF/`text=auto` attributes | stored raw; restore writes raw blob bytes itself (no smudge), so a round trip is byte exact | design; `CK-4` test |
| staged state | **not** restored (only worktree contents); the index is never written | by design |
| git operations in the turn (`commit`, `checkout`, `reset`) | contents are restored; **HEAD, branches and stash are not rewound** — the dialog says so when HEAD moved | §5.3 |
| edits outside the checkout, databases, installs | not captured; the label says "files inside `<checkout>`" | — |

### 3.5 Budgets and guards (defaults, all in `daemon.toml [checkpoints]`)
`max_file_bytes = 10 MiB`; `snapshot_budget_bytes = 64 MiB` of new content per snapshot (over it: files are skipped, largest first, and listed); `max_files = 5 000` changed files per snapshot; `max_untracked_files = 20 000` at seeding; per-git-call timeout 60 s (read steps) / no kill during object writes; refuse Home/roots/volume roots; **3 consecutive failures or free disk < 2 GiB** switch the checkout's checkpoints off with a visible reason until the user turns them on; `[checkpoints] mode = "off"` and a per-project override apply to new snapshots immediately (no job outlives the setting).

### 3.6 Concurrent agents in one checkout
The existing warning (`shared_with`, `docs/orchestration.md:32`) stays. Snapshots are whole-checkout (they cannot attribute an edit to a session), but **restore is path-scoped and conflict-checked** (§5.2): restoring A's turn never rewinds a file A did not change, and a file B changed afterwards shows as a conflict. The restore dialog names the sharing agents. A restore is refused (`invalid_state`) while **any** agent session in the checkout has a running turn (interrupt first; the dialog offers *Interrupt and restore*).

### 3.7 Retention and `git gc`
Proposed defaults (decision D2): per session the newest **100** turns (what Claude Code keeps), sessions untouched for **30 days** (Claude Code's sweep), a recorded-bytes cap of **1 GiB** per repository (oldest turns first), at most **1 000** checkpoint refs per repository. Retention runs after each turn end, at daemon start, and daily; it deletes refs (`update-ref --stdin` batches) and rows, and runs `pack-refs --include 'refs/hutan/*'` when more than 200 loose refs exist (measured to work).
- Reachable objects are safe: `gc --prune=now` kept an object that only a checkpoint ref held; `git fsck` clean (§4.5).
- After a ref is deleted its objects become ordinary unreachable objects: with `--prune=1.hour.ago` the object survived, with `--prune=now` it was collected (§4.5). Hutan **never runs `gc` or `prune`** (an expire-now prune races with other git processes writing objects not yet referenced); disk use therefore trails retention by git's own `gc.pruneExpire` (default two weeks per [git-gc](https://git-scm.com/docs/git-gc) — [UNVERIFIED here; the 1 h / now behaviour is measured]).
- Reflogs: git keeps no reflog for `refs/hutan/*` by default (measured: only `logs/refs/heads`), so deleted refs leave no pinned history.
- Removing a worktree keeps its checkpoint refs (measured) until retention drops them — they may be the only copy of an agent's work in a deleted worktree; `hutan checkpoint ls` still lists them (restore needs an existing checkout).

### 3.8 Privacy
Ignored files are never read. Untracked non-ignored files are captured like `git add` would, into the repository's own object store (same trust domain as the repo), so a name denylist skips likely secrets (`.env*`, `*.pem`, `*.key`, `*.p12`, `id_rsa*`, `id_ed25519*`, `*.kdbx`, `credentials*`; configurable `[checkpoints] exclude`). Journals, MCP output and logs carry paths and counts, never file contents; `checkpoint.diff` content is bounded and goes only to the caller. **Visibility of the refs:** `git log --all` shows the checkpoint commits (measured) and `git push --mirror` would push them (measured: 5 of 5 refs); `--branches --tags --remotes` and `push --all` do not. Hutan never pushes; the docs say "do not `push --mirror` this repository" (decision D3 weighs a private store instead).

---

## 4. Experiment

### 4.1 Setup
Temporary repository (generator and scripts were throw-away; recipe reproducible): 100 directories × 100 `.rs` files of 60–150 pseudo-random lines (3–8 KiB), 20 random 1 MiB binaries, `.gitignore` (`node_modules/`, `target/`, `.env`), an ignored `node_modules/` of 5 000 small files and an ignored `.env`; committed and `git gc`'d. **10 021 tracked files, 15 022 files, 171 MB on disk, packed `.git` 40.8 MiB.** `git status` takes 35–38 ms; `ls-files -o --exclude-standard` 26 ms; `diff-files --name-only` 30 ms.

Method A = copy the user's index to a temp file, `git add -A`, `write-tree`, `commit-tree`, `update-ref` (a 40-line shell script). Method B = the design of §3.2 (candidate lists + `hash-object --no-filters` + `update-index --index-info`), prototyped in Python.

### 4.2 Time and object growth

| Scenario | Wall time | Object growth |
|---|---|---|
| A: clean tree, real index stat-clean (5 runs) | median **117 ms** (min 113, max 487 — the first run) | 5 commits only: +20 KiB (4 KiB blocks) |
| A with persistent private index, clean (5 runs) | **95–102 ms** | — |
| A breakdown (clean): copy index 10 ms, `add` 74, `write-tree` 31, `commit-tree` 21, `update-ref` 20 | ≈ 156 ms through a shell | — |
| A: 3 modified + 1 new + 1 deleted | **125 ms** (3 runs: 120–126) | **+52 KiB**, 13 objects |
| A: ≈ 200 modified + 50 new files (≈ 1 MB new content) | **326 ms** first, 143–161 ms for the same state again | **+1 144 KiB**, 286 objects; ~0 on repeat (dedupe) |
| A: + one 5 MiB random binary | **404 ms** | +5 136 KiB |
| A: + one 120 MiB random file | **3 533 ms** | **+132 068 KiB** (loose, zlib on random data) |
| A: all 10 k source files `touch`ed (same content; real index stale) | **1 825 ms** first, 725 ms next (copy mode: every snapshot rehashes until the *user's* git refreshes its index) | ~0 |
| A same storm, persistent private index | **513 ms** once, then **95 ms** | ~0 |
| A: cold — no commits, no index, 10 k + 20 untracked files (5 k ignored present) | **6 691 ms** `add` + 138 ms `write-tree`; second `add` 35 ms | 10 126 loose objects, **64.6 MiB** |
| B: clean, warm (refresh 53 ms, `ls-files -o` 21, `diff-files` 23, `write-tree` 16, `commit-tree`+`update-ref` 28) | **117–141 ms** | commit only |
| B: ≈ 200 changed + 50 new + a 12 MiB file (cap 8 MiB → skipped, listed) | **323 ms** (hash 133 ms) | as A minus the skipped file |
| preview/restore helpers on 10 k-file trees: `diff-tree --raw` (≈ 250 files) 42 ms, `--numstat` 99 ms, `read-tree` into a private index 35 ms | — | — |

Reading: warm snapshots cost about **three `git status`**; they scale with *changed bytes*, not repository size; the pathological cases are first-time hashing (cold or stat-dirty) and big files — exactly Codex's failure surface, hence the caps and the persistent private index. Projected disk (typical agent turn ≈ 5–50 changed source files ≈ 50–300 KiB) is a few MB per working day before any `gc` compression **[INFERENCE]**.

### 4.3 The user's state is untouched (verified)
On the 10 k repository and on a small repository with staged + unstaged + untracked + ignored + nested-repo files and a **held `.git/index.lock`**: the snapshot succeeded; `git status --porcelain=v2 -uall` before/after identical; `.git/index` SHA-1 and mtime, `.git/HEAD` mtime, `HEAD`, stash (empty) and non-`hutan` refs identical. Ignored paths (`node_modules/`, `.env`, `target/`, `*.log`) absent from the snapshot tree; nested repo stored as a `160000` gitlink with git's warning; staged and unstaged content of the same file captured as the worktree content. Two snapshots started concurrently in one checkout both succeeded and produced the same tree under different refs. A linked worktree snapshot used its own git dir for the index and wrote the ref into the shared ref store; `git worktree remove` left the ref in place.

### 4.4 Many refs hurt other git commands (measured)
20 000 refs (200 sessions × 50 turns × start/end) created by one `update-ref --stdin`: **5.5 s**, `.git` +80 MB of 4 KiB blocks; afterwards `git branch` **711 ms** (17 ms before/after packing), `git log --all` 721 ms, `for-each-ref refs/hutan` 1 306 ms loose / 580 ms packed, `git status` unaffected (17 ms); one new ref with 20 k packed refs 16 ms; deleting 10 000 packed refs 1.8 s. Linear extrapolation puts 1 000 loose refs at ≈ 35 ms of `git branch` **[INFERENCE]** — hence one ref per turn (not per snapshot), the 1 000-ref cap, and `pack-refs --include 'refs/hutan/*'`.

### 4.5 Restore, undo, gc, mirror (measured on small repos)
- Plumbing restore round trip: start/end snapshots of a turn that edits two files, deletes one, adds two (one in a new directory); the user then edits one of the agent's files and an unrelated file; `diff-tree` start→end lists exactly the five agent paths (`A d/new.txt`, `M f1.txt`, `M f2.txt`, `D f3.txt`, `A new/dir/n.txt`); end→now lists `M f2.txt`, `M f4.txt`, so `f2.txt` is the conflict and `f4.txt` is out of scope. Restoring the three clean paths (`read-tree` into a private index + `checkout-index -f -z --stdin`, two deletions, empty directories removed) left `git status` at the user's two edits; restoring from the pre-restore checkpoint returned a tree **identical** (`write-tree` equal) to the pre-restore one.
- `gc --prune=now` with a checkpoint ref: object kept; after the ref is deleted: kept with `--prune=1.hour.ago`, gone with `--prune=now`; `fsck` clean; `push --mirror --dry-run` listed 5 `hutan` refs.

### 4.6 Limits of the experiment
One machine and filesystem (APFS, warm cache, no antivirus/FSEvents load); random bytes compress worse than real binaries; no real LFS (`git-lfs` is not installed); no submodule with content, no partial clone, no CRLF attributes were exercised — those rows of §3.4 are design with a slice test, not measurements.

---

## 5. Restore UX and algorithm

### 5.1 Entry, preview, confirm
1. **Entry**: the footer of every chat turn that changed files — `3 files changed · +41 −7 · Rewind…` — and ⌘K *Chat: Rewind Files to Before This Turn…* (picks the turn); Agents page → session → *Checkpoints*; `hutan checkpoint restore`.
2. **Preview** (always, nothing is written): a file list — `M f1.txt`, `A d/new.txt` *(will be deleted)*, `D f3.txt` *(will be recreated)* — with per-file diff (`now → restored`), a checkbox per file, and these groups: **clean** (checked), **edited after the turn** (unchecked, red label "you or another agent changed this since the turn — restoring overwrites that change"), **not captured** (greyed, with the reason), **already equal** (hidden). Facts shown in the header: `<harness>`, the turns covered (one or "5 turns"), the checkout, `shared with …`, and a line when HEAD moved during the turn.
3. **Confirm**: the button is `Restore 4 files` (never the default; Esc and Return-on-nothing cancel; default focus on *Cancel*), fixing the Claude Code #64615 failure. Conflicts need their own checkbox; *Restore all, including edited-after* is a separate, second-click action.
4. **Result**: toast `Restored 4 files · Undo` (5 s, like *Closed … · Undo* in `workbench/closing.rs`), plus a persistent `Restored — Undo` line in the turn footer; `hutan checkpoint undo` prints the same.

### 5.2 Algorithm (three-way, per path)
Turn `i` has start tree `S_i` and end tree `E_i`; its delta is `D_i = diff(S_i, E_i)`. To rewind **before turn k** over turns `k…n` (the default is the single turn *k = n*):
- `F = ⋃ paths(D_i)`, `i ∈ k…n`; for each `f`: `want(f) = S_j[f]` where `j` is the first turn touching `f`; `last(f) = E_m[f]` where `m` is the last; `now(f)` = the file in the checkout (hashed raw; from the pre-restore snapshot).
- `now == want` → nothing to do. `now == last` → **clean**: write `want` (or delete the file when `want` is absent). Anything else → **conflict, kind `after`**.
- Between consecutive touching turns `j < j'`, `E_j[f] ≠ S_{j'}[f]` → conflict, kind `gap` ("edited between turns").
- `fs.changed` attribution of the editor ADR (when E-3 lands) adds kind `during` for paths the user saved while the turn ran ("you edited this too").
- Paths in `F` whose checkpoint was skipped (too large, LFS, secret name, symlink, gitlink) are **not restorable** and listed; a conflicting overwrite of an uncaptured file is refused.
- Everything else in the checkout — your own edits to files no turn touched, other agents' files — is out of scope by construction. A whole-tree reset would be wrong in a shared checkout.

Before writing: take the **pre-restore checkpoint** (`restore-pre`, a normal snapshot, so the conflicting edits it is about to overwrite are in it). After: `restore-post`. The restore *is* a synthetic turn `(S = pre, E = post)`, so **Undo = the same algorithm rewound before that turn**, and undo-of-undo works. The request carries the digest of the previewed plan; if a file changed between preview and apply the daemon answers `conflict` with the new plan instead of writing.

Writing (daemon): confined to the checkout root, `openat(O_NOFOLLOW)` style as `context_file.rs`, never under `.git/`, atomic temp-file + rename, mode from the tree entry (`100644`/`100755`), blobs read with `cat-file --batch` and written **raw**, empty parent directories removed only when the restore emptied them, symlinks skipped. Files are announced as `fs.changed` with `origin {kind: "restore", checkpoint_id}` so editors reload (`CK-4`). The user's index and HEAD are not touched.

### 5.3 What it will not do (label text)
"Restores files only. <Harness> still remembers this turn and may mention or redo the changes. Git history (commits, branches, stash) and anything outside `<checkout>` are not rewound." If HEAD moved during the turn the second sentence names the commit.

### 5.4 Which harnesses can rewind the conversation (verified, §sources)

| Harness | Its own rewind | Drivable by Hutan over today's protocol? | v1 label |
|---|---|---|---|
| **Claude Code** | `/rewind` / Esc Esc: restore code + conversation, conversation, code, summarize; 100 checkpoints, ~30 days; code tracking misses Bash, subagents, symlinks ([docs](https://code.claude.com/docs/en/checkpointing)); SDK `rewindFiles` files only ([SDK](https://code.claude.com/docs/en/agent-sdk/file-checkpointing)) | **No.** ACP v1 has no rewind or truncate method (stable methods: `session/new|load|resume|prompt|cancel|close|list|delete|set_mode|set_config_option`, [schema](https://agentclientprotocol.com/protocol/v1/schema)); `session/fork` is a [Draft RFD](https://agentclientprotocol.com/rfds/session-fork) ("could accept a message ID … for checkpoints" in the future) and `claude-agent-acp` serves it only in its **experimental v2** mode ([README](https://github.com/agentclientprotocol/claude-agent-acp)) | files only |
| **Codex** | Esc rewind is conversation only ([#11626](https://github.com/openai/codex/issues/11626)); `/undo` removed ([#9203](https://github.com/openai/codex/issues/9203)); app-server: `thread/fork` with `lastTurnId` copies history through a turn; `thread/rollback` (units disputed, [#23000](https://github.com/openai/codex/issues/23000)) was deprecated and **removed**, replaced by `thread/revert {threadId, beforeTurnId}` whose doc says it "only changes persisted conversation history. It does not revert local file changes" ([thread.rs](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/src/protocol/v2/thread.rs), [README § Thread rollback](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)) | Possible in principle, **not in the pinned 0.160–0.169 range [UNVERIFIED]** (no local codex to read its schema; `docs/agents.md:22`), and a fork yields a new thread id (Hutan's journal and `ChatSpec.thread_id` would need a design) | files only |
| **OMP 18.12.3** | `/rewind` "Rewind to a previous message, keeping the old path as a branch", `/tree`, a fork command; agent-side `checkpoint` ("Create a git-based checkpoint to save and restore session state") and `rewind` tools behind a setting (strings of the installed binary; not run) | **No** (interactive TUI commands and agent tools; not ACP methods) | files only |

Conclusion: **files only for all three, labelled honestly**. After a restore the dialog offers (default on) to put a note in the composer: "Hutan restored these files to before turn N: …. Disregard the changes of that turn." — Hutan's one honest bridge to the agent's memory. Real conversation rewind is spike `CK-7`.

---

## 6. Protocol, CLI, desktop, MCP sketch

### 6.1 Protocol (additive minor, capability `checkpoint`; next free minor after `UX-PROTO-1` — today `0.32`, E-3 takes 0.33, `UX-PROTO-1` 0.34)
```text
checkpoint.list    {session_id?, workspace_id?, limit?≤200, before?}            -> {checkpoints: [CheckpointInfo], next?}
checkpoint.diff    {checkpoint_id, against?: "end"|"now"|<checkpoint_id>, path?} -> {files:[{path,status,added,removed,binary,truncated}], patch?}   // bounded like worktree.diff
checkpoint.preview {checkpoint_id, through?: <checkpoint_id>, paths?}            -> {plan_digest, entries:[{path, action: write|delete|skip, state: clean|after|gap|during|uncaptured, reason?}], head_moved?, shared_with:[SessionId]}
checkpoint.restore {checkpoint_id, through?, paths?, accept_conflicts?: [path], plan_digest}
                                                                                  -> {pre: CheckpointId, post: CheckpointId, written, deleted, skipped:[{path,reason}]}
checkpoint.remove  {checkpoint_id | session_id}                                  -> {removed}
events: checkpoint.created {checkpoint: CheckpointInfo}, checkpoint.removed {ids}, checkpoint.state {session_id, state, reason?}
CheckpointInfo {id, session_id, turn_id?, kind: turn|restore, created_ms, checkout, files_changed, added, removed, skipped:[{path,reason}], state: ok|late|failed}
ChatInfo.checkpoints? {state: on|off|unsupported, reason?}
```
Errors: `unsupported` (not git, Home, LFS-only…), `not_found`, `invalid_state` (a turn is running in the checkout), `conflict` (`data.entries` when `plan_digest` is stale or conflicts are not accepted), `forbidden` (agent-originated restore). Golden fixtures per `AGENTS.md`; `docs/protocol.md` gets a § Checkpoints. `checkpoint.restore` is `Access::UserOnly` (`router.rs:615`): an agent-originated connection gets `forbidden` regardless of `[control]` (an agent must not be able to unwind the user's work, and a request may never raise its caller's privileges, ADR 0016); `list`, `diff`, `preview` are reads.

### 6.2 CLI
```text
hutan checkpoint ls [SESSION|--ws W] [--json]          # turn, time, files, +/-, state, skipped
hutan checkpoint diff CHECKPOINT [--against end|now|CP] [--stat] [PATH]
hutan checkpoint restore CHECKPOINT [--through CP] [--path P]... [--accept-conflicts P]... [--yes]   # prints the preview; needs --yes without a terminal
hutan checkpoint undo [SESSION]                         # undoes the latest restore
hutan checkpoint rm CHECKPOINT|SESSION
```
`restore` without `--yes` shows the preview and asks `[y/N]` (default No); conflicts are never overwritten without `--accept-conflicts` naming them.

### 6.3 Desktop
Turn footer button *Rewind…* (and `Restored — Undo`), preview dialog of §5.1 (keyboard: ↑↓ files, Space toggles, Return confirms only when a button has focus), ⌘K entries (*Chat: Rewind Files to Before This Turn…*, *Chat: Undo Last Restore*), an **Agents page → session → Checkpoints** list (time, turn prompt, files, size, skipped), a *Checkpoints* section in Settings → Agents & tools (mode, caps, retention). The Changes view may later offer "diff vs checkpoint" (reuses `checkpoint.diff`). Hidden when `capability checkpoint` is absent; a chat whose state is `unsupported` shows the reason instead of a button. State and conflict labels never rely on colour alone.

### 6.4 MCP (`hutan mcp`, ADR 0016)
`checkpoint_list` and `checkpoint_diff` (read-only, bounded; descriptions say "what the user can rewind to; you cannot restore"). `checkpoint_restore` is **not** exposed. `checkpoint.preview` is exposed as a read.

---

## 7. Slice plan (each ≤ 1–2 agent-days; contract owner owns `core` / `protocol`)

| Slice | Content | Crates | Acceptance (tests) | Effort | Deps |
|---|---|---|---|---|---|
| **CK-1** engine | `checkpoints/{git,snapshot,caps}.rs`: runner variant with `GIT_INDEX_FILE`, hooks off, no lazy fetch; seed/refresh; candidate lists; `lstat` filters (size, denylist, `filter=`, symlink, gitlink, nested repo); raw hashing; `write-tree`/`commit-tree`/`update-ref`; per-checkout mutex; no cancel during writes | `daemon` | on temp repos: user index SHA/mtime, HEAD, stash, status identical (held `index.lock` too); ignored absent; staged+unstaged+untracked captured; nested repo gitlink; oversize skipped with reason and no object written; unborn HEAD; conflict-stage index seeds from HEAD; two concurrent snapshots; linked worktree; a `reference-transaction` hook does not run; hostile config (`commit.gpgsign`, `core.hooksPath`) ignored | M | ADR accepted |
| **CK-2** turns + store | core types (`CheckpointId`, `CheckpointInfo`), SQLite migration, `[checkpoints]` config (`mode`, caps, retention) via `daemon.settings`, hook in `ChatManager::send` (start, 5 s budget, `late`) and the pump's turn end (end/no-change), pre-warm at chat open, reconcile at start, retention (count/age/bytes/refs, `pack-refs --include`), failure counter and low-disk switch | `core`, `storage`, `daemon` | fixture chat (`hutan-codex-fixture replay`, `acp_fixture`) edits files via a script → one ref per turn, none for a read-only turn; interrupted/failed turn still gets an end; restart reconciles rows and refs; retention drops oldest first and keeps pre-restore; off-switch effective for the next snapshot; Home/non-git → `unsupported` | M | CK-1, contract owner |
| **CK-3** read surface | protocol 0.N `checkpoint.list/diff/remove`, events, `ChatInfo.checkpoints`, client methods, CLI `ls/diff/rm`, MCP `checkpoint_list/diff`, golden fixtures, `docs/protocol.md`, `docs/agents.md` § Checkpoints | `protocol`, `client`, `cli`, `daemon` (MCP) | diff bounds (2 MiB/file, 20 MiB, 5 000 files) and binary flag like `worktree.diff`; agent-originated list works, `remove` asks per `[control]`; CLI against a real daemon | M | CK-2 |
| **CK-4** restore | pure `restore_plan` in `core` (three-way classification, chains, `gap`, `uncaptured`), daemon preview/restore/undo with pre/post checkpoints, plan digest, busy-checkout refusal, confined atomic writes, `fs.changed` origin `restore`; `checkpoint.preview/restore`, CLI `restore/undo`; `Access::UserOnly` | `core`, `daemon`, `protocol`, `cli` | the three acceptance cases of the UX card on a temp repo: a turn edits three files incl. a new one → restore returns the exact tree (`write-tree` equal); undo restores it again (and undo-of-undo); a file edited after the turn is listed and untouched without `--accept-conflicts`; stale `plan_digest` → `conflict`; agent-originated restore → `forbidden`; running turn in the checkout → `invalid_state`; symlink/LFS/oversize skipped with reasons; CRLF file byte-exact; two agents in one checkout | L | CK-3, E-3 (origin), shares the guarded-write primitive with E-10 per-hunk Reject |
| **CK-5** desktop | footer button/`Restored — Undo`, preview dialog, palette, Agents page list, Settings section, capability gating, honest label and note-to-agent | `desktop`, `chat_view` | GPUI tests (real clicks): button on a turn with changes only; dialog groups, Esc/Return behaviour, conflict checkbox; undo toast; screenshot of the preview (acceptance S of the card) | L | CK-4 |
| **CK-6** health + docs | `hutan checkpoint status` (per-repo refs, recorded bytes, skipped, state), notices when off/late/failed, docs `docs/checkpoints.md`, `docs/vision.md`/`orchestration.md` lines, perf note (snapshot ≤ 150 ms warm on the bench repo as a regression budget) | `cli`, `daemon`, docs, `xtask` | status JSON; budget test on the generated 10 k repo | M | CK-2 |
| **CK-7** spike | real conversation rewind: Codex `thread/fork {lastTurnId}` / `thread/revert` in the pinned range, Claude ACP `session/fork` at a message when stable, OMP; terminal agents via integration hooks; what a fork does to Hutan's journal and `ChatSpec` | docs only | spike report with recorded transcripts | S | CK-4 |

Order: CK-1 → CK-2 → CK-3 → CK-4 → CK-5, with CK-6/CK-7 after CK-2/CK-4. Wire work fits the protocol lane after `UX-PROTO-1`.

## 8. Risks

| # | Risk | Mitigation / residual |
|---|---|---|
| R1 | Disk blow-up as in Codex #19588/#50920 | `lstat` budgets before any write; no cancel mid-write; Home/roots refused; failure counter + low-disk switch; `git gc` clears `tmp_obj_*` older than its expiry (residual: a killed `hash-object` leaves one temp file) |
| R2 | Latency before the prompt on big repos / first use | pre-warm at chat open, persistent private index, 5 s budget then `late`; measured warm ≈ 0.1 s; cold 6.7 s only when the repo was never indexed |
| R3 | Ref clutter, `git push --mirror` pushes checkpoints, many-ref slowdown | hidden namespace, ≤ 1 000 refs, `pack-refs --include`; docs; decision D3 (private store) |
| R4 | Filters/attributes (CRLF, LFS, ident) corrupt a round trip | raw hashing and raw writing; `filter=` paths skipped; LFS pointer never written; CRLF test |
| R5 | A restore surprises: HEAD moved, agent remembers, shared checkout | label §5.3, `head_moved`, shared-with list, busy-checkout refusal, conflicts unchecked |
| R6 | Concurrent agents misattributed | path-scoped three-way restore; `during` conflicts from `fs.changed` once E-3 lands |
| R7 | Partial clones / missing blobs | `GIT_NO_LAZY_FETCH=1`; a missing blob skips the path with a reason |
| R8 | User hooks/config interfere | `core.hooksPath=/dev/null`, fixed identity, private index; tests with hostile config |
| R9 | Users expect conversation rewind | honest label; CK-7 |
| R10 | Edits by Bash outside the checkout (or in submodules) are invisible | stated in the label; Claude Code has the same gap and more |

## 9. Decisions for you (at most three)

1. **Default on or off?** *Recommended:* **on** for chat sessions in git checkouts (it costs ≈ 0.1 s per turn and a few MB a day), with `[checkpoints] mode = "off"` globally and per project, a one-line first-use notice, and `unsupported` reasons shown. Off-by-default would leave the +538/+229 gap open for everyone who never finds the setting.
2. **Retention and caps.** *Recommended:* 100 turns per session, 30 days, 1 GiB recorded per repository, 1 000 refs, 10 MiB per file, 64 MiB per snapshot (the first two mirror Claude Code's documented 100 / ≈ 30 days).
3. **Where the refs live.** *Recommended:* hidden refs in the user's own repository (dedupes against existing objects; objects stay reachable under every `gc`; worktrees share them) despite visibility in `git log --all` and `push --mirror` (§3.8). Alternative: a private bare store under `HUTAN_HOME` — no clutter or push risk, our own gc, but every new blob is stored twice and it cannot use `alternates` safely (`git gc` in the source repository may prune objects the store still needs **[INFERENCE]**; see [gitrepository-layout](https://git-scm.com/docs/gitrepository-layout)).

## 10. Sources
- Hutan: `docs/spikes/2026-10-21-ux-plan.md:154-168,639-645,763`; `docs/spikes/2026-10-21-competitor-ux/{codex,claude-code}.md`; `docs/orchestration.md:3-33,73,142,178`; `docs/agents.md:22-24,60-66,129-149,201`; `docs/adr/0001-daemon-authoritative-topology.md`, `0002-ipc-transport-and-framing.md`, `0012-embedded-editor.md:33-45`, `0016-agent-control-surface.md`; `docs/spikes/2026-10-21-editor.md` (E-3, E-10, `fs.changed` origin); `crates/core/src/agent.rs`; `crates/daemon/src/{chat,worktrees,placement,projects,router}.rs`.
- Claude Code: [checkpointing](https://code.claude.com/docs/en/checkpointing), [Agent SDK file checkpointing](https://code.claude.com/docs/en/agent-sdk/file-checkpointing); issues [#64615](https://github.com/anthropics/claude-code/issues/64615), [#8763](https://github.com/anthropics/claude-code/issues/8763).
- Codex: issues [#9203](https://github.com/openai/codex/issues/9203), [#11626](https://github.com/openai/codex/issues/11626), [#23000](https://github.com/openai/codex/issues/23000), [#19588](https://github.com/openai/codex/issues/19588), [#50920](https://github.com/openai/codex/issues/50920); [app-server README](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md); [`ThreadRevertParams`](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/src/protocol/v2/thread.rs).
- ACP: [v1 schema](https://agentclientprotocol.com/protocol/v1/schema), [session/fork RFD](https://agentclientprotocol.com/rfds/session-fork), [RFD updates](https://agentclientprotocol.com/rfds/updates), [claude-agent-acp](https://github.com/agentclientprotocol/claude-agent-acp).
- Zed [#28676](https://github.com/zed-industries/zed/issues/28676) (as summarised in the UX plan). OMP: strings of `omp` 18.12.3 (`/rewind`, `checkpoint`, `rewind` entries).
- Experiment: plain `git` 2.54.0 on a throw-away repository; scripts not committed (recipe in §4.1).
