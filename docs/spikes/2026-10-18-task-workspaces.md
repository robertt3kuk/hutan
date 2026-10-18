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

…

…

…
Read-only source and documentation inspection. **No repository edits, commits, application launches, builds, tests, paid model calls, secret-store access, or user-data mutations were performed.** The acceptance tests above are proposed work, not exercised results.

[Showing lines 1-300 of 812. Use :301 to continue]