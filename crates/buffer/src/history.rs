use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::error::BufferError;
use crate::transaction::Transaction;

/// Bounds of the undo history. The history is a tree of *groups* (one group = one user action);
/// when a bound is exceeded the oldest groups on the way to the current state are forgotten first
/// (together with the alternative branches that fork before them). The current state and every
/// group after it (the redo side) are never dropped, so redo keeps working.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryLimits {
    /// Most groups kept (all branches together).
    pub max_groups: usize,
    /// Most bytes of stored text and bookkeeping (forward and inverse transactions together).
    pub max_bytes: usize,
    /// Groups older than this are forgotten, oldest first; `None` keeps them regardless of age.
    pub max_age: Option<Duration>,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            max_groups: 10_000,
            max_bytes: 32 << 20,
            max_age: Some(Duration::from_secs(24 * 60 * 60)),
        }
    }
}

struct Node {
    parent: Option<u64>,
    children: Vec<u64>,
    /// The child redo follows: the one created or visited last.
    redo: Option<u64>,
    forward: Transaction,
    inverse: Transaction,
    created: Instant,
    bytes: usize,
}

impl Node {
    fn new(
        parent: Option<u64>,
        forward: Transaction,
        inverse: Transaction,
        created: Instant,
    ) -> Self {
        let bytes = forward.text_bytes() + inverse.text_bytes() + std::mem::size_of::<Node>();
        Self {
            parent,
            children: Vec::new(),
            redo: None,
            forward,
            inverse,
            created,
            bytes,
        }
    }
}

struct OpenGroup {
    depth: usize,
    /// The node the group's transactions are composed into, once its first one was recorded.
    node: Option<u64>,
}

/// The undo tree. Node 0 is the state the buffer started in (it holds no transactions).
pub(crate) struct History {
    nodes: HashMap<u64, Node>,
    root: u64,
    current: u64,
    /// Root to current state, both included: pruning and undo never walk the tree.
    path: VecDeque<u64>,
    next_id: u64,
    saved: Option<u64>,
    open: Option<OpenGroup>,
    limits: HistoryLimits,
    total_bytes: usize,
}

impl History {
    pub(crate) fn new(now: Instant) -> Self {
        let mut nodes = HashMap::new();
        let root = Node::new(None, Transaction::empty(), Transaction::empty(), now);
        let total_bytes = root.bytes;
        nodes.insert(0, root);
        Self {
            nodes,
            root: 0,
            current: 0,
            path: VecDeque::from([0]),
            next_id: 1,
            saved: Some(0),
            open: None,
            limits: HistoryLimits::default(),
            total_bytes,
        }
    }

    pub(crate) fn limits(&self) -> HistoryLimits {
        self.limits
    }

    pub(crate) fn set_limits(&mut self, limits: HistoryLimits, now: Instant) {
        self.limits = limits;
        self.prune(now);
    }

    /// Number of groups in the tree (every branch), not counting the starting state.
    pub(crate) fn groups(&self) -> usize {
        self.nodes.len() - 1
    }

    pub(crate) fn bytes(&self) -> usize {
        self.total_bytes
    }

    pub(crate) fn is_clean(&self) -> bool {
        self.saved == Some(self.current)
    }

    pub(crate) fn mark_saved(&mut self) {
        self.saved = Some(self.current);
    }

    pub(crate) fn begin_group(&mut self) {
        match &mut self.open {
            Some(g) => g.depth += 1,
            None => {
                self.open = Some(OpenGroup {
                    depth: 1,
                    node: None,
                })
            }
        }
    }

    pub(crate) fn end_group(&mut self) {
        if let Some(g) = &mut self.open {
            g.depth -= 1;
            if g.depth == 0 {
                self.open = None;
            }
        }
    }

    /// The next transaction starts a new node even inside an open group.
    pub(crate) fn split_group(&mut self) {
        if let Some(g) = &mut self.open {
            g.node = None;
        }
    }

    pub(crate) fn close_groups(&mut self) {
        self.open = None;
    }

