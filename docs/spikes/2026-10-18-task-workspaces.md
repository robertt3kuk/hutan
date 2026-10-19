# Task workspaces and unassigned sessions — D-1 design

**Status:** research and recommendation; no implementation or decision enacted.

## Executive recommendation

Accept D-1: **a workspace groups sessions for a task; projects describe where those sessions work.**

1. Keep the canonical **Home row as the internal unassigned container**. Preserve `sessions.workspace_id NOT NULL` and required `SessionInfo.workspace_id`. Present Home membership as **Sessions · Unassigned**, not as a workspace named Home.
2. Reuse `WorkspaceKind::Workspace` for task workspaces; **do not add a `task` enum variant**. A task workspace is not the same object as an executable `Task`.
3. Make **session membership independent of project attribution and Git checkout placement**. Moving a session changes neither its process nor its cwd.
4. Projects cease being session containers. Retain their durable catalog identities and discovery rules; allow the same project to supply checkouts to several workspaces.
5. Make new desktop sessions belong to the selected workspace; new sessions without an explicit or authenticated caller context go to unassigned. MCP-created agents inherit their caller’s **current daemon-owned membership**, never the desktop’s selected workspace.
6. Retain client-owned per-workspace tabs/splits. Add session-level layout reconciliation instead of merging entire project layouts.
7. Right panels follow an explicit selection scope—workspace, session, or pinned All—not merely whichever terminal remains focused.
8. **Distinguish an additive API from the full model cutover.** `session.assign` alone fits a protocol minor. The complete clean cutover recommended here changes existing method meanings and defaults; it should use a **protocol major**, unless the implementation deliberately preserves the old contract through a longer compatibility period.

That last point corrects an important overstatement in direction §2: Home preserves wire shape, but does not by itself make all semantic changes additive.

## 1. Evidence and current behavior

Paths without an external prefix are relative to Hutan. External source paths use these verified official checkouts:

- **herdr:** `/Users/workie/proj/herdr-src`, <https://github.com/herdrdev/herdr>
- **Orca:** `/Users/workie/proj/orca-src-git`, <https://github.com/stablyai/orca>

The requirements are explicit: group terminals and agents, show loose sessions with cwd, support task workspaces, and scope the right columns to the selected workspace/session (`docs/spikes/2026-10-18-direction.md:13-29,39-50`).

### Hutan’s existing model

| Area | Observed implementation |
|---|---|
| Membership | Every session has a required `workspace_id`; storage uses a non-null foreign key with **ON DELETE CASCADE** (`crates/storage/migrations/0001_init.sql:16-31`; `crates/protocol/src/messages.rs:829-857`). |
| Container kinds | `Home`, `Project`, `Workspace`; the enum is a closed serde enum without an unknown-value case (`crates/protocol/src/messages.rs:471-481`). |
| Project ownership | `workspaces.parent_id` allows a project to have one workspace parent. SQLite’s CHECK permits only the three current kinds; a unique partial index permits at most one Home (`crates/storage/migrations/0008_projects.sql:16-42`). |
| Automatic assignment | Missing `SessionCreate.workspace_id` invokes cwd-based project/Home assignment. Chats do the same (`crates/daemon/src/registry.rs:317-330,427-439,514-525,1937-1958`). |
| Project identity | Deepest Git repository’s canonical common directory; main checkout and linked worktrees share identity. Non-Git folders can also be projects. `/`, user home, and non-repository Home descendants receive special treatment (`crates/daemon/src/projects.rs:37-76,79-138`). |
| Workspace mutation | Creating or adding a folder to a workspace creates/reuses its project and **moves the whole project** into that workspace (`crates/daemon/src/registry.rs:1294-1342,1671-1688`). |
| Existing assignment API | `workspace.assign {project_id, workspace_id?}` changes project parenting, not session membership (`crates/protocol/src/project.rs:34-42`). |
| Live cwd | OSC-derived cwd updates `folder`, `cwd`, session events, and persistence; it does not change today’s project ownership (`crates/daemon/src/registry.rs:2485-2489`; `crates/protocol/src/messages.rs:853-857`). |
| Tree | User workspace → projects → sessions, unparented projects, Home. “No workspace” currently means **unparented projects**, not loose sessions (`crates/desktop/src/sessions/tree.rs:44-97,156-238`). |
| Drag/drop | Workspace reorder and project grouping, including multi-project selection; not session reassignment (`crates/desktop/src/workbench/panels/sessions.rs:68-80,883-905,1196-1225`). |
| Layouts | Already client-owned and per workspace. Parked workspace terminals detach from rendering (`crates/desktop/src/workbench/model/workspace.rs:1-3,68-96`; `crates/desktop/src/workbench/workspaces.rs:1-12`). |
| Layout resolution | Current logic merges project layouts into their parent workspace; this is unsuitable for individual session moves (`crates/desktop/src/workbench/workspaces.rs:370-438`). |
| Task launch | A task in a user workspace deliberately omits the workspace ID so the session lands in its project. This must change (`crates/daemon/src/tasks.rs:796-813`). |
| Inspector | Its own description promises requests from every chat plus focused-session info; Info derives workspace from that session’s membership (`crates/desktop/src/workbench/panels/inspector.rs:1-2,104-135`). |
| Changes | Already follows `active_workspace_id`, with generation-based stale-response protection (`crates/desktop/src/workbench/panels/changes.rs:103-158`). |

### A relevant correction to the earlier spikes

The current repository has advanced beyond the cited layout inventory:

- `history.sessions` now exists at protocol **0.29**, with summaries, sorting, counts, and scopes (`crates/daemon/src/history.rs:235-273`; `crates/protocol/src/history.rs:187-220,321-326`).
- Its workspace scope still means **cwd within roots/member repositories**, not actual task membership (`crates/daemon/src/history_catalog.rs:5-12,193-220`).
- Desktop saved-state format is now **version 3**, not version 1 (`crates/desktop/src/workbench/model/persist.rs:10-13`).

D-1 should extend these implementations, not reimplement the missing catalog described in the older spike.

## 2. Comparison: what to borrow and what not to copy

### herdr

herdr directly supports the useful conceptual model: named workspace → tabs → panes. A workspace has a stable identity, custom name, identity cwd, Git metadata, tabs, and active tab (`herdr/src/workspace.rs:175-205`).

Its `pane.move` supports:

- existing tab and split position;
- new tab in a workspace;
- new workspace.

The schema is explicit (`herdr/src/api/schema/panes.rs:85-117`). The implementation takes an existing pane and inserts it elsewhere, rather than starting a replacement terminal (`herdr/src/app/api/panes.rs:1135-1164,1235-1267`).

**Borrow:**

- Move the occupant without restarting it.
- Validate destination before taking the source.
- Separate membership movement from destination layout placement.
- Roll attention up from the contained sessions.

**Do not copy literally:**

- herdr can remove the source workspace when its last pane moves (`herdr/src/app/api/panes.rs:1166-1188`). Hutan’s task workspace may still contain notes, executable tasks, repository bindings, or just a deliberately empty task. **Keep it.**
- Cross-workspace public pane numbering needs aliases in herdr (`herdr/src/app/api/panes.rs:1157-1164`). Hutan has stable `SessionId` and `TerminalId`; **keep both unchanged**.
- herdr has no evidenced unassigned-session inbox. Its “session” is a server namespace, not a Hutan session (`docs/spikes/2026-10-18-herdr-model.md:15-23`).

herdr’s attention priority is blocked → unseen idle/done → working → seen idle → unknown (`herdr/src/workspace/aggregate.rs:50-74`). This is a good starting point, extended with Hutan’s explicit failures and task review state.

### Orca

Orca’s worktree cards combine workspace identity, branch/review metadata, activity, unread state, and inline agents. Attention takes priority over passive branch/review identity; unread remains accessible even when its glyph is temporarily superseded (`Orca/src/renderer/src/components/sidebar/WorktreeCardStatusSlot.tsx:118-154`). Inline agents appear immediately under the card heading (`Orca/src/renderer/src/components/sidebar/worktree-card-secondary-rows.tsx:87-94`).

**Borrow:**

- Strong workspace identity with a compact status lane.
- Inline agent/session rows.
- Optional branch/issue metadata, not mandatory decoration.
- Attention and unread indicators that do not disappear behind Git metadata.

**Do not adopt “workspace equals worktree” as an invariant.** Hutan must also support non-Git investigations, agents in different repositories, shared checkouts, and entirely unassigned sessions. Orca’s retained workspace surfaces are useful layout evidence, not a reason to make a checkout mandatory (`docs/spikes/2026-10-18-orca-layout.md:40-45,100-126`).

## 3. Domain model and invariants

### Three orthogonal identities

```text
Session
  id                    stable runtime/conversation identity
  workspace_id          Home or a deliberate task workspace
  project_id?           automatically attributed repository/folder
  cwd / folder?         latest known working directory
  placement?            actual Git checkout, branch, sharing information
```

A task workspace owns membership and contextual organization, **not filesystem containment**.

Examples:

- Two workspaces can contain sessions from the same repository.
- Two sessions in one workspace can work in different repositories.
- A session can belong to “Investigate latency” while its shell runs in `/tmp`.
- An unassigned session may have a project and worktree badge.
- Moving an agent to a workspace with another worktree does **not** move that agent’s checkout.

### Workspace kinds

Use the existing representation:

| Kind | New role |
|---|---|
| `home` | Internal membership sentinel for unassigned sessions. Non-deletable; not the personal Assistant. |
| `workspace` | Deliberate task container with direct sessions and optional repository/checkout bindings. |
| `project` | Project catalog record, **not a session membership target**. |

Retaining project rows in the current discriminated table avoids replacing every durable project ID at once. This is an implementation representation, not an assertion that projects remain containers. Document that distinction clearly.

Do not add `kind: task`: executable `Task` already exists and can be one of several tasks in a workspace. The eventual personal Assistant is a separate product role; do not rename the Home sentinel to “Hutan” or reuse it as the assistant workspace.

### Project attribution

Keep existing repository discovery and common-directory identity. Decouple it from membership:

- `[projects] auto=true`: resolve/update `session.project_id` from its known cwd.
- OSC 7 can update cwd, placement, and automatic project attribution; **never membership**.
- `[projects] auto=false`: do not automatically create project records; cwd and placement still work.
- Missing cwd means unknown project, not an invented repository.

This deliberately changes today’s “project chosen at start” behavior. It makes the visible repo badge and optional project grouping reflect the live directory. If the product instead wants launch-project identity, name it explicitly `launch_project_id`; do not use one field ambiguously for both.

Filesystem resolution stays off the registry actor. Use a per-session directory generation so an older resolution cannot overwrite a newer OSC 7 result. This is a proposed implementation, not observed behavior.

### Selection, focus, and membership are different

The desktop needs three distinct concepts:

1. **Active layout workspace**: which workspace’s center is shown.
2. **Selected subject**: workspace, session, or a multi-selection.
3. **Focused input surface**: terminal/chat/composer receiving keyboard input.

A workspace click must be able to show workspace context even when restoring a layout also restores a previously focused terminal. An incoming event must not steal selection or keyboard focus.

## 4. Storage design and migration

### Retain non-null Home membership

| Option | Benefits | Costs |
|---|---|---|
| **Home sentinel — recommended** | Preserves required membership field; reuses existing snapshot/filter/layout keys; fewer nullable branches; existing FK structure remains useful. | Must prevent Home from being confused with a normal task workspace or Assistant. |
| Nullable membership | Direct mathematical representation of “no workspace.” | Table rebuild, optional Rust/wire fields, null handling throughout filters/layouts/settings/restore, and old clients cannot deserialize null into required `WorkspaceId`. |
| Separate membership table | Natural if sessions can belong to multiple workspaces. | Unneeded joins and ambiguity; no requirement for multi-membership. |

A session belongs to exactly one task workspace or the Home sentinel. There is no duplication across workspaces.

### Proposed schema changes

Next migration after current schema 10:

```sql
-- Conceptual schema; final migration must use the existing table-rebuild discipline.

sessions.workspace_id
    TEXT NOT NULL REFERENCES workspaces(id) ON DELETE RESTRICT;

sessions.project_id
    TEXT REFERENCES workspaces(id) ON DELETE SET NULL;

CREATE INDEX sessions_by_project
    ON sessions(project_id, created_at_ms, id);

CREATE TABLE workspace_projects (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    project_id   TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    checkout_path TEXT NOT NULL,
    position     INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, project_id)
) STRICT;
```

Also add explicit `default_cwd` and optional `note` / `issue_url` columns to workspace records if these fields ship in this slice. `checkout_path` identifies the workspace’s preferred checkout for a project, either main or a linked worktree. Branch is derived from Git rather than duplicated as authoritative database state.

Constrain row kinds in the registry and corresponding storage integrity checks:

[Showing lines 1-200 of 812. Use :201 to continue]

- `sessions.workspace_id`: `home` or `workspace`;
- `sessions.project_id`: `project`;
- binding source: `workspace`; binding target: `project`.

SQLite foreign keys alone cannot enforce those discriminator constraints.

Remove `parent_id` and its storage write path after migration. The m:n binding replaces exclusive project ownership.

### Migration mapping

Perform the relational transformation before normal restoration:

| Old session membership | New membership | Project attribution |
|---|---|---|
| Home | Same Home ID | Resolve when possible |
| User workspace | Same workspace ID | Resolve from cwd |
| Project with parent workspace | Parent workspace ID | Old project ID initially |
| Unparented project | Home ID | Old project ID initially |

Other migration rules:

1. Preserve every session/conversation ID and all journal, approval, question, tree, and resume references.
2. Convert each old project `parent_id` into a `workspace_projects` binding.
3. Preserve existing user workspace IDs, names, pins, and order.
4. Move tasks formerly owned by a project to its parent workspace. For a project with tasks but no parent, create a deliberate workspace named from that project and attach its repository there. Do **not** invent a task workspace for every loose terminal.
5. Preserve managed worktree provenance. The existing `worktrees.workspace_id` is the workspace that requested creation and intentionally has no foreign key; the filesystem worktree outlives that workspace (`crates/storage/migrations/0006_worktrees.sql:1-12`). Membership assignment must not rewrite creation provenance.
6. Record migration completion and the project→membership mapping needed by desktop layout migration. Do not perform filesystem discovery in SQL.
7. Ensure exactly one Home before restoring sessions. The current unique index proves **at most one**, not that one exists.
8. Preserve the Home ID and its configured folder. The folder remains a launch fallback; it does not mean unassigned sessions must run beneath it.

The migration runner already provides per-migration transactions and pre-migration backups (`crates/storage/src/migrate.rs:1-5,18-19,29-80`). Use that mechanism.

### Persistence must precede successful assignment

Existing `write` and `batch` APIs return after enqueueing, whereas `write_checked` returns the SQLite outcome (`crates/storage/src/lib.rs:180-201`). Current workspace mutations often publish/update memory and merely log later storage failures (`crates/daemon/src/registry.rs:1323-1341,1384-1399`).

Assignment must not report success that disappears after restart. Add a checked compound storage mutation, such as `Write::AssignSessions`, executing all membership updates in one transaction. After successful commit, update in-memory membership and publish events. Do not claim ordinary queued `batch()` provides an acknowledged durable mutation.

## 5. Registry and protocol contract

### Assignment APIs

Keep the requested simple mutation:

```json
{
  "id": 41,
  "method": "session.assign",
  "params": {
    "session_id": "SESSION_ID",
    "workspace_id": "WORKSPACE_ID"
  }
}
```

Omitted or null `workspace_id` means Home/unassigned:

```json
{
  "id": 42,
  "method": "session.assign",
  "params": {
    "session_id": "SESSION_ID",
    "workspace_id": null
  }
}
```

Return the existing `SessionResult` shape with the complete canonical session.

For multi-select, add an atomic companion:

```json
{
  "id": 43,
  "method": "session.assign_many",
  "params": {
    "session_ids": ["SESSION_A", "SESSION_B"],
    "workspace_id": "WORKSPACE_ID"
  }
}
```

Return complete updated sessions. Validate the entire set and target first; one unknown/disallowed session produces no changes. Duplicate IDs are normalized. Assigning to the current target is an idempotent no-op.

