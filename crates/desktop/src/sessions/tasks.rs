//! The desktop's copy of the daemon's tasks (protocol 0.16) and what the Tasks panel draws from
//! it: the dependency tree, state glyphs, row actions, elapsed time. No GPUI here.
//!
//! The daemon owns tasks. The store lists them once per connection (`task.list`) and then
//! follows `task.updated` / `task.removed`; [`TaskBook`] merges the two so an event that arrives
//! while the list is in flight is not undone by the older list.

use std::collections::{HashMap, HashSet};

use hutan_core::task::{default_branch, topological_order};
use hutan_core::{
    DeliveryState, SessionId, Task, TaskId, TaskPlacementKind, TaskRun, TaskState, WorkspaceId,
};

use super::Tone;
use super::agents::format_duration;
use super::models::ModelNames;

/// Every task the daemon told this window about.
#[derive(Debug, Clone, Default)]
pub struct TaskBook {
    tasks: HashMap<TaskId, Task>,
    /// A `task.list` answered since the connection came up.
    loaded: bool,
    /// Tasks an event changed or removed since the last list was asked for: the list's version
    /// of them is older than the event.
    touched: HashSet<TaskId>,
}

/// The longest prompt a tooltip shows.
const PROMPT_TIP_CHARS: usize = 600;

impl TaskBook {
    /// A `task.list` is about to be asked for.
    pub fn start_loading(&mut self) {
        self.touched.clear();
    }

    /// The answer of `task.list`: adds what is missing and replaces what no event changed
    /// since; forgets tasks the daemon no longer lists.
    pub fn merge_listed(&mut self, listed: Vec<Task>) {
        let ids: HashSet<TaskId> = listed.iter().map(|t| t.id).collect();
        self.tasks
            .retain(|id, _| ids.contains(id) || self.touched.contains(id));
        for task in listed {
            if !self.touched.contains(&task.id) {
                self.tasks.insert(task.id, task);
            }
        }
        self.loaded = true;
    }

    /// `task.updated`.
    pub fn updated(&mut self, task: Task) {
        self.touched.insert(task.id);
        self.tasks.insert(task.id, task);
    }

    /// `task.removed`.
    pub fn removed(&mut self, id: TaskId) {
        self.touched.insert(id);
        self.tasks.remove(&id);
    }

    pub fn get(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(&id)
    }

    /// The task whose work the worktree of `branch` holds: the one that still waits for its
    /// delivery, else the latest. A task placed in the main checkout is never found this way.
    pub fn for_branch(&self, branch: &str) -> Option<&Task> {
        self.tasks
            .values()
            .filter(|task| {
                task.placement.kind == TaskPlacementKind::Worktree
                    && placement_label(task).1.as_deref() == Some(branch)
            })
            .max_by_key(|task| {
                (
                    task.state == TaskState::Done && task.delivery.is_open(),
                    task.created_at_ms,
                )
            })
    }

    /// Whether the finished run of `session` waits for a reviewer: its task is `done` and
    /// `awaiting_review`. Such a session counts as a soft *Ready for review* item until the user has
    /// seen it (see [`review_key`](Self::review_key)).
    pub fn awaits_review(&self, session: SessionId) -> bool {
        self.review_key(session).is_some()
    }

    /// What identifies the wait for review of `session`'s task: when its work last became
    /// `awaiting_review`. A later change that needs review again has another key, so having
    /// seen one wait is not having seen the next. `None` while nothing waits.
    pub fn review_key(&self, session: SessionId) -> Option<u64> {
        self.tasks
            .values()
            .filter(|task| {
                task.state == TaskState::Done
                    && task.delivery == DeliveryState::AwaitingReview
                    && task.sessions.last() == Some(&session)
            })
            .map(|task| awaited_at(task).unwrap_or(0))
            .max()
    }

