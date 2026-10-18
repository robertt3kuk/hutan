//! Where the interactive parts of the workbench were painted last, for tests and probes.
//!
//! Every `probe` records its parent's painted bounds under a [`Region`]. Views drawn through
//! `AnyView::cached` are not painted on a frame where they did not change (GPUI replays their
//! last scene), so their probes do not run either: the regions they recorded must be kept, and
//! dropped when the view paints again or leaves the window. Each cached view therefore owns the
//! regions painted inside it ([`Regions::scope`]); everything else is cleared with every render
//! of the workbench.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{Bounds, EntityId, IntoElement, Pixels, Styled, canvas};

use super::view::Region;

type Painted = HashMap<Region, Bounds<Pixels>>;

#[derive(Default)]
struct Scope {
    /// What the view's last paint recorded.
    painted: Painted,
    /// What the paint in progress has recorded so far.
    pending: Painted,
    /// A probe ran during the paint in progress: the view was drawn anew, not replayed.
    touched: bool,
    /// The same for the probes that record while the frame is prepared, before anything is
    /// painted.
    prepared: Painted,
    preparing: Painted,
    touched_in_prepare: bool,
    /// The view was rendered since its last paint (it may have no probe left to prove it).
    rendered: bool,
}

#[derive(Default)]
struct State {
    /// Regions of everything outside a cached view; cleared with every render of the workbench.
    root: Painted,
    scopes: HashMap<EntityId, Scope>,
    /// The cached views whose paint is in progress, innermost last.
    open: Vec<EntityId>,
    /// The cached views this frame's render put in the tree.
    live: HashSet<EntityId>,
    /// A cached view appeared this frame.
    fresh: bool,
}

/// The painted bounds of the probed regions; a cheap handle, shared by everything that probes.
#[derive(Clone, Default)]
pub struct Regions(Rc<RefCell<State>>);

impl std::fmt::Debug for Regions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Regions").finish_non_exhaustive()
    }
}

impl Regions {
    /// Painted bounds of `region` from the last frame, if it was drawn.
    pub fn get(&self, region: Region) -> Option<Bounds<Pixels>> {
        let state = self.0.borrow();
        state.root.get(&region).copied().or_else(|| {
            state.scopes.values().find_map(|scope| {
                scope
                    .painted
                    .get(&region)
                    .or_else(|| scope.prepared.get(&region))
                    .copied()
            })
        })
    }

    /// The workbench renders: what it paints outside cached views is recorded anew.
    pub(super) fn begin_frame(&self) {
        let mut state = self.0.borrow_mut();
        state.root.clear();
        state.live.clear();
    }

    /// Whether a cached view appeared since the last call; the workbench draws once more then
    /// (see [`Self::scope`]).
    pub(super) fn take_fresh(&self) -> bool {
        std::mem::take(&mut self.0.borrow_mut().fresh)
    }

    /// The workbench rendered: cached views that are no longer in the tree lose their regions.
    pub(super) fn end_frame(&self) {
        let mut state = self.0.borrow_mut();
        let State { scopes, live, .. } = &mut *state;
        scopes.retain(|id, _| live.contains(id));
    }

    /// A view that owns regions ran its `render`: if it paints, what it recorded before is
    /// stale even when nothing is recorded now.
    pub(crate) fn rendered(&self, view: EntityId) {
        if let Some(scope) = self.0.borrow_mut().scopes.get_mut(&view) {
            scope.rendered = true;
        }
    }

    /// Records the bounds a probe painted. `view` is the view being painted: a probe in a
    /// deferred draw (a menu, a popover) is painted after the tree, outside the brackets of the
    /// cached view it belongs to, and says whose it is that way.
    pub(crate) fn record(&self, view: EntityId, region: Region, bounds: Bounds<Pixels>) {
        let mut state = self.0.borrow_mut();
        let State {
            root, scopes, open, ..
        } = &mut *state;
        match open.last().and_then(|open| scopes.get_mut(open)) {
            Some(scope) => {
                scope.pending.insert(region, bounds);
                scope.touched = true;
            }
            None => match scopes.get_mut(&view) {
                Some(scope) => {
                    scope.painted.insert(region, bounds);
                }
                None => {
                    root.insert(region, bounds);
                }
            },
        }
    }

    /// [`Self::record`] for a probe that records while the frame is prepared (before paint).
    pub(crate) fn record_prepaint(&self, region: Region, bounds: Bounds<Pixels>) {
        let mut state = self.0.borrow_mut();
        let State {
            root, scopes, open, ..
        } = &mut *state;
        match open.last().and_then(|view| scopes.get_mut(view)) {
            Some(scope) => {
                scope.preparing.insert(region, bounds);
                scope.touched_in_prepare = true;
            }
            None => {
                root.insert(region, bounds);
            }
        }
    }

    /// The elements that bracket a cached view (`view`) in the tree: what is painted between
    /// them is that view's. Both paint nothing.
    pub(super) fn scope(
        &self,
        view: EntityId,
    ) -> (impl IntoElement + use<>, impl IntoElement + use<>) {
        {
            let mut state = self.0.borrow_mut();
            state.live.insert(view);
            if !state.scopes.contains_key(&view) {
                // GPUI follows (re-renders on notification) the entities a cached view touched
                // while it was rendered *that were not already touched earlier in the frame*;
                // one created this frame, like a panel, is therefore not followed after its
                // first render. A second render, next frame, is.
                state.fresh = true;
                state.scopes.insert(view, Scope::default());
            }
        }
        let (open, close) = (self.clone(), self.clone());
        let (open_prepare, close_prepare) = (self.clone(), self.clone());
        (
            canvas(
                move |_, _, _| {
                    let mut state = open_prepare.0.borrow_mut();
                    state.open.push(view);
                    if let Some(scope) = state.scopes.get_mut(&view) {
                        scope.preparing.clear();
                        scope.touched_in_prepare = false;
                    }
                },
                move |_, (), _, _| {
                    let mut state = open.0.borrow_mut();
                    state.open.push(view);
                    if let Some(scope) = state.scopes.get_mut(&view) {
                        scope.pending.clear();
                        scope.touched = false;
                    }
                },
            )
            .absolute()
            .size_0(),
            canvas(
                move |_, _, _| {
                    let mut state = close_prepare.0.borrow_mut();
                    state.open.pop();
                    if let Some(scope) = state.scopes.get_mut(&view)
                        && (scope.touched_in_prepare || scope.rendered)
                    {
                        scope.prepared = std::mem::take(&mut scope.preparing);
                    }
                },
                move |_, (), _, _| {
                    let mut state = close.0.borrow_mut();
                    state.open.pop();
                    if let Some(scope) = state.scopes.get_mut(&view)
                        && (scope.touched || std::mem::take(&mut scope.rendered))
                    {
                        scope.painted = std::mem::take(&mut scope.pending);
                        scope.rendered = false;
                    }
                },
            )
            .absolute()
            .size_0(),
        )
    }
}
