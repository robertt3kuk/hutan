# File explorer — what Hutan has, what users expect, what to build (FILE-EXPLORER-SPIKE, 2026-10-27)

Trigger: the user, 2026-10-27 — *"let's evaluate how workable Hutan really is; there's no file explorer — let's think it through, that's the point of bringing an editor inside; scrolling down must work and things must fit the window size"*. Their screenshot (Changes view, 88 new Markdown files) showed a right rail of **History · Changes · Requests · Agents** and no Files icon.

Docs only. Nothing in the product was changed and the workspace was not built. Mock: [`2026-10-27-file-explorer-1440x900.svg`](2026-10-27-file-explorer-1440x900.svg).

## 0. Recommendation (read this first)

1. **Hutan already has an explorer; the user could not reach it.** The Files panel (E-13a, `panels/files.rs`) is a lazy, virtualized, git-decorated, watched tree with keyboard navigation and auto-reveal. The user's rail is exactly the **v0.1.0 preset** (`history, changes, inspector, agents, resources`); v0.1.0 had no `files` panel and its left dock was `[sessions, tasks]`. The only migration that places Files (`migrate_v7`) fires when the left dock is *exactly* `[sessions, tasks, files]`, so a v0.1.0-era saved layout never gets Files anywhere (§1.3). The first fix is a migration, not a new panel — it is already in flight as FX-CHANGES-VIEW item 4 (§4, EX-0).
2. **Keep the placement: Files is the first icon of the right rail, beside Changes, one body at a time.** One-body-at-a-time is how VS Code (views in one container), JetBrains (tool windows) and Zed (docks) work; stacking two panels in one dock is a 0-reaction request elsewhere (Orca #29611). Placement stays a user choice through the existing *Move to ▸ Left · Right · Bottom · Center tab* (users of Zed, Orca and Cursor ask for exactly this: Zed #5582, Orca #10153, Cursor forum 171835). Cursor's Agents Window made the same move to the right and drew the same complaint (forum 164985: "the sidebar disappeared").
3. **Make the panel earn the rail slot.** Today it is a bare tree. Give it the three views users expect around the tree, as *sections of one panel* (VS Code's model): **Open Editors**, **Changed by agents** (Hutan's own: the E-10 ledger), and the **tree**, plus filter-as-you-type, folded single-child folders, indent guides, diagnostics and agent marks.
4. **Make it an explorer, not a viewer: file operations through the daemon.** Create, rename, move, copy, duplicate, trash (system Trash, never permanent delete in v1) and undo, as one additive protocol family `fs.create|rename|move|copy|trash|restore` (capability `fs.ops`, Mutate class, same confinement as `fs.write`, ADR 0012 amendment first). Nothing in the desktop touches the disk (ADR 0001/0012).
5. **Fit the window.** Name-first rows, folded folders, an indent cap and a **rail peek** (the body opens over the center instead of squeezing it) below the width where the center would drop under 480 px.
6. **Performance is mostly already right** (lazy per folder, `uniform_list`). Measured here on a 120 000-file repo: `fs.list` of 100 entries 20 ms, of a flat 20 000-entry folder 650 ms and capped at 5 000 (the rest is unreachable today), first `fs.find` 0.71 s then 0.13 s, `worktree.diff` 0.60 s vs `git status` 0.25 s (debug daemon, CLI wall time) (§1.4). The plan adds a bench before more features.
7. **Slices:** EX-0 reach (in flight) → EX-1 tree polish + bench → EX-2 narrow windows → EX-3 filter → EX-4 row menu → EX-5 decorations + sections → EX-6 daemon file ops (the only protocol change, 0.54) → EX-7 explorer file ops → EX-8 roots/worktrees → EX-9 Finder import → EX-10 perf follow-ups. EX-6 runs in parallel with EX-1…EX-5 (different crates).
8. **Decisions needed from the user** are in §5 (default open panel, Trash-only delete, no MCP file ops, Open Editors default).

## 1. What Hutan has today

### 1.1 Inventory

| Piece | State | Where |
|---|---|---|
| **Files panel** | Registered panel `files`, default dock Right, first icon of the right rail in `default`, `focus`, `overview` (SB-ATTACH, user 2026-10-22: "Orca puts the explorer on the right"). Tree of the *active container's folder* (active checkout, else the workspace's active project, else Home's `~/Hutan`; follows Mawas's scope). Header: scope chip + an eye toggle + the panel gear. | `crates/desktop/src/workbench/panels/{mod,files}.rs`, `sessions/store.rs::files_root`, `docs/editor.md` § E-13a, `docs/design.md` § Layout (*Files in the right dock*) and § Files at Home |
| Reading | Lazy: `fs.list` of one folder when it is opened; rows are one flat `Vec<Row>` drawn by `uniform_list` (a frame builds only the rows in view; tested with 20 000 open entries). A listing is capped at 5 000 entries (`truncated` → a row "More entries than the tree lists"). Switching container keeps drawing the old tree until the new root lists. | `files.rs` `Tree`, `render`, `docs/protocol.md` § Files |
| Live | Open folders watched (`fs.watch`, ≤ 64; `fs.changed` re-lists the folder, `fs.generation` re-lists unwatched open folders). | `files.rs` `watch`, `listen`, `on_notice` |
| Decorations | `.gitignore` / `.ignore` matches hidden, or dimmed with the eye; git letters `M A U D R` + tint, a dot on folders holding changes (from `worktree.diff`'s file list, no patches); symlinks italic. One generic file icon (no per-type icons). | `files.rs` `render_rows`, `panels/files_git.rs` |
| Keyboard | ↑ ↓, → (open / step in), ← (close / parent), Return (open, keyboard follows), Space (show, keyboard stays), click shows, double click opens. No Home/End/PageUp/PageDown, no type-ahead, no collapse-all. | `files.rs::key` |
| Reveal | The workbench reveals the focused tab's file (`panels.files.auto_reveal`, default on; *Files: Reveal Active File*; the file tab's *Reveal in Files Panel*). | `workbench/files.rs` |
| Context menu | **Open · Show File History** (files), **Copy Path**. Nothing else (no new / rename / delete / duplicate / reveal in Finder / open in terminal / copy relative path / compare / find in folder). | `workbench/file_history.rs::show_files_menu` |
| Keys / commands | ⌘⇧E *Files: Show Files Panel* (opens and focuses; does not toggle back), *Files: Reveal Active File*, View → Files, ⌘K *Panel: Open Files*. | `workbench/actions.rs` |
| Settings | `panels.files.show_ignored` (false), `panels.files.auto_reveal` (true). | `settings.rs::FilesPanelSettings` |
| Placement tools | Per-panel ⋯ *Move to ▸ Left · Right · Bottom · Center tab · Hide*, right-click checklists on the rail / nav / bottom strip, *Reset to built-in layout*, a 10 s undo toast. | `docs/design.md` § Customizing the workbench (WIDGET-CONFIG) |
| **Quick open** | ⌘P: Files (daemon `fs.find`: fuzzy over a ≤ 200 000-file index, `.git` / `target` / `node_modules` / ignored out; 100 best re-ranked by recency, 20 shown) then Sessions; `path:line:col`; ⌘Return opens in a split. | `workbench/quick_open.rs`, `docs/editor.md` |
| **Search** | `search` panel over `fs.search` (≤ 1 000 hits, `.gitignore` honoured, include/exclude globs, replace all as one transaction per file). Default dock Left, **not on any built-in rail**; reached with ⌘⇧F. | `panels/search.rs`, `docs/editor.md` § E-13b |
| Daemon file service | `fs.read`, `fs.write.*` (atomic, `expect` version, `create`), `fs.stat`, `fs.list`, `fs.find`, `fs.search`, `fs.watch` / `fs.changed` (with `renamed {from}` and agent origin) / `fs.generation`. CLI `hutan fs ls\|cat\|stat\|write\|find\|search`. **No create-directory, rename, move, copy, delete or trash**: a file can be created (`fs.write` + `create`) and never removed. | `crates/daemon/src/files/`, `crates/protocol/src/fs.rs`, `docs/protocol.md` § Files |
| Agent-aware editing | E-10 ledger of agents' pending hunks per file (`agent_edits.rs`, survives restarts), `fs.changed` origin `agent {session}`, Follow agent. Not shown in the tree. | `workbench/agent_edits.rs`, `docs/editor.md` § E-10 |
| Diagnostics | `lsp.diagnostics` read by the Problems panel (E-12a). Not shown in the tree. | `panels/problems.rs` |

