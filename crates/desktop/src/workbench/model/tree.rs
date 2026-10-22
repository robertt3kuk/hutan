//! Pane-tree surgery: lookup, split, remove, normalize, geometry.

use super::types::{Axis, Edge, Pane, PaneId, PaneTree, Rect};

/// Smallest share a child of a split may have after a resize.
pub const MIN_RATIO: f32 = 0.05;

/// `ratio / total`, leaving shares alone that already sum to 1 within float noise so that
/// normalizing twice (save, load) changes nothing.
fn rescale(ratio: f32, total: f32) -> f32 {
    if (total - 1.0).abs() <= 1e-5 {
        ratio
    } else {
        ratio / total
    }
}

impl PaneTree {
    /// A tree with one pane.
    pub fn single(pane: Pane) -> Self {
        PaneTree::Pane(pane)
    }

    /// Panes in depth-first, left-to-right / top-to-bottom order.
    pub fn panes(&self) -> Vec<&Pane> {
        let mut out = Vec::new();
        self.collect_panes(&mut out);
        out
    }

    fn collect_panes<'a>(&'a self, out: &mut Vec<&'a Pane>) {
        match self {
            PaneTree::Pane(pane) => out.push(pane),
            PaneTree::Split { children, .. } => {
                for child in children {
                    child.collect_panes(out);
                }
            }
        }
    }

    pub fn panes_mut(&mut self) -> Vec<&mut Pane> {
        let mut out = Vec::new();
        self.collect_panes_mut(&mut out);
        out
    }

    fn collect_panes_mut<'a>(&'a mut self, out: &mut Vec<&'a mut Pane>) {
        match self {
            PaneTree::Pane(pane) => out.push(pane),
            PaneTree::Split { children, .. } => {
                for child in children {
                    child.collect_panes_mut(out);
                }
            }
        }
    }

    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        match self {
            PaneTree::Pane(pane) => (pane.id == id).then_some(pane),
            PaneTree::Split { children, .. } => children.iter().find_map(|c| c.pane(id)),
        }
    }

    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        match self {
            PaneTree::Pane(pane) => (pane.id == id).then_some(pane),
            PaneTree::Split { children, .. } => children.iter_mut().find_map(|c| c.pane_mut(id)),
        }
    }

    /// First pane in order; `None` only for a malformed (childless) split.
    pub fn first_pane(&self) -> Option<&Pane> {
        match self {
            PaneTree::Pane(pane) => Some(pane),
            PaneTree::Split { children, .. } => children.first().and_then(PaneTree::first_pane),
        }
    }

    pub fn first_pane_mut(&mut self) -> Option<&mut Pane> {
        match self {
            PaneTree::Pane(pane) => Some(pane),
            PaneTree::Split { children, .. } => {
                children.first_mut().and_then(PaneTree::first_pane_mut)
            }
        }
    }

    /// Child-index path from the root to the pane.
    pub fn path_to(&self, id: PaneId) -> Option<Vec<usize>> {
        match self {
            PaneTree::Pane(pane) => (pane.id == id).then(Vec::new),
            PaneTree::Split { children, .. } => children.iter().enumerate().find_map(|(i, c)| {
                c.path_to(id).map(|mut rest| {
                    rest.insert(0, i);
                    rest
                })
            }),
        }
    }

    pub fn node_at(&self, path: &[usize]) -> Option<&PaneTree> {
        match path.split_first() {
            None => Some(self),
            Some((i, rest)) => match self {
                PaneTree::Split { children, .. } => children.get(*i)?.node_at(rest),
                PaneTree::Pane(_) => None,
            },
        }
    }

    pub fn node_at_mut(&mut self, path: &[usize]) -> Option<&mut PaneTree> {
        match path.split_first() {
            None => Some(self),
            Some((i, rest)) => match self {
                PaneTree::Split { children, .. } => children.get_mut(*i)?.node_at_mut(rest),
                PaneTree::Pane(_) => None,
            },
        }
    }

    /// Place `new` next to pane `target` on `edge`. Returns `false` when `target` is absent.
    ///
    /// A same-axis parent gains a sibling (the target's share is halved); otherwise the target is
    /// replaced by a new two-child split.
    pub fn split_at(&mut self, target: PaneId, edge: Edge, new: &mut Option<Pane>) -> bool {
        match self {
            PaneTree::Pane(pane) if pane.id == target => {
                let Some(new_pane) = new.take() else {
                    return false;
                };
                let old = std::mem::replace(
                    self,
                    PaneTree::Split {
                        axis: edge.axis(),
                        ratios: Vec::new(),
                        children: Vec::new(),
                    },
                );
                let new_node = PaneTree::Pane(new_pane);
                let children = if edge.is_after() {
                    vec![old, new_node]
                } else {
                    vec![new_node, old]
                };
                *self = PaneTree::Split {
                    axis: edge.axis(),
                    ratios: vec![0.5, 0.5],
                    children,
                };
                true
            }
            PaneTree::Pane(_) => false,
            PaneTree::Split {
                axis,
                ratios,
                children,
            } => {
                let direct = children
                    .iter()
                    .position(|c| matches!(c, PaneTree::Pane(p) if p.id == target));
                if let (Some(i), true) = (direct, *axis == edge.axis()) {
                    let Some(new_pane) = new.take() else {
                        return false;
                    };
                    let share = ratios.get(i).copied().unwrap_or(0.5) / 2.0;
                    if let Some(r) = ratios.get_mut(i) {
                        *r = share;
                    }
                    let at = if edge.is_after() { i + 1 } else { i };
                    ratios.insert(at, share);
                    children.insert(at, PaneTree::Pane(new_pane));
                    return true;
                }
                children.iter_mut().any(|c| c.split_at(target, edge, new))
            }
        }
    }

    /// Every split shares its space equally among its children (Vim's `⌃W =`).
    pub fn equalize(&mut self) {
        if let PaneTree::Split {
            ratios, children, ..
        } = self
        {
            let n = children.len().max(1);
            *ratios = vec![1.0 / n as f32; children.len()];
            for child in children {
                child.equalize();
            }
        }
    }

    /// Unit-square rectangles for every pane, following the ratios.
    pub fn layout(&self, rect: Rect) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        self.layout_into(rect, &mut out);
        out
    }

    fn layout_into(&self, rect: Rect, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            PaneTree::Pane(pane) => out.push((pane.id, rect)),
            PaneTree::Split {
                axis,
                ratios,
                children,
            } => {
                let mut offset = 0.0;
                for (child, ratio) in children.iter().zip(ratios) {
                    let child_rect = match axis {
                        Axis::Horizontal => Rect {
                            x: rect.x + rect.w * offset,
                            y: rect.y,
                            w: rect.w * ratio,
                            h: rect.h,
                        },
                        Axis::Vertical => Rect {
                            x: rect.x,
                            y: rect.y + rect.h * offset,
                            w: rect.w,
                            h: rect.h * ratio,
                        },
                    };
                    offset += ratio;
                    child.layout_into(child_rect, out);
                }
            }
        }
    }

    /// Restore every tree invariant. `fallback` is the id used for the sole empty pane when
    /// everything was pruned.
    pub fn normalize(&mut self, fallback: PaneId) {
        if !self.prune(true) {
            *self = PaneTree::Pane(Pane::new(fallback, Vec::new()));
        }
    }

    /// Returns whether the node should be kept.
    fn prune(&mut self, is_root: bool) -> bool {
        match self {
            PaneTree::Pane(pane) => {
                if pane.items.is_empty() {
                    pane.active = 0;
                    is_root
                } else {
                    pane.active = pane.active.min(pane.items.len() - 1);
                    true
                }
            }
            PaneTree::Split {
                axis,
                ratios,
                children,
            } => {
                if ratios.len() != children.len() {
                    *ratios = vec![1.0; children.len()];
                }
                let old_children = std::mem::take(children);
                let old_ratios = std::mem::take(ratios);
                let mut kept: Vec<(f32, PaneTree)> = Vec::new();
                for (mut child, ratio) in old_children.into_iter().zip(old_ratios) {
                    // Only the overall root may stay empty, and only when it is a lone pane.
                    if child.prune(false) {
                        kept.push((sanitize_ratio(ratio), child));
                    }
                }
                // Merge same-axis child splits into this split.
                let mut merged: Vec<(f32, PaneTree)> = Vec::with_capacity(kept.len());
                let total: f32 = kept.iter().map(|(r, _)| r).sum();
                for (ratio, child) in kept {
                    let share = if total > 0.0 {
                        rescale(ratio, total)
                    } else {
                        0.0
                    };
                    match child {
                        PaneTree::Split {
                            axis: child_axis,
                            ratios: child_ratios,
                            children: grandchildren,
                        } if child_axis == *axis => {
                            for (r, g) in child_ratios.into_iter().zip(grandchildren) {
                                merged.push((share * r, g));
                            }
                        }
                        other => merged.push((share, other)),
                    }
                }
                match merged.len() {
                    0 => return false,
                    1 => {
                        if let Some((_, only)) = merged.pop() {
                            *self = only;
                        }
                        return true;
                    }
                    _ => {}
                }
                let total: f32 = merged.iter().map(|(r, _)| r).sum();
                for (ratio, child) in merged {
                    ratios.push(rescale(ratio, total));
                    children.push(child);
                }
                true
            }
        }
    }

    /// Move the boundary between child `divider` and `divider + 1` of the split at `path` so the
    /// cumulative share before it equals `position` (clamped so both neighbours keep
    /// [`MIN_RATIO`]). Returns `false` when the path/divider does not exist.
    pub fn set_divider(&mut self, path: &[usize], divider: usize, position: f32) -> bool {
        let Some(PaneTree::Split { ratios, .. }) = self.node_at_mut(path) else {
            return false;
        };
        if divider + 1 >= ratios.len() {
            return false;
        }
        let before: f32 = ratios[..divider].iter().sum();
        let pair = ratios[divider] + ratios[divider + 1];
        let min = before + MIN_RATIO.min(pair / 2.0);
        let max = before + pair - MIN_RATIO.min(pair / 2.0);
        let position = position.clamp(min, max);
        ratios[divider] = position - before;
        ratios[divider + 1] = before + pair - position;
        true
    }
}

fn sanitize_ratio(ratio: f32) -> f32 {
    if ratio.is_finite() && ratio > 0.0 {
        ratio
    } else {
        1.0
    }
}