Extend `workspace.create` with `session_ids` for **New workspace from selection**. Workspace creation and session reassignment commit together. This avoids a half-created workspace when assignment fails.

### Registry behavior

Add one shared assignment implementation behind both APIs:

1. Verify all session IDs and target kind.
2. Reject cross-host movement; IDs are daemon/host-scoped.
3. Commit membership updates.
4. Replace membership only; preserve all runtime handles.
5. Recompute source/destination counts and activity.
6. Emit canonical session and workspace updates.
7. Do not stop/restart a PTY, ACP harness, direct chat, child agent tree, or active turn.

Remove cwd-based membership selection from `RegistryHandle::assign` / `workspace_for`. Retain discovery as an attribution operation, rather than deleting `projects.rs`.

Update both terminal and chat creation, restart/restore, assistant-created sessions, and executable task launches. In particular, the exception at `tasks.rs:796-801` must become an explicit `Some(task.workspace_id)`.

### Events

Reuse existing events:

```json
{
  "event": "session.updated",
  "epoch": "BOOT_EPOCH",
  "seq": 1042,
  "data": {
    "session": {
      "id": "SESSION_ID",
      "workspace_id": "WORKSPACE_ID",
      "project_id": "PROJECT_ID"
    }
  }
}
```

The example elides other required `SessionInfo` fields.

Follow with `workspace.updated` for affected membership counts. No `session.closed`, terminal detach command, fake process-start event, or novel `session.moved` state is necessary.

A multi-session mutation is atomic in storage and registry state; its existing per-session events are individually sequenced. Clients must not assume the intermediate event list is a transaction boundary. The originating desktop can apply the complete response together; other clients converge through canonical updates/snapshot. Preserve subscribe-before-snapshot, epoch/sequence replay, credit-1 terminal flow, and bounded queues (`docs/adr/0002-ipc-transport-and-framing.md:29-39`).

### Workspace repository APIs

Change roots from exclusive ownership into explicit bindings:

- `workspace.add_root`: add/update a binding; never steal another workspace’s project.
- `workspace.remove_root`: remove that binding, not sessions or a checkout.
- `workspace.create.paths/project_ids`: attach repositories, not move their sessions.
- Remove obsolete `workspace.assign`; migrate callers to session assignment or repository binding.
- Reject a `project` row as a new session membership destination.
- Keep project open/list/rename/recent behavior as catalog operations, without creating a task workspace as a side effect.

### Deleting a workspace

Recommended semantics:

- Never delete Home.
- Moving the last session out leaves an empty workspace.
- Deleting a workspace moves all its sessions, including ended sessions, to Home first.
- Running executable tasks must be stopped/cancelled before deletion.
- Preserve the existing destructive task-deletion behavior only with explicit confirmation displaying the task count.
- Do not delete a Git worktree, branch, transcript, terminal history, or agent conversation as an implicit consequence.
- Emit session updates before `workspace.closed`; enforce database `RESTRICT` as protection against accidental session cascade.

This intentionally replaces the current behavior that deletes ended sessions and their terminals with the workspace (`crates/daemon/src/registry.rs:1475-1515`).

## 6. New-session context: desktop, CLI, MCP, tasks

The daemon has no authoritative “selected workspace” shared by all clients. New-session context therefore must be explicit or derived from a verified caller.

### Resolution order

1. Explicit `workspace_id` wins.
2. Explicit **unassigned** choice wins over inheritance.
3. An authenticated Hutan caller context may inherit its current session’s membership.
4. Otherwise Home.

Use an explicit optional creation field such as `inherit_origin_workspace: true`; absence must not ambiguously mean both “inherit” and “unassigned.” Desktop launchers always send their intended workspace explicitly. MCP tools set inheritance by default when no target/unassigned choice was supplied.

For agents launched from the personal Assistant, inherit the assistant workspace by default. Creating a separate task workspace is an explicit agent tool action, not a silent spawn side effect.

### MCP

Current MCP already carries `HUTAN_SESSION_ID` as protocol origin, and the daemon verifies live origin and applies control policy (`crates/cli/src/mcp/mod.rs:1-5`; `crates/cli/src/mcp/server.rs:45-54`; `crates/daemon/src/control.rs:156-170`).

Add:

- `session_assign`;
- `session_assign_many`;
- `workspace_create.session_ids`;
- target/unassigned/inherit fields in `session_create` and `chat_create`;
- updated descriptions for workspace repository attachment.

Resolve inheritance from the live registry on each call, **not a workspace ID cached when the MCP bridge started**. A caller moved from workspace A to B must create its next child in B.

Resolve the effective destination before presenting an approval so the user sees the real target. If the caller’s inherited membership changes while approval is pending, refuse with a conflict rather than silently executing in a different workspace.

All mutations remain `Access::Mutate`; `[control] agents=ask|allow|read_only` still applies. Assignment does not expand sandbox roots, change approval grants, reconfigure an existing backend, or confer filesystem permission. Existing control audit policy covers agent mutation/refusal (`crates/daemon/src/control.rs:10-18`).

A plain external MCP process with no valid origin defaults to unassigned. There is no reliable desktop selection to infer.

### CLI

Preserve current top-level session commands and add:

```text
hutan assign SESSION --ws WORKSPACE
hutan assign SESSION --unassigned
hutan assign SESSION_A SESSION_B --ws WORKSPACE

hutan new --ws WORKSPACE
hutan new --unassigned

[Showing lines 201-400 of 812. Use :401 to continue]
hutan chat new ... --ws WORKSPACE
hutan ls --unassigned
hutan ws new "Investigate latency" --session SESSION_A --session SESSION_B
```

