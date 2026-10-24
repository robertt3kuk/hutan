//! The MCP tools of `hutan mcp`, declared once (ADR 0016): each is a name, a description written
//! for a model, a JSON Schema of its arguments, and how it maps to a daemon request. Adding a
//! tool is adding a row; the tests check that every row decodes into a real request, that its
//! schema is valid, and that `read_only` agrees with the daemon's classification.

use serde_json::{Value, json};

/// How a tool reaches the daemon.
#[derive(Debug, Clone, Copy)]
pub enum Call {
    /// One daemon request: the arguments (shaped by `shape`, with `fixed` string fields added)
    /// are its params.
    Request {
        method: &'static str,
        shape: Option<fn(Value) -> Result<Value, String>>,
        fixed: &'static [(&'static str, &'static str)],
    },
    /// The last lines of a terminal session as text (`session.list`, then the terminal's
    /// snapshot and scrollback).
    SessionOutput,
    /// Diagnostics of the files an agent changed and their direct importers, or of the given
    /// files, or of the whole project (`doc.open`, `lsp.diagnostics`, `fs.search`).
    Diagnostics,
}

/// One tool.
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    /// Agrees with the daemon's classification of the request: reads run without asking.
    pub read_only: bool,
    pub call: Call,
    pub schema: fn() -> Value,
}

impl std::fmt::Debug for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tool").field("name", &self.name).finish()
    }
}

/// Appends what every mutating tool's description says about approval.
macro_rules! ask {
    ($text:literal) => {
        concat!(
            $text,
            " This changes state: depending on the user's Hutan settings the call may wait \
             (minutes, not seconds) for the user to approve it, and the result says so; if the \
             user denies it, it fails and must not be retried unless the user asks."
        )
    };
}

const ULID_EXAMPLE: &str = "01K7F3B5N2Q9W8E4R6T1Y0V3KM";

fn id(description: &str) -> Value {
    json!({"type": "string", "description": description, "examples": [ULID_EXAMPLE]})
}

fn text(description: &str) -> Value {
    json!({"type": "string", "description": description})
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn session_id() -> Value {
    id("The session id (`session_list` shows them).")
}

fn workspace_id(description: &str) -> Value {
    id(description)
}

fn task_id() -> Value {
    id("The task id (`task_list` shows them).")
}

fn path(description: &str) -> Value {
    json!({"type": "string", "description": description, "examples": ["/Users/me/project"]})
}

/// `git.log` takes its `limit` on the wire: 30 when the agent gives none.
fn with_default_limit(mut params: Value) -> Result<Value, String> {
    if let Some(map) = params.as_object_mut() {
        map.entry("limit").or_insert(json!(30));
    }
    Ok(params)
}

/// Every tool, in the order `tools/list` shows them.
pub fn tools() -> &'static [Tool] {
    TOOLS
}

pub fn find(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|tool| tool.name == name)
}

const fn request(method: &'static str) -> Call {
    Call::Request {
        method,
        shape: None,
        fixed: &[],
    }
}

const fn shaped(method: &'static str, shape: fn(Value) -> Result<Value, String>) -> Call {
    Call::Request {
        method,
        shape: Some(shape),
        fixed: &[],
    }
}

const fn fixed(method: &'static str, fixed: &'static [(&'static str, &'static str)]) -> Call {
    Call::Request {
        method,
        shape: None,
        fixed,
    }
}

