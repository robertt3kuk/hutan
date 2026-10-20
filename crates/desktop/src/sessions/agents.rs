//! What the agent monitor shows of an observation tree (roadmap M13-B): which nodes are listed
//! and in which order, the glyph and tone of a node, durations, usage labels, the badge a session
//! carries in rows, tabs and cards, and the event M6 turns into a notification.
//!
//! GPUI-free and unit tested. The tree itself is the daemon's ([`hutan_core::AgentObservation`]);
//! nothing here invents evidence: a node without a start time has no duration, a usage counter
//! the source did not send is not shown.

use std::collections::{BTreeMap, HashSet};

use hutan_core::{AgentNode, AgentObservation, NodeKind, NodeOutcome, NodeState, SessionId, Usage};

use super::attention::{Presentation, Tone};

/// Longest activity line the panel shows, in characters (the daemon keeps 200).
pub const ACTIVITY_CHARS: usize = 80;

/// Emitted by [`SessionsStore`](super::SessionsStore) when a node of a session becomes blocked.
/// M6 turns it into a macOS notification; until then it only moves the status bar's attention
/// count (the store folds blocked nodes into every session's [`Presentation`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAttentionChanged {
    pub session_id: SessionId,
    /// Every node of the session that is blocked now, by node id, in tree order.
    pub blocked_nodes: Vec<String>,
}

/// What the monitor calls `node`: its name, else its definition, else (for the main agent) the
/// agent program, else its id.
pub fn display_name(node: &AgentNode) -> &str {
    match (&node.name, &node.agent_type) {
        (Some(name), _) => name,
        (None, Some(agent_type)) => agent_type,
        (None, None) if node.kind == NodeKind::Main => &node.agent,
        (None, None) => &node.node_id,
    }
}

/// The state glyph of a node. State is never color alone: every state and outcome has its own
/// shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    pub icon: crate::icons::Icon,
    pub tone: Tone,
    /// Working nodes pulse (unless the user asked to reduce motion).
    pub animated: bool,
    /// What the glyph means, for tooltips and tests.
    pub meaning: &'static str,
}

/// The glyph of `node`: its outcome when it has finished, else its state.
pub fn glyph(node: &AgentNode) -> Glyph {
    let g = |icon, tone, animated, meaning| Glyph {
        icon,
        tone,
        animated,
        meaning,
    };
    match (node.outcome, node.state) {
        (Some(NodeOutcome::Completed), _) => g(
            crate::icons::Icon::StateCompleted,
            Tone::Success,
            false,
            "completed",
        ),
        (Some(NodeOutcome::Failed), _) => g(
            crate::icons::Icon::StateFailed,
            Tone::Error,
            false,
            "failed",
        ),
        (Some(NodeOutcome::Interrupted), _) => g(
            crate::icons::Icon::StateInterrupted,
            Tone::Muted,
            false,
            "interrupted",
        ),
        (Some(NodeOutcome::Disconnected), _) => g(
            crate::icons::Icon::StateDisconnected,
            Tone::Muted,
            false,
            "disconnected",
        ),
        (None, NodeState::Working) => g(
            crate::icons::Icon::StateWorking,
            Tone::Accent,
            true,
            "working",
        ),
        (None, NodeState::Blocked) => g(
            crate::icons::Icon::StateBlocked,
            Tone::Warning,
            false,
            "blocked",
        ),
        (None, NodeState::Idle) => g(crate::icons::Icon::StateIdle, Tone::Muted, false, "idle"),
        (None, NodeState::Unknown) => g(
            crate::icons::Icon::StateUnknown,
            Tone::Muted,
            false,
            "state unavailable",
        ),
    }
}

/// What [`visible_rows`] filters by (`agents.show_finished`, `agents.only_blocked`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Filters {
    pub show_finished: bool,
    pub only_blocked: bool,
}

/// One listed node.
#[derive(Debug, Clone, Copy)]
pub struct NodeRow<'a> {
    pub node: &'a AgentNode,
    pub depth: usize,
    /// The node has children in the tree (listed or not).
    pub has_children: bool,
    /// Its whole subtree has finished and it has children: the panel lets the user fold and
    /// unfold it.
    pub foldable: bool,
    /// Its children are folded away: it is foldable and the user did not expand it.
    pub collapsed: bool,
    /// How many nodes the fold hides.
    pub hidden: usize,
}