    /// Records an applied transaction and its inverse: a child of the current state, or composed
    /// into the open group's node. Mutates nothing when it fails.
    pub(crate) fn record(
        &mut self,
        forward: Transaction,
        inverse: Transaction,
        now: Instant,
    ) -> Result<(), BufferError> {
        let joined = self
            .open
            .as_ref()
            .and_then(|g| g.node)
            .filter(|id| *id == self.current);
        if let Some(id) = joined {
            let Some(node) = self.nodes.get_mut(&id) else {
                return Ok(());
            };
            let forward = node.forward.compose(&forward)?;
            let inverse = inverse.compose(&node.inverse)?;
            let bytes = forward.text_bytes() + inverse.text_bytes() + std::mem::size_of::<Node>();
            self.total_bytes = self.total_bytes - node.bytes + bytes;
            node.forward = forward;
            node.inverse = inverse;
            node.bytes = bytes;
        } else {
            let id = self.next_id;
            self.next_id += 1;
            let node = Node::new(Some(self.current), forward, inverse, now);
            self.total_bytes += node.bytes;
            self.nodes.insert(id, node);
            if let Some(parent) = self.nodes.get_mut(&self.current) {
                parent.children.push(id);
                parent.redo = Some(id);
            }
            self.current = id;
            self.path.push_back(id);
            if let Some(g) = &mut self.open {
                g.node = Some(id);
            }
        }
        self.prune(now);
        Ok(())
    }

    pub(crate) fn can_undo(&self) -> bool {
        self.current != self.root
    }

    /// The transaction that undoes the current group, if there is one.
    pub(crate) fn undo_transaction(&self) -> Option<Transaction> {
        let node = self.nodes.get(&self.current)?;
        node.parent?;
        Some(node.inverse.clone())
    }

    /// Moves to the parent once the undo transaction was applied.
    pub(crate) fn step_back(&mut self) {
        let Some(parent) = self.nodes.get(&self.current).and_then(|n| n.parent) else {
            return;
        };
        if let Some(p) = self.nodes.get_mut(&parent) {
            p.redo = Some(self.current);
        }
        self.current = parent;
        self.path.pop_back();
    }

    /// How many groups redo can choose between.
    pub(crate) fn redo_branches(&self) -> usize {
        self.nodes
            .get(&self.current)
            .map_or(0, |n| n.children.len())
    }

    /// The redo transaction of branch `branch` (`None` = the most recent one).
    pub(crate) fn redo_transaction(&self, branch: Option<usize>) -> Option<(u64, Transaction)> {
        let node = self.nodes.get(&self.current)?;
        let child = match branch {
            Some(i) => *node.children.get(i)?,
            None => node.redo.or_else(|| node.children.last().copied())?,
        };
        Some((child, self.nodes.get(&child)?.forward.clone()))
    }

    /// Moves to `child` once its redo transaction was applied.
    pub(crate) fn step_forward(&mut self, child: u64) {
        if let Some(n) = self.nodes.get_mut(&self.current) {
            n.redo = Some(child);
        }
        self.current = child;
        self.path.push_back(child);
    }

    /// Forgets groups beyond the limits, oldest first (see [`HistoryLimits`]).
    pub(crate) fn prune(&mut self, now: Instant) {
        while let Some(&first) = self.path.get(1) {
            let age_exceeded = self.limits.max_age.is_some_and(|age| {
                self.nodes
                    .get(&first)
                    .is_some_and(|n| now.saturating_duration_since(n.created) > age)
            });
            let over = self.groups() > self.limits.max_groups
                || self.total_bytes > self.limits.max_bytes
                || age_exceeded;
            if !over {
                return;
            }
            let open_node = self.open.as_ref().and_then(|g| g.node);
            if first == self.current && open_node == Some(first) {
                // The group being typed into is the history's only entry: nothing older to drop.
                return;
            }
            self.make_root(first);
        }
    }

    /// Drops the root, its other branches and their subtrees; `first` becomes the new root.
    fn make_root(&mut self, first: u64) {
        let mut stack = vec![self.root];
        while let Some(id) = stack.pop() {
            let Some(node) = self.nodes.get(&id) else {
                continue;
            };
            stack.extend(node.children.iter().copied().filter(|c| *c != first));
            if let Some(node) = self.nodes.remove(&id) {
                self.total_bytes -= node.bytes;
                if self.saved == Some(id) {
                    self.saved = None;
                }
            }
        }
        if let Some(node) = self.nodes.get_mut(&first) {
            self.total_bytes -= node.bytes;
            node.parent = None;
            node.forward = Transaction::empty();
            node.inverse = Transaction::empty();
            node.bytes = std::mem::size_of::<Node>();
            self.total_bytes += node.bytes;
        }
        self.root = first;
        self.path.pop_front();
    }
}