static TOOLS: &[Tool] = &[
    // ---- sessions ---------------------------------------------------------------------------
    Tool {
        name: "session_list",
        description: "List Hutan's sessions (terminals and agent chats): id, title, state, \
                      working directory, workspace, the agent reported in it. Start here to find \
                      a session id.",
        read_only: true,
        call: request("session.list"),
        schema: || {
            object(
                json!({"workspace_id": workspace_id("Only the sessions of this workspace.")}),
                &[],
            )
        },
    },
    Tool {
        name: "session_create",
        description: ask!(
            "Start a new terminal session, running `command` (default: the user's login \
             shell) in `cwd`. Returns the session with its id; use `session_send_text` to type \
             into it and `session_output` to read it."
        ),
        read_only: false,
        call: shaped("session.create", shape_session_create),
        schema: || {
            object(
                json!({
                    "command": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "argv of the process, e.g. [\"cargo\", \"test\"]; empty or absent runs the login shell.",
                    },
                    "cwd": path("Working directory (absolute path)."),
                    "title": text("Name shown for the session."),
                    "env": {
                        "type": "object",
                        "additionalProperties": {"type": "string"},
                        "description": "Extra environment variables for this process.",
                    },
                    "workspace_id": workspace_id("The workspace to put it in (default: the one of `cwd`, else Home)."),
                }),
                &[],
            )
        },
    },
    Tool {
        name: "session_send_text",
        description: ask!(
            "Type text into a running terminal session, as if the user typed it; with `enter` \
             it also presses Return. The text is not echoed back: read the effect with \
             `session_output`. Not for agent chats (use `chat_send`)."
        ),
        read_only: false,
        call: request("session.send_text"),
        schema: || {
            object(
                json!({
                    "session_id": session_id(),
                    "text": text("What to type."),
                    "enter": {"type": "boolean", "description": "Press Return after the text."},
                }),
                &["session_id", "text"],
            )
        },
    },
    Tool {
        name: "session_output",
        description: "Read the last lines of a terminal session as plain text (scrollback plus \
                      the current screen), oldest first. For agent chats use `chat_log`.",
        read_only: true,
        call: Call::SessionOutput,
        schema: || {
            object(
                json!({
                    "session_id": session_id(),
                    "lines": {"type": "integer", "minimum": 1, "maximum": 2000, "default": 100,
                              "description": "How many lines from the end."},
                }),
                &["session_id"],
            )
        },
    },
    Tool {
        name: "session_stop",
        description: ask!(
            "Stop a session: its process gets SIGHUP, then SIGKILL after a few seconds. The \
             session stays listed (stopped) and can be restarted."
        ),
        read_only: false,
        call: request("session.stop"),
        schema: || object(json!({"session_id": session_id()}), &["session_id"]),
    },
    Tool {
        name: "session_restart",
        description: ask!(
            "Start an ended session again (same id, new process in its last directory)."
        ),
        read_only: false,
        call: request("session.restart"),
        schema: || object(json!({"session_id": session_id()}), &["session_id"]),
    },
    Tool {
        name: "session_rename",
        description: ask!("Change the title shown for a session."),
        read_only: false,
        call: request("session.rename"),
        schema: || {
            object(
                json!({"session_id": session_id(), "title": text("The new title.")}),
                &["session_id", "title"],
            )
        },
    },
    Tool {
        name: "session_kill",
        description: ask!(
            "Kill a session now: SIGKILL for its whole process tree (a chat's agent process and \
             everything below it). Use `session_stop` for a polite stop first; kill only a \
             session that does not stop. The session ends and stays in the list."
        ),
        read_only: false,
        call: request("session.kill"),
        schema: || object(json!({"session_id": session_id()}), &["session_id"]),
    },
    Tool {
        name: "session_close",
        description: ask!(
            "Close a session: it is stopped (as `session_stop`) and removed from the session \
             list; its history stays and `session_reopen` brings it back. A session something \
             runs in (a foreground command, a working agent, a chat turn or approval) is \
             refused with what runs unless `stop_running` is true; only set it when ending that \
             work is intended."
        ),
        read_only: false,
        call: request("session.close"),
        schema: || {
            object(
                json!({
                    "session_id": session_id(),
                    "stop_running": {"type": "boolean", "default": false, "description": "Stop what runs in the session and close it anyway."},
                }),
                &["session_id"],
            )
        },
    },
    Tool {
        name: "session_reopen",
        description: ask!(
            "Bring a closed session back (same id): a terminal starts its command again in its \
             last directory (or resumes its agent), a chat continues its conversation."
        ),
        read_only: false,
        call: request("session.reopen"),
        schema: || object(json!({"session_id": session_id()}), &["session_id"]),
    },
    Tool {
        name: "process_signal",
        description: ask!(
            "Send a signal to one process of a session's process tree (a runaway build, say), \
             without ending the session. `resources_list` shows the pids. Only processes of \
             that session can be signalled."
        ),
        read_only: false,
        call: request("process.signal"),
        schema: || {
            object(
                json!({
                    "session_id": session_id(),
                    "pid": {"type": "integer", "minimum": 1, "description": "A pid from `resources_list`."},
                    "signal": {"type": "string", "enum": ["term", "kill", "int", "hup", "stop", "cont"], "description": "`int` is ^C, `term` asks to quit, `kill` cannot be caught, `stop`/`cont` pause and resume."},
                }),
                &["session_id", "pid", "signal"],
            )
        },
    },
    Tool {
        name: "agent_interrupt",
        description: ask!(
            "Stop what an agent of a session is doing without ending the session: the running \
             turn of a chat, or the ^C a user would type in a terminal agent. With `node_id` \
             (from the session's agent tree) it interrupts one subagent; `force` follows a \
             subagent's SIGINT with SIGTERM after two seconds."
        ),
        read_only: false,
        call: request("agent.interrupt"),
        schema: || {
            object(
                json!({
                    "session_id": session_id(),
                    "node_id": text("A subagent's node id; default: the main agent."),
                    "force": {"type": "boolean", "default": false},
                }),
                &["session_id"],
            )
        },
    },
    // ---- chats ------------------------------------------------------------------------------
    Tool {
        name: "chat_create",
        description: ask!(
            "Open a conversation with another agent as a Hutan session: `agent` is `codex` (the \
             Codex app server), `omp` or `claude` (those agents over ACP) or `direct` (a model \
             provider; needs `provider_id`). Returns the \
             session; send the first message with `chat_send`."
        ),
        read_only: false,
        call: request("chat.create"),
        schema: || {
            object(
                json!({
                    "cwd": path("Directory the agent works in (absolute, must exist)."),
                    "agent": {"type": "string", "enum": ["codex", "omp", "claude", "direct"], "description": "Which agent."},
                    "mode": text("`omp` / `claude` only: a session mode the agent offers (e.g. `plan`)."),
                    "model": text("A model id (default: the agent's default)."),
                    "title": text("Name shown for the session."),
                    "workspace_id": workspace_id("The workspace (default: the one of `cwd`, else Home)."),
                    "approval_policy": {"type": "string", "enum": ["untrusted", "on-request", "never"], "description": "When the agent asks the user before acting (default on-request)."},
                    "sandbox": {"type": "string", "enum": ["read-only", "workspace-write", "danger-full-access"], "description": "What the agent may touch (default workspace-write)."},
                    "provider_id": text("`direct` only: the provider (`hutan provider ls`)."),
                    "system_prompt": text("`direct` only: sent to the model ahead of the conversation."),
                }),
                &["cwd", "agent"],
            )
        },
    },
    Tool {
        name: "chat_send",
        description: ask!(
            "Send a message to an agent chat session; the agent starts a turn. Returns the turn \
             id; read the answer with `chat_log`. Fails while a turn is running."
        ),
        read_only: false,
        call: request("chat.send"),
        schema: || {
            object(
                json!({"session_id": session_id(), "text": text("The message.")}),
                &["session_id", "text"],
            )
        },
    },
    Tool {
        name: "chat_log",
        description: "Read an agent chat session's journal: messages, tool calls, results, oldest \
                      first, the newest `limit` events (page back with `before_seq`). Includes \
                      `last_seq` and `has_more`.",
        read_only: true,
        call: shaped("chat.history", shape_chat_log),
        schema: || {
            object(
                json!({
                    "session_id": session_id(),
                    "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 50},
                    "before_seq": {"type": "integer", "minimum": 1, "description": "Only events older than this seq (the first seq of the previous page)."},
                }),
                &["session_id"],
            )
        },
    },
    Tool {
        name: "resources_list",
        description: "One fresh sample of what Hutan runs: per live session its process tree \
                      (pid, parent, command, CPU %, memory) with totals and the agents found in \
                      it, and `hutand` itself. Use it to find a runaway process (then \
                      `process_signal`) or to see which session is heavy.",
        read_only: true,
        call: request("resources.list"),
        schema: || {
            object(
                json!({"session_id": id("Only this session (default: all live sessions).")}),
                &[],
            )
        },
    },
    // ---- approvals --------------------------------------------------------------------------
    Tool {
        name: "approval_list",
        description: "List the permission requests that wait for the user (including your own \
                      pending requests to control Hutan). Only the user answers them; you cannot.",
        read_only: true,
        call: request("approval.list"),
        schema: || object(json!({"session_id": session_id()}), &[]),
    },
    // ---- usage ------------------------------------------------------------------------------
    Tool {
        name: "usage_summary",
        description: "How much of their limits the agent harnesses (OMP, Claude Code, Codex) report \
                      used (the 5-hour and weekly windows: percent used and when they reset), and \
                      the tokens and cost of the turns Hutan ran today and this week, per harness \
                      and per model provider. A cost is the harness's own estimate. Use it before \
                      starting expensive work. `refresh` asks the harnesses first (a few \
                      seconds); without it the answer is what the daemon already knows.",
        read_only: true,
        call: request("usage.summary"),
        schema: || {
            object(
                json!({"refresh": {"type": "boolean", "description": "Ask the harnesses what is left first (slower)."}}),
                &[],
            )
        },
    },
    // ---- tasks ------------------------------------------------------------------------------
    Tool {
        name: "task_add",
        description: ask!(
            "Create a task in a workspace: a unit of work with optional dependencies (`depends_on`), \
             a placement (main checkout or its own git worktree) and a run (a terminal command, \
             or a Codex chat with the `prompt` as first message). It starts when started with \
             `task_start`, or by itself with `auto_start` once its dependencies are done."
        ),
        read_only: false,
        call: request("task.create"),
        schema: || {
            object(
                json!({
                    "workspace_id": workspace_id("The workspace the task belongs to."),
                    "title": text("One line, at most 200 characters."),
                    "prompt": text("What to do; the first message of a chat run."),
                    "depends_on": {"type": "array", "items": task_id(), "description": "Tasks (same workspace) that must be done first."},
                    "placement": {
                        "type": "object",
                        "properties": {
                            "kind": {"type": "string", "enum": ["main", "worktree"]},
                            "branch": text("`worktree` only; default task/<slug of the title>."),
                            "base": text("`worktree` only: where a new branch starts (default HEAD)."),
                        },
                        "required": ["kind"],
                        "additionalProperties": false,
                    },
                    "run": {
                        "description": "What runs: a terminal command, or a chat with an agent.",
                        "oneOf": [
                            {"type": "object", "properties": {"kind": {"const": "terminal"}, "argv": {"type": "array", "items": {"type": "string"}, "minItems": 1}}, "required": ["kind", "argv"], "additionalProperties": false},
                            {"type": "object", "properties": {"kind": {"const": "chat"}, "agent": {"type": "string", "enum": ["codex"]}}, "required": ["kind", "agent"], "additionalProperties": false},
                        ],
                    },
                    "auto_start": {"type": "boolean", "description": "Start by itself when ready and a slot is free."},
                    "after_policy": {"type": "string", "enum": ["done", "accepted", "merged"], "default": "done", "description": "What each dependency must reach before this task is ready: its run done (default), a person's acceptance, or the merge of its branch."},
                }),
                &["workspace_id", "title"],
            )
        },
    },
    Tool {
        name: "task_list",
        description: "List tasks in dependency order with their state (todo, ready, running, \
                      blocked, done, failed, cancelled), their `delivery` (awaiting_review, \
                      accepted, changes_requested, merged, discarded: where the finished work is), \
                      dependencies and sessions.",
        read_only: true,
        call: request("task.list"),
        schema: || {
            object(
                json!({
                    "workspace_id": workspace_id("Only this workspace's tasks."),
                    "state": {"type": "string", "enum": ["todo", "ready", "running", "blocked", "done", "failed", "cancelled"]},
                }),
                &[],
            )
        },
    },
    Tool {
        name: "task_start",
        description: ask!(
            "Start a ready task now: it runs in a new session (in a worktree when its placement \
             says so). Returns the task and the session."
        ),
        read_only: false,
        call: request("task.start"),
        schema: || object(json!({"id": task_id()}), &["id"]),
    },
    Tool {
        name: "task_review",
        description: ask!(
            "Give a verdict on a finished task: `accept` the work, or `request_changes` with a \
             `note` saying what to change (sent to the task's chat as a user message when its \
             session is still live, and the task runs again). Only a done task whose work was \
             not merged or discarded can be reviewed. Reviewing is the user's decision: ask \
             for it unless they told you to."
        ),
        read_only: false,
        call: request("task.review"),
        schema: || {
            object(
                json!({
                    "id": task_id(),
                    "decision": {"type": "string", "enum": ["accept", "request_changes"]},
                    "note": text("What to change (request_changes), or a comment (accept)."),
                }),
                &["id", "decision"],
            )
        },
    },
    Tool {
        name: "task_done",
        description: ask!("Mark a task done (your word overrides what Hutan saw)."),
        read_only: false,
        call: fixed("task.update", &[("state", "done")]),
        schema: || object(json!({"id": task_id()}), &["id"]),
    },
    Tool {
        name: "task_cancel",
        description: ask!("Give a task up; stops its session when it is running."),
        read_only: false,
        call: fixed("task.update", &[("state", "cancelled")]),
        schema: || object(json!({"id": task_id()}), &["id"]),
    },
    Tool {
        name: "task_reset",
        description: ask!(
            "Run an ended task again: back to todo, earlier sessions stay in its history."
        ),
        read_only: false,
        call: fixed("task.update", &[("state", "todo")]),
        schema: || object(json!({"id": task_id()}), &["id"]),
    },
    // ---- worktrees --------------------------------------------------------------------------
    Tool {
        name: "worktree_new",
        description: ask!(
            "Create a git worktree for a branch (a new branch from `base`, or the existing one) \
             so work can run isolated from the main checkout. Returns its path."
        ),
        read_only: false,
        call: request("worktree.create"),
        schema: || {
            object(
                json!({
                    "workspace_id": workspace_id("The workspace whose repository it is."),
                    "branch": text("Branch name."),
                    "base": text("Where a new branch starts (default HEAD)."),
                    "repo": path("A directory inside the repository (default: the workspace's repository)."),
                    "path": path("Where to create it (absolute, not existing; default beside the repository)."),
                }),
                &["workspace_id", "branch"],
            )
        },
    },
    Tool {
        name: "worktree_list",
        description: "List the git worktrees of a workspace's repositories: branch, path, dirty \
                      or not, live sessions inside.",
        read_only: true,
        call: request("worktree.list"),
        schema: || {
            object(
                json!({"workspace_id": workspace_id("Only this workspace's repositories.")}),
                &[],
            )
        },
    },
    Tool {
        name: "worktree_diff",
        description: "List the files a git checkout (a worktree, or the main checkout) changed \
                      against its base branch: the commits since it branched off plus uncommitted \
                      changes, each file with status (added, modified, deleted, renamed) and \
                      added/removed line counts. Read-only. Use `worktree_file_diff` for one \
                      file's patch. `worktree_list` shows the paths.",
        read_only: true,
        call: request("worktree.diff"),
        schema: || {
            object(
                json!({
                    "path": path("The checkout's directory (absolute, as `worktree_list` shows it)."),
                    "base": text("Branch or commit to compare against (default: the repository's default branch)."),
                    "include_uncommitted": {"type": "boolean", "default": true, "description": "Include staged, unstaged and untracked changes."},
                }),
                &["path"],
            )
        },
    },
    Tool {
        name: "worktree_file_diff",
        description: "The unified diff of one changed file of a checkout (a file `worktree_diff` \
                      listed). Read-only; very large patches are cut at 2 MiB and say so.",
        read_only: true,
        call: request("worktree.file_diff"),
        schema: || {
            object(
                json!({
                    "path": path("The checkout's directory."),
                    "file": text("The file, relative to the checkout, as `worktree_diff` lists it."),
                    "old_path": text("For a renamed file: its old path from `worktree_diff`."),
                    "base": text("Same as in `worktree_diff`."),
                    "include_uncommitted": {"type": "boolean", "default": true},
                }),
                &["path", "file"],
            )
        },
    },
    Tool {
        name: "git_log",
        description: "The commits of a git checkout, or of one file or folder in it, newest \
                      first: hash, author, time, subject. For a file, renames are followed and \
                      each commit says the file's name then. Read-only and bounded: at most 200 \
                      commits a call; when `more` is true, pass `next_before` as `before` for \
                      the next page. `path` is the checkout's directory, or a file or folder \
                      inside it (absolute).",
        read_only: true,
        call: shaped("git.log", with_default_limit),
        schema: || {
            object(
                json!({
                    "path": path("The checkout's directory, or a file or folder in it (absolute)."),
                    "rev": text("A branch, tag or commit to start from (default: HEAD)."),
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 30},
                    "before": text("The page after this commit: the `next_before` of an earlier answer."),
                }),
                &["path"],
            )
        },
    },
    Tool {
        name: "git_blame",
        description: "Which commit last changed each line of a file: runs of consecutive lines \
                      (`line`, `count`, `commit`) and the commits they name (author, time, \
                      subject, the file's name then). Without `rev` the file is the one on disk: \
                      lines not committed yet are `0000000000000000000000000000000000000000`. \
                      Read-only and bounded.",
        read_only: true,
        call: request("git.blame"),
        schema: || {
            object(
                json!({
                    "path": path("The file (absolute), inside a git checkout."),
                    "rev": text("Blame the file as it was at this branch, tag or commit (default: the working tree)."),
                }),
                &["path"],
            )
        },
    },
    Tool {
        name: "worktree_merge",
        description: ask!(
            "Merge the branch checked out in a git worktree into the branch the repository's \
             main checkout has checked out (default: the default branch), with a local commit; \
             nothing is pushed. `strategy`: `merge` (a merge commit), `squash` (one commit) or \
             `rebase` (linear history). Refused, changing nothing, when the main checkout has \
             uncommitted changes, when the target is not checked out there, or when there is \
             nothing to merge; on conflicts it aborts cleanly and lists the conflicting files. \
             It does not remove the worktree (`worktree_rm` does)."
        ),
        read_only: false,
        call: request("worktree.merge"),
        schema: || {
            object(
                json!({
                    "path": path("The worktree's directory (not the main checkout)."),
                    "into": text("Target branch; it must be the one checked out in the main checkout (default: the default branch)."),
                    "strategy": {"type": "string", "enum": ["merge", "squash", "rebase"], "default": "merge"},
                    "message": text("Commit message for `merge` and `squash` (not for `rebase`)."),
                }),
                &["path"],
            )
        },
    },
    Tool {
        name: "worktree_commit",
        description: ask!(
            "Stage everything in a git worktree (tracked changes and untracked files that are \
             not ignored) and commit it with `message`, so a following `worktree_merge` takes \
             it: a merge integrates commits only. Hooks do not run. Refused for the main \
             checkout and when there is nothing to commit."
        ),
        read_only: false,
        call: request("worktree.commit"),
        schema: || {
            object(
                json!({
                    "path": path("The worktree's directory (not the main checkout)."),
                    "message": text("The commit message."),
                }),
                &["path", "message"],
            )
        },
    },
    Tool {
        name: "worktree_rm",
        description: ask!(
            "Remove a git worktree. Refuses the main worktree, one with live sessions, and one \
             with uncommitted changes unless `force`. `delete_branch` also deletes the branch \
             when merged."
        ),
        read_only: false,
        call: request("worktree.remove"),
        schema: || {
            object(
                json!({
                    "path": path("The worktree's directory."),
                    "force": {"type": "boolean", "description": "Remove despite uncommitted changes."},
                    "delete_branch": {"type": "boolean"},
                }),
                &["path"],
            )
        },
    },
    // ---- checkpoints --------------------------------------------------------------------------
    Tool {
        name: "checkpoint_list",
        description: "List the checkpoints of agent turns: what the user can rewind to; you \
                      cannot restore. A checkpoint is the files of a git checkout before and \
                      after one turn of an agent chat (Hutan keeps them in hidden refs; your \
                      index and branches are never touched). Newest first: id, session, turn, \
                      time, files changed, lines added and removed, state (`ok`, `late`, \
                      `failed`, `off`, with the reason) and files that were not captured. \
                      Read-only. `checkpoint_diff` shows what a turn changed.",
        read_only: true,
        call: request("checkpoint.list"),
        schema: || {
            object(
                json!({
                    "session_id": id("Only this chat session's checkpoints (`session_list` shows the sessions)."),
                    "workspace_id": workspace_id("Only the checkpoints of this workspace's (or project's) sessions."),
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50,
                              "description": "How many, newest first."},
                    "before": id("The page after this checkpoint: the `next` of the previous answer."),
                }),
                &[],
            )
        },
    },
    Tool {
        name: "checkpoint_diff",
        description: "What one agent turn changed in the checkout: from a checkpoint's start to \
                      its end, or to the checkout as it is now (`against: \"now\"`), or through \
                      a later checkpoint's turn (`against`: that checkpoint's id). Files with \
                      status (added, modified, deleted), line counts and a binary flag, and the \
                      unified diff unless `stat`. Bounded like `worktree_diff`: 2 MiB of patch \
                      per file, 20 MiB in all, 5000 files; what is cut says `truncated`. This is \
                      what the user can rewind to; you cannot restore. Read-only.",
        read_only: true,
        call: request("checkpoint.diff"),
        schema: || {
            object(
                json!({
                    "checkpoint_id": id("The checkpoint (`checkpoint_list` shows them)."),
                    "against": {"type": "string", "default": "end", "examples": ["end", "now"],
                                "description": "`end` (what the turn changed), `now` (the checkout as it is) or another checkpoint's id."},
                    "path": text("Only this file or directory, relative to the checkout."),
                    "stat": {"type": "boolean", "default": false, "description": "Only the files, no patch."},
                }),
                &["checkpoint_id"],
            )
        },
    },
    Tool {
        name: "checkpoint_preview",
        description: "What restoring the checkout's files to before a checkpoint's turn would do \
                      (through a later checkpoint's turn with `through`), file by file: \
                      `action` (`write`, `delete`, `skip`), `state` (`clean`; `after`, `gap`, \
                      `during`: changed by someone else since, between or during the turns, \
                      left alone unless the user accepts; `uncaptured`: never restored) and \
                      why, plus who else works in the checkout and whether a turn is running. \
                      Nothing is written. Only the user can restore, in Hutan; you cannot \
                      restore. Read-only.",
        read_only: true,
        call: request("checkpoint.preview"),
        schema: || {
            object(
                json!({
                    "checkpoint_id": id("Rewind to before this checkpoint's turn (`checkpoint_list` shows them)."),
                    "through": id("A later checkpoint of the same session: rewind its turn and every one in between too."),
                    "paths": {"type": "array", "items": text("A file or directory, relative to the checkout."), "description": "Only these files or directories."},
                }),
                &["checkpoint_id"],
            )
        },
    },
    // ---- language servers -------------------------------------------------------------------
    Tool {
        name: "diagnostics",
        description: "Errors and warnings the project's language servers (rust-analyzer, \
                      typescript-language-server, gopls, …) report, as `path:line:col: severity: \
                      message`. By default (`scope` `changed`) the files changed in the \
                      checkout you run in (`git status`) plus the files that directly import \
                      them, so you see what your edit broke; `paths` checks exactly those \
                      files; `scope` `project` returns everything the servers know about the \
                      workspace (slow, large: ask for it only when needed). Results are \
                      `stale` until the server is ready: a server that is still indexing, not \
                      installed or not trusted by the user (the user decides that in Hutan, you \
                      cannot) says so instead of reporting no problems. Only the workspace's \
                      own folders are checked. This is not a code search: use `fs_search` or \
                      your own tools to find references. Read-only.",
        read_only: true,
        call: Call::Diagnostics,
        schema: || {
            object(
                json!({
                    "paths": {
                        "type": "array",
                        "items": path("A file to check (absolute, or relative to your working directory)."),
                        "description": "Check exactly these files (at most 40).",
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["changed", "project"],
                        "default": "changed",
                        "description": "`changed`: the files changed in your checkout and their direct importers. `project`: everything the servers know about the workspace.",
                    },
                    "min_severity": {
                        "type": "string",
                        "enum": ["error", "warning", "information", "hint"],
                        "description": "Only problems at least this serious (default: all).",
                    },
                    "wait_secs": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 120,
                        "default": 30,
                        "description": "How long to wait for a server that is still starting.",
                    },
                }),
                &[],
            )
        },
    },
    Tool {
        name: "lsp_status",
        description: "The language servers Hutan runs: for each its state (`starting`, `indexing`, \
                      `ready`, `failed`, `stopped`, `not_installed` with the command that \
                      installs it, `untrusted` until the user trusts the folder), root, open \
                      documents and memory, and the folders the user trusted. Read-only.",
        read_only: true,
        call: request("lsp.status"),
        schema: || {
            object(
                json!({"workspace_id": workspace_id("Only the servers of this workspace.")}),
                &[],
            )
        },
    },
    // ---- workspaces and projects ------------------------------------------------------------
    Tool {
        name: "workspace_list",
        description: "List workspaces and projects: id, name, `kind` (`home`, `project` - a folder \
                      or git repository, made automatically - or `workspace` - a group of \
                      projects the user made), `parent_id` (the workspace a project is in), \
                      the project's folder, pinned, how many sessions are live.",
        read_only: true,
        call: request("workspace.list"),
        schema: || object(json!({}), &[]),
    },
    Tool {
        name: "project_recent",
        description: "List recently opened projects (folders), newest first.",
        read_only: true,
        call: request("workspace.recent"),
        schema: || {
            object(
                json!({"limit": {"type": "integer", "minimum": 1, "maximum": 50, "default": 20}}),
                &[],
            )
        },
    },
    Tool {
        name: "workspace_create",
        description: ask!(
            "Create a workspace: a named group of projects. `paths` are folders whose projects \
             become members (a project is made for a folder that has none); `project_ids` move \
             existing projects in."
        ),
        read_only: false,
        call: fixed("workspace.create", &[("created_by", "agent")]),
        schema: || {
            object(
                json!({
                    "name": text("The workspace's name."),
                    "paths": {"type": "array", "items": path("A folder."), "description": "Folders whose projects join the workspace."},
                    "project_ids": {"type": "array", "items": id("A project id (`workspace_list`, kind `project`)."), "description": "Existing projects to move in."},
                }),
                &["name"],
            )
        },
    },
    Tool {
        name: "workspace_assign",
        description: ask!(
            "Put a project into a workspace, or out of its workspace when `workspace_id` is \
             left out. Both ids come from `workspace_list`."
        ),
        read_only: false,
        call: request("workspace.assign"),
        schema: || {
            object(
                json!({
                    "project_id": id("The project (kind `project`)."),
                    "workspace_id": workspace_id("The workspace (kind `workspace`); omit to ungroup the project."),
                }),
                &["project_id"],
            )
        },
    },
    Tool {
        name: "workspace_delete",
        description: ask!(
            "Delete a workspace (a group). Its projects stay, ungrouped. A workspace with live \
             sessions of its own cannot be deleted."
        ),
        read_only: false,
        call: request("workspace.delete"),
        schema: || {
            object(
                json!({"workspace_id": workspace_id("The workspace (kind `workspace`).")}),
                &["workspace_id"],
            )
        },
    },
    Tool {
        name: "workspace_open",
        description: ask!(
            "Open a folder as a project: finds the workspace that contains it, else creates one \
             (for the git root when inside a repository). Returns the workspace and whether it \
             was created. To show it in the window use `ui_run` with `workbench::OpenWorkspace`."
        ),
        read_only: false,
        call: fixed("workspace.open", &[("created_by", "agent")]),
        schema: || {
            object(
                json!({"path": path("An existing folder (absolute).")}),
                &["path"],
            )
        },
    },
    // ---- notifications ----------------------------------------------------------------------
    Tool {
        name: "notification_list",
        description: "List the pending notification intents: things that need the user (an \
                      approval, a blocked agent, a finished task).",
        read_only: true,
        call: request("notify.list"),
        schema: || object(json!({}), &[]),
    },
    // ---- search -----------------------------------------------------------------------------
    Tool {
        name: "history_search",
        description: "Search past sessions: Hutan terminal blocks and chats, and Claude Code, \
                      Codex and OMP transcripts. Fails with invalid_state while the user has not \
                      enabled the history index.",
        read_only: true,
        call: request("history.search"),
        schema: || {
            object(
                json!({
                    "query": text("Words that must all match; \"quoted phrases\" match as phrases."),
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 20},
                    "sources": {"type": "array", "items": {"type": "string", "enum": ["hutan", "claude", "codex", "omp"]}},
                    "project": text("Only sessions in this directory or below."),
                }),
                &["query"],
            )
        },
    },
    // ---- the window -------------------------------------------------------------------------
    Tool {
        name: "ui_actions",
        description: "List what the Hutan desktop window can do: the actions (the same ones as \
                      its command palette, with their argument schemas) and the settings (key, \
                      type, current value, layers). Use it before `ui_run` or `ui_set_setting`. \
                      Fails with invalid_state when no window is open.",
        read_only: true,
        call: request("ui.actions"),
        schema: || {
            object(
                json!({"window_id": text("Default: the window used most recently.")}),
                &[],
            )
        },
    },
    Tool {
        name: "ui_layout",
        description: "Show the window's layout: preset, docks and their panels, the panes with \
                      their tabs (terminals, chats, panels, with session ids), what is focused \
                      or zoomed, appearance.",
        read_only: true,
        call: request("ui.layout"),
        schema: || {
            object(
                json!({"window_id": text("Default: the window used most recently.")}),
                &[],
            )
        },
    },
    Tool {
        name: "ui_run",
        description: ask!(
            "Run an action in the Hutan window, e.g. `workbench::SplitRight`, \
             `workbench::ApplyPreset` with {\"name\": \"focus\"}, `workbench::OpenSession` with \
             {\"id\": \"<session id>\"}. `ui_actions` lists the ids and each one's `args_schema`."
        ),
        read_only: false,
        call: request("ui.run"),
        schema: || {
            object(
                json!({
                    "action": text("The action id from `ui_actions`."),
                    "args": {"type": "object", "description": "Arguments matching the action's args_schema."},
                    "window_id": text("Default: the window used most recently."),
                }),
                &["action"],
            )
        },
    },
    Tool {
        name: "open_file",
        description: ask!(
            "Show the user a file in the Hutan window: a file tab (or the tab that already shows \
             it) at a line and column, so you can point at the exact spot. The path must be \
             absolute and inside the workspace's folders. The same as `ui_run` with \
             `workbench::OpenFile`."
        ),
        read_only: false,
        call: shaped("ui.run", shape_open_file),
        schema: || {
            object(
                json!({
                    "path": path("Absolute path of the file."),
                    "line": {"type": "integer", "minimum": 1, "description": "1-based line to show."},
                    "column": {"type": "integer", "minimum": 1, "description": "1-based column (characters) on that line."},
                    "window_id": text("Default: the window used most recently."),
                }),
                &["path"],
            )
        },
    },
    // ---- Mawas's memory ---------------------------------------------------------------------
    Tool {
        name: "memory_list",
        description: "List what Hutan remembers about the user because they approved it: profile \
                      entries (about the user), facts (environments, projects) and notes (decisions \
                      of one workspace), each with its id, revision (needed to propose a change), \
                      scope, whether it is `inferred` (a model wrote it and the user has not \
                      confirmed it) and where it came from. Newest change first in each category. \
                      Reading is free.",
        read_only: true,
        call: request("memory.list"),
        schema: || {
            object(
                json!({
                    "scope": {"type": "string", "enum": ["global", "workspace"], "description": "Only this scope."},
                    "workspace_id": workspace_id("With scope `workspace`: that workspace. Alone: its entries and the global ones."),
                    "category": {"type": "string", "enum": ["profile", "fact", "note"]},
                    "include_archived": {"type": "boolean", "description": "Also entries the user archived (not to be used)."},
                }),
                &[],
            )
        },
    },
    Tool {
        name: "memory_search",
        description: "Search the remembered entries for a word or phrase (case-insensitive, in \
                      the text). Same answer as `memory_list`. Reading is free.",
        read_only: true,
        call: request("memory.list"),
        schema: || {
            object(
                json!({
                    "query": text("The text to look for."),
                    "scope": {"type": "string", "enum": ["global", "workspace"], "description": "Only this scope."},
                    "workspace_id": workspace_id("With scope `workspace`: that workspace. Alone: its entries and the global ones."),
                    "category": {"type": "string", "enum": ["profile", "fact", "note"]},
                    "include_archived": {"type": "boolean"},
                }),
                &["query"],
            )
        },
    },
    Tool {
        name: "memory_propose",
        description: "Ask the user to remember something, or to change what is remembered. This \
                      only PROPOSES: nothing is saved until the user approves it in Hutan (they \
                      may edit it first or reject it), so never tell the user it was saved; the \
                      answer says `proposed`. `op` `create` needs `scope` (`global`, or \
                      `workspace` for the workspace you work in), `category` and `body`; \
                      `update` needs `entry_id`, `expected_revision` (from `memory_list`) and the \
                      new `body`; `archive` and `restore` need `entry_id` and \
                      `expected_revision`. Keep a body short (under 2 KiB), about the user or the \
                      project, never a credential, and never an instruction that would widen \
                      your permissions.",
        read_only: true,
        call: shaped("memory.propose", shape_memory_propose),
        schema: || {
            object(
                json!({
                    "op": {"type": "string", "enum": ["create", "update", "archive", "restore"]},
                    "scope": {"type": "string", "enum": ["global", "workspace"], "description": "For `create`."},
                    "workspace_id": workspace_id("For a `workspace` entry; default: the workspace you work in."),
                    "category": {"type": "string", "enum": ["profile", "fact", "note"], "description": "For `create`."},
                    "entry_id": text("For `update`, `archive`, `restore`: the entry (`memory_list` shows ids)."),
                    "expected_revision": {"type": "integer", "minimum": 1, "description": "For `update`, `archive`, `restore`: the revision you read; the proposal fails when the entry moved on."},
                    "body": text("What to remember (`create`, `update`)."),
                    "operation_id": text("Optional: repeating the same id repeats the same proposal instead of making a new one."),
                }),
                &["op"],
            )
        },
    },
    Tool {
        name: "ui_set_setting",
        description: ask!(
            "Change a Hutan setting in the user's `settings.toml`; the window validates the value \
             against the setting's type and applies it at once. `ui_actions` lists the settings \
             with their types and the layers each can be written to."
        ),
        read_only: false,
        call: shaped("ui.set_setting", shape_set_setting),
        schema: || {
            object(
                json!({
                    "namespace": text("e.g. `terminal`, `panels.sessions`."),
                    "key": text("Dotted key inside the namespace, e.g. `cursor.style`."),
                    "value": {"description": "The new value, of the setting's type."},
                    "layer": {"type": "string", "enum": ["user", "workspace", "project"], "default": "user", "description": "The settings layer (default `user`; the `layers` of a setting in `ui_actions` say which it takes)."},
                    "window_id": text("Default: the window used most recently."),
                }),
                &["namespace", "key", "value"],
            )
        },
    },
];