struct Shape<'a> {
    children: BTreeMap<&'a str, Vec<&'a AgentNode>>,
    roots: Vec<&'a AgentNode>,
}

impl<'a> Shape<'a> {
    fn of(observation: &'a AgentObservation) -> Self {
        let order = |a: &&AgentNode, b: &&AgentNode| {
            a.started_at_ms
                .unwrap_or(0)
                .cmp(&b.started_at_ms.unwrap_or(0))
                .then_with(|| a.node_id.cmp(&b.node_id))
        };
        let mut children: BTreeMap<&str, Vec<&AgentNode>> = BTreeMap::new();
        let mut roots = Vec::new();
        for node in observation.nodes.values() {
            match node
                .parent
                .as_deref()
                .filter(|parent| observation.nodes.contains_key(*parent))
            {
                Some(parent) => children.entry(parent).or_default().push(node),
                // The root, or a node whose parent was dropped from the bounded tree.
                None => roots.push(node),
            }
        }
        for list in children.values_mut() {
            list.sort_by(order);
        }
        // The main agent first, then whatever lost its parent.
        roots.sort_by(|a, b| {
            let main = |n: &&AgentNode| n.node_id != observation.root;
            main(a).cmp(&main(b)).then_with(|| order(a, b))
        });
        Self { children, roots }
    }

    fn children_of(&self, node: &AgentNode) -> &[&'a AgentNode] {
        self.children
            .get(node.node_id.as_str())
            .map_or(&[], Vec::as_slice)
    }

    /// Whether `node` or something below it has not finished.
    fn live(&self, node: &AgentNode) -> bool {
        !node.is_finished() || self.children_of(node).iter().any(|c| self.live(c))
    }

    /// Whether `node` or something below it is blocked.
    fn blocked(&self, node: &AgentNode) -> bool {
        node.state == NodeState::Blocked || self.children_of(node).iter().any(|c| self.blocked(c))
    }

    fn count(&self, node: &AgentNode) -> usize {
        self.children_of(node)
            .iter()
            .map(|c| 1 + self.count(c))
            .sum()
    }
}

/// The nodes of `observation` to list, depth first with the main agent first and siblings in
/// the order they started (then by id, so the order never jumps when a node changes).
///
/// - A finished node with a finished subtree is dimmed by the panel, hidden unless
///   `show_finished`, and folded into one row (children hidden) unless its id is in `expanded`.
/// - A finished node that still has live descendants stays, so the tree stays connected.
/// - Blocked nodes are always listed, with their ancestors; `only_blocked` lists nothing else.
pub fn visible_rows<'a>(
    observation: &'a AgentObservation,
    filters: Filters,
    expanded: &HashSet<String>,
) -> Vec<NodeRow<'a>> {
    let shape = Shape::of(observation);
    let mut rows = Vec::new();
    for &root in &shape.roots {
        push_rows(&shape, root, 0, filters, expanded, &mut rows);
    }
    rows
}

fn listed(shape: &Shape<'_>, node: &AgentNode, filters: Filters) -> bool {
    if shape.blocked(node) {
        return true;
    }
    if filters.only_blocked {
        return false;
    }
    filters.show_finished || shape.live(node)
}

fn push_rows<'a>(
    shape: &Shape<'a>,
    node: &'a AgentNode,
    depth: usize,
    filters: Filters,
    expanded: &HashSet<String>,
    rows: &mut Vec<NodeRow<'a>>,
) {
    if !listed(shape, node, filters) {
        return;
    }
    let children = shape.children_of(node);
    let foldable = !shape.live(node) && !children.is_empty();
    let collapsed = foldable && !expanded.contains(&node.node_id);
    rows.push(NodeRow {
        node,
        depth,
        has_children: !children.is_empty(),
        foldable,
        collapsed,
        hidden: if collapsed { shape.count(node) } else { 0 },
    });
    if !collapsed {
        for &child in children {
            push_rows(shape, child, depth + 1, filters, expanded, rows);
        }
    }
}