### 1.2 What the panel lacks, against §2

Filter / type-ahead; any write operation; multi-select; Open Editors; sort and dotfile controls (the eye mixes `.gitignore` with nothing else; dotfiles are always shown); folded single-child folders; indent guides; collapse-all and refresh buttons; per-type icons; diagnostics and agent marks; a way to reach the 5 001st entry of a big folder; per-root memory of expanded folders (a new `Tree` is made at every root change); more than one root; behaviour in a narrow window. Auto-reveal, git marks, ignore-awareness, watching and keyboard basics exist.

### 1.3 How a saved layout loses Files (root cause)

Evidence is from the repository; the user's `desktop-state.json` was not read (private), so "the user's layout is a v0.1.0-era layout" is an **[INFERENCE]** from their screenshot, but the match is exact.

| Release | Right dock built-in | Left dock built-in | Files |
|---|---|---|---|
| v0.1.0 (2026-10-19) `preset.rs` | `history, changes, inspector, agents, resources` | `sessions, tasks` | **no `files` panel exists** (E-13a, `9232d05e`, came after) |
| v0.2.0 (2026-10-22 13:21) | `changes, agents, history, inspector, resources` (`inspector` / `resources` in ⋯) | `sessions, tasks, files` | a **left page whose nav row shows only while it is the shown page** (`sidebars.rs` ~188: `files` / `search` rows are hidden unless selected), reachable by ⌘⇧E only |
| v0.3.0 / master (SB-ATTACH `858ffcfa`, 2026-10-22 23:48; not in v0.2.0) | `files, changes, agents, history, inspector, resources` | `sessions, tasks` | first rail icon |

Why the user's rail has no Files icon, in order of likelihood:
1. **The saved layout wins over the preset.** A layout saved by v0.1.0 has `right = [history, changes, inspector, agents, resources]` — its first four entries are the four icons in the screenshot in the same order (a fifth, Resources, may be below the crop) — and `left = [sessions, tasks]`. Nothing ever adds a panel that was introduced after the layout was saved.
2. **`migrate_v7` cannot help it.** `files_to_right` (`model/persist.rs`) moves Files to the right dock only when `left == [sessions, tasks, files]` (a Memory page ignored) *and* the right dock lacks it. For `[sessions, tasks]` it returns `false`; nothing inserts Files. (The v0.2.0 → v0.3.0 path for a layout that *did* have Files in the built-in left dock works.)
3. **A Files page the user keeps in a left dock they arranged stays there with no nav row** (item above), so it is unreachable except by ⌘⇧E. Same exposure for `search`.
4. Recovery exists and is invisible: ⌘⇧E (`open_panel` puts a panel that is nowhere into its `default_dock`), a right-click on the rail's empty room (checklist), *Reset to built-in layout*.

The fix (§4 EX-0): a v9 migration — if `files` is in no dock and no center tab of the live layout, the overview stash and every parked layout, insert it at the front of the right dock, log once, show the one-time note (parity rule); show a left-dock nav row for any panel the user placed there. Test layouts: v0.1.0, v0.2.0 default, v0.2.0 with an arranged left dock, a layout where the user hid Files *after* v9 (must stay hidden).

### 1.4 Measurements made for this spike