Continue existing exact-ID, unique-prefix, exact-name resolution; ambiguity must remain an error. Update `ws ls` to list task workspaces and direct session counts; `project ls` remains the separate catalog.

The CLI currently resolves `--ws`, then user-exported `HUTAN_WORKSPACE` (`crates/cli/src/commands/workspaces.rs:25-26,295-303`). Preserve that as an explicit user override. Then use live origin inheritance; outside Hutan, fall back to unassigned.

Do not use daemon-injected `HUTAN_WORKSPACE_ID` as live truth. It is frozen in a running process’s environment and cannot change after moving the session (`crates/daemon/src/registry.rs:2051-2057`). Document it as launch context or remove it in the major cutover; `HUTAN_SESSION_ID` plus daemon lookup is the durable context mechanism.

Migrate `ws add/remove`, `project open/close`, worktree helpers, task defaults, help text, MCP guides, and JSON output together. Current CLI messages explicitly describe exclusive project membership (`crates/cli/src/commands/workspaces.rs:94-175`).

## 7. Worktrees and repository bindings

**A workspace may prefer a worktree, but does not require one.**

- Default creation: no Git action.
- New workspace sheet can choose **Use existing checkout** or **Create worktree** for each attached project.
- At most one preferred checkout per project per workspace; several projects per workspace.
- New session cwd defaults to the selected repository binding, then workspace `default_cwd`, then the configured new-session fallback.
- Session assignment never runs `cd`, `git checkout`, or creates a worktree.
- Show a readable warning when an assigned session runs outside the workspace’s preferred checkout.
- Preserve existing shared-checkout warnings and global live-session checks before removal. Membership does not make deleting a worktree safe.

The existing worktree service discovers repositories from workspace roots (`crates/daemon/src/worktrees.rs:427-457`). Replace that source with:

1. explicit workspace bindings;
2. actual placements of contained sessions, deduplicated by repository/checkout.

**Workspace Changes** defaults to bound checkouts and contained sessions’ checkouts. Offer **Other worktrees in these repositories** separately. Showing every repository worktree as if it belonged to the selected task would leak unrelated task context.

For multi-repository workspaces, worktree creation must require repository selection when ambiguous; do not silently pick an arbitrary first root. The existing request already permits explicit `repo` (`crates/protocol/src/worktree.rs:20-33`).

## 8. Desktop tree, layouts, and right sidebar

### Left sidebar

Replace project-parent nesting in `sessions/tree.rs` with:

```text
Workspaces
  WorkspaceNode { info, direct_sessions, attention }

Sessions · Unassigned
  SessionNode
```

`sidebar.organize=projects` is an **alternative projection**, not another membership model. It groups sessions by `project_id`, and shows their task-workspace badge. A project row cannot be a session-assignment drop target.

Keep unassigned cwd visible by default. Reuse `folder_label`, but full path must be available on hover, keyboard inspection, and Copy cwd. Unknown cwd displays **Directory unknown** rather than a misleading Home path.

Current Needs-you rendering removes those sessions from their ordinary tree (`crates/desktop/src/workbench/panels/sessions.rs:664-694`). Change it to a compact shortcut section: canonical workspace/unassigned rows remain present. Deduplicate multi-selection and counts by `SessionId`.

### Left wireframe

```text
┌──────────────────────────────────────┐
│ Hutan                    Local  ▾    │
│ Search                               │
│ Tasks                                │
│ Agents                               │
│ Hutan                                │
├──────────────────────────────────────┤
│ Needs you  2                         │
│   ! Claude · Fix auth                │  shortcuts
│   ✓ Codex  · Review scheduler        │
├──────────────────────────────────────┤
│ WORKSPACES                  +   ···  │
│ ▾ ! Fix auth                  1 / 3  │
│     app · wt fix/auth                │
│     ! Claude · permissions          │
│       ~/proj/app-wt/fix-auth         │
│     ● Terminal · tests              │
│       ~/proj/app-wt/fix-auth         │
│     ○ API logs                      │
│       ~/proj/api                    │
│ ▸ ✓ Review scheduler             2  │
│ ▸   Notes and investigation      0  │
├──────────────────────────────────────┤
│ SESSIONS · UNASSIGNED       +   ···  │
│   ○ zsh                             │
│     ~/Downloads                     │
│   ● Codex · investigate             │
│     ~/proj/hutan       main         │
│                                      │
│ Drop sessions here to unassign       │
└──────────────────────────────────────┘
```

One-line primary rows use existing density tokens; the cwd subtitle intentionally makes a session row taller. Section counts and attention dots cannot rely on color alone.

### Drag/drop and multi-select

- Drag a session row onto a workspace body: move membership, preserve process.
- Drop on Unassigned header/body: move to Home.
- Drag a selected session: move the whole selected set, including selections across workspaces.
- `Mod`-click toggles; Shift-click extends across visible session rows. Workspace reorder selection stays distinct from session selection.
- Hovering a collapsed workspace expands it after a short delay; dropping does not require expansion.
- Show target name and “Move N sessions”; no hidden project-wide move.
- Differentiate workspace reorder zones from session assignment targets.
- On success, retain sidebar focus and show **Moved N sessions — Open workspace**. Do not unexpectedly navigate away from the source.
- On failure, retain selection and display the error; no fake optimistic success.
- **New workspace from selection…** names the workspace and optionally attaches repositories. It does not move files.
- Multi-select context: Move…, Unassign, New workspace from selection…, Stop selected… with an explicit destructive confirmation.
- Dragging a center terminal/chat tab across workspace boundaries invokes assignment plus local layout placement. Dragging within one workspace is layout-only.