/// Whether anything of the listed rows is doing work (so the durations tick).
pub fn any_running(rows: &[NodeRow<'_>]) -> bool {
    rows.iter().any(|row| {
        !row.node.is_finished() && matches!(row.node.state, NodeState::Working | NodeState::Blocked)
    })
}

/// How long `node` has been running (or ran): from its start to its end, or to `now_ms` while
/// it has not ended. `None` without a start time.
pub fn duration_ms(node: &AgentNode, now_ms: u64) -> Option<u64> {
    let start = node.started_at_ms?;
    let end = node.ended_at_ms.unwrap_or(now_ms);
    Some(end.saturating_sub(start))
}

/// `12s`, `3m 05s`, `1h 04m`, `2d 3h`.
pub fn format_duration(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {:02}s", secs / 60, secs % 60),
        3600..=86_399 => format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60),
        _ => format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3600),
    }
}

/// `999`, `1.5k`, `12k`, `1.2M`.
pub fn format_count(n: u64) -> String {
    fn scaled(n: u64, unit: u64, suffix: &str) -> String {
        let whole = n / unit;
        if whole >= 10 {
            return format!("{whole}{suffix}");
        }
        let tenth = (n % unit) * 10 / unit;
        if tenth == 0 {
            format!("{whole}{suffix}")
        } else {
            format!("{whole}.{tenth}{suffix}")
        }
    }
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => scaled(n, 1_000, "k"),
        _ => scaled(n, 1_000_000, "M"),
    }
}

/// What a usage figure measures, from its `kind` (`claude.context` → `context`). OMP's tokens
/// leave out cache reads, which is worth saying; kinds this build does not know show as sent.
fn kind_label(kind: &str) -> &str {
    match kind.rsplit('.').next().unwrap_or(kind) {
        "tokens" => "excl. cache",
        "" => kind,
        last => last,
    }
}

/// `1.5k tok · context`, `3.2k/200k tok · context`, `12k tok · total · $0.31`; `None` when the
/// source sent no counter at all. The numbers are the source's own: context kinds show the
/// context size, other kinds input plus output, and a missing counter is never zero-filled.
pub fn usage_label(usage: &Usage) -> Option<String> {
    let context_kind = usage.kind.ends_with("context");
    let sum = |a: Option<u64>, b: Option<u64>| match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
    };
    let tokens = if context_kind {
        usage
            .context_tokens
            .or_else(|| sum(usage.input, usage.output))
    } else {
        sum(usage.input, usage.output).or(usage.context_tokens)
    };
    let mut parts = Vec::new();
    if let Some(tokens) = tokens {
        let mut text = format_count(tokens);
        if context_kind && let Some(window) = usage.context_window {
            text = format!("{text}/{}", format_count(window));
        }
        parts.push(format!("{text} tok"));
    }
    if let Some(cost) = usage.cost_usd {
        parts.push(if cost > 0.0 && cost < 0.01 {
            "<$0.01".to_owned()
        } else {
            format!("${cost:.2}")
        });
    }
    if parts.is_empty() {
        return None;
    }
    let label = kind_label(&usage.kind);
    let mut text = parts.remove(0);
    if !label.is_empty() {
        text = format!("{text} · {label}");
    }
    for part in parts {
        text = format!("{text} · {part}");
    }
    Some(text)
}

/// `activity` cut to [`ACTIVITY_CHARS`] characters on one line.
pub fn activity_text(activity: &str) -> String {
    let line = activity.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= ACTIVITY_CHARS {
        return line;
    }
    let cut: String = line.chars().take(ACTIVITY_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

/// A blocked node, for tooltips and the attention event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedNode {
    pub node_id: String,
    pub label: String,
    /// The main agent (the root), as opposed to a subagent.
    pub main: bool,
}

/// What a session's row, tab and card say about its agents.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentBadge {
    /// Subagents that are working or waiting for the user right now.
    pub running_subagents: usize,
    /// Every blocked node, in tree order.
    pub blocked: Vec<BlockedNode>,
}

impl AgentBadge {
    pub fn is_empty(&self) -> bool {
        self.running_subagents == 0 && self.blocked.is_empty()
    }

    /// The subagent count; artwork is rendered separately.
    pub fn running_text(&self) -> Option<String> {
        (self.running_subagents > 0).then(|| self.running_subagents.to_string())
    }

    /// The blocked marker; `None` while nothing is blocked.
    pub fn blocked_icon(&self) -> Option<crate::icons::Icon> {
        (!self.blocked.is_empty()).then_some(crate::icons::Icon::StateBlocked)
    }

    /// `2 subagents running` and `Blocked: reviewer, main (omp)`, one per line.
    pub fn tooltip(&self) -> String {
        let mut lines = Vec::new();
        if self.running_subagents > 0 {
            lines.push(format!(
                "{} subagent{} running",
                self.running_subagents,
                if self.running_subagents == 1 { "" } else { "s" }
            ));
        }
        if !self.blocked.is_empty() {
            let names: Vec<&str> = self.blocked.iter().map(|n| n.label.as_str()).collect();
            lines.push(format!("Blocked: {}", names.join(", ")));
        }
        lines.join("\n")
    }
}

/// Depth-first node ids, the main agent first (the order every list of nodes uses).
fn tree_order(observation: &AgentObservation) -> Vec<&AgentNode> {
    let shape = Shape::of(observation);
    fn walk<'a>(shape: &Shape<'a>, node: &'a AgentNode, out: &mut Vec<&'a AgentNode>) {
        out.push(node);
        for &child in shape.children_of(node) {
            walk(shape, child, out);
        }
    }
    let mut out = Vec::new();
    for &root in &shape.roots {
        walk(&shape, root, &mut out);
    }
    out
}

/// The nodes of `observation` that wait for the user, in tree order.
pub fn blocked_nodes(observation: &AgentObservation) -> Vec<BlockedNode> {
    tree_order(observation)
        .into_iter()
        .filter(|node| node.state == NodeState::Blocked && !node.is_finished())
        .map(|node| BlockedNode {
            node_id: node.node_id.clone(),
            label: if node.node_id == observation.root || node.parent.is_none() {
                format!("{} (main)", display_name(node))
            } else {
                display_name(node).to_owned()
            },
            main: node.node_id == observation.root || node.parent.is_none(),
        })
        .collect()
}

/// The badge of a session with this tree.
pub fn badge(observation: &AgentObservation) -> AgentBadge {
    let running_subagents = observation
        .nodes
        .values()
        .filter(|node| {
            node.parent.is_some()
                && node.node_id != observation.root
                && !node.is_finished()
                && matches!(node.state, NodeState::Working | NodeState::Blocked)
        })
        .count();
    AgentBadge {
        running_subagents,
        blocked: blocked_nodes(observation),
    }
}

