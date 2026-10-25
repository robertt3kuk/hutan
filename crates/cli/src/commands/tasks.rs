//! `hutan task add|ls|start|done|review|cancel|rm|show`: tasks with dependencies (protocol
//! 0.16) and their delivery (protocol 0.25, `docs/orchestration.md` § Tasks).

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use hutan_client::Client;
use hutan_client::time::local_time_text;
use hutan_core::{
    AfterPolicy, DeliveryState, ReviewDecision, Task, TaskId, TaskPlacement, TaskPlacementKind,
    TaskRun, TaskState, WorkspaceId,
};
use hutan_protocol::messages::{SessionInfo, WorkspaceInfo};
use hutan_protocol::task::{TaskCreate, TaskList, TaskUpdate};
use serde_json::json;
use unicode_width::UnicodeWidthStr;

use super::Ctx;
use super::worktrees::no_workspace;
use crate::args::{
    AfterPolicyArg, ReviewArg, TaskAddArgs, TaskCommand, TaskLsArgs, TaskReviewArgs, TaskRmArgs,
    TaskTargetArgs,
};
use crate::error::CliError;

/// Shortest task id prefix accepted as a target.
const MIN_PREFIX: usize = 4;

impl Ctx {
    pub(super) async fn task(&self, command: TaskCommand) -> Result<(), CliError> {
        match command {
            TaskCommand::Add(args) => self.task_add(args).await,
            TaskCommand::Ls(args) => self.task_ls(args).await,
            TaskCommand::Start(args) => self.task_start(args).await,
            TaskCommand::Done(args) => self.task_set_state(args, TaskState::Done).await,
            TaskCommand::Review(args) => self.task_review(args).await,
            TaskCommand::Cancel(args) => self.task_set_state(args, TaskState::Cancelled).await,
            TaskCommand::Reset(args) => self.task_set_state(args, TaskState::Todo).await,
            TaskCommand::Rm(args) => self.task_rm(args).await,
            TaskCommand::Show(args) => self.task_show(args).await,
        }
    }