### Keyboard flows

Reuse existing configurable keymap/action machinery:

- `Mod+J`: existing jump palette; search workspace names, session titles, cwd, project, branch, attention.
- Arrow keys: navigate sidebar rows.
- Left/Right: collapse/expand workspace.
- Enter: open selected workspace/session.
- Space in the sidebar: toggle session selection without entering terminal input.
- `Mod+Shift+M`: Move selected sessions…; picker offers workspaces, Unassigned, New workspace….
- Context-menu key / Shift+F10: same menu as pointer users.
- Escape: close picker, then clear multi-selection.
- Expose named palette actions even if a suggested shortcut conflicts with a user keymap.
- Stop/delete keys are never overloaded onto ordinary Backspace while editing/filtering.

### Per-workspace layouts

Keep existing `WorkspaceLayout` parking and detach behavior. Add session-level reconciliation:

1. Scan live, parked, and stashed center layouts after canonical membership updates or snapshot hydration.
2. Remove terminal/chat references whose membership no longer matches their layout.
3. Collapse empty split leaves while preserving unrelated tabs and ratios.
4. For a locally requested move, transfer that session’s presentation to a destination tab; destination split placement is a desktop-only choice.
5. For a move performed by another client, prune stale source references but do not open new tabs or steal focus.
6. New destination layouts open the moved session when requested; pre-existing destination layouts remain intact.
7. Multiple references to the same session are reconciled by session identity; no second process is created.
8. Keep the runtime model in the shared session store. Reparent/recreate a view if necessary without stopping its occupant.

Do **not** call `merge_workspace` for every session move. Its current contract lets an existing destination layout win over an entire project layout (`crates/desktop/src/workbench/model/workspace.rs:139-160`).

During one-time migration, merge retained tabs from old project layouts into their new task workspace or Home. Preserve existing destination splits and append otherwise-unrepresented sessions as tabs; never silently discard tabs because the destination already has a layout.

### Right sidebar scope

Add a shared client-owned context:

```rust
enum SelectedSubject {
    Workspace(WorkspaceId),
    Session(SessionId),
    Sessions(Vec<SessionId>),
    Unassigned,
}

enum ContextScope {
    FollowSelection,
    All, // pinned global scope
}
```

Panels consume the resolved context through `PanelEnv`, rather than each inventing a fallback to active/focused session.

| Selection | Changes | Requests | Agents | History / Info |
|---|---|---|---|---|
| Workspace | Its bound/occupied checkouts | Its sessions’ approvals/questions | Its agents | Explicit workspace membership |
| Session | Its actual checkout | That session | That session’s tree | Session conversation + cwd/runtime info |
| Multiple sessions | Deduplicated selected checkouts | Selected sessions | Selected agents | Selection summary |
| Unassigned | Checkouts of loose sessions | Unassigned requests | Unassigned agents | Unassigned Hutan history |
| Pinned All | Global checkout picker | All | All | All |

Pinning All must display **All · Pinned**, not an unlabeled scope override. Incoming requests may reveal the panel if configured, but must not silently change its scope.

### Right wireframes

```text
WORKSPACE SELECTED                    SESSION SELECTED
┌─────────────────────────────┐       ┌─────────────────────────────┐
│ Fix auth          Scope ▾ 📌│       │ Claude · permissions       │
│ Workspace · 3 sessions      │       │ Fix auth / Claude   Scope ▾│
├─────────────────────────────┤       ├─────────────────────────────┤
│ Changes Requests Agents Hist│       │ Info Changes Requests Hist │
├─────────────────────────────┤       ├─────────────────────────────┤
│ Changes                     │       │ Workspace  Fix auth  Move… │
│ app · wt fix/auth           │       │ Directory  ~/proj/app-wt/… │
│   4 changed files           │       │ Project    app             │
│ api · main                  │       │ Branch     fix/auth        │
│   Clean                     │       │ Backend    Claude / ACP    │
│                             │       │ Status     Approval needed │
│ + Attach repository         │       ├─────────────────────────────┤
│                             │       │ Requests · 1               │
│ Requests · 1                │       │ Allow shell command?       │
│ Claude asks to run tests    │       │ [Allow once] [Deny]        │
└─────────────────────────────┘       └─────────────────────────────┘

PINNED ALL                            EMPTY WORKSPACE
┌─────────────────────────────┐       ┌─────────────────────────────┐
│ All · Pinned        Unpin   │       │ Notes and investigation    │
├─────────────────────────────┤       ├─────────────────────────────┤
│ Requests · 2                │       │ No sessions yet            │
│ Fix auth / Claude           │       │ Start a terminal or agent, │
│ Unassigned / Codex          │       │ or move existing sessions. │
│                             │       │ [Start…] [Move sessions…]  │
└─────────────────────────────┘       └─────────────────────────────┘

[Showing lines 401-600 of 812. Use :601 to continue]
```

The pin symbol is illustrative; implementation should use the shared icon enum and accessible labels.

### History must not equate repo with task

Two workspaces may share the same cwd. Therefore a task’s history cannot be determined by a cwd-prefix filter.

- Hutan conversations use persisted membership, including ended sessions.
- External conversations without an explicit Hutan association remain project-scoped.
- Resuming/importing an external conversation into a workspace records that association.
- Workspace History must not silently include every external conversation in the repository.
- All and Project remain available for broader discovery.