Setup: a synthetic git repo of **120 000 files** (100 × 10 folders × 100 `.rs` files, plus one flat folder of 20 000 `.txt`), 50 appends to 10 of them and 200 untracked files (210 changed paths); the **debug** `hutand` / `hutan` of a sibling slice worktree (the daemon reported build `0.3.0 (5ee5be9644f9)`, this branch's HEAD) copied to `~/hutan-scratch/explorer-spike/bin`, run with `HUTAN_HOME=~/hutan-scratch/explorer-spike/home`, stopped afterwards (`pgrep` found none of mine). Numbers are `/usr/bin/time` wall time of one CLI invocation (**includes ~20 ms of CLI start-up**; a debug daemon), one run each unless said; they bound, not describe, the GPUI panel.

| What | Result |
|---|---|
| `fs.list` the root (~100 entries) | 50 ms |
| `fs.list` a 100-file folder | 20–30 ms |
| `fs.list` the flat **20 000-entry** folder | **650 ms, 840 KB, 5 000 entries, `truncated: true`** — the other 15 000 cannot be reached in the tree |
| `fs.find` first query (builds the 120 001-file index) | 0.71 s |
| `fs.find` warm, exact path or `item19999` | 0.13–0.14 s |
| `fs.find "mod5 file09"` (two words) | **0 matches** — the query is a subsequence of the path and a space matches nothing |
| `worktree.diff` (210 changed files; what the panel reads for its git marks) | 0.60 s (twice) |
| `git status --porcelain` in the same repo | 0.25 s |

Conclusions: lazy per-folder listing keeps a 120k-file project cheap until a huge *flat* folder is opened; the 5 000-entry cap needs a way out (the filter, §3.4); the git-marks read is 2.4× the plain `git status` and runs on every `fs.changed` burst and checkout activity end, so it needs a throttle and probably a files-only form (§3.6); multi-word filtering needs a small daemon behaviour fix (§3.4). Not measured: the GPUI panel's `rebuild()` at 100 000 open rows (`Tree::rebuild` clones every `Row`; **[INFERENCE]** O(open rows) per listing) — EX-1 adds the bench.

## 2. How explorers work where users live

Method and limits. Docs read: Zed ([project panel](https://zed.dev/docs/project-panel)), VS Code ([user interface](https://code.visualstudio.com/docs/getstarted/userinterface) § Explorer), JetBrains ([Project tool window](https://www.jetbrains.com/help/idea/project-tool-window.html)), Orca (source at `~/proj/research/stablyai_orca`, HEAD `de2700d` 2026-10-22, read with `git cat-file`; docs page `docs/site/content/docs/editing/file-explorer.mdx`), Superset (docs and changelog; the site now redirects to paradev.com), Cursor (community forum JSON), Conductor and Claude Code desktop (docs / announcement). No repository was cloned in full: the Orca clone already existed (blobless) and Zed / VS Code were read through docs and the GitHub search API (`gh api search/issues … sort=reactions`, 2026-10-27, `gh` rate limit hit once and retried). **Reaction counts are "total (👍)" as the API returned them on that day, directional only.** The user's own Hutan and repositories were not touched.

### 2.1 Zed — project panel (the closest in feel and in stack)

Auto-reveal (`auto_reveal_entries`); **sticky scroll** of ancestor folders; **auto-fold directory chains** (`src/utils/helpers` as one row); multi-select with ⇧↑↓ and *compare marked files* (⌥D); new file / new directory with an inline editor; **F2 rename with the stem pre-selected**; cut / copy / paste (name conflicts get a "copy" suffix and the rename editor opens), duplicate (⌘D); **Trash (⌘⌫) vs permanent delete** behind a confirmation that lists the files and warns about unsaved changes; **drag to move, ⌥ to copy, drag in from Finder**; git tint plus optional letter badges (`M A D U !`), next / previous git entry; **diagnostics on icons propagating to parents**, optional numeric badges; hide gitignored / hidden; sort modes (`directories_first`, `files_first`, `mixed`) and orders (natural by default); reveal in file manager, **new search in directory**, remove folder from project; **undo / redo of file operations** (create, rename, trash, paste, duplicate, drag; a batch is one step; creation undo moves to Trash; history not persisted). ⌘⇧E toggles focus. Docks can be on either side.

### 2.2 VS Code — Explorer (the expectation baseline)

Explorer is a *view container* with views: **Open Editors** (groups, unsaved dot), the folder tree (multi-root: one root per folder), **Outline**, **Timeline** (git + local history). Exclusions (`files.exclude`, `explorer.excludeGitIgnore`), **file nesting**, **find in tree** (⌘F: highlight vs filter, fuzzy), multi-select with *Compare Selected*, drag / drop and copy / paste with the OS, *Open in Integrated Terminal*, *Reveal in Finder*, *Open to the Side*, git decorations with propagated folder colours, indent guides, sticky scroll, compact folders, `explorer.autoReveal`, confirm-delete / confirm-drag settings. Primary Side Bar can sit on either side; views can be dragged to the Secondary Side Bar.

### 2.3 JetBrains — Project tool window

Several *views* of the same project (Project, Packages, **scope views such as Open Files and Changed Files**); **speed search** (type in the tree); preview on Space; *Always Select Opened File*; optional file details (size, modified); sort by name / type / mtime, folders on top; **Compare With… (⌘D)**; many *Copy Path* forms (absolute, from content root, from repository root, GitHub URL, `Copy Reference` with a line); VCS colours with *changed descendants* on folders; *Synchronize* (refresh); local history; *Open in* Finder / Terminal; tool windows move to any side and have view modes (docked, floating, windowed).

### 2.4 Cursor — the same layout question as Hutan's

Classic Cursor is VS Code. The **Agents Window** (Cursor 3, 2026-04) is agent-first: sessions on the left, the file tree in a right panel. Forum evidence: *Add File Explorer to Agents Window* (158374: answer "there's already a file explorer… ⌘G", an **overlay**, limited to the workspace root, position fixed, "I'll log this as a separate feature request"); *Agents Window Customize Layout* (158646, 6 likes: "move the file explorer tree to the left and keep the chats on the right"); *Allow moving File Explorer in Agents Window* (171835, 2026-09); **bug report 164985: "The project directory sidebar has disappeared"** — the tree had moved to the right panel and the user thought it was gone. This is Hutan's situation verbatim.

### 2.5 Orca — the product Hutan's chrome follows

Orca's explorer is in the right sidebar (default tab) and is deep. Source: toolbar with repository name, **collapse all, refresh, show dotfiles, show gitignored** toggles; **name filter** and a query strip; **root select** for folder workspaces (*display root*); a scope notice; **virtualized rows** with display-relative indentation; **natural (numeric-aware) sort** (docs: also for SSH / remote / mobile); **VS Code-equivalent keyboard navigation** (↑ ↓ Home End PgUp PgDn, ← → fold / parent); **multi-select** and multi-drag with a drag image; **drag edge auto-scroll**; external drag from Finder copies in (uploaded first for SSH worktrees); inline new-file / folder rows; rename (Return), delete (with batch classification and *editors affected by deletion*), duplicate; context menu **New File · New Folder · Copy · Copy Path · Copy Relative Path · Duplicate · View File · Download (remote) · Collapse Folder · Find in Folder · Reveal in Finder · Rename · Delete**; watch scheduler and reconcile (`file-explorer-watch-refresh-scheduler.ts`); name-filter projection. Docs: "create, rename, delete, and move all map to filesystem operations, so external changes (say, from the agent) show up instantly"; files coloured by git status. Orca issues below.

### 2.6 Other agent environments

| Product | Explorer | Source |
|---|---|---|
| **Superset** (now paradev.com) | Right sidebar: tree with git decorations; **create / rename / delete / move / copy with context menus; drag to move — "open editors, including ones with unsaved edits, follow the new path"**; hidden-files toggle, refresh, instant file search; Material icon theme; **expanded folders, scroll and selection persist when switching between Changes and Files tabs** (PR #1329); explorer shipped 2026-02-02 (PR #1112). Creating / deleting / moving stay inside the workspace. | [docs](https://docs.superset.sh/editor) (redirects to docs.paradev.com), [changelog](https://superset.sh/changelog/2026-02-02-file-explorer-workspace-improvements) |
| **Conductor** | No explorer in what its docs lead with: the Diff Viewer (⌘⇧D) and a **Changes-tab file tree** (0.62.0 "collapse directories that contain the selected file", folder totals of lines added / removed); open the workspace in your IDE for the rest. | [docs](https://www.conductor.build/docs/reference/diff-viewer), [0.62.0](https://www.conductor.build/changelog/0.62.0-repo-settings-browser-preview) |
| **Claude Code desktop** (2026-04-14) | Announcement lists integrated terminal, in-app file editor, diff viewer, preview, drag-and-drop panes; **it does not mention a file tree**. | [article](https://claude.com/resources/articles/claude-code-desktop-redesign) |
| **Warp** | "Project Explorer" (repository-bound). Top complaints are *stale*: "does not Refresh" (#7900, 26), "only updates after switching tabs" (#8374, 22); "work anywhere, not just in repositories" (#7663, 10); hide it (#7288, 72). | GitHub |
| **vibe-kanban** | #3158 "Add file explorer in workspace" (0 reactions, open). | GitHub |

### 2.7 What users ask for, ranked by reactions (GitHub issues)

| # | Request | Evidence | Hutan today |
|---|---|---|---|
| 1 | Git status in the explorer | VS Code [#178](https://github.com/microsoft/vscode/issues/178) 2887 (2285 👍), closed | **has** (letters, tint, folder dots) |
| 2 | Hide / dim gitignored files | VS Code [#38878](https://github.com/microsoft/vscode/issues/38878) 510 (435), [#5944](https://github.com/microsoft/vscode/issues/5944) 24 | **has** (eye) |
| 3 | Indent guides | VS Code [#17777](https://github.com/microsoft/vscode/issues/17777) 990 (747) | missing |
| 4 | **Type to find / filter in the tree** | VS Code [#10026](https://github.com/microsoft/vscode/issues/10026) 434 (334); Zed [#17547](https://github.com/zed-industries/zed/issues/17547) 86 (83), open | missing |
| 5 | **Explorer out of sync with changes outside the editor** (agents, builds) | Zed [#38109](https://github.com/zed-industries/zed/issues/38109) 178 (210 comments), closed; Zed [#60882](https://github.com/zed-industries/zed/issues/60882) "project panel stale when files are modified by an agent running in a different window" 20; Warp #7900 / #8374 | **has** (`fs.watch` ≤ 64 folders + `fs.generation`); needs a stress test with agent bursts |
| 6 | Auto-reveal the active file, reliably | VS Code [#260478](https://github.com/microsoft/vscode/issues/260478) 114 (95), [#87956](https://github.com/microsoft/vscode/issues/87956) per-folder 443 (397) | **has** (`auto_reveal`) |
| 7 | **Undo file operations** | Zed [#5039](https://github.com/zed-industries/zed/issues/5039) 174 (172), shipped; VS Code [#9390](https://github.com/microsoft/vscode/issues/9390) 25, [#249263](https://github.com/microsoft/vscode/issues/249263) 28 | no operations at all |
| 8 | Copy / paste and drag files with the OS file manager | VS Code [#164](https://github.com/microsoft/vscode/issues/164) 235 (168); Zed [#29026](https://github.com/zed-industries/zed/issues/29026) 108 (107), open; Orca #14530 | missing |
| 9 | **Open Editors** list / vertical tabs; sorting it | VS Code [#12453](https://github.com/microsoft/vscode/issues/12453) 243 (219), [#27286](https://github.com/microsoft/vscode/issues/27286) 225 (192); Zed [#5240](https://github.com/zed-industries/zed/issues/5240) 65 (51) | missing |
| 10 | Errors / warnings in the tree | Zed [#5016](https://github.com/zed-industries/zed/issues/5016) 95 (95), shipped | missing |
| 11 | Move the panel to the other side; swap sidebars | Zed [#5582](https://github.com/zed-industries/zed/issues/5582) 68, shipped; Orca [#10153](https://github.com/stablyai/orca/issues/10153) 8, [#1355](https://github.com/stablyai/orca/issues/1355) 4; Cursor forum 171835 / 158646 | **has** (*Move to ▸*) — undiscoverable |
| 12 | **Auto-hide the side bar** (more room for code) | VS Code [#3742](https://github.com/microsoft/vscode/issues/3742) 696 (597), open | partial (auto-collapse of the right body below 1180 px) → rail peek |
| 13 | Multi-root / multi-repo workspaces | Orca [#1099](https://github.com/stablyai/orca/issues/1099) 81 (49), [#21118](https://github.com/stablyai/orca/issues/21118) 23, [#8377](https://github.com/stablyai/orca/issues/8377) 20; Zed [#59221](https://github.com/zed-industries/zed/issues/59221) 129 (92) (git panel) | one root (active checkout) |
| 14 | Collapse folders: discoverability, fold chains, sticky scroll | Zed [#6880](https://github.com/zed-industries/zed/issues/6880) 68, [#7243](https://github.com/zed-industries/zed/issues/7243) 26; VS Code [#194903](https://github.com/microsoft/vscode/issues/194903) 24 | missing |
| 15 | Horizontal scroll / long nested names | Zed [#7001](https://github.com/zed-industries/zed/issues/7001) 59 (52), [#26097](https://github.com/zed-industries/zed/issues/26097) 31 "files in nested directories are hard to distinguish" | names ellipsised at the end only |
| 16 | Sort: natural order, folders first / mixed | VS Code [#149951](https://github.com/microsoft/vscode/issues/149951) 37, [#5222](https://github.com/microsoft/vscode/issues/5222) 27; Orca docs (natural) | case-insensitive, not natural |
| 17 | File icon themes | Orca [#8975](https://github.com/stablyai/orca/issues/8975) 5, [#22401](https://github.com/stablyai/orca/issues/22401) 4; Superset ships Material icons | one icon (`item.file`) |
| 18 | Keyboard focus bugs: `ctrl-w` closes the *panel* instead of the editor tab | Zed [#34865](https://github.com/zed-industries/zed/issues/34865) 48 (44) | check in EX-1: ⌘W with the tree focused closes the tab |
| 19 | Find in folder, "Add to .gitignore" | Orca docs (find in folder); Orca #9060 (prior spike) | missing |

### 2.8 Takeaways that drive the design

1. **Everyone puts the tree one click away at all times and lets you move it.** Where an agent-first app moved it (Cursor Agents Window, Orca, Hutan) the failure mode is *"where did my explorer go"*, never "it is on the wrong side". → reachability, then a one-step move.
2. **The tree alone is not the product.** VS Code wraps it in Open Editors / Outline / Timeline, JetBrains in scope views, Superset keeps its state between Changes and Files. Hutan's equivalents are Open Editors, **Changed by agents**, and state kept per root.
3. **Agents change files while you look.** Staleness is the loudest explorer complaint in agent-adjacent tools (Zed #38109 / #60882, Warp). Hutan's watching is already ahead; the work is to prove it under bursts and to *show* what an agent touched.
4. **Operations must keep editors honest**: Superset's "open editors … follow the new path", Zed's unsaved-changes warning on delete, Orca's *editors affected by deletion*. Hutan's `BufferStore` is keyed by canonical path and `fs.changed` already carries `renamed {from}`.
5. **Type-to-filter is the most-requested missing interaction** (VS Code 334 👍, Zed 83 👍); quick open does not replace it (it opens, the tree *shows where*).
6. **Cheap polish with outsized votes:** indent guides (747 👍), folded folders, natural sort, collapse-all, refresh.

## 3. Design for Hutan

Principles (from `AGENTS.md`, ADR 0001 / 0010 / 0012): the daemon owns every disk effect and permission; the explorer is a registered panel with its own settings namespace (`panels.files.*`); one panel, several sections (not several panels); every behaviour also reachable by command / key; no destructive default; nothing blocks input (the tree never waits on the daemon in a frame).

### 3.1 Where it lives

| Question | Decision | Why / rejected |
|---|---|---|
| Left or right | **Right rail, first icon** (SB-ATTACH stands). The left sidebar answers *where work is* (workspace › project › worktree › session); the right dock answers *what is in it* (Files, Changes, Agents, History). The explorer's root *is* the left selection, which the scope chip already says. | Left-by-default would push sessions down or replace the Projects body (Orca swaps its left body; Hutan's nav pages do too) — a tree and a session list are both wanted at once. Users who want it left: *Move to ▸ Left* (one step, undoable) or Zed-style dock choice. |
| Always on vs toggle | The panel is always *present* on the rail; the body is a toggle (rail icon, **⌘⇧E**, ⌘L). ⌘⇧E becomes **toggle focus**: closed → open and focus the tree; tree focused → focus returns to the last center item (the body stays). Esc in the tree does the same. | Zed's `toggle focus`; avoids Zed #34865-style focus theft. |
| Beside Changes | **Same dock, one body at a time**, Files first then Changes; Changes shows a **count badge** on its rail icon when the checkout has uncommitted changes, so review state stays visible while Files is open. *Show in Files* on Changes rows and the diff header, and Files following the checkout Changes shows, are FX-CHANGES-VIEW item 4. State (expanded, scroll, selection) is kept when switching panels and roots (Superset #1329). | Stacked panels in one dock: the model has one active panel per dock (`Dock.active`), VS Code and JetBrains have none either, the only request is Orca #29611 (0 reactions). Rejected. |
| First thing the body shows | **Open question Q1.** Today `dock.active = changes` in every preset. A fresh project has no changes and the Changes body is empty; Orca's default is Explorer. Proposal: fresh layouts open on **Files**; an existing layout keeps its last panel. | One line in `preset.rs`. |
| Default for existing users | EX-0's migration gives them the icon; nothing else is changed under them. | Parity rule. |
| As a center tab | Already possible (*Move to ▸ Center tab*); not a goal. | |
| Narrow windows | Body opens as a **peek** over the center below the width where the center would be < 480 px (§3.7). | |

### 3.2 What it shows

One panel, three collapsible **sections** (state kept per root; a section with no rows shows nothing; headers 22 px):

| Section | Content | Data (all exist today) |
|---|---|---|
| **Open Editors** | File tabs of this window whose file is below the root: name, muted folder, ● unsaved, ⌘-click closes. Click focuses the tab, middle-click closes. Default **auto**: shown when ≥ 2 file tabs are open and the panel is tall enough (Q4). Order: tab order (VS Code's sort requests, #12453, are the reason for a later `sort` option). | `Workbench` center tabs, `BufferStore` dirty state |
| **Changed by agents** | Files with pending agent hunks (E-10 ledger) in this root, newest first, capped at 50 ("N more"): `◆ name  Codex · 3 hunks`; click opens the file at its first hunk. Hidden while empty. | `agent_edits.rs` ledger (survives restarts), session names from the sessions store |
| **Tree** | The root(s): rows below. | `fs.list`, `fs.watch`, `worktree.diff` |

Row anatomy (name first, never middle-ellipsis the name; tooltip = full path and, for a changed file, the git state): `chevron · icon · name · [folder, in Open Editors / filter results only] · … · diagnostics count · agent ◆ · git letter`.

| Decoration | Source | Notes |
|---|---|---|
| Git letter + tint, folder dot | `worktree.diff` file list (exists) | Add conflict `!` if the diff reports conflicts **[INFERENCE: check `DiffStatus`]**; folder dot = *changed descendants* (JetBrains' term). |
| Ignored → dimmed or hidden | `fs.list` `ignored` (exists) | Separate **dotfiles** toggle (`show_hidden`) from **ignored** (`show_ignored`); both in the header ⋯, not three eyes. |
| **Diagnostics** | `lsp.diagnostics {min_severity}` read like the Problems panel (one read at a time, after `lsp.state` / `lsp.diagnostics` notices) | Error = red name + count, warning = amber; **propagates to ancestor folders** (Zed); `panels.files.show_diagnostics = errors_warnings \| errors \| off`. No protocol change. |
| **Agent-touched** | E-10 ledger; recent `fs.changed` with `origin agent` | `◆` in the accent colour; tooltip *"Codex changed this · 3 hunks · 2 min ago"*; folder dot rolls up. Both a mark and the section. A checkout whose session is *working* shows a muted "Codex working" dot on the scope chip. |
| Open / unsaved | tabs | Open: tinted dot; unsaved ●. |
| Symlink | `FsKind::Symlink` | italic (exists); a link out of the roots is not followed (daemon). |
| Details | `FsEntry.size`, `mtime_ns` | optional `panels.files.details = none \| size \| modified` (JetBrains). |
| File-type icons | icon pack ids `item.file.<kind>` | needs pack work (`docs/themes.md`); a small separate item, not in the critical path. |

**Roots, workspaces, worktrees.** Today one root: the active checkout. Design: the scope chip becomes a **root switcher** (menu: this project's checkouts — main and each worktree, with their placement badges and *working* dots — then the workspace's other projects; selecting one changes the *panel's* root without changing the window's active container, and a *Follow active container* item returns). A second step (EX-8) adds `panels.files.roots = active \| workspace`: in *workspace* mode each project of the active workspace is a collapsible root row (VS Code multi-root, Orca #21118, mirroring *All in workspace* of the Changes panel, CHANGES-MULTI). Worktrees stay under their project's chip menu, not as tree nodes. Per-root state cache (8 roots, LRU): expanded set, scroll, selection, filter; the expanded set is persisted per root in `desktop-state.json` (≤ 200 folders per root).

### 3.3 Mock at 1440 × 900

To scale; numbers refer to the callouts in [`2026-10-27-file-explorer-1440x900.svg`](2026-10-27-file-explorer-1440x900.svg): titlebar 36, status bar 24, left sidebar 280, **center 770**, right body **350** + rail **40**.

```
┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ ● ● ●                                  hutan — Hutan                                                         │ 36
├────────────────────────┬─────────────────────────────────────────────────────────┬───────────────────────┬──┤
│ ▢ Agents               │ Codex · refactor…  │ files.rs ● │ Terminal              │ Files ⑦[hutan·main ▾]⟳⇤◐⋯│▤ │
│ ▢ Mawas   ▢ Memory     │ crates › desktop › … › panels › files.rs               │ ② ⌕ Filter files…   ⌘F │± ③│⑥
│ ▢ Tasks   ▢ Search     │ 41  // Files panel: the folder as a tree …             │ ① ▾ OPEN EDITORS 3     │◍ │
│ ─────────────────────  │ 42▌ let rows: Vec<(usize, Row)> = range                │   ● files.rs   M       │↺ │
│ PROJECTS               │ 43▌     .filter_map(|i| …)                             │   quick_open.rs        │⋯ │
│ ▾ hutan   workspace    │ 46▌ rows.into_iter().map(|(index, row)| {              │   editor.md            │  │
│  ▾ hutan  main     ●   │ …                                                      │ ③ ▾ CHANGED BY AGENTS 2│  │
│     ◆ Codex · refactor │ ────────────────────────────────────────────────────── │   ◆ files.rs  Codex·3h │  │
│     ▪ zsh · cargo watch│ Terminal · zsh                                         │   ◆ files_git.rs       │  │
│  ▾ hutan  wt fx-chang… │ $ cargo nextest run -p hutan-desktop …                 │ ▾ HUTAN  main          │  │
│     ◆ OMP · changes …  │ PASS [0.412s] files_gpui::reveal_active_file           │ ④ ▾ crates/…/panels    │  │
│  ▸ book   main         │                                                        │      common.rs         │  │
│ ▸ ripgrep              │                                                        │ ⑤    files.rs  ◆ M     │  │
│                        │                                                        │      files_git.rs   M  │  │
│  Left: where work is   │                                                        │      problems.rs    2  │  │
│                        │                                                        │    ▸ crates/protocol   │  │
│                        │                                                        │    ▸ docs  ▸ target(dim)│  │
├────────────────────────┴─────────────────────────────────────────────────────────┴───────────────────────┴──┤
│ ● hutand 0.3.0   hutan · main                                                    Ln 42, Col 7   UTF-8 · LF   │ 24
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

① Open Editors (file tabs, ● unsaved). ② Filter field (type-to-filter, `fs.find`). ③ Changed by agents (E-10 ledger) and the **count badge** on Changes' rail icon. ④ A folded single-child chain (`crates/desktop/src/workbench/panels`, middle-ellipsis on the *folder*, never on a file name). ⑤ Decorations on one row: agent ◆, git `M`, the selected row = the active tab's file; `problems.rs` carries its diagnostics count. ⑥ Rail: Files first, selected. ⑦ Scope chip = root switcher. With rows ≈ 24 px the tree shows about 30 rows at this height **[INFERENCE from `sidebar_row`; EX-1 measures]**.

### 3.4 Behaviour

**Keyboard** (the tree is focused with ⌘⇧E; all keys also exist as commands in ⌘K, `FilesPanel` key context):

| Keys | Action | Today |
|---|---|---|
| ↑ ↓ · Home End · PgUp PgDn | move | ↑ ↓ only |
| → · ← | open / step in · close / parent | yes |
| ⌘→ · ⌘← | expand / collapse all (expand-all refuses beyond 2 000 rows or 10 unlisted folders and says so) | no |
| Return · Space | open (keyboard follows) · show (keyboard stays) | yes |
| ⌥Return / ⌘Return | open to the side (split right), as quick open | no |
| ⇧↑ ⇧↓ · ⌘-click · ⇧-click | multi-select | no |
| *printable key* · ⌘F | **filter** (below); Esc clears, then returns focus to the editor | no |
| F2 | rename, name stem pre-selected (Zed) | — |
| ⌘⌫ | move to Trash (confirm) | — |
| ⌘D · ⌘C ⌘X ⌘V | duplicate · copy / cut / paste | — |
| ⌘N · ⌥⌘N | new file · new folder (only while the tree is focused; checked against `docs/design.md` § Keyboard defaults in EX-7) | — |
| ⌘⌥C · ⌥⇧⌘C | copy path · copy relative path (JetBrains / Orca: ⌘⌥⇧C) | menu only, path only |
| ⌘Z ⇧⌘Z | undo / redo file operations of this window (session only, a batch is one step) | — |
| ⌘W | closes the active *tab*, never the panel (Zed #34865) | verify |

**Filter as you type.** A field above the tree (also opened by typing a printable character in the focused tree or ⌘F). Two modes like VS Code (`panels.files.filter_mode = filter \| highlight`, default *filter*). Matching: `fs.find` with `root` = the explorer's root (or a folder: *Filter in this folder* from the menu and from the "More entries" row, which is how the 15 000 unreachable entries of §1.4 are reached), debounce 60 ms, the previous request dropped (exactly quick open's pattern), ≤ 200 matches, fuzzy, matched characters in the accent colour. Results are shown as the **tree projection**: matches and their ancestors, ancestors auto-expanded, no `fs.list` per folder (ancestors are synthesised from the paths; Orca's `name-filter-projection`). Instant feedback while the daemon builds its index (0.7 s first time at 120k files): match the already-listed rows locally, then replace. ↓ moves into the results; Return opens; Esc clears and the tree comes back exactly as it was (expansion untouched). Two small daemon-side facts to fix in the same slice, wire unchanged: **multi-word queries** (`mod5 file09` finds nothing today) split on whitespace and require every word (⌘P gains it too); ignored files are not in the index (`show_ignored` on → the filter says "Ignored files are not searched; use Search"). *Find in Folder* in the menu opens the Search panel with the include glob `dir/**`.

**Opening.** Click shows (preview semantics as today), double-click / Return opens, ⌥Return opens to the side, middle-click opens permanently (Zed). Dropping a row on a pane opens it there (pane drop targets exist for tabs; **[INFERENCE]** reusable).

**Operations** (EX-7 over EX-6's daemon calls; every one also a command):

| Operation | Daemon | Confirm | Undo | Open tabs |
|---|---|---|---|---|
| New file / folder | `fs.create` | no (inline name editor, validates name, never overwrites) | trash it | file opens |
| Rename (F2) / move by drag (⌥ = copy) | `fs.rename` / `fs.move` (batch) | drag: first time per session, `panels.files.confirm_drag` | rename back | tabs, buffers (incl. unsaved) and watches follow; LSP document is closed and reopened under the new path |
| Duplicate / paste copy | `fs.copy` (name conflict → ` copy`, ` copy 2`; a single result opens the rename editor, Zed) | no | trash the copy | — |
| Delete (⌘⌫) | `fs.trash` → **system Trash**, ticket | **always** a sheet listing ≤ 5 names and "N more", plus "unsaved changes in N tabs" and **"Codex is working in this checkout"** when a session is | `fs.restore {ticket}` (session only) | tabs show the *deleted* bar (exists); dirty buffers keep their text |
| Paste from Finder / drag from Finder | `fs.copy` with a picked source (EX-9) | no | trash the copies | — |
| Copy path, relative path, reveal in Finder | client only | — | — | Reveal hidden for a remote daemon |
| Open in Terminal | `session.new` with cwd = folder (a file → its parent), same checkout | — | — | — |
| Attach to chat | the composer's `@file path` chip (existing mention path); with no chat focused, copies `@path` | — | — | — |
| Compare | *Select for Compare* + *Compare with Selected* (two files, client diff as in the history tab); *Compare with HEAD / base* from `git.file` | — | — | read-only diff tab |

No permanent delete in v1: **Trash only**. Where the daemon has no trash (a Linux host without one), delete is refused with the reason; a typed-name confirmation can come later.

**Agents and the user at once.** Operations are plain filesystem effects; git sees delete + add. A move while an agent runs in the checkout is allowed but warned (above). Nothing is added to checkpoints (ADR 0019 captures the checkout's files per turn; a user rename between turns is just a change).

**State.** Per root: expanded set, scroll, selection, filter, section collapse. Kept across panel switches and root changes (Superset #1329); expanded set persisted.

### 3.5 Daemon ownership and protocol

Everything that touches the disk goes through `hutand` (ADR 0001; ADR 0012 amendment of 2026-10-21, decision 3). Today the file service has no operation that removes, moves or creates a folder, so the explorer's write side is a **new protocol family**. It needs an **ADR 0012 amendment ("file operations", 2026-10-27)** as EX-6's first commit (hard-to-reverse, `AGENTS.md`).

| Method | Params | Result | Notes |
|---|---|---|---|
| `fs.create` | `{path, kind: file\|dir, parents?}` | `FsEntry` | file: exclusive create (`conflict reason exists`); dir: `mkdir`, `parents` only when asked |
| `fs.rename` | `{from, to}` | `FsEntry` | same root; never overwrites (`renamex_np`-style exclusive rename on macOS **[INFERENCE: verify in EX-6]**); a case-only rename on APFS works |
| `fs.move` | `{items: [{from, to}]}` | per-item `{entry}` or `{error}` | batch for drag; best effort, each item reported |
| `fs.copy` | `{items: [{from, to, picked_from?}]}` | per-item result | recursive, `clonefile` where possible; bounded per call (≤ 20 000 entries / 2 GiB, `unsupported reason too_large`) **[bounds to be settled in EX-6]**; `picked_from` lets a user-chosen source outside the roots in (EX-9; local daemon only, a remote one gets the bytes through `fs.write`) |
| `fs.trash` | `{paths}` | `{ticket, items}` | macOS: `NSFileManager trashItem` (the `trash` crate wraps it; **licence / cargo-deny check is part of EX-6**); no trash → `unsupported reason no_trash` |
| `fs.restore` | `{ticket}` | per-item | the daemon keeps ≤ 200 tickets in memory for 24 h (not persisted, like Zed's undo); an occupied original path → `conflict` |

- **Confinement** exactly as `fs.write`: every path below a tracked root, canonical, opened from the root's descriptor with `O_NOFOLLOW` per component; a link that leaves the roots is `forbidden symlink`; **never below `.git`** (source or target); never the root itself; source and target in the same root in v1 (cross-root = copy, then trash).
- **Permissions** (`docs/control.md`): all six are **Mutate**. The desktop connection is the user and is allowed (the confirmation sheets are UI); an agent connection under `agents = ask` raises a `control` approval ("Agent in session “api” asks Hutan to move …"), under `read_only` it is `forbidden`. **No MCP tool** for them (agents have their own file tools; a second write path through approvals would only add surface) — Q3.
- **Events.** The existing `fs.changed` (`created`, `removed`, `renamed {from}`) already feeds tabs and the tree; the operation's `write_id` makes the originating connection's own events `origin: self`, so the panel does not flash and undo does not echo.
- **CLI parity** (the project's rule): `hutan fs mkdir [-p]`, `touch`, `mv`, `cp`, `trash`, `restore`, with `--json`.
- **Version:** additive minor bump, capability `fs.ops` (EX-6 takes the next free number at merge; **0.54** today, current is 0.53); golden fixtures and the old-client test (ADR 0002).
- **Listing beyond 5 000.** No paging: the filter (scoped `fs.find`) is the way out, and the "More entries" row says so. Paging would repeat the 100 000-entry directory read for every page (measured 650 ms for one page).
- **Cheaper git marks.** EX-10, only after the bench: a files-only form of `worktree.diff` (additive flag) or a `worktree.status`, if the 2.4× over `git status` matters in the GPUI measurement. Until then the panel throttles the read.

### 3.6 Performance

Proposed budgets (new, to be added to `docs/perf.md` by EX-1 and enforced by `just bench`; `docs/vision.md` budgets are the parents):

| Budget | Target | Basis |
|---|---|---|
| Panel shown → first rows (warm daemon, cached root) | ≤ 100 ms | `fs.list` 20–50 ms measured |
| Expand a 1 000-entry folder → frame | ≤ 50 ms plus the daemon's listing | listing measured 20–30 ms / 100 entries |
| `Tree::rebuild` with **100 000 open rows** | ≤ 16 ms, goal ≤ 2 ms by splicing only the listed folder's rows | **[INFERENCE: today O(open rows) per listing; unmeasured]** |
| Key → selection change drawn | ≤ 8 ms | ADR 0011 |
| Filter key → results drawn (warm index) | ≤ 100 ms | 130 ms CLI wall incl. 20 ms start-up, debug |
| Burst of 1 000 `fs.changed` (an agent) → at most 4 re-lists / s / folder, no dropped frame | | trailing debounce per folder **[to be added]** |
| Git marks re-read | ≤ 1 per 2 s, never overlapping | `worktree.diff` 0.6 s at 120k files |
| Rows retained at 100 000 open | ≤ 30 MB **[INFERENCE]** | `Row` holds a `PathBuf` and a `SharedString` |

What already scales: per-folder lazy listing (a 120k-file project costs nothing until a folder opens), `uniform_list`, daemon-side watching (one FSEvents watcher per root, `.git` / `target` / `node_modules` filtered, 50 ms batches, `fs.generation` once a second). Hazards found and their plan: the flat 20 000-entry folder (filter + honest row); expand-all on a huge tree (capped); the 64-folder watch cap (re-list on `fs.generation`; revisit with one recursive watch on the root and client-side filtering if the bench shows the cap bites); the git marks read (throttle, then files-only); the first `fs.find` index build (shown as "Indexing…").

### 3.7 Window sizes

Thresholds are the existing ones (`docs/design.md` § Docks (DK-1), *Responsive collapse*): right body → rail below **1180 px**, left → compact below **900 px**, the center keeps ≥ **480 px**; body 350 (240–480), rail 40, titlebar 36, status bar 24.

| Window | Left | Right | Center | Explorer |
|---|---|---|---|---|
| 1440 × 900 | 280 | body 350 + rail 40 | 770 | beside the editor (mock) |
| 1280 × 800 | 280 | body 350 + rail 40 | 610 | beside the editor |
| 1180 × 760 | 280 | body 350 + rail 40 | 510 | beside the editor |
| 1100 × 720 | 280 | **rail only** | 780 | rail click → **peek** |
| 900 × 640 | 280 (compact 44 below 900) | rail only | 580 | peek |
| 640 × 600 | compact rail 44 | rail 40 | 556 | peek, at most 300 wide, full height |

**Rail peek** (EX-2, `layout.rail_peek`, default on, desktop only, no protocol): when a rail click would open a body that leaves the center under 480 px, the body opens as a **floating layer over the center's right edge** at its normal width instead of pushing (VS Code's "automatically hide the side bar", #3742 696 👍; Cursor's ⌘G overlay). It closes on opening a file, Esc, a click outside or the same rail icon; it never shifts the center, takes the keyboard while open and hands it back after. What happened before the peek, **measured 2026-10-27 (EX-2, Cua on a debug build with a scratch home; `layout.rail_peek = false` is that behaviour)**: the rail click was not refused — the body was pushed in and stayed open (`design.md`'s "a dock the user opens in a narrow window stays open"), and the center gave up the room: at 1100 px the left sidebar then went to its 44 px rail by itself (a 280 px sidebar and the body would have left the center 430 px), the center was 666 px and the Projects list was out of sight; at 900 px the same, with the center at about 466 px, under the 480 px floor. Neither width lost the Files icon, but neither kept both docks. Screenshots, dark and light, before and after: `~/hutan-scratch/ex2/shots/`.

**Fit rules for the panel:** the tree is the only scroller and takes all the height (sections have a cap of 6 rows then "N more", and Open Editors collapses itself below a 360 px tree viewport); long folders fold into one row (`panels.files.compact_folders`, default on) with the middle ellipsis on the folder part and the file name always whole; indentation is 12 px per level to depth 6, then 6 px, so depth never takes more than ~100 px of the body; indent guides on hover (`panels.files.indent_guides = hover \| always \| off`, VS Code #17777); the filter and the header never wrap (the header folds its buttons into ⋯ below 300 px). No horizontal scrolling in the tree (Zed #7001 is answered by folding and the tooltip). Heights: at 720 px or less the sections collapse first and the tree keeps at least 12 rows.

```
 narrow (peek):  ┌────┬──────────────────────────────┐        ┌────────────────────────────┬──┐
                 │ 44 │ center (≥ 480)                │  →     │ center  ┌───────────────┐ │▤ │
                 │    │                               │ rail   │         │ Files (peek)  │ │± │
                 │    │                               │ click  │         │ ⌕ filter     │ │  │
                 └────┴──────────────────────────────┘        │         │ …tree…        │ │  │
                                                               └─────────┴───────────────┴─┴──┘
```

### 3.8 Settings (`panels.files.*`) and commands

| Key | Default | Note |
|---|---|---|
| `show_ignored` | false | exists |
| `auto_reveal` | true | exists |
| `show_hidden` | true | dotfiles; separate from ignored |
| `compact_folders` | true | |
| `indent_guides` | `hover` | `always` / `off` |
| `sort` | `natural` | client side; `directories_first` / `files_first` / `mixed` as `sort_mode` |
| `show_diagnostics` | `errors_warnings` | `errors` / `off` |
| `show_agent_changes` | true | the ◆ marks and the section |
| `open_editors` | `auto` | `show` / `hide` (Q4) |
| `filter_mode` | `filter` | `highlight` |
| `confirm_delete` | true | never turns Trash into permanent delete |
| `confirm_drag` | true | |
| `details` | `none` | `size` / `modified` |
| `layout.rail_peek` | true | outside the namespace (layout) |

Commands (⌘K): *Files: Toggle Focus*, *Files: Filter*, *Files: Collapse All*, *Files: Refresh*, *Files: New File / New Folder / Rename / Duplicate / Move to Trash / Copy / Cut / Paste / Undo*, *Files: Copy Relative Path*, *Files: Reveal in Finder*, *Files: Open in Terminal*, *Files: Attach to Chat*, *Files: Select for Compare / Compare with Selected*, *Files: Find in Folder*, *Files: Choose Root…*. Each also in the row menu and, for the panel's own state, in `hutan ui` actions (the control surface lists registered actions automatically, `docs/control.md`).

### 3.9 Tests the slices owe

GPUI `TestAppContext` for view state (keys, filter projection, folding, sections, peek geometry, per-root cache, migration of the four saved layouts); daemon integration tests over a real Unix socket for every `fs.*` operation (confinement, `..`, link escape, `.git`, exists, case-only rename, cross-root, ticket round-trip, batch partial failure, `fs.changed` with `origin self`, agent approval under `ask` / `read_only`); desktop tests against a real daemon for rename-follows-tabs and delete-with-unsaved; golden protocol fixtures; CLI tests; a `just bench` entry on a generated 100k-file tree (the generator of §1.4 becomes a bench fixture); Cua screenshots dark + light at 1440 × 900, 1180 × 760 and 900 × 640 (`docs/process.md` § UI checks; scratch `HUTAN_HOME`, never the user's app). Tests wait on state, never on sleeps.

## 4. Slice plan

Small slices, commit-order items inside each, protocol impact stated. All desktop slices keep the rule: nothing edits `crates/core` or `crates/protocol` except EX-6 (and EX-10 if it happens), whose brief names the contract change. Sizes: S ≈ a day, M ≈ 2–3, L ≈ a week.

| # | Slice | Crates | Protocol | Size | Depends on |
|---|---|---|---|---|---|
| **EX-0** | **Files reachable** — *in flight as FX-CHANGES-VIEW item 4* (message sent to `ChangesView` with the root cause of §1.3) | desktop | none | S | — |
| **EX-1** | **Tree polish + bench** | desktop | none | M | EX-0 |
| **EX-2** | **Narrow windows: rail peek** | desktop | none | M | — |
| **EX-3** | **Filter as you type** | desktop, daemon (`fs.find` words) | none (behaviour of an existing query) | M | EX-1 |
| **EX-4** | **Row menu, multi-select, terminal and chat hooks** | desktop | none | M | EX-1 |
| **EX-5** | **Decorations and sections** | desktop | none | M | EX-1 |
| **EX-6** | **Daemon file operations** | protocol, daemon, client, cli | **additive minor** (0.54 at time of writing), capability `fs.ops`, goldens | L | ADR amendment |
| **EX-7** | **Explorer file operations** | desktop | none | L | EX-1, EX-6 |
| **EX-8** | **Roots and worktrees** | desktop | none | M | EX-1 |
| **EX-9** | **Finder import** | desktop (+ the `picked_from` field already in EX-6) | none | S | EX-7 |
| **EX-10** | **Perf follow-ups from the bench** | desktop, maybe protocol | maybe additive (`worktree.status` / files-only flag) | S–M | EX-1 |

**EX-0 — Files reachable (FX-CHANGES-VIEW item 4, not mine to implement).** Commit items: (1) v9 layout migration: `files` in no dock and no center tab → front of the right dock, for the live layout, the overview stash and every parked layout; logged once; one-time note; (2) a nav row in the left sidebar for any panel the user placed in the left dock (the `files | search` rows are shown only while selected today); (3) Changes rows / diff header *Show in Files*, Files follows the checkout Changes shows (already in the brief). Test layouts: v0.1.0, v0.2.0 default, v0.2.0 arranged, hidden-after-v9. Docs: `design.md` Layout migration, `editor.md` E-13a.

**EX-1 — Tree polish + bench.** (1) `just bench` entry B-EX: generated 100k-file tree, `Tree::rebuild`, scroll, expand; budgets into `docs/perf.md`; (2) incremental splice in `Tree` instead of full rebuild if the bench says so; (3) per-root state cache (8 roots) + persisted expanded set; (4) natural sort, dotfiles toggle, collapse-all and refresh buttons, header ⋯; (5) folded single-child folders, indent cap, indent guides (hover); (6) keys: Home / End / PgUp / PgDn, ⌘← / ⌘→, Esc and ⌘⇧E toggle focus, ⌘W with the tree focused closes the tab; (7) the "More entries" row says "Showing the first 5 000 — filter in this folder" (action wired in EX-3); (8) trailing debounce per folder on `fs.changed` + the git read throttle; (9) per-type file icons (icon pack ids) if the pack work is small, else its own item. Tests: GPUI for every item, a burst test (1 000 `fs.changed`). Docs: `editor.md` E-13a, `design.md` Files panel, `perf.md`.

**EX-2 — Rail peek.** (1) reproduce today's behaviour at 1100 and 900 px with Cua and record it; (2) the decision function (window width, dock widths → peek or push) with tests over the §3.7 table; (3) the peek layer (floating body over the center, Esc / outside / open-file / same-icon close, keyboard in and back); (4) `layout.rail_peek` setting and Settings → Layout row; (5) docs `design.md` § Docks (DK-1), *Responsive collapse*. Applies to every right-dock panel, not only Files.

**EX-3 — Filter as you type.** (1) daemon: `fs.find` splits on whitespace, all words must match (`crates/daemon/src/files/find.rs`, test + `protocol.md`), unchanged wire; (2) filter field and local instant matching; (3) `fs.find` scoped to the root or a folder, debounce, drop stale, ≤ 200; (4) tree projection with synthesised ancestors, highlight mode, ↓ into results, Esc restores; (5) *Filter in this folder* / *Find in Folder* (Search include glob). Tests: projection unit tests, GPUI typing test with a fake slow daemon (key → frame time counted), real-daemon test at 100k.

**EX-4 — Row menu, multi-select, hooks.** (1) multi-select model (⇧ / ⌘ click, ⇧↑↓), with the read-only actions applying to all; (2) menu: Open, Open to the Side, Reveal in Finder (local only), Copy Path, Copy Relative Path, Open in Terminal, Attach to Chat, Find in Folder, Show File History, Collapse Folder; (3) Select for Compare / Compare with Selected / Compare with HEAD (two-file diff tab built on the history tab's diff rows); (4) drag a row onto the chat composer or a terminal → the relative path (drop targets). No daemon change. Tests: GPUI with real key and drag events.

**EX-5 — Decorations and sections.** (1) diagnostics marks + folder propagation (`lsp.diagnostics` read shared with Problems); (2) agent ◆ marks and **Changed by agents** from the E-10 ledger; (3) Open Editors section; (4) section state, `open_editors = auto` rule; (5) the Changes rail icon's count badge; (6) *Working* dot on the scope chip. Tests with the fake LSP (`hutan-fake-lsp`) and a scripted agent write.

**EX-6 — Daemon file operations** (contract owner: protocol + daemon + client + CLI, one agent). (1) **ADR 0012 amendment** (file operations: why the daemon, Trash only, confinement, permissions, undo tickets); (2) protocol types, capability `fs.ops`, `docs/protocol.md` § Files and `docs/control.md` classification, goldens; (3) daemon: create, rename, move, copy (`clonefile`), with the `fs.write` confinement and `.git` refusal; (4) trash / restore with tickets (dependency decision recorded: `trash` crate vs `objc2-foundation`; `cargo deny`); (5) `fs.changed` origin `self` for operations; (6) client `Connection::fs_*`, CLI `hutan fs mkdir|touch|mv|cp|trash|restore`; (7) tests listed in §3.9. Parallel with EX-1…EX-5.

**EX-7 — Explorer file operations.** (1) inline name editor rows (new file / folder / rename, stem selected, validation); (2) Trash with the confirm sheet (names, unsaved tabs, "agent working here"); (3) duplicate, copy / cut / paste with the internal clipboard and the pasteboard's file URLs; (4) drag to move (⌥ copy, spring-loaded folders, edge auto-scroll, `confirm_drag`); (5) **tabs follow**: `BufferStore` re-key on `renamed`, unsaved buffers keep their text, LSP document reopened, watches moved; (6) the session-only undo journal (batch = one step; Trash undo = `fs.restore`); (7) commands, keys, docs. Tests: real daemon — rename with an open dirty tab, delete with unsaved text, undo of a batch, a conflicting name, an agent write racing a rename.

**EX-8 — Roots and worktrees.** (1) the scope chip's root switcher (checkouts of the project, other projects of the workspace, *Follow active container*); (2) `panels.files.roots = active | workspace` stacked root rows; (3) per-root caches reused from EX-1. No protocol: the sessions store already lists checkouts.

**EX-9 — Finder import.** Drop files from Finder into the tree and ⌘V of Finder files copy them in (`fs.copy` with `picked_from`); a remote daemon is refused with the reason until the upload path exists (`docs/remote.md`).

**EX-10 — Perf follow-ups.** Only what the EX-1 bench and the Cua timings justify: files-only git marks (`worktree.diff` flag or `worktree.status`, additive), one recursive watch on the root, a lower per-row memory footprint.

Order and parallelism: EX-0 (running) → EX-1 → {EX-2, EX-3, EX-4, EX-5 in any order; EX-6 starts now in its own worktree} → EX-7 (needs EX-6) → EX-8 → EX-9 → EX-10. Docs per slice: `docs/editor.md` § E-13a (or a new § Files explorer), `docs/design.md` § Files panel, `docs/protocol.md` (EX-6), `docs/control.md` (EX-6), `docs/perf.md` (EX-1); `CHANGELOG.md` is the integrator's.

## 5. Risks, non-goals, questions

**Risks.** Data loss is the one that matters: move / trash racing an agent's write or an unsaved buffer (mitigations: Trash only, confirm sheet with unsaved-tab and working-agent warnings, rename-follows-tabs tests, undo tickets); symlink and `.git` escapes (the `fs.write` confinement reused, tested); case-insensitive APFS renames; a 100k-entry `rebuild()` (bench first); `trash` dependency and licence; a remote daemon without Trash; key clashes (⌘N, ⌘D, ⌘⌫ inside `FilesPanel` only) — EX-7 checks the keymap; the Open Editors section duplicating the tab strip (hidden by default below two tabs, Q4).

**Non-goals.** Outline and Timeline views, file nesting, bulk rename, permanent delete, git operations in the tree (staging and discard live in Changes), image / media preview, import updates on rename (LSP `workspace/willRenameFiles`: a servers' feature, later), several explorers or windows, stacking two panels in one dock, Windows / Linux.

**Questions for the user** (my pick first):
- **Q1** What does a fresh layout's right body open on — **Files** (Orca's default; Changes is empty on a new project and keeps its count badge) or Changes (today)?
- **Q2** Delete = **system Trash only, always confirmed**, no permanent delete in v1 — agreed?
- **Q3** File operations are **not** exposed as MCP tools (agents keep their own file tools; `hutan fs mv|cp|trash` exist for scripts and pass through the approval policy) — agreed?
- **Q4** Open Editors: **auto** (shown with ≥ 2 file tabs open) rather than always on.
- **Q5** Keep Files on the right for everyone, or ship a built-in *Explorer left* preset next to `default | focus | overview` (one dock arrangement, no new code besides the preset)?

## 6. Sources

Hutan: `docs/editor.md` § E-13a / E-13b / E-10, `docs/design.md` § Layout (*Files in the right dock*), § Docks (DK-1), § Customizing the workbench, § Keyboard defaults, § Layout migration, `docs/protocol.md` § Files, `docs/control.md` § file service, ADR 0001 / 0002 / 0010 / 0011 / 0012 / 0019, `docs/spikes/2026-10-25-user-needs/ade.md`, `docs/spikes/2026-10-21-competitor-ux/orca.md`, `docs/spikes/2026-10-22-ui-polish/orca-sidebar.md`, `docs/briefs/FX-CHANGES-VIEW.md`; code `crates/desktop/src/workbench/{panels/files.rs,model/persist.rs,model/preset.rs,sidebars.rs,files.rs}`, `crates/protocol/src/fs.rs`; tags `v0.1.0`, `v0.2.0`, `v0.3.0` (`git show <tag>:…/preset.rs`).
External (read 2026-10-27): Zed [project panel](https://zed.dev/docs/project-panel); VS Code [user interface § Explorer](https://code.visualstudio.com/docs/getstarted/userinterface); JetBrains [Project tool window](https://www.jetbrains.com/help/idea/project-tool-window.html); Orca `docs/site/content/docs/editing/file-explorer.mdx` and `src/renderer/src/components/right-sidebar/FileExplorer*.tsx`, `file-explorer-*.ts` at `de2700d`; [Superset editor & files](https://docs.superset.sh/editor), [changelog 2026-02-02](https://superset.sh/changelog/2026-02-02-file-explorer-workspace-improvements); Cursor forum [158374](https://forum.cursor.com/t/add-file-explorer-to-agents-window-cursor-3/158374), [158646](https://forum.cursor.com/t/agents-window-customize-layout/158646), [164985](https://forum.cursor.com/t/the-project-directory-sidebar-has-disappeared-and-clicking-the-file-icon-opens-an-empty-untitled-tab-instead/164985), [171835](https://forum.cursor.com/t/allow-moving-file-explorer-in-agents-window/171835); Conductor [diff viewer](https://www.conductor.build/docs/reference/diff-viewer), [0.62.0](https://www.conductor.build/changelog/0.62.0-repo-settings-browser-preview); [Claude Code desktop redesign](https://claude.com/resources/articles/claude-code-desktop-redesign); GitHub issues linked in §2.7 and Warp [#7900](https://github.com/warpdotdev/Warp/issues/7900) / [#8374](https://github.com/warpdotdev/Warp/issues/8374) / [#7663](https://github.com/warpdotdev/Warp/issues/7663) / [#7288](https://github.com/warpdotdev/Warp/issues/7288), Orca [#29611](https://github.com/stablyai/orca/issues/29611), vibe-kanban [#3158](https://github.com/BloopAI/vibe-kanban/issues/3158).
Scratch (not committed): `~/hutan-scratch/explorer-spike/` (query script, mock generator, the measurement setup of §1.4; the 120k-file repo was deleted afterwards).