    /// The tasks `here` accepts whose work is still open (`awaiting_review`, `accepted`,
    /// `changes_requested`), the most recent change first: what a Finish delivers.
    pub fn open_work(&self, here: impl Fn(&Task) -> bool) -> Vec<&Task> {
        let mut open: Vec<&Task> = self
            .tasks
            .values()
            .filter(|task| task.delivery.is_open() && here(task))
            .collect();
        open.sort_by_key(|task| {
            std::cmp::Reverse((awaited_at(task).unwrap_or(0), task.created_at_ms))
        });
        open
    }

    /// The prompt `session` was started with when it says more than `title`: the full request
    /// behind a short proposed title, for a tooltip. At most [`PROMPT_TIP_CHARS`] characters.
    pub fn prompt_of(&self, session: SessionId, title: &str) -> Option<String> {
        let task = self
            .tasks
            .values()
            .filter(|task| task.sessions.contains(&session))
            .max_by_key(|task| task.created_at_ms)?;
        let prompt = task.prompt.as_deref()?.trim();
        if prompt.is_empty() || prompt == title.trim() {
            return None;
        }
        let mut tip: String = prompt.chars().take(PROMPT_TIP_CHARS).collect();
        if prompt.chars().count() > PROMPT_TIP_CHARS {
            tip.push('…');
        }
        Some(tip)
    }

    /// Whether `session` ran a task (its delivery says what the session's turns changed).
    pub fn ran_a_task(&self, session: SessionId) -> bool {
        self.tasks
            .values()
            .any(|task| task.sessions.contains(&session))
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// A list answered on this connection.
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    /// Every task, dependencies first, otherwise oldest first.
    pub fn ordered(&self) -> Vec<&Task> {
        topological_order(self.tasks.values())
            .into_iter()
            .filter_map(|id| self.tasks.get(&id))
            .collect()
    }
}

/// What the panel offers to do with a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskAction {
    Start,
    Done,
    Cancel,
    Reset,
    Delete,
}

impl TaskAction {
    pub fn label(self) -> &'static str {
        match self {
            TaskAction::Start => "Start",
            TaskAction::Done => "Done",
            TaskAction::Cancel => "Cancel",
            TaskAction::Reset => "Reset",
            TaskAction::Delete => "Delete",
        }
    }

    /// The element-id suffix and the name tests click by.
    pub fn key(self) -> &'static str {
        match self {
            TaskAction::Start => "start",
            TaskAction::Done => "done",
            TaskAction::Cancel => "cancel",
            TaskAction::Reset => "reset",
            TaskAction::Delete => "delete",
        }
    }
}

/// The actions the daemon accepts for a task shown in `state` (`docs/protocol.md` § Tasks):
/// start needs `ready`; done is allowed from everything but `cancelled`; cancel from every live
/// state; reset from every ended one (and `todo`, which it leaves alone); a running task cannot
/// be deleted.
pub fn actions_for(state: TaskState) -> Vec<TaskAction> {
    match state {
        TaskState::Ready => vec![
            TaskAction::Start,
            TaskAction::Done,
            TaskAction::Cancel,
            TaskAction::Delete,
        ],
        TaskState::Todo => vec![TaskAction::Done, TaskAction::Cancel, TaskAction::Delete],
        TaskState::Running | TaskState::Blocked => vec![TaskAction::Done, TaskAction::Cancel],
        TaskState::Failed => vec![TaskAction::Done, TaskAction::Reset, TaskAction::Delete],
        TaskState::Done | TaskState::Cancelled => vec![TaskAction::Reset, TaskAction::Delete],
    }
}

/// Semantic state artwork and tone, independent of CLI text output.
pub fn state_glyph(state: TaskState) -> (crate::icons::Icon, Tone) {
    use crate::icons::Icon;
    match state {
        TaskState::Todo => (Icon::StateTodo, Tone::Muted),
        TaskState::Ready => (Icon::StateReady, Tone::Accent),
        TaskState::Running => (Icon::StateRunning, Tone::Accent),
        TaskState::Blocked => (Icon::StateBlocked, Tone::Warning),
        TaskState::Done => (Icon::StateCompleted, Tone::Success),
        TaskState::Failed => (Icon::StateFailed, Tone::Error),
        TaskState::Cancelled => (Icon::StateCancelled, Tone::Muted),
    }
}

/// When the task's work last became `awaiting_review`: when a turn last changed files.
fn awaited_at(task: &Task) -> Option<u64> {
    task.delivery_history
        .iter()
        .rev()
        .find(|r| r.state == DeliveryState::AwaitingReview)
        .map(|r| r.at_ms)
}

/// The delivery pill of a task, as `hutan task ls` words it; none while there is nothing to
/// review (yet).
pub fn delivery_pill(delivery: DeliveryState) -> Option<(&'static str, Tone)> {
    match delivery {
        DeliveryState::None => None,
        DeliveryState::AwaitingReview => Some(("Awaiting review", Tone::Accent)),
        DeliveryState::Accepted => Some(("Accepted", Tone::Success)),
        DeliveryState::ChangesRequested => Some(("Changes requested", Tone::Warning)),
        DeliveryState::Merged => Some(("Merged", Tone::Success)),
        DeliveryState::Discarded => Some(("Discarded", Tone::Muted)),
    }
}

/// What the panel lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskFilters {
    /// List tasks that are `done` or `cancelled` (failed ones always stay: they need a look,
    /// and so do done ones that wait for a reviewer or whose changes were requested).
    pub show_done: bool,
    /// Only tasks of this container: a project, Home, or a user workspace together with its
    /// member projects (ADR 0014); `None`: every one.
    pub workspace: Option<WorkspaceId>,
}

impl TaskFilters {
    /// `parent`: the user workspace a container belongs to (`WorkspaceInfo.parent_id`).
    fn keeps(&self, task: &Task, parent: &dyn Fn(WorkspaceId) -> Option<WorkspaceId>) -> bool {
        let needs_review = task.state == TaskState::Done
            && matches!(
                task.delivery,
                DeliveryState::AwaitingReview | DeliveryState::ChangesRequested
            );
        self.workspace
            .is_none_or(|w| w == task.workspace_id || parent(task.workspace_id) == Some(w))
            && (self.show_done
                || needs_review
                || !matches!(task.state, TaskState::Done | TaskState::Cancelled))
    }
}

/// One listed task, as the panel draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskRowView {
    pub id: TaskId,
    pub workspace: WorkspaceId,
    pub depth: usize,
    pub title: String,
    pub state: TaskState,
    pub glyph: crate::icons::Icon,
    pub tone: Tone,
    /// `main` or `wt <branch>`.
    pub placement: String,
    /// The branch of a worktree placement, when it is a worktree task.
    pub branch: Option<String>,
    /// `shell`, `$ cargo test`, `codex chat`.
    pub run: String,
    /// Running time, or how long it took; none before it started.
    pub elapsed: Option<String>,
    /// Why it ended.
    pub outcome: Option<String>,
    /// The other tasks it waits for besides its parent in the tree.
    pub also_after: Vec<String>,
    pub auto_start: bool,
    pub actions: Vec<TaskAction>,
    /// The session that runs it, or ran it last.
    pub session: Option<SessionId>,
    /// It has (or had) a checkout worth reviewing: a worktree task, or one that ran.
    pub reviewable: bool,
    /// Where its finished work is (protocol 0.25).
    pub delivery: DeliveryState,
    /// The pill for `delivery`: `Awaiting review`, `Accepted`, …; none while there is nothing
    /// to review.
    pub delivery_label: Option<&'static str>,
    pub delivery_tone: Tone,
    /// What a reviewer asked to change, while it is still to be dealt with.
    pub change_request: Option<String>,
}