Update `history_catalog.rs` and the index association path, not only the panel label. Current `in_workspace` explicitly matches roots and common directories (`crates/daemon/src/history_catalog.rs:193-220`).

## 9. Attention roll-up

Preserve the existing distinction between runtime state and unread attention. Hutan already has client-side unread tracking, explicit failed/done states, approval/question classification, and task review attention (`crates/desktop/src/sessions/attention.rs:1-10,96-116,133-181`; `crates/desktop/src/sessions/store.rs:619-641`).

Recommended priority:

```text
blocked request/question
> unread failure
> unread completion / awaiting task review
> working
> idle
```

Workspace counts aggregate **unique member sessions**, not approvals, child-agent nodes, or duplicated Needs-you shortcut rows.

- Selection of a workspace alone does not mark every completion read.
- Focusing/viewing a session follows current read semantics.
- Moving a session preserves its unread state.
- Unassigned has the same roll-up.
- Workspace-level state updates must be proportional to affected sessions; terminal output should not rebuild the whole sidebar tree.
- Do not persist a single global “done/read” state in `WorkspaceInfo`: different desktop clients may have seen different sessions. CLI can compute runtime attention from canonical sessions; read/unread is client presentation state.

## 10. Settings and implementation layout

### Settings

```toml
[projects]
auto = true                  # attribution, never task membership

[sidebar]
organize = "workspaces"       # "projects" is an alternative projection
group_unassigned = "none"     # "project"
show_unassigned = true

[panels.sessions]
needs_you = true
row = ["state", "kind", "title", "folder", "placement", "age"]

[panels.context]
scope = "follow_selection"   # "all"
reveal_on_approval = true

[sessions]
new_cwd = "follow_selection" # "home" or configured path
```

Use existing namespaces and keys where possible; migrate `panels.inspector.reveal_on_approval` if introducing the common context namespace. Pin state and last selected panel belong in desktop saved state, not daemon-wide settings.

Do not retain two daemon membership algorithms behind an “old organization” switch. The requested project preference is a view; otherwise moving between settings would itself become a data migration.

Workspace settings must no longer implicitly come from the **first project root**, because the same repository can participate in several tasks. Store workspace overrides under the daemon data directory keyed by workspace ID; retain repository `.hutan/settings.toml` as the project layer. Preserve existing setting precedence deliberately during migration. Current first-root layering is explicit at `crates/desktop/src/workbench/workspaces.rs:307-367`.

### Crate/module responsibilities

| Location | Change |
|---|---|
| `crates/core` | Keep session/workspace IDs and executable Task separate; no new runtime crate. |
| `crates/storage/migrations` | Membership/project split, m:n bindings, safe FK behavior, relational migration. |
| `crates/storage/src/{rows,sql,lib}.rs` | Project attribution fields, binding operations, checked compound assignment. |
| `crates/protocol/src/messages.rs` | Creation context, `SessionInfo.project_id`, existing-kind semantics. |
| `crates/protocol/src/project.rs` | Repository binding DTOs; remove project-parent assignment contract at major cutover. |
| `crates/protocol/src/session.rs` — proposed small module | Assignment DTOs, batch result, validation limits. |
| `crates/daemon/src/registry.rs` | Sole membership authority, creation resolver, mutation/events/counts. |
| `crates/daemon/src/projects.rs` | Discovery/attribution only; stop choosing task membership. |
| `crates/daemon/src/{router,control}.rs` | Route/classify/describe/audit new mutations and resolved destinations. |
| `crates/daemon/src/{tasks,worktrees,history_catalog}.rs` | Explicit task ownership, checkout scope, membership-aware history. |
| `crates/desktop/src/sessions/{tree,store,attention}.rs` | Direct session tree, selection context, project projection, roll-up. |
| `crates/desktop/src/workbench/{workspaces,projects}.rs` | Launch targets, repository bindings, remove whole-project move flow. |
| `crates/desktop/src/workbench/model/workspace.rs` | Session-level live/parked/stashed layout reconciliation. |
| `crates/desktop/src/workbench/panels/{sessions,inspector,changes,agents,history,tasks}.rs` | Shared resolved context and keyboard/drag surfaces. |
| `crates/cli/src/{args,commands,mcp}` | CLI mutation, target precedence, updated tool schemas/guides. |

No additional service crate or generic “container abstraction” is needed.

## 11. Compatibility and rollout

ADR 0002 says minor means additive, major means breaking; matching majors interoperate and capabilities gate additions (`crates/protocol/src/lib.rs:68-84`; `docs/adr/0002-ipc-transport-and-framing.md:29,39`). Current protocol is **0.29**.

### What fits a minor

An optional `session.assign` API, optional attribution field, and new desktop presentation can be added as **0.30**, gated by `session.assignment`, if existing behavior remains:

- required workspace ID stays required;
- old creation defaults still derive project membership;
- `workspace.assign` still moves projects;
- old root and history-scope meanings remain;
- existing enum values are reused.

This is a viable **additive preparatory release**, not the finished clean D-1 cutover.

### What is breaking in the recommended finished design

- Projects no longer accept session membership.
- Missing creation context defaults to unassigned rather than cwd project.
- `workspace.assign` disappears.
- Workspace root attachment stops stealing exclusive project ownership.
- Workspace history means actual task membership, not cwd overlap.
- Workspace deletion preserves sessions instead of cascading them.
- Project-parent fields and their semantics disappear.