    async fn task_add(&self, args: TaskAddArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let (workspace, _) = self
            .wt_scope(&client, args.workspace)
            .await?
            .ok_or_else(no_workspace)?;
        let mut depends_on = Vec::new();
        if !args.after.is_empty() {
            let tasks = self.all_tasks(&client).await?;
            for target in &args.after {
                let found = resolve_task(&tasks, target, Some(workspace))?;
                if found.workspace_id != workspace {
                    return Err(CliError::Usage(format!(
                        "task `{}` belongs to another workspace",
                        found.title
                    )));
                }
                depends_on.push(found.id);
            }
        }
        let chat_agent = args
            .agent
            .clone()
            .or_else(|| args.codex.then(|| "codex".to_owned()));
        let run = if let Some(agent) = chat_agent {
            Some(TaskRun::Chat {
                contain: None,
                agent,
                model: args.model.clone(),
                account: args.account.clone(),
                safety: args.safety,
            })
        } else if args.argv.is_empty() {
            None
        } else {
            Some(TaskRun::Terminal {
                contain: None,
                argv: args.argv,
            })
        };
        let placement = args.worktree.map(|branch| TaskPlacement {
            kind: TaskPlacementKind::Worktree,
            branch: (!branch.is_empty()).then_some(branch),
            base: args.base,
        });
        let task = client
            .task_create(TaskCreate {
                workspace_id: workspace,
                title: args.title,
                prompt: args.prompt,
                depends_on,
                placement,
                run,
                auto_start: args.auto,
                after_policy: match args.after_policy {
                    AfterPolicyArg::Done => AfterPolicy::Done,
                    AfterPolicyArg::Accepted => AfterPolicy::Accepted,
                    AfterPolicyArg::Merged => AfterPolicy::Merged,
                },
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&json!({ "task": task }), || task.id.to_string())
    }

    async fn task_ls(&self, args: TaskLsArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let scope = self.wt_scope(&client, args.workspace).await?.map(|s| s.0);
        let all = self.all_tasks(&client).await?;
        let ids = short_ids(&all);
        let tasks: Vec<Task> = all
            .into_iter()
            .filter(|task| scope.is_none_or(|ws| task.workspace_id == ws))
            .collect();
        let list = TaskList { tasks };
        self.print(&list, || task_tree(&list.tasks, &ids))
    }

    async fn task_start(&self, args: TaskTargetArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let task = self.target(&client, &args).await?;
        let started = client.task_start(task.id).await.map_err(|e| self.fail(e))?;
        if !self.json {
            for warning in &started.warnings {
                eprintln!("warning: {warning}");
            }
        }
        self.print(&started, || {
            let place = started
                .session
                .cwd
                .as_ref()
                .map_or_else(String::new, |cwd| format!(" in {}", cwd.display()));
            format!(
                "started `{}`: session {}{place}",
                started.task.title, started.session.id
            )
        })
    }

    async fn task_set_state(&self, args: TaskTargetArgs, state: TaskState) -> Result<(), CliError> {
        let client = self.connect().await?;
        let task = self.target(&client, &args).await?;
        let updated = client
            .task_update(TaskUpdate {
                id: task.id,
                title: None,
                prompt: None,
                depends_on: None,
                placement: None,
                run: None,
                auto_start: None,
                after_policy: None,
                state: Some(state),
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&json!({ "task": updated }), || {
            format!("`{}` is {}", updated.title, updated.state)
        })
    }

    async fn task_review(&self, args: TaskReviewArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let task = self
            .target(
                &client,
                &TaskTargetArgs {
                    target: args.target,
                    workspace: args.workspace,
                },
            )
            .await?;
        let decision = match args.decision {
            ReviewArg::Accept => ReviewDecision::Accept,
            ReviewArg::Changes => ReviewDecision::RequestChanges,
        };
        let reviewed = client
            .task_review(task.id, decision, args.note)
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&reviewed, || {
            let task = &reviewed.task;
            match (decision, reviewed.session) {
                (ReviewDecision::Accept, _) => format!("`{}` is accepted", task.title),
                (ReviewDecision::RequestChanges, Some(session)) => format!(
                    "changes requested on `{}`: the note went to session {session}, which works on it again",
                    task.title
                ),
                (ReviewDecision::RequestChanges, None) => match task.change_request() {
                    Some(_) => format!(
                        "changes requested on `{}`: the note is kept on the task (nothing to send it to)",
                        task.title
                    ),
                    None => format!("changes requested on `{}`", task.title),
                },
            }
        })
    }

    async fn task_rm(&self, args: TaskRmArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let task = self
            .target(
                &client,
                &TaskTargetArgs {
                    target: args.target,
                    workspace: args.workspace,
                },
            )
            .await?;
        let removed = client
            .task_delete(task.id, args.cascade)
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&json!({ "removed": removed }), || match removed.len() {
            1 => format!("removed `{}`", task.title),
            n => format!(
                "removed `{}` and {} task(s) that depended on it",
                task.title,
                n - 1
            ),
        })
    }

    async fn task_show(&self, args: TaskTargetArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let found = self.target(&client, &args).await?;
        let task = found.clone();
        let all = self.all_tasks(&client).await?;
        let workspaces = self.workspace_list(&client).await?.workspaces;
        let sessions = client
            .call::<hutan_protocol::messages::SessionList>(
                hutan_protocol::messages::Request::SessionList(
                    hutan_protocol::messages::SessionListFilter {
                        workspace_id: Some(task.workspace_id),
                    },
                ),
            )
            .await
            .map_err(|e| self.fail(e))?
            .sessions;
        let dependants: Vec<&Task> = all
            .iter()
            .filter(|other| other.depends_on.contains(&task.id))
            .collect();
        let mine: Vec<&SessionInfo> = task
            .sessions
            .iter()
            .filter_map(|id| sessions.iter().find(|s| s.id == *id))
            .collect();
        self.print(
            &json!({
                "task": task,
                "depends_on": task.depends_on.iter().filter_map(|id| all.iter().find(|t| t.id == *id)).collect::<Vec<_>>(),
                "dependants": dependants,
                "sessions": mine,
            }),
            || task_details(&task, &all, &dependants, &mine, &workspaces),
        )
    }