/// How a live session whose tree has blocked nodes presents itself when the session summary
/// has not caught up (or never said so, as for a chat): blocked, and it needs the user.
pub fn blocked_presentation(blocked: &[BlockedNode]) -> Presentation {
    let names: Vec<&str> = blocked.iter().map(|n| n.label.as_str()).collect();
    Presentation {
        label: "blocked".into(),
        detail: match names.as_slice() {
            [only] => format!("agent: {only}"),
            many => format!("{} agents: {}", many.len(), names.join(", ")),
        },
        tone: Tone::Warning,
        filled: true,
        needs_you: true,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use hutan_core::Liveness;

    use super::*;

    fn node(id: &str, parent: Option<&str>, started: u64) -> AgentNode {
        AgentNode {
            pid: None,
            node_id: id.into(),
            parent: parent.map(str::to_owned),
            kind: if parent.is_some() {
                NodeKind::Sub
            } else {
                NodeKind::Main
            },
            source: "test".into(),
            agent: "omp".into(),
            name: Some(id.into()),
            agent_type: None,
            state: NodeState::Working,
            outcome: None,
            liveness: Liveness::Alive,
            model: None,
            started_at_ms: Some(started),
            ended_at_ms: None,
            last_activity_ms: started,
            activity: None,
            usage: None,
            transcript_path: None,
        }
    }

    fn finished(mut n: AgentNode, outcome: NodeOutcome) -> AgentNode {
        n.state = NodeState::Idle;
        n.outcome = Some(outcome);
        n.ended_at_ms = Some(n.started_at_ms.unwrap_or(0) + 5_000);
        n
    }

    fn tree(nodes: Vec<AgentNode>) -> AgentObservation {
        AgentObservation {
            session_id: SessionId::new(),
            root: "root".into(),
            nodes: nodes
                .into_iter()
                .map(|n| (n.node_id.clone(), n))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn ids(rows: &[NodeRow<'_>]) -> Vec<(String, usize)> {
        rows.iter()
            .map(|r| (r.node.node_id.clone(), r.depth))
            .collect()
    }

    const ALL: Filters = Filters {
        show_finished: true,
        only_blocked: false,
    };

    #[test]
    fn every_state_and_outcome_has_its_own_glyph() {
        let mut seen = HashSet::new();
        let mut n = node("a", None, 1);
        for state in [
            NodeState::Working,
            NodeState::Blocked,
            NodeState::Idle,
            NodeState::Unknown,
        ] {
            n.state = state;
            assert!(seen.insert(glyph(&n).icon), "{state:?}");
        }
        for outcome in [
            NodeOutcome::Completed,
            NodeOutcome::Failed,
            NodeOutcome::Interrupted,
            NodeOutcome::Disconnected,
        ] {
            n.outcome = Some(outcome);
            assert!(seen.insert(glyph(&n).icon), "{outcome:?}");
        }
        n.outcome = Some(NodeOutcome::Completed);
        assert_eq!(
            (glyph(&n).icon, glyph(&n).tone),
            (crate::icons::Icon::StateCompleted, Tone::Success)
        );
        n.outcome = Some(NodeOutcome::Failed);
        assert_eq!(
            (glyph(&n).icon, glyph(&n).tone),
            (crate::icons::Icon::StateFailed, Tone::Error)
        );
        n.outcome = None;
        n.state = NodeState::Unknown;
        assert_eq!(
            (glyph(&n).icon, glyph(&n).tone),
            (crate::icons::Icon::StateUnknown, Tone::Muted)
        );
        n.state = NodeState::Blocked;
        assert_eq!(glyph(&n).tone, Tone::Warning);
        n.state = NodeState::Working;
        assert!(glyph(&n).animated);
    }

    #[test]
    fn nodes_are_named_by_name_then_type_then_program() {
        let mut n = node("n1", Some("root"), 1);
        n.name = None;
        assert_eq!(display_name(&n), "n1");
        n.agent_type = Some("Explore".into());
        assert_eq!(display_name(&n), "Explore");
        n.name = Some("scout".into());
        assert_eq!(display_name(&n), "scout");
        let mut main = node("root", None, 1);
        main.name = None;
        assert_eq!(display_name(&main), "omp");
    }

    #[test]
    fn rows_are_depth_first_in_start_order_with_ties_by_id() {
        let t = tree(vec![
            node("root", None, 10),
            node("late", Some("root"), 30),
            node("b", Some("root"), 20),
            node("a", Some("root"), 20),
            node("deep", Some("a"), 25),
        ]);
        let rows = visible_rows(&t, ALL, &HashSet::new());
        assert_eq!(
            ids(&rows),
            [
                ("root".into(), 0),
                ("a".into(), 1),
                ("deep".into(), 2),
                ("b".into(), 1),
                ("late".into(), 1),
            ]
        );
    }

    #[test]
    fn a_subagent_before_its_root_still_hangs_below_it() {
        // The daemon makes up a root for a subagent that reports first; a node whose parent
        // was dropped from the bounded tree is listed as a root of its own, after the main agent.
        let t = tree(vec![
            node("orphan", Some("gone"), 5),
            node("root", None, 10),
            node("kid", Some("root"), 11),
        ]);
        let rows = visible_rows(&t, ALL, &HashSet::new());
        assert_eq!(
            ids(&rows),
            [("root".into(), 0), ("kid".into(), 1), ("orphan".into(), 0)]
        );
    }

    #[test]
    fn finished_subtrees_collapse_and_hide_unless_asked() {
        let t = tree(vec![
            node("root", None, 1),
            finished(node("done", Some("root"), 2), NodeOutcome::Completed),
            finished(node("done-kid", Some("done"), 3), NodeOutcome::Failed),
            node("live", Some("root"), 4),
        ]);
        let hidden = visible_rows(&t, Filters::default(), &HashSet::new());
        assert_eq!(ids(&hidden), [("root".into(), 0), ("live".into(), 1)]);

        let shown = visible_rows(&t, ALL, &HashSet::new());
        assert_eq!(
            ids(&shown),
            [("root".into(), 0), ("done".into(), 1), ("live".into(), 1)]
        );
        let done = &shown[1];
        assert!(done.collapsed && done.has_children && done.foldable);
        assert!(
            !shown[0].foldable,
            "a tree with something live is never folded"
        );
        assert_eq!(done.hidden, 1);

        let open: HashSet<String> = ["done".to_owned()].into();
        let expanded = visible_rows(&t, ALL, &open);
        assert_eq!(expanded.len(), 4);
        assert!(!expanded[1].collapsed);
        assert_eq!(expanded[2].node.node_id, "done-kid");
    }

    #[test]
    fn a_finished_node_with_a_live_descendant_stays_and_stays_open() {
        let t = tree(vec![
            node("root", None, 1),
            finished(node("mid", Some("root"), 2), NodeOutcome::Completed),
            node("leaf", Some("mid"), 3),
        ]);
        let rows = visible_rows(&t, Filters::default(), &HashSet::new());
        assert_eq!(
            ids(&rows),
            [("root".into(), 0), ("mid".into(), 1), ("leaf".into(), 2)]
        );
        assert!(!rows[1].collapsed);
    }

    #[test]
    fn a_finished_main_agent_disappears_with_its_subagents_by_default() {
        let t = tree(vec![
            finished(node("root", None, 1), NodeOutcome::Disconnected),
            finished(node("kid", Some("root"), 2), NodeOutcome::Disconnected),
        ]);
        assert!(visible_rows(&t, Filters::default(), &HashSet::new()).is_empty());
        let rows = visible_rows(&t, ALL, &HashSet::new());
        assert_eq!(ids(&rows), [("root".into(), 0)]);
        assert!(rows[0].collapsed);
    }

    #[test]
    fn blocked_nodes_are_always_listed_with_their_ancestors() {
        let mut blocked = node("b", Some("mid"), 3);
        blocked.state = NodeState::Blocked;
        let t = tree(vec![
            node("root", None, 1),
            node("mid", Some("root"), 2),
            blocked,
            node("other", Some("root"), 4),
        ]);
        let only = Filters {
            show_finished: false,
            only_blocked: true,
        };
        assert_eq!(
            ids(&visible_rows(&t, only, &HashSet::new())),
            [("root".into(), 0), ("mid".into(), 1), ("b".into(), 2)]
        );
        // Nothing blocked: nothing listed.
        let calm = tree(vec![node("root", None, 1), node("a", Some("root"), 2)]);
        assert!(visible_rows(&calm, only, &HashSet::new()).is_empty());
    }

    #[test]
    fn running_means_a_listed_node_works_or_waits_for_you() {
        let mut idle = node("root", None, 1);
        idle.state = NodeState::Idle;
        let t = tree(vec![idle.clone()]);
        assert!(!any_running(&visible_rows(&t, ALL, &HashSet::new())));
        idle.state = NodeState::Working;
        let t = tree(vec![idle.clone()]);
        assert!(any_running(&visible_rows(&t, ALL, &HashSet::new())));
        idle.state = NodeState::Blocked;
        let t = tree(vec![idle]);
        assert!(any_running(&visible_rows(&t, ALL, &HashSet::new())));
        let t = tree(vec![finished(
            node("root", None, 1),
            NodeOutcome::Completed,
        )]);
        assert!(!any_running(&visible_rows(&t, ALL, &HashSet::new())));
    }

    #[test]
    fn durations_run_to_now_until_the_node_ends() {
        let mut n = node("a", None, 1_000);
        assert_eq!(duration_ms(&n, 4_500), Some(3_500));
        n.ended_at_ms = Some(2_000);
        assert_eq!(duration_ms(&n, 99_000), Some(1_000));
        n.started_at_ms = None;
        assert_eq!(duration_ms(&n, 99_000), None);
        let late = node("b", None, 9_000);
        assert_eq!(
            duration_ms(&late, 1_000),
            Some(0),
            "a clock skew is not negative"
        );
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(59_999), "59s");
        assert_eq!(format_duration(65_000), "1m 05s");
        assert_eq!(format_duration(3_840_000), "1h 04m");
        assert_eq!(format_duration(2 * 86_400_000 + 3 * 3_600_000), "2d 3h");
    }

    #[test]
    fn counts_are_compact() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1k");
        assert_eq!(format_count(1_500), "1.5k");
        assert_eq!(format_count(12_345), "12k");
        assert_eq!(format_count(200_000), "200k");
        assert_eq!(format_count(1_200_000), "1.2M");
    }

    fn usage(kind: &str) -> Usage {
        Usage {
            kind: kind.into(),
            ..Usage::default()
        }
    }

    #[test]
    fn usage_is_labelled_by_its_kind_and_never_zero_filled() {
        let context = Usage {
            context_tokens: Some(1_500),
            ..usage("claude.context")
        };
        assert_eq!(usage_label(&context).as_deref(), Some("1.5k tok · context"));
        let windowed = Usage {
            context_window: Some(200_000),
            ..context
        };
        assert_eq!(
            usage_label(&windowed).as_deref(),
            Some("1.5k/200k tok · context")
        );
        let total = Usage {
            input: Some(1_000),
            output: Some(2_200),
            cost_usd: Some(0.314),
            ..usage("codex.total")
        };
        assert_eq!(
            usage_label(&total).as_deref(),
            Some("3.2k tok · total · $0.31")
        );
        let omp = Usage {
            input: Some(800),
            ..usage("omp.tokens")
        };
        assert_eq!(usage_label(&omp).as_deref(), Some("800 tok · excl. cache"));
        let cost_only = Usage {
            cost_usd: Some(0.004),
            ..usage("x.y")
        };
        assert_eq!(usage_label(&cost_only).as_deref(), Some("<$0.01 · y"));
        assert_eq!(usage_label(&usage("omp.tokens")), None, "nothing was sent");
        let unknown = Usage {
            input: Some(5),
            ..usage("vendor")
        };
        assert_eq!(usage_label(&unknown).as_deref(), Some("5 tok · vendor"));
    }

    #[test]
    fn activity_is_one_short_line() {
        assert_eq!(
            activity_text("  reading\n  src/lib.rs "),
            "reading src/lib.rs"
        );
        let long = "x".repeat(300);
        let cut = activity_text(&long);
        assert_eq!(cut.chars().count(), ACTIVITY_CHARS);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn the_badge_counts_running_subagents_and_names_blocked_nodes() {
        let mut blocked_sub = node("rev", Some("root"), 3);
        blocked_sub.state = NodeState::Blocked;
        blocked_sub.name = Some("reviewer".into());
        let mut idle_sub = node("idle", Some("root"), 4);
        idle_sub.state = NodeState::Idle;
        let t = tree(vec![
            node("root", None, 1),
            node("a", Some("root"), 2),
            blocked_sub,
            idle_sub,
            finished(node("old", Some("root"), 5), NodeOutcome::Completed),
        ]);
        let b = badge(&t);
        assert_eq!(
            b.running_subagents, 2,
            "working and blocked count; idle and finished do not"
        );
        assert_eq!(b.running_text().as_deref(), Some("2"));
        assert_eq!(b.blocked_icon(), Some(crate::icons::Icon::StateBlocked));
        assert_eq!(b.tooltip(), "2 subagents running\nBlocked: reviewer");
        assert!(!b.blocked[0].main);

        let mut main_blocked = node("root", None, 1);
        main_blocked.state = NodeState::Blocked;
        let b = badge(&tree(vec![main_blocked]));
        assert_eq!(b.running_subagents, 0, "the main agent is not a subagent");
        assert_eq!(b.running_text(), None);
        assert_eq!(b.tooltip(), "Blocked: root (main)");
        assert!(b.blocked[0].main);

        let calm = badge(&tree(vec![node("root", None, 1)]));
        assert!(calm.is_empty());
        assert_eq!(calm.tooltip(), "");
    }

    #[test]
    fn a_blocked_tree_presents_as_needing_you() {
        let p = blocked_presentation(&[BlockedNode {
            node_id: "a".into(),
            label: "reviewer".into(),
            main: false,
        }]);
        assert_eq!((p.label.as_str(), p.needs_you), ("blocked", true));
        assert_eq!(p.detail, "agent: reviewer");
        let two = blocked_presentation(&[
            BlockedNode {
                node_id: "a".into(),
                label: "x".into(),
                main: false,
            },
            BlockedNode {
                node_id: "b".into(),
                label: "y".into(),
                main: false,
            },
        ]);
        assert_eq!(two.detail, "2 agents: x, y");
    }
}