**Recommendation:** accept a new task-workspaces ADR replacing the affected parts of ADR 0014, and ship the complete clean change with a protocol major bump. Reusing the existing enum and keeping Home still reduces migration risk; it does not waive semantic compatibility.

A major mismatch should produce the existing explicit handshake refusal, not partial data rendering. Desktop/CLI/daemon upgrade together for a local installation. For a remote older daemon, offer the old supported client/model or require upgrade; never quietly present task controls whose semantics that daemon cannot implement.

Adding `kind: task` would introduce a further compatibility hazard: current clients cannot deserialize an unknown variant. Reuse `workspace`.

### Desktop migration

Increment saved-state version from current 3. Migrate live, parked, and stashed layouts, retaining user sizes, split ratios, non-session tabs, pins, and selection when valid. Consume migration mapping plus canonical membership; reconcile again after snapshot completion.

Rollback is backup-based, not an in-place downgrade of the new schema. Explain this before applying the database migration.

## 12. Risks and acceptance slices

| Risk | Mitigation |
|---|---|
| “Unassigned” mistaken for cwd Home | Separate sentinel membership from launch directory in UI and APIs. |
| Session move restarts a process | Assignment implementation never calls runtime start/stop paths; test stable PID/runtime handles. |
| Stale environment or MCP context creates agents in the old task | Resolve live membership from verified `HUTAN_SESSION_ID`. |
| Multi-select partially moves | One checked registry/storage transaction. |
| Existing layout merge loses tabs | Session-level reconciliation; migration appends missing tabs rather than destination-wins replacement. |
| Task A shows Task B’s changes/history | Explicit membership and checkout bindings; no repository-wide fallback under a task label. |
| Session cascade destroys conversations | `RESTRICT`, move-to-Home deletion transaction, retained journals. |
| Moving changes permissions | Membership is organizational only; sandbox and capability policy are unchanged. |
| Empty task disappears | Never auto-delete a workspace after its last session moves. |
| Concurrent client changes steal focus | Canonical reconciliation without implicit navigation; per-client selection. |

### Slice WS-0 — ADR and contract

**Deliver:** accepted membership, creation-default, deletion, history-scope, and protocol-version decisions; updated protocol schema/fixtures and user-facing model documentation.

**Acceptance:** a compatibility matrix explicitly covers old/new daemon and clients, unknown enum handling, old root/assignment methods, and changed defaults. No claim of minor compatibility based solely on `workspace_id` remaining non-null.

### Slice WS-1 — storage and registry

**Deliver:** migration, independent attribution, bindings, checked `session.assign` / `assign_many`, atomic create-from-selection, counts/events.

**Acceptance tests:**

- Schema-10 fixture covers Home, parented/unparented projects, direct workspace sessions, ended/live-restorable sessions, chats, tasks, and worktrees.
- No session or journal IDs are lost.
- One Home exists and cannot be deleted.
- Unknown batch member/invalid project target/storage failure leaves all memberships unchanged.
- Live terminal/harness/chat moves preserve runtime identity and pending requests.
- Reassignment persists through daemon restart.
- Deleting a workspace preserves its sessions and does not remove worktrees.
- OSC 7 updates attribution without task membership changes; stale resolution is discarded.

### Slice WS-2 — creation and control clients

**Deliver:** terminal/chat/task/assistant launch paths, CLI, MCP, control policy/audit, stale-environment removal.

**Acceptance tests:**

- Explicit workspace, explicit unassigned, inherited origin, and no-origin defaults are distinct.
- A moved MCP caller creates its next agent in its new workspace.
- Concurrent desktop selection does not influence external MCP.
- Read-only control policy refuses assignment; ask policy displays the effective destination.
- Task runs always remain in the task’s workspace.
- CLI ambiguous IDs/names fail without mutation.
- No creation path silently resurrects project-container membership.

### Slice WS-3 — sidebar and layout movement

**Deliver:** task tree, unassigned pool/cwd, project projection, drag/drop, multi-select, keyboard commands, saved-layout migration.

**Acceptance tests:**

- Assigned and unassigned terminal/agent rows have correct cwd and membership.
- Moving a selected set invokes one atomic mutation.
- Needs-you shortcuts do not hide canonical rows or double-count.
- Source/destination/live/parked/stashed layouts retain unrelated tabs and ratios.
- Last-session movement leaves an empty workspace.
- External assignment prunes stale references without opening a new tab or stealing focus.
- All pointer operations have keyboard equivalents and stable automation probes.

### Slice WS-4 — contextual right rail and Git/history scope

**Deliver:** shared selection context, scoped Requests/Agents/Changes/History/Info, pinned All, repository bindings, membership-aware catalog association.

**Acceptance tests:**


[Showing lines 601-800 of 812. Use :801 to continue]
- Selecting a workspace shows workspace context despite restored terminal focus.
- Selecting a session narrows requests/diff/history to that session.
- Pinning All survives selection changes and is visibly labeled.
- Two workspaces sharing one repo do not share task history or requests accidentally.
- Bound and occupied checkouts are deduplicated; unrelated repository worktrees are opt-in.
- A moved session’s pending approval and attention roll-up follow its new workspace.
- Workspace selection alone does not clear all unread completions.
- New context generation discards stale async panel responses.

## Verification performed

Read-only source and documentation inspection. **No repository edits, commits, application launches, builds, tests, paid model calls, secret-store access, or user-data mutations were performed.** The acceptance tests above are proposed work, not exercised results.