    /// The task a command names: `--ws`/`$HUTAN_WORKSPACE`/the current directory only decide
    /// between equal titles.
    async fn target(&self, client: &Client, args: &TaskTargetArgs) -> Result<Task, CliError> {
        let scope = self
            .wt_scope(client, args.workspace.clone())
            .await?
            .map(|s| s.0);
        let tasks = self.all_tasks(client).await?;
        resolve_task(&tasks, &args.target, scope).cloned()
    }

    async fn all_tasks(&self, client: &Client) -> Result<Vec<Task>, CliError> {
        client.task_list(None, None).await.map_err(|e| self.fail(e))
    }
}

/// Picks one task. Tried in order: full id, exact title (among equal titles the ones of
/// `scope`), id prefix of at least [`MIN_PREFIX`] characters. A title or prefix matching
/// several tasks is a usage error that lists the candidates; no match is a plain failure.
fn resolve_task<'a>(
    tasks: &'a [Task],
    target: &str,
    scope: Option<WorkspaceId>,
) -> Result<&'a Task, CliError> {
    if let Ok(id) = target.parse::<TaskId>()
        && let Some(task) = tasks.iter().find(|t| t.id == id)
    {
        return Ok(task);
    }
    let mut by_title: Vec<&Task> = tasks.iter().filter(|t| t.title == target).collect();
    if by_title.len() > 1
        && let Some(scope) = scope
    {
        let narrowed: Vec<&Task> = by_title
            .iter()
            .copied()
            .filter(|t| t.workspace_id == scope)
            .collect();
        if !narrowed.is_empty() {
            by_title = narrowed;
        }
    }
    if !by_title.is_empty() {
        return unique(target, by_title);
    }
    if target.len() >= MIN_PREFIX {
        let prefix = target.to_ascii_uppercase();
        let by_prefix: Vec<&Task> = tasks
            .iter()
            .filter(|t| t.id.to_string().starts_with(&prefix))
            .collect();
        if !by_prefix.is_empty() {
            return unique(target, by_prefix);
        }
    }
    let hint = if target.len() < MIN_PREFIX {
        format!(" (an id prefix needs at least {MIN_PREFIX} characters)")
    } else {
        String::new()
    };
    Err(CliError::Failed(format!(
        "no task matches `{target}`{hint}; `hutan task ls` lists tasks"
    )))
}

fn unique<'a>(target: &str, mut matches: Vec<&'a Task>) -> Result<&'a Task, CliError> {
    match matches.len() {
        1 => Ok(matches.remove(0)),
        n => {
            let mut message = format!("`{target}` matches {n} tasks; use a longer id:");
            for task in matches {
                message.push_str(&format!(
                    "\n  {}  {:<9} {}",
                    task.id,
                    task.state.as_str(),
                    task.title
                ));
            }
            Err(CliError::Usage(message))
        }
    }
}

/// The shortest prefix of every id (at least [`MIN_PREFIX`] characters) that no other task's id
/// starts with: what `hutan task ls` shows and any command accepts.
fn short_ids(tasks: &[Task]) -> HashMap<TaskId, String> {
    let full: Vec<String> = tasks.iter().map(|t| t.id.to_string()).collect();
    tasks
        .iter()
        .zip(&full)
        .map(|(task, id)| {
            let len = (MIN_PREFIX..=id.len())
                .find(|len| {
                    full.iter()
                        .filter(|other| other.starts_with(&id[..*len]))
                        .count()
                        == 1
                })
                .unwrap_or(id.len());
            (task.id, id[..len].to_owned())
        })
        .collect()
}

/// A glyph for every state; the word follows it.
fn glyph(state: TaskState) -> &'static str {
    match state {
        TaskState::Todo => "○",
        TaskState::Ready => "◎",
        TaskState::Running => "▶",
        TaskState::Blocked => "◆",
        TaskState::Done => "✓",
        TaskState::Failed => "✗",
        TaskState::Cancelled => "⊘",
    }
}

/// The delivery as the `ls` column shows it: empty while there is nothing to say.
fn delivery_text(delivery: DeliveryState) -> &'static str {
    match delivery {
        DeliveryState::None => "-",
        DeliveryState::AwaitingReview => "awaiting review",
        DeliveryState::Accepted => "accepted",
        DeliveryState::ChangesRequested => "changes requested",
        DeliveryState::Merged => "merged",
        DeliveryState::Discarded => "discarded",
    }
}