/// `wt <branch>` / `main`, the branch for a worktree task (its default when the task names none).
pub fn placement_label(task: &Task) -> (String, Option<String>) {
    match task.placement.kind {
        TaskPlacementKind::Main => ("main".to_owned(), None),
        TaskPlacementKind::Worktree => {
            let branch = task
                .placement
                .branch
                .clone()
                .unwrap_or_else(|| default_branch(&task.title));
            (format!("wt {branch}"), Some(branch))
        }
    }
}

/// The run of a task in a few words; a chat's model by its name when `names` know it.
pub fn run_label(run: Option<&TaskRun>, names: &ModelNames) -> String {
    match run {
        None => "shell".to_owned(),
        Some(TaskRun::Terminal { argv, .. }) => {
            let line = argv.join(" ");
            let mut short: String = line.chars().take(40).collect();
            if line.chars().count() > 40 {
                short.push('…');
            }
            format!("$ {short}")
        }
        Some(TaskRun::Chat { agent, model, .. }) => match model {
            Some(model) => format!("{agent} chat · {}", names.label(agent, model)),
            None => format!("{agent} chat"),
        },
    }
}

fn elapsed(task: &Task, now_ms: u64) -> Option<String> {
    let started = task.started_at_ms?;
    let end = if task.state.is_active() {
        now_ms
    } else {
        task.ended_at_ms?
    };
    Some(format_duration(end.saturating_sub(started)))
}

