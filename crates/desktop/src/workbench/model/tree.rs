//! Pane-tree surgery: lookup, split, remove, normalize, geometry.

use super::types::{Axis, Direction, Edge, Pane, PaneId, PaneTree, Rect};

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

    /// Makes room for two panes (`pair`) that sit next to each other in a split along `axis`:
    /// each gets at least `min` points of the `length` points this node has along that axis,
    /// taken from their siblings by how much more than `min` each has. When the siblings cannot
    /// give that much, the split is shared equally. Panes that are not `pair` never grow
    /// because of it. Returns whether a split holding both was found.
    pub fn fit_pair(&mut self, pair: [PaneId; 2], axis: Axis, length: f32, min: f32) -> bool {
        let PaneTree::Split {
            axis: own,
            ratios,
            children,
        } = self
        else {
            return false;
        };
        let direct = |c: &PaneTree, id: PaneId| matches!(c, PaneTree::Pane(p) if p.id == id);
        if *own == axis
            && pair
                .iter()
                .all(|id| children.iter().any(|c| direct(c, *id)))
        {
            let n = children.len();
            let mut sizes: Vec<f32> = ratios.iter().map(|r| r * length).collect();
            let wanted: Vec<bool> = children
                .iter()
                .map(|c| pair.iter().any(|id| direct(c, *id)))
                .collect();
            let deficit: f32 = sizes
                .iter()
                .zip(&wanted)
                .filter(|(_, w)| **w)
                .map(|(size, _)| (min - size).max(0.0))
                .sum();
            if deficit <= 0.0 {
                return true;
            }
            // Whoever has more than `min` gives, by how much more (a pane of the pair that is
            // wide enough too, a pane that is narrow already not at all).
            let spare: f32 = sizes.iter().map(|size| (size - min).max(0.0)).sum();
            if spare < deficit || length <= 0.0 {
                *ratios = vec![1.0 / n as f32; n];
                return true;
            }
            let give = deficit / spare;
            for (size, wanted) in sizes.iter_mut().zip(&wanted) {
                if *wanted && *size < min {
                    *size = min;
                } else {
                    *size -= (*size - min).max(0.0) * give;
                }
            }
            let total: f32 = sizes.iter().sum();
            *ratios = sizes.iter().map(|size| size / total).collect();
            return true;
        }
        let (own, lengths): (Axis, Vec<f32>) = (*own, ratios.iter().map(|r| r * length).collect());
        children.iter_mut().zip(lengths).any(|(child, own_length)| {
            child.fit_pair(
                pair,
                axis,
                if own == axis { own_length } else { length },
                min,
            )
        })
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
                // The share of a child that went away: it goes to the neighbour it was next to
                // (the one before it, else the one after), so the other panes keep their widths
                // instead of all growing a little (a pane closed and opened again comes back to
                // the size it had).
                let mut freed = 0.0;
                for (mut child, ratio) in old_children.into_iter().zip(old_ratios) {
                    let ratio = sanitize_ratio(ratio);
                    // Only the overall root may stay empty, and only when it is a lone pane.
                    if child.prune(false) {
                        kept.push((ratio + std::mem::take(&mut freed), child));
                    } else if let Some((before, _)) = kept.last_mut() {
                        *before += ratio;
                    } else {
                        freed += ratio;
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
        self.set_divider_min(path, divider, position, MIN_RATIO)
    }

    /// [`set_divider`](Self::set_divider) with the smallest share either neighbour may keep
    /// given as `min` (still never more than half of the pair).
    pub fn set_divider_min(
        &mut self,
        path: &[usize],
        divider: usize,
        position: f32,
        min: f32,
    ) -> bool {
        let Some(PaneTree::Split { ratios, .. }) = self.node_at_mut(path) else {
            return false;
        };
        if divider + 1 >= ratios.len() {
            return false;
        }
        let before: f32 = ratios[..divider].iter().sum();
        let pair = ratios[divider] + ratios[divider + 1];
        let min = min.max(MIN_RATIO).min(pair / 2.0);
        let lo = before + min;
        let hi = before + pair - min;
        let position = position.clamp(lo, hi);
        ratios[divider] = position - before;
        ratios[divider + 1] = before + pair - position;
        true
    }

    /// Share the space of the split at `path` equally among its children (a double-click on a
    /// divider). Returns `false` when `path` is not a split.
    pub fn equalize_at(&mut self, path: &[usize]) -> bool {
        match self.node_at_mut(path) {
            Some(PaneTree::Split { ratios, .. }) => {
                let n = ratios.len().max(1);
                ratios.iter_mut().for_each(|r| *r = 1.0 / n as f32);
                true
            }
            _ => false,
        }
    }

    /// Move the divider of the nearest ancestor split of leaf `id` along `dir`'s axis one step
    /// in `dir`, by `delta` of that split. Returns whether anything moved.
    pub fn resize_leaf(&mut self, id: PaneId, dir: Direction, delta: f32) -> bool {
        let Some(path) = self.path_to(id) else {
            return false;
        };
        let axis = dir.axis();
        for depth in (0..path.len()).rev() {
            let split_path = &path[..depth];
            let child = path[depth];
            let Some(PaneTree::Split {
                axis: a, ratios, ..
            }) = self.node_at(split_path)
            else {
                continue;
            };
            if *a != axis {
                continue;
            }
            let n = ratios.len();
            let divider = if dir.is_positive() {
                if child + 1 < n {
                    child
                } else if child > 0 {
                    child - 1
                } else {
                    continue;
                }
            } else if child > 0 {
                child - 1
            } else if child + 1 < n {
                child
            } else {
                continue;
            };
            let before: f32 = ratios[..=divider].iter().sum();
            let step = if dir.is_positive() { delta } else { -delta };
            return self.set_divider(split_path, divider, before + step);
        }
        false
    }
}

fn sanitize_ratio(ratio: f32) -> f32 {
    if ratio.is_finite() && ratio > 0.0 {
        ratio
    } else {
        1.0
    }
}