/// `hutan task ls`: every task once, under the task it waits for last; a task that waits for
/// several lists the others in its notes.
fn task_tree(tasks: &[Task], ids: &HashMap<TaskId, String>) -> String {
    if tasks.is_empty() {
        return "no tasks".to_owned();
    }
    let position: HashMap<TaskId, usize> =
        tasks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
    let title = |id: &TaskId| {
        position
            .get(id)
            .map_or_else(|| id.to_string(), |i| tasks[*i].title.clone())
    };
    // The parent is the dependency that comes last in the (dependency-first) order.
    let mut children: HashMap<Option<usize>, Vec<usize>> = HashMap::new();
    for (index, task) in tasks.iter().enumerate() {
        let parent = task
            .depends_on
            .iter()
            .filter_map(|dep| position.get(dep).copied())
            .max();
        children.entry(parent).or_default().push(index);
    }
    // (tree prefix, task index), depth first.
    let mut lines: Vec<(String, usize)> = Vec::with_capacity(tasks.len());
    let mut stack: Vec<(String, bool, usize, bool)> = children
        .get(&None)
        .into_iter()
        .flatten()
        .rev()
        .map(|i| (String::new(), true, *i, true))
        .collect();
    let mut seen = HashSet::new();
    while let Some((indent, last, index, root)) = stack.pop() {
        if !seen.insert(index) {
            continue;
        }
        let connector = if root {
            ""
        } else if last {
            "└─ "
        } else {
            "├─ "
        };
        lines.push((format!("{indent}{connector}"), index));
        let below = if root {
            String::new()
        } else if last {
            format!("{indent}   ")
        } else {
            format!("{indent}│  ")
        };
        let kids = children.get(&Some(index)).cloned().unwrap_or_default();
        let count = kids.len();
        for (n, kid) in kids.into_iter().enumerate().rev() {
            stack.push((below.clone(), n + 1 == count, kid, false));
        }
    }
    let rows: Vec<[String; 5]> = lines
        .iter()
        .map(|(prefix, index)| {
            let task = &tasks[*index];
            let mut notes: Vec<String> = Vec::new();
            let parent = task
                .depends_on
                .iter()
                .filter_map(|dep| position.get(dep).copied())
                .max();
            let others: Vec<String> = task
                .depends_on
                .iter()
                .filter(|dep| position.get(dep).copied() != parent)
                .map(&title)
                .collect();
            if !others.is_empty() {
                notes.push(format!("also after {}", others.join(", ")));
            }
            if task.placement.kind == TaskPlacementKind::Worktree {
                notes.push(match &task.placement.branch {
                    Some(branch) => format!("worktree {branch}"),
                    None => "worktree".to_owned(),
                });
            }
            match &task.run {
                Some(TaskRun::Chat { agent, model, .. }) => notes.push(match model {
                    Some(model) => format!("chat {agent} ({model})"),
                    None => format!("chat {agent}"),
                }),
                Some(TaskRun::Terminal { argv, .. }) => notes.push(format!("$ {}", argv.join(" "))),
                None => {}
            }
            if task.auto_start {
                notes.push("auto".to_owned());
            }
            if task.after_policy != AfterPolicy::Done {
                notes.push(format!("after: {}", task.after_policy));
            }
            if let Some(outcome) = &task.outcome {
                notes.push(format!("({outcome})"));
            }
            [
                format!("{prefix}{} {}", glyph(task.state), task.title),
                task.state.as_str().to_owned(),
                delivery_text(task.delivery).to_owned(),
                ids.get(&task.id)
                    .cloned()
                    .unwrap_or_else(|| task.id.to_string()),
                notes.join("  "),
            ]
        })
        .collect();
    let header = ["TASK", "STATE", "DELIVERY", "ID", "NOTES"].map(str::to_owned);
    let mut widths = [0usize; 4];
    for row in std::iter::once(&header).chain(&rows) {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(UnicodeWidthStr::width(cell.as_str()));
        }
    }
    std::iter::once(&header)
        .chain(&rows)
        .map(|row| {
            let mut line = String::new();
            for (cell, width) in row.iter().zip(widths) {
                let pad = width.saturating_sub(UnicodeWidthStr::width(cell.as_str()));
                line.push_str(cell);
                line.push_str(&" ".repeat(pad + 2));
            }
            line.push_str(&row[4]);
            line.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `awaiting review`, or `changes requested (“rename it”)`; then the history of moves, oldest
/// first.
fn delivery_line(task: &Task) -> String {
    let mut text = delivery_text(task.delivery).to_owned();
    if let Some(note) = task.change_request() {
        text.push_str(&format!(" (“{note}”)"));
    }
    for record in &task.delivery_history {
        text.push_str(&format!(
            "\n           {} ({} ago)  {} by {}{}{}",
            local_time_text(record.at_ms),
            duration(now_ms().saturating_sub(record.at_ms)),
            delivery_text(record.state),
            record.by.as_str(),
            record.commit.as_ref().map_or_else(String::new, |c| format!(
                ", {} {}",
                record.strategy.as_deref().unwrap_or("merge"),
                c.chars().take(7).collect::<String>()
            )),
            record
                .note
                .as_ref()
                .map_or_else(String::new, |n| format!(": {n}"))
        ));
    }
    text
}

/// `hutan task show`.
fn task_details(
    task: &Task,
    all: &[Task],
    dependants: &[&Task],
    sessions: &[&SessionInfo],
    workspaces: &[WorkspaceInfo],
) -> String {
    let mut out = format!("{} {}\n", glyph(task.state), task.title);
    let mut line = |label: &str, value: String| {
        out.push_str(&format!("{label:<11}{value}\n"));
    };
    line("id", task.id.to_string());
    line("state", state_line(task));
    line("delivery", delivery_line(task));
    line(
        "workspace",
        workspaces
            .iter()
            .find(|w| w.id == task.workspace_id)
            .map_or_else(
                || task.workspace_id.to_string(),
                |w| format!("{} ({})", w.name, w.id),
            ),
    );
    line(
        "placement",
        match (
            task.placement.kind,
            &task.placement.branch,
            &task.placement.base,
        ) {
            (TaskPlacementKind::Main, _, _) => "main checkout".to_owned(),
            (TaskPlacementKind::Worktree, branch, base) => format!(
                "worktree, branch {}{}",
                branch.as_deref().unwrap_or("task/<slug of the title>"),
                base.as_ref()
                    .map_or_else(String::new, |b| format!(" from {b}"))
            ),
        },
    );
    line(
        "run",
        match &task.run {
            Some(TaskRun::Terminal { argv, .. }) => format!("terminal: {}", argv.join(" ")),
            Some(TaskRun::Chat { agent, model, .. }) => match model {
                Some(model) => format!("chat with {agent} on {model}"),
                None => format!("chat with {agent}"),
            },
            None => "terminal: the login shell".to_owned(),
        },
    );
    line(
        "auto start",
        if task.auto_start { "yes" } else { "no" }.to_owned(),
    );
    line("wait for", format!("{} dependencies", task.after_policy));
    let named = |ids: &mut dyn Iterator<Item = &Task>| {
        let list: Vec<String> = ids
            .map(|t| format!("{} ({}, {})", t.title, t.state, t.id))
            .collect();
        if list.is_empty() {
            "-".to_owned()
        } else {
            list.join("\n           ")
        }
    };
    line(
        "after",
        named(
            &mut task
                .depends_on
                .iter()
                .filter_map(|id| all.iter().find(|t| t.id == *id)),
        ),
    );
    line("needed by", named(&mut dependants.iter().copied()));
    if sessions.is_empty() {
        line("sessions", "-".to_owned());
    } else {
        let list: Vec<String> = sessions
            .iter()
            .map(|s| {
                format!(
                    "{} ({}{})",
                    s.id,
                    super::sessions::state_text(&s.state),
                    s.cwd
                        .as_ref()
                        .map_or_else(String::new, |cwd| format!(", {}", cwd.display()))
                )
            })
            .collect();
        line("sessions", list.join("\n           "));
    }
    if let Some(prompt) = &task.prompt {
        out.push_str("\nprompt\n");
        for text in prompt.lines() {
            out.push_str(&format!("  {text}\n"));
        }
    }
    out.trim_end().to_owned()
}

/// `done (exited 0), ran 1m 5s, ended 3m ago (2026-10-17 14:31 +0500)`.
fn state_line(task: &Task) -> String {
    let mut text = task.state.as_str().to_owned();
    if let Some(outcome) = &task.outcome {
        text.push_str(&format!(" ({outcome})"));
    }
    if let (Some(start), Some(end)) = (task.started_at_ms, task.ended_at_ms) {
        text.push_str(&format!(", ran {}", duration(end.saturating_sub(start))));
    }
    if let Some(end) = task.ended_at_ms {
        text.push_str(&format!(
            ", ended {} ago ({})",
            duration(now_ms().saturating_sub(end)),
            local_time_text(end)
        ));
    } else if let Some(start) = task.started_at_ms {
        text.push_str(&format!(
            ", started {} ago ({})",
            duration(now_ms().saturating_sub(start)),
            local_time_text(start)
        ));
    }
    text
}

fn duration(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {}s", secs / 60, secs % 60),
        3600..=86_399 => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d {}h", secs / 86_400, secs % 86_400 / 3600),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(title: &str, created: u64, deps: &[&Task], state: TaskState) -> Task {
        Task {
            id: TaskId::new(),
            workspace_id: WorkspaceId::new(),
            title: title.to_owned(),
            prompt: None,
            state,
            depends_on: deps.iter().map(|t| t.id).collect(),
            placement: TaskPlacement::default(),
            run: None,
            auto_start: false,
            sessions: Vec::new(),
            created_at_ms: created,
            started_at_ms: None,
            ended_at_ms: None,
            outcome: None,
            delivery: DeliveryState::None,
            after_policy: AfterPolicy::Done,
            delivery_history: Vec::new(),
        }
    }

    #[test]
    fn a_target_is_an_id_a_title_or_an_id_prefix() {
        let a = task("Build", 1, &[], TaskState::Done);
        let b = task("Build", 2, &[], TaskState::Ready);
        let c = task("Ship", 3, &[&a], TaskState::Todo);
        let tasks = [a.clone(), b.clone(), c.clone()];

        assert_eq!(
            resolve_task(&tasks, &c.id.to_string(), None).unwrap().id,
            c.id
        );
        assert_eq!(resolve_task(&tasks, "Ship", None).unwrap().id, c.id);
        let prefix = c.id.to_string()[..14].to_lowercase();
        assert_eq!(resolve_task(&tasks, &prefix, None).unwrap().id, c.id);

        // Two tasks called Build: a usage error naming both, unless the workspace decides.
        let error = resolve_task(&tasks, "Build", None).unwrap_err();
        assert!(matches!(error, CliError::Usage(_)), "{error:?}");
        assert!(error.to_string().contains(&a.id.to_string()));
        assert!(error.to_string().contains(&b.id.to_string()));
        assert_eq!(
            resolve_task(&tasks, "Build", Some(b.workspace_id))
                .unwrap()
                .id,
            b.id
        );
        assert!(resolve_task(&tasks, "Build", Some(WorkspaceId::new())).is_err());

        // A prefix shared by several, too short a prefix, and nothing.
        let shared = &a.id.to_string()[..4];
        assert!(matches!(
            resolve_task(&tasks, shared, None),
            Err(CliError::Usage(_))
        ));
        let error = resolve_task(&tasks, "01K", None).unwrap_err();
        assert!(matches!(error, CliError::Failed(_)));
        assert!(
            error.to_string().contains("at least 4 characters"),
            "{error}"
        );
        assert!(matches!(
            resolve_task(&tasks, "nothing", None),
            Err(CliError::Failed(_))
        ));
    }

    #[test]
    fn shown_ids_are_the_shortest_that_name_one_task() {
        let tasks: Vec<Task> = (0..4).map(|n| task("t", n, &[], TaskState::Todo)).collect();
        let ids = short_ids(&tasks);
        for t in &tasks {
            let shown = &ids[&t.id];
            assert!(shown.len() >= MIN_PREFIX);
            assert!(t.id.to_string().starts_with(shown.as_str()));
            let others = tasks
                .iter()
                .filter(|o| o.id.to_string().starts_with(shown.as_str()))
                .count();
            assert_eq!(others, 1, "{shown} names one task");
            if shown.len() > MIN_PREFIX {
                let shorter = &shown[..shown.len() - 1];
                assert!(
                    tasks
                        .iter()
                        .filter(|o| o.id.to_string().starts_with(shorter))
                        .count()
                        > 1,
                    "{shown} is the shortest"
                );
            }
        }
    }

    #[test]
    fn the_tree_puts_every_task_under_the_one_it_waits_for_last() {
        let build = task("Build", 1, &[], TaskState::Done);
        let lint = task("Lint", 2, &[], TaskState::Running);
        let test = task("Test", 3, &[&build], TaskState::Ready);
        let docs = task("Docs", 4, &[&build], TaskState::Todo);
        let mut ship = task("Ship", 5, &[&test, &lint], TaskState::Todo);
        ship.auto_start = true;
        let mut tasks = vec![
            build.clone(),
            lint.clone(),
            test.clone(),
            docs.clone(),
            ship.clone(),
        ];
        tasks[2].placement = TaskPlacement {
            kind: TaskPlacementKind::Worktree,
            branch: Some("task/test".into()),
            base: None,
        };
        tasks[1].run = Some(TaskRun::Terminal {
            contain: None,
            argv: vec!["cargo".into(), "clippy".into()],
        });
        tasks[0].outcome = Some("exited 0".into());
        let ids = short_ids(&tasks);
        let text = task_tree(&tasks, &ids);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 6, "{text}");
        assert!(lines[0].starts_with("TASK") && lines[0].contains("STATE"));
        let cut = lines[0].find("STATE").unwrap();
        let shapes: Vec<String> = lines[1..]
            .iter()
            .map(|l| {
                l.chars()
                    .take(cut)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            shapes,
            [
                "✓ Build",
                "├─ ◎ Test",
                "│  └─ ○ Ship",
                "└─ ○ Docs",
                "▶ Lint",
            ],
            "{text}"
        );
        assert!(lines[1].contains("(exited 0)"), "{text}");
        assert!(lines[2].contains("worktree task/test"), "{text}");
        assert!(
            lines[3].contains("also after Lint") && lines[3].contains("auto"),
            "{text}"
        );
        assert!(lines[5].contains("$ cargo clippy"), "{text}");
        assert_eq!(task_tree(&[], &HashMap::new()), "no tasks");
    }

    #[test]
    fn details_say_where_a_task_runs_what_it_needs_and_what_it_blocks() {
        let build = task("Build", 1, &[], TaskState::Done);
        let mut ship = task("Ship it", 2, &[&build], TaskState::Failed);
        ship.prompt = Some("Release 1.0\nand tag it".into());
        ship.placement = TaskPlacement {
            kind: TaskPlacementKind::Worktree,
            branch: None,
            base: Some("main".into()),
        };
        ship.run = Some(TaskRun::Chat {
            contain: None,
            agent: "codex".into(),
            model: None,
            account: None,
            safety: None,
        });
        ship.outcome = Some("turn failed: boom".into());
        ship.started_at_ms = Some(1_000);
        ship.ended_at_ms = Some(66_000);
        let after = task("After", 3, &[&ship], TaskState::Todo);
        let all = [build.clone(), ship.clone(), after.clone()];
        let text = task_details(&ship, &all, &[&after], &[], &[]);
        assert!(text.starts_with("✗ Ship it\n"), "{text}");
        assert!(
            text.contains("failed (turn failed: boom), ran 1m 5s, ended "),
            "{text}"
        );
        assert!(
            text.contains("worktree, branch task/<slug of the title> from main"),
            "{text}"
        );
        assert!(text.contains("chat with codex"), "{text}");
        assert!(
            text.contains(&format!("Build (done, {})", build.id)),
            "{text}"
        );
        assert!(
            text.contains(&format!("After (todo, {})", after.id)),
            "{text}"
        );
        assert!(
            text.ends_with("prompt\n  Release 1.0\n  and tag it"),
            "{text}"
        );
    }

    #[test]
    fn durations_are_short() {
        assert_eq!(duration(0), "0s");
        assert_eq!(duration(59_999), "59s");
        assert_eq!(duration(65_000), "1m 5s");
        assert_eq!(duration(3_661_000), "1h 1m");
        assert_eq!(duration(90_000_000), "1d 1h");
    }
}