/// `operation_id` defaults to a fresh id: a model need not invent one.
fn shape_memory_propose(args: Value) -> Result<Value, String> {
    let Value::Object(mut args) = args else {
        return Err("arguments must be an object".to_owned());
    };
    args.entry("operation_id")
        .or_insert_with(|| json!(ulid::Ulid::generate().to_string()));
    Ok(Value::Object(args))
}

/// `layer` defaults to `user`.
fn shape_set_setting(args: Value) -> Result<Value, String> {
    let Value::Object(mut args) = args else {
        return Err("arguments must be an object".to_owned());
    };
    args.entry("layer").or_insert_with(|| json!("user"));
    Ok(Value::Object(args))
}

fn shape_session_create(args: Value) -> Result<Value, String> {
    let Value::Object(mut args) = args else {
        return Err("arguments must be an object".to_owned());
    };
    let mut params = serde_json::Map::new();
    if let Some(workspace) = args.remove("workspace_id") {
        params.insert("workspace_id".to_owned(), workspace);
    }
    params.insert("kind".to_owned(), json!("terminal"));
    for key in ["command", "cwd", "env", "title"] {
        if let Some(value) = args.remove(key) {
            params.insert(key.to_owned(), value);
        }
    }
    if let Some(unknown) = args.keys().next() {
        return Err(format!("unknown argument `{unknown}`"));
    }
    Ok(Value::Object(params))
}