/// The tasks to list as a dependency tree: every task once, under the dependency it waits for
/// last (the others are named in `also_after`), dependencies before dependants, roots oldest
/// first. A task whose dependencies are all filtered out is a root.
pub fn tree_rows(
    book: &TaskBook,
    filters: TaskFilters,
    parent: &dyn Fn(WorkspaceId) -> Option<WorkspaceId>,
    now_ms: u64,
    names: &ModelNames,
) -> Vec<TaskRowView> {
    let visible: Vec<&Task> = book
        .ordered()
        .into_iter()
        .filter(|t| filters.keeps(t, parent))
        .collect();
    let position: HashMap<TaskId, usize> = visible
        .iter()
        .enumerate()
        .map(|(at, t)| (t.id, at))
        .collect();
    let mut children: HashMap<TaskId, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    let mut also: HashMap<TaskId, Vec<String>> = HashMap::new();
    for (at, task) in visible.iter().enumerate() {
        let mut deps: Vec<TaskId> = task
            .depends_on
            .iter()
            .copied()
            .filter(|d| position.get(d).is_some_and(|p| *p < at))
            .collect();
        deps.sort_by_key(|d| position[d]);
        deps.dedup();
        match deps.pop() {
            Some(parent) => {
                children.entry(parent).or_default().push(at);
                let others: Vec<String> = deps
                    .iter()
                    .filter_map(|d| book.get(*d))
                    .map(|d| d.title.clone())
                    .collect();
                if !others.is_empty() {
                    also.insert(task.id, others);
                }
            }
            None => roots.push(at),
        }
    }
    let mut rows = Vec::with_capacity(visible.len());
    // Depth-first, an explicit stack: the dependency chains of a plan can be long.
    let mut stack: Vec<(usize, usize)> = roots.into_iter().rev().map(|at| (at, 0)).collect();
    while let Some((at, depth)) = stack.pop() {
        let task = visible[at];
        let (glyph, tone) = state_glyph(task.state);
        let (placement, branch) = placement_label(task);
        rows.push(TaskRowView {
            id: task.id,
            workspace: task.workspace_id,
            depth,
            title: task.title.clone(),
            state: task.state,
            glyph,
            tone,
            placement,
            reviewable: branch.is_some() || !task.sessions.is_empty(),
            branch,
            run: run_label(task.run.as_ref(), names),
            elapsed: elapsed(task, now_ms),
            outcome: task.outcome.clone(),
            also_after: also.remove(&task.id).unwrap_or_default(),
            auto_start: task.auto_start,
            actions: actions_for(task.state),
            session: task.sessions.last().copied(),
            delivery: task.delivery,
            delivery_label: delivery_pill(task.delivery).map(|(label, _)| label),
            delivery_tone: delivery_pill(task.delivery).map_or(Tone::Muted, |(_, tone)| tone),
            change_request: task.change_request().map(str::to_owned),
        });
        for child in children
            .remove(&task.id)
            .unwrap_or_default()
            .into_iter()
            .rev()
        {
            stack.push((child, depth + 1));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_core::{AfterPolicy, TaskPlacement};

    fn task(title: &str, age: u64, state: TaskState, deps: &[&Task]) -> Task {
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
            created_at_ms: age,
            started_at_ms: None,
            ended_at_ms: None,
            outcome: None,
            delivery: DeliveryState::None,
            after_policy: AfterPolicy::Done,
            delivery_history: Vec::new(),
        }
    }

    fn book(tasks: &[&Task]) -> TaskBook {
        let mut book = TaskBook::default();
        book.merge_listed(tasks.iter().map(|t| (*t).clone()).collect());
        book
    }

    const ALL: TaskFilters = TaskFilters {
        show_done: true,
        workspace: None,
    };

    fn no_parent(_: WorkspaceId) -> Option<WorkspaceId> {
        None
    }

    #[test]
    fn the_tree_puts_each_task_under_the_dependency_it_waits_for_last() {
        let build = task("Build", 1, TaskState::Done, &[]);
        let lint = task("Lint", 2, TaskState::Ready, &[]);
        let test = task("Test", 3, TaskState::Ready, &[&build]);
        let ship = task("Ship", 4, TaskState::Todo, &[&test, &lint]);
        let rows = tree_rows(
            &book(&[&ship, &test, &lint, &build]),
            ALL,
            &no_parent,
            0,
            &ModelNames::default(),
        );
        let shape: Vec<(&str, usize)> = rows.iter().map(|r| (r.title.as_str(), r.depth)).collect();
        assert_eq!(
            shape,
            [("Build", 0), ("Test", 1), ("Ship", 2), ("Lint", 0)],
            "Ship waits for Test last (Test comes after Lint in dependency order) and names Lint as `also_after`"
        );
        assert_eq!(rows[2].also_after, ["Lint"]);
        assert!(rows[0].also_after.is_empty());
    }

    #[test]
    fn hidden_done_tasks_make_their_dependants_roots() {
        let build = task("Build", 1, TaskState::Done, &[]);
        let old = task("Old", 2, TaskState::Cancelled, &[]);
        let test = task("Test", 3, TaskState::Ready, &[&build]);
        let failed = task("Failed", 4, TaskState::Failed, &[&old]);
        let all = book(&[&build, &old, &test, &failed]);
        let rows = tree_rows(
            &all,
            TaskFilters {
                show_done: false,
                workspace: None,
            },
            &no_parent,
            0,
            &ModelNames::default(),
        );
        let shape: Vec<(&str, usize)> = rows.iter().map(|r| (r.title.as_str(), r.depth)).collect();
        assert_eq!(
            shape,
            [("Test", 0), ("Failed", 0)],
            "failed tasks always show"
        );
        assert_eq!(
            tree_rows(&all, ALL, &no_parent, 0, &ModelNames::default()).len(),
            4
        );
    }

    #[test]
    fn the_workspace_filter_keeps_one_container_and_the_projects_of_a_workspace() {
        let a = task("A", 1, TaskState::Todo, &[]);
        let b = task("B", 2, TaskState::Todo, &[]);
        let only_b = TaskFilters {
            show_done: true,
            workspace: Some(b.workspace_id),
        };
        let rows = tree_rows(
            &book(&[&a, &b]),
            only_b,
            &no_parent,
            0,
            &ModelNames::default(),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "B");

        // A task of a project in a user workspace is listed when that workspace is shown.
        let workspace = WorkspaceId::new();
        let a_project = a.workspace_id;
        let in_workspace = move |id: WorkspaceId| (id == a_project).then_some(workspace);
        let shown = TaskFilters {
            show_done: true,
            workspace: Some(workspace),
        };
        let rows = tree_rows(
            &book(&[&a, &b]),
            shown,
            &in_workspace,
            0,
            &ModelNames::default(),
        );
        assert_eq!(
            rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(),
            ["A"]
        );
    }

    #[test]
    fn row_views_carry_placement_run_elapsed_and_outcome() {
        let mut t = task("Fix login bug", 1, TaskState::Running, &[]);
        t.placement = TaskPlacement {
            kind: TaskPlacementKind::Worktree,
            branch: None,
            base: None,
        };
        t.run = Some(TaskRun::Terminal {
            contain: None,
            argv: vec!["cargo".into(), "test".into()],
        });
        t.started_at_ms = Some(10_000);
        let row = &tree_rows(
            &book(&[&t]),
            ALL,
            &no_parent,
            75_000,
            &ModelNames::default(),
        )[0];
        assert_eq!(row.placement, "wt task/fix-login-bug");
        assert_eq!(row.branch.as_deref(), Some("task/fix-login-bug"));
        assert_eq!(row.run, "$ cargo test");
        assert_eq!(row.elapsed.as_deref(), Some("1m 05s"));
        assert!(row.reviewable);
        assert_eq!(row.glyph, crate::icons::Icon::StateRunning);

        t.state = TaskState::Failed;
        t.ended_at_ms = Some(40_000);
        t.outcome = Some("exited 3".into());
        t.run = Some(TaskRun::Chat {
            contain: None,
            agent: "omp".into(),
            model: None,
            account: None,
            safety: None,
        });
        let row = &tree_rows(
            &book(&[&t]),
            ALL,
            &no_parent,
            75_000,
            &ModelNames::default(),
        )[0];
        assert_eq!(
            row.elapsed.as_deref(),
            Some("30s"),
            "ended: how long it took"
        );
        assert_eq!(row.outcome.as_deref(), Some("exited 3"));
        assert_eq!(row.run, "omp chat");

        let plain = task("Plain", 2, TaskState::Todo, &[]);
        let row = &tree_rows(&book(&[&plain]), ALL, &no_parent, 0, &ModelNames::default())[0];
        assert_eq!(
            (row.placement.as_str(), row.run.as_str()),
            ("main", "shell")
        );
        assert_eq!(row.elapsed, None);
        assert!(
            !row.reviewable,
            "a main task that never ran has nothing to review"
        );
    }

    #[test]
    fn actions_follow_the_state_machine() {
        use TaskAction::*;
        assert_eq!(actions_for(TaskState::Ready), [Start, Done, Cancel, Delete]);
        assert_eq!(actions_for(TaskState::Todo), [Done, Cancel, Delete]);
        assert_eq!(actions_for(TaskState::Running), [Done, Cancel]);
        assert_eq!(actions_for(TaskState::Blocked), [Done, Cancel]);
        assert_eq!(actions_for(TaskState::Failed), [Done, Reset, Delete]);
        assert_eq!(actions_for(TaskState::Done), [Reset, Delete]);
        assert_eq!(actions_for(TaskState::Cancelled), [Reset, Delete]);
    }

    #[test]
    fn an_event_during_a_list_is_not_undone_by_the_older_list() {
        let a = task("A", 1, TaskState::Todo, &[]);
        let mut book = TaskBook::default();
        book.start_loading();
        let mut newer = a.clone();
        newer.state = TaskState::Done;
        book.updated(newer);
        let gone = task("Gone", 2, TaskState::Todo, &[]);
        book.removed(gone.id);
        // The list was answered before the events happened.
        book.merge_listed(vec![a.clone(), gone.clone()]);
        assert_eq!(book.get(a.id).map(|t| t.state), Some(TaskState::Done));
        assert!(book.get(gone.id).is_none(), "a removed task stays removed");
        assert!(book.loaded());

        // A later list drops what the daemon no longer has.
        book.start_loading();
        book.merge_listed(vec![]);
        assert!(book.is_empty());
    }

    fn reviewed(title: &str, age: u64, delivery: DeliveryState, branch: Option<&str>) -> Task {
        let mut done = task(title, age, TaskState::Done, &[]);
        done.delivery = delivery;
        if let Some(branch) = branch {
            done.placement = TaskPlacement {
                kind: TaskPlacementKind::Worktree,
                branch: Some(branch.to_owned()),
                base: None,
            };
        }
        done
    }

    #[test]
    fn every_delivery_but_none_has_a_pill() {
        let labels: Vec<Option<&str>> = DeliveryState::ALL
            .iter()
            .map(|d| delivery_pill(*d).map(|(label, _)| label))
            .collect();
        assert_eq!(
            labels,
            [
                None,
                Some("Awaiting review"),
                Some("Accepted"),
                Some("Changes requested"),
                Some("Merged"),
                Some("Discarded")
            ]
        );
        assert_eq!(
            delivery_pill(DeliveryState::AwaitingReview).map(|(_, tone)| tone),
            Some(Tone::Accent)
        );
    }

    #[test]
    fn finished_tasks_that_wait_for_a_reviewer_stay_listed_without_show_done() {
        let waiting = reviewed("Waiting", 1, DeliveryState::AwaitingReview, None);
        let sent_back = reviewed("Sent back", 2, DeliveryState::ChangesRequested, None);
        let accepted = reviewed("Accepted", 3, DeliveryState::Accepted, None);
        let plain = reviewed("Plain", 4, DeliveryState::None, None);
        let rows = tree_rows(
            &book(&[&waiting, &sent_back, &accepted, &plain]),
            TaskFilters {
                show_done: false,
                workspace: None,
            },
            &no_parent,
            0,
            &ModelNames::default(),
        );
        let shown: Vec<(&str, Option<&str>)> = rows
            .iter()
            .map(|r| (r.title.as_str(), r.delivery_label))
            .collect();
        assert_eq!(
            shown,
            [
                ("Waiting", Some("Awaiting review")),
                ("Sent back", Some("Changes requested"))
            ]
        );
    }

    #[test]
    fn the_task_of_a_branch_is_the_one_still_waiting_for_its_delivery() {
        let merged = reviewed("Old run", 1, DeliveryState::Merged, Some("feature/x"));
        let waiting = reviewed(
            "New run",
            2,
            DeliveryState::AwaitingReview,
            Some("feature/x"),
        );
        let newer_merged = reviewed("Newest", 3, DeliveryState::Merged, Some("feature/x"));
        let elsewhere = reviewed("Other", 4, DeliveryState::AwaitingReview, Some("other"));
        let main = reviewed("Main", 5, DeliveryState::AwaitingReview, None);
        let all = book(&[&merged, &waiting, &newer_merged, &elsewhere, &main]);
        assert_eq!(all.for_branch("feature/x").map(|t| t.id), Some(waiting.id));
        assert!(all.for_branch("nothing").is_none());
        let only_merged = book(&[&merged, &newer_merged]);
        assert_eq!(
            only_merged.for_branch("feature/x").map(|t| t.id),
            Some(newer_merged.id),
            "without one waiting, the latest"
        );
    }

    #[test]
    fn a_session_awaits_review_while_its_done_task_is_awaiting() {
        let session = SessionId::new();
        let mut waiting = reviewed("Waiting", 1, DeliveryState::AwaitingReview, None);
        waiting.sessions = vec![SessionId::new(), session];
        let mut accepted = reviewed("Accepted", 2, DeliveryState::Accepted, None);
        let accepted_session = SessionId::new();
        accepted.sessions = vec![accepted_session];
        let all = book(&[&waiting, &accepted]);
        assert!(all.awaits_review(session));
        assert!(!all.awaits_review(accepted_session));
        assert!(
            !all.awaits_review(waiting.sessions[0]),
            "only the last session of a task counts"
        );
    }
}