/// `chat_log` defaults to 50 events: the protocol's 200 are more than a model wants to read.
fn shape_chat_log(args: Value) -> Result<Value, String> {
    let Value::Object(mut args) = args else {
        return Err("arguments must be an object".to_owned());
    };
    args.entry("limit").or_insert(json!(50));
    Ok(Value::Object(args))
}

/// `open_file {path, line?, column?, window_id?}` is `ui.run workbench::OpenFile {path, line?,
/// column?}`.
fn shape_open_file(args: Value) -> Result<Value, String> {
    let Value::Object(mut args) = args else {
        return Err("arguments must be an object".to_owned());
    };
    let mut run = serde_json::Map::new();
    let mut open = serde_json::Map::new();
    for key in ["path", "line", "column"] {
        if let Some(value) = args.remove(key) {
            open.insert(key.to_owned(), value);
        }
    }
    if !open.contains_key("path") {
        return Err("`path` is required".to_owned());
    }
    run.insert("action".to_owned(), json!("workbench::OpenFile"));
    run.insert("args".to_owned(), Value::Object(open));
    if let Some(window) = args.remove("window_id") {
        run.insert("window_id".to_owned(), window);
    }
    if let Some(unknown) = args.keys().next() {
        return Err(format!("unknown argument `{unknown}`"));
    }
    Ok(Value::Object(run))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A document made of the tools' schemas is valid JSON Schema (draft 2020-12 vocabulary as
    /// far as the structure goes): every schema is an object schema, `required` names declared
    /// properties, and nested schemas have a known `type` or a composition keyword.
    fn check_schema(path: &str, schema: &Value) {
        let Some(object) = schema.as_object() else {
            panic!("{path}: a schema is an object, got {schema}");
        };
        const TYPES: [&str; 7] = [
            "object", "array", "string", "integer", "number", "boolean", "null",
        ];
        if let Some(kind) = object.get("type") {
            let kinds: Vec<&str> = match kind {
                Value::String(kind) => vec![kind.as_str()],
                Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).collect(),
                other => panic!("{path}: bad type {other}"),
            };
            assert!(!kinds.is_empty(), "{path}: empty type");
            for kind in kinds {
                assert!(TYPES.contains(&kind), "{path}: unknown type `{kind}`");
            }
        }
        if let Some(properties) = object.get("properties") {
            let properties = properties
                .as_object()
                .unwrap_or_else(|| panic!("{path}: properties is an object"));
            for (name, property) in properties {
                check_schema(&format!("{path}.{name}"), property);
            }
            if let Some(required) = object.get("required") {
                for name in required.as_array().expect("required is an array") {
                    let name = name.as_str().expect("required names are strings");
                    assert!(
                        properties.contains_key(name),
                        "{path}: required `{name}` is not a property"
                    );
                }
            }
        }
        if let Some(items) = object.get("items") {
            check_schema(&format!("{path}[]"), items);
        }
        for keyword in ["oneOf", "anyOf", "allOf"] {
            if let Some(variants) = object.get(keyword) {
                for (i, variant) in variants
                    .as_array()
                    .expect("composition is an array")
                    .iter()
                    .enumerate()
                {
                    check_schema(&format!("{path}.{keyword}[{i}]"), variant);
                }
            }
        }
        if let Some(extra) = object.get("additionalProperties")
            && extra.is_object()
        {
            check_schema(&format!("{path}.*"), extra);
        }
    }

    #[test]
    fn open_file_is_ui_run_of_workbench_open_file() {
        let tool = find("open_file").unwrap();
        assert!(!tool.read_only, "showing a file changes the window");
        let shaped = shape_open_file(json!({
            "path": "/w/src/lib.rs",
            "line": 42,
            "column": 7,
            "window_id": "window-1",
        }))
        .unwrap();
        assert_eq!(
            shaped,
            json!({
                "action": "workbench::OpenFile",
                "args": {"path": "/w/src/lib.rs", "line": 42, "column": 7},
                "window_id": "window-1",
            })
        );
        assert!(shape_open_file(json!({"line": 1})).is_err());
        assert!(shape_open_file(json!({"path": "/a", "mode": "x"})).is_err());
    }

    #[test]
    fn names_are_unique_and_schemas_are_object_schemas() {
        let mut seen = std::collections::BTreeSet::new();
        for tool in tools() {
            assert!(seen.insert(tool.name), "duplicate tool {}", tool.name);
            assert!(
                tool.name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_'),
                "{}",
                tool.name
            );
            let schema = (tool.schema)();
            assert_eq!(schema["type"], "object", "{}", tool.name);
            check_schema(tool.name, &schema);
            assert!(
                tool.description.len() > 40,
                "{}: describe it for a model",
                tool.name
            );
        }
    }

    /// An example argument object for `schema`: required properties only, from `examples`,
    /// `enum`, `const` or a value of the declared type.
    fn example(schema: &Value) -> Value {
        if let Some(first) = schema.get("examples").and_then(|e| e.get(0)) {
            return first.clone();
        }
        if let Some(first) = schema.get("enum").and_then(|e| e.get(0)) {
            return first.clone();
        }
        if let Some(constant) = schema.get("const") {
            return constant.clone();
        }
        if let Some(first) = schema.get("oneOf").and_then(|v| v.get(0)) {
            return example(first);
        }
        match schema.get("type").and_then(Value::as_str) {
            Some("object") => {
                let mut out = serde_json::Map::new();
                let properties = schema.get("properties").and_then(Value::as_object);
                for name in schema
                    .get("required")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    if let Some(property) = properties.and_then(|p| p.get(name)) {
                        out.insert(name.to_owned(), example(property));
                    }
                }
                Value::Object(out)
            }
            Some("array") => {
                let item = schema.get("items").map_or(Value::Null, example);
                Value::Array(vec![item])
            }
            Some("integer") | Some("number") => json!(1),
            Some("boolean") => json!(true),
            Some("string") => json!("x"),
            _ => json!("x"),
        }
    }

    pub(crate) fn params_for(tool: &Tool) -> Option<(&'static str, Value)> {
        let Call::Request {
            method,
            shape,
            fixed,
        } = tool.call
        else {
            return None;
        };
        let mut params = example(&(tool.schema)());
        if let Some(shape) = shape {
            params = shape(params).expect("the example shapes");
        }
        if let Value::Object(map) = &mut params {
            for (key, value) in fixed {
                map.insert((*key).to_owned(), json!(value));
            }
        }
        Some((method, params))
    }

    #[test]
    fn every_tool_maps_to_a_real_daemon_request() {
        for tool in tools() {
            let Some((method, params)) = params_for(tool) else {
                continue;
            };
            let request = hutan_protocol::Request::decode(method, params.clone())
                .unwrap_or_else(|e| panic!("{}: {method} {params}: {}", tool.name, e.message));
            let (wire, _) = request.encode().unwrap();
            assert_eq!(wire, method, "{}", tool.name);
        }
    }

    #[test]
    fn read_only_agrees_with_the_daemons_classification() {
        use hutan_daemon::control::Access;
        for tool in tools() {
            let Some((method, params)) = params_for(tool) else {
                // `session_output` reads sessions and terminals.
                assert!(tool.read_only, "{}", tool.name);
                continue;
            };
            let request = hutan_protocol::Request::decode(method, params).unwrap();
            let access = hutan_daemon::router::classify(&request);
            assert_ne!(
                access,
                Access::UserOnly,
                "{}: an agent cannot use a user-only request",
                tool.name
            );
            assert_eq!(
                tool.read_only,
                access == Access::Read,
                "{}: read_only disagrees with the daemon's classification of {method}",
                tool.name
            );
        }
    }

    #[test]
    fn the_brief_s_tool_families_are_all_there() {
        for name in [
            "session_list",
            "session_create",
            "session_send_text",
            "session_output",
            "session_stop",
            "session_restart",
            "session_rename",
            "session_close",
            "session_reopen",
            "chat_create",
            "chat_send",
            "chat_log",
            "approval_list",
            "usage_summary",
            "task_add",
            "task_list",
            "task_start",
            "task_done",
            "task_cancel",
            "task_reset",
            "task_review",
            "worktree_commit",
            "worktree_new",
            "worktree_list",
            "worktree_diff",
            "worktree_file_diff",
            "git_log",
            "git_blame",
            "worktree_merge",
            "worktree_rm",
            "workspace_list",
            "workspace_create",
            "workspace_assign",
            "workspace_delete",
            "workspace_open",
            "notification_list",
            "history_search",
            "ui_actions",
            "ui_layout",
            "ui_run",
            "open_file",
            "ui_set_setting",
            "memory_list",
            "memory_search",
            "memory_propose",
            "resources_list",
            "session_kill",
            "process_signal",
            "agent_interrupt",
            "checkpoint_list",
            "checkpoint_diff",
            "checkpoint_preview",
            "diagnostics",
            "lsp_status",
        ] {
            assert!(find(name).is_some(), "missing tool {name}");
        }
        assert!(
            tools().iter().all(|t| t.name != "approval_respond"),
            "agents never answer approvals"
        );
        // Checkpoints are read: an agent never restores the user's files (ADR 0019).
        for name in ["checkpoint_restore", "checkpoint_undo"] {
            assert!(find(name).is_none(), "{name}: agents cannot restore");
        }
        for name in ["checkpoint_list", "checkpoint_diff", "checkpoint_preview"] {
            let tool = find(name).unwrap();
            assert!(tool.read_only, "{name}");
            assert!(
                tool.description.contains("you cannot restore"),
                "{name}: {}",
                tool.description
            );
        }
        // Putting a folder under version control is the user's alone (`project.git_init`).
        assert!(
            tools()
                .iter()
                .all(|t| t.name != "project_git_init" && t.name != "project_git-init"),
            "agents cannot start tracking a folder"
        );
        // Language servers: an agent reads their findings; trusting a folder (which lets a
        // server run the project's build scripts) is the user's alone.
        for name in ["lsp_trust", "lsp_restart", "lsp_stop"] {
            assert!(find(name).is_none(), "{name}: not an agent's tool");
        }
        for name in ["diagnostics", "lsp_status"] {
            let tool = find(name).unwrap();
            assert!(tool.read_only, "{name}");
        }
        assert!(find("diagnostics").unwrap().description.contains("cannot"));
        // Memory has no direct write tool for an agent: it can only propose.
        for name in [
            "memory_write",
            "memory_update",
            "memory_archive",
            "memory_restore",
            "memory_resolve",
        ] {
            assert!(
                find(name).is_none(),
                "{name}: an agent never writes memory directly"
            );
        }
    }
}
