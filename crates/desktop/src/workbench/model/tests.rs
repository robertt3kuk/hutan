use super::*;
use std::collections::HashSet;

// ---- helpers ---------------------------------------------------------------------------

fn ids(panels: &[&str]) -> Vec<PanelId> {
    panels.iter().map(|p| PanelId::new(*p)).collect()
}

fn item(title: &str) -> ItemRef {
    ItemRef::placeholder(title)
}

fn leaf(id: u64, titles: &[&str]) -> PaneTree {
    PaneTree::Pane(Pane::new(
        PaneId(id),
        titles.iter().map(|t| item(t)).collect(),
    ))
}

fn split(axis: Axis, ratios: &[f32], children: Vec<PaneTree>) -> PaneTree {
    PaneTree::Split {
        axis,
        ratios: ratios.to_vec(),
        children,
    }
}

fn pane_titles(state: &WorkbenchState, id: u64) -> Vec<String> {
    state
        .pane(PaneId(id))
        .map(|p| {
            p.items
                .iter()
                .map(|i| match i {
                    ItemRef::Placeholder { title } => title.clone(),
                    other => format!("{other:?}"),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn sorted_items(state: &WorkbenchState) -> Vec<String> {
    let mut items: Vec<String> = state.all_items().iter().map(|i| format!("{i:?}")).collect();
    items.sort();
    items
}

fn check_tree(tree: &PaneTree, is_root: bool, errors: &mut Vec<String>) {
    match tree {
        PaneTree::Pane(pane) => {
            if pane.items.is_empty() && !is_root {
                errors.push(format!("empty non-root pane {}", pane.id));
            }
            if !pane.items.is_empty() && pane.active >= pane.items.len() {
                errors.push(format!("pane {} active out of range", pane.id));
            }
        }
        PaneTree::Split {
            axis,
            ratios,
            children,
        } => {
            if children.len() < 2 {
                errors.push("split with fewer than two children".to_owned());
            }
            if ratios.len() != children.len() {
                errors.push("ratios/children length mismatch".to_owned());
            }
            let sum: f32 = ratios.iter().sum();
            if (sum - 1.0).abs() > 1e-4 {
                errors.push(format!("ratios sum to {sum}"));
            }
            if ratios
                .iter()
                .any(|r| r.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater))
            {
                errors.push("non-positive ratio".to_owned());
            }
            for child in children {
                if matches!(child, PaneTree::Split { axis: a, .. } if a == axis) {
                    errors.push("same-axis nested split".to_owned());
                }
                check_tree(child, false, errors);
            }
        }
    }
}

/// Assert every documented invariant of the model.
#[track_caller]
fn check(state: &WorkbenchState) {
    let mut errors = Vec::new();
    check_tree(&state.center, true, &mut errors);
    let ids: Vec<PaneId> = state.panes().iter().map(|p| p.id).collect();
    if ids.iter().collect::<HashSet<_>>().len() != ids.len() {
        errors.push("duplicate pane ids".to_owned());
    }
    if let Some(z) = state.zoomed
        && !ids.contains(&z)
    {
        errors.push("zoomed pane missing".to_owned());
    }
    if let FocusTarget::Pane(p) = state.focus
        && !ids.contains(&p)
    {
        errors.push("focused pane missing".to_owned());
    }
    let mut panels = HashSet::new();
    for pos in DockPosition::ALL {
        let dock = state.docks.get(pos);
        for panel in &dock.panels {
            if !panels.insert(panel.clone()) {
                errors.push(format!("panel {panel} placed twice"));
            }
        }
        match &dock.active {
            Some(a) if !dock.panels.contains(a) => errors.push("dock active not in dock".into()),
            None if !dock.panels.is_empty() => errors.push("dock without active".into()),
            _ => {}
        }
    }
    for pane in state.panes() {
        for it in &pane.items {
            if let Some(p) = it.as_panel()
                && !panels.insert(p.clone())
            {
                errors.push(format!("panel {p} placed twice"));
            }
        }
    }
    assert!(
        errors.is_empty(),
        "invariants violated: {errors:?}\n{state:#?}"
    );
}

fn two_columns() -> WorkbenchState {
    // [ A | B ]
    WorkbenchState::with_center(split(
        Axis::Horizontal,
        &[0.5, 0.5],
        vec![leaf(1, &["a1", "a2"]), leaf(2, &["b1"])],
    ))
}

fn default_presets() -> Vec<LayoutPreset> {
    LayoutPreset::builtin()
}

// ---- default & tree basics -------------------------------------------------------------

#[test]
fn default_state_is_valid() {
    let state = WorkbenchState::default();
    check(&state);
    assert_eq!(state.docks.left.panels, ids(&["sessions", "tasks"]));
    assert_eq!(state.docks.left.active, Some(PanelId::new("sessions")));
    assert_eq!(
        state.docks.right.panels,
        ids(&["history", "changes", "inspector", "agents", "resources"])
    );
    assert_eq!(state.docks.right.active, Some(PanelId::new("history")));
    assert_eq!(
        (state.docks.left.size, state.docks.right.size),
        (280.0, 350.0)
    );
    assert!(state.docks.bottom.panels.is_empty() && !state.docks.bottom.visible);
    assert_eq!(state.panes().len(), 1);
}

#[test]
fn split_same_axis_adds_flat_sibling_and_halves_share() {
    let mut state = two_columns();
    let new = state
        .split_pane(PaneId(2), Edge::Right, item("new"))
        .unwrap();
    check(&state);
    let PaneTree::Split {
        ratios, children, ..
    } = &state.center
    else {
        panic!("expected split");
    };
    assert_eq!(children.len(), 3);
    assert!((ratios[0] - 0.5).abs() < 1e-5);
    assert!((ratios[1] - 0.25).abs() < 1e-5);
    assert!((ratios[2] - 0.25).abs() < 1e-5);
    assert_eq!(state.focus, FocusTarget::Pane(new));
}

#[test]
fn split_left_and_top_place_new_pane_before() {
    let mut state = two_columns();
    let left = state.split_pane(PaneId(1), Edge::Left, item("l")).unwrap();
    let order: Vec<PaneId> = state.panes().iter().map(|p| p.id).collect();
    assert_eq!(order[0], left);
    let top = state.split_pane(PaneId(2), Edge::Top, item("t")).unwrap();
    check(&state);
    let order: Vec<PaneId> = state.panes().iter().map(|p| p.id).collect();
    let b = order.iter().position(|p| *p == PaneId(2)).unwrap();
    let t = order.iter().position(|p| *p == top).unwrap();
    assert_eq!(t + 1, b);
}

#[test]
fn split_other_axis_nests() {
    let mut state = two_columns();
    state
        .split_pane(PaneId(2), Edge::Bottom, item("below"))
        .unwrap();
    check(&state);
    let PaneTree::Split { children, .. } = &state.center else {
        panic!("expected split");
    };
    assert_eq!(children.len(), 2);
    assert!(matches!(
        &children[1],
        PaneTree::Split {
            axis: Axis::Vertical,
            ..
        }
    ));
}

#[test]
fn split_root_pane_creates_split() {
    let mut state = WorkbenchState::default();
    let first = state.first_pane_id();
    state.split_pane(first, Edge::Right, item("x")).unwrap();
    check(&state);
    assert_eq!(state.panes().len(), 2);
}

#[test]
fn split_unknown_pane_errors_and_panel_duplicate_rejected() {
    let mut state = WorkbenchState::default();
    assert_eq!(
        state.split_pane(PaneId(99), Edge::Right, item("x")),
        Err(ModelError::NoSuchPane(PaneId(99)))
    );
    let first = state.first_pane_id();
    assert!(
        state
            .split_pane(first, Edge::Right, ItemRef::panel("sessions"))
            .is_err()
    );
    check(&state);
}

#[test]
fn pane_ids_are_never_reused() {
    let mut state = WorkbenchState::default();
    let first = state.first_pane_id();
    let a = state.split_pane(first, Edge::Right, item("a")).unwrap();
    state.close_item(a, 0).unwrap();
    let b = state.split_pane(first, Edge::Right, item("b")).unwrap();
    assert_ne!(a, b);
}

// ---- closing ---------------------------------------------------------------------------

#[test]
fn close_item_keeps_pane_when_items_remain() {
    let mut state = two_columns();
    let closed = state.close_item(PaneId(1), 0).unwrap();
    assert_eq!(closed, item("a1"));
    check(&state);
    assert_eq!(pane_titles(&state, 1), vec!["a2"]);
}

#[test]
fn close_adjusts_active_index() {
    let mut state = WorkbenchState::with_center(leaf(1, &["a", "b", "c"]));
    state.set_active_item(PaneId(1), 2).unwrap();
    state.close_item(PaneId(1), 0).unwrap();
    assert_eq!(state.pane(PaneId(1)).unwrap().active, 1);
    state.close_item(PaneId(1), 1).unwrap();
    assert_eq!(state.pane(PaneId(1)).unwrap().active, 0);
}

#[test]
fn closing_last_item_collapses_pane_and_flattens_split() {
    let mut state = two_columns();
    state.close_item(PaneId(2), 0).unwrap();
    check(&state);
    assert!(matches!(state.center, PaneTree::Pane(_)));
    assert_eq!(state.panes().len(), 1);
    assert_eq!(state.focus, FocusTarget::Pane(PaneId(1)));
}

#[test]
fn collapse_renormalizes_ratios() {
    let mut state = WorkbenchState::with_center(split(
        Axis::Horizontal,
        &[0.2, 0.3, 0.5],
        vec![leaf(1, &["a"]), leaf(2, &["b"]), leaf(3, &["c"])],
    ));
    state.close_item(PaneId(2), 0).unwrap();
    check(&state);
    let PaneTree::Split { ratios, .. } = &state.center else {
        panic!("expected split");
    };
    assert!((ratios[0] - 0.2 / 0.7).abs() < 1e-5);
    assert!((ratios[1] - 0.5 / 0.7).abs() < 1e-5);
}

#[test]
fn collapse_merges_same_axis_remainder() {
    // H[ a, V[ b, H[ c, d ] ] ]: closing b leaves H[a, H[c, d]] which must merge flat.
    let mut state = WorkbenchState::with_center(split(
        Axis::Horizontal,
        &[0.5, 0.5],
        vec![
            leaf(1, &["a"]),
            split(
                Axis::Vertical,
                &[0.5, 0.5],
                vec![
                    leaf(2, &["b"]),
                    split(
                        Axis::Horizontal,
                        &[0.5, 0.5],
                        vec![leaf(3, &["c"]), leaf(4, &["d"])],
                    ),
                ],
            ),
        ],
    ));
    state.close_item(PaneId(2), 0).unwrap();
    check(&state);
    let PaneTree::Split {
        children, ratios, ..
    } = &state.center
    else {
        panic!("expected split");
    };
    assert_eq!(children.len(), 3);
    assert!((ratios[1] - 0.25).abs() < 1e-5);
}

#[test]
fn closing_the_last_item_of_the_last_pane_leaves_an_empty_root() {
    let mut state = WorkbenchState::default();
    state.close_active_item().unwrap();
    check(&state);
    assert_eq!(state.panes().len(), 1);
    assert!(state.active_pane().unwrap().items.is_empty());
    assert!(state.close_active_item().is_none());
}

#[test]
fn closing_focused_pane_focuses_nearest_survivor() {
    let mut state = WorkbenchState::with_center(split(
        Axis::Horizontal,
        &[1.0 / 3.0; 3],
        vec![leaf(1, &["a"]), leaf(2, &["b"]), leaf(3, &["c"])],
    ));
    state.focus_pane(PaneId(2)).unwrap();
    state.close_item(PaneId(2), 0).unwrap();
    assert_eq!(state.focus, FocusTarget::Pane(PaneId(1)));
    check(&state);
}

#[test]
fn close_errors() {
    let mut state = WorkbenchState::default();
    assert!(matches!(
        state.close_item(PaneId(42), 0),
        Err(ModelError::NoSuchPane(_))
    ));
    assert!(matches!(
        state.close_item(PaneId(1), 5),
        Err(ModelError::IndexOutOfRange { .. })
    ));
}

// ---- moving ----------------------------------------------------------------------------

#[test]
fn reorder_within_pane() {
    let mut state = WorkbenchState::with_center(leaf(1, &["a", "b", "c"]));
    // Drop `a` after `c` (slot 3).
    state
        .move_item(
            ItemSource::Pane {
                pane: PaneId(1),
                index: 0,
            },
            DropTarget::Pane {
                pane: PaneId(1),
                index: 3,
            },
        )
        .unwrap();
    assert_eq!(pane_titles(&state, 1), vec!["b", "c", "a"]);
    assert_eq!(state.pane(PaneId(1)).unwrap().active, 2);
    // Drop `a` before `b` (slot 0).
    state
        .move_item(
            ItemSource::Pane {
                pane: PaneId(1),
                index: 2,
            },
            DropTarget::Pane {
                pane: PaneId(1),
                index: 0,
            },
        )
        .unwrap();
    assert_eq!(pane_titles(&state, 1), vec!["a", "b", "c"]);
    check(&state);
}

#[test]
fn move_item_to_other_pane_and_collapse_source() {
    let mut state = two_columns();
    state
        .move_item(
            ItemSource::Pane {
                pane: PaneId(2),
                index: 0,
            },
            DropTarget::Pane {
                pane: PaneId(1),
                index: 1,
            },
        )
        .unwrap();
    check(&state);
    assert_eq!(pane_titles(&state, 1), vec!["a1", "b1", "a2"]);
    assert_eq!(state.panes().len(), 1);
    assert_eq!(state.focus, FocusTarget::Pane(PaneId(1)));
    assert_eq!(state.pane(PaneId(1)).unwrap().active, 1);
}

#[test]
fn move_item_to_new_split_edge() {
    let mut state = two_columns();
    let before = sorted_items(&state);
    state
        .move_item(
            ItemSource::Pane {
                pane: PaneId(1),
                index: 1,
            },
            DropTarget::Split {
                pane: PaneId(2),
                edge: Edge::Bottom,
            },
        )
        .unwrap();
    check(&state);
    assert_eq!(sorted_items(&state), before);
    assert_eq!(pane_titles(&state, 1), vec!["a1"]);
    assert_eq!(state.panes().len(), 3);
    let FocusTarget::Pane(f) = state.focus else {
        panic!("pane focus expected")
    };
    assert_eq!(state.pane(f).unwrap().items, vec![item("a2")]);
}

#[test]
fn moving_only_tab_to_split_of_its_own_pane_is_a_noop() {
    let mut state = two_columns();
    let before = state.clone();
    state
        .move_item(
            ItemSource::Pane {
                pane: PaneId(2),
                index: 0,
            },
            DropTarget::Split {
                pane: PaneId(2),
                edge: Edge::Right,
            },
        )
        .unwrap();
    assert_eq!(state, before);
}

#[test]
fn split_off_a_tab_of_its_own_pane() {
    let mut state = two_columns();
    state
        .move_item(
            ItemSource::Pane {
                pane: PaneId(1),
                index: 0,
            },
            DropTarget::Split {
                pane: PaneId(1),
                edge: Edge::Bottom,
            },
        )
        .unwrap();
    check(&state);
    assert_eq!(pane_titles(&state, 1), vec!["a2"]);
    assert_eq!(state.panes().len(), 3);
}

#[test]
fn non_panels_cannot_enter_docks() {
    let mut state = two_columns();
    let before = state.clone();
    let err = state
        .move_item(
            ItemSource::Pane {
                pane: PaneId(1),
                index: 0,
            },
            DropTarget::Dock {
                pos: DockPosition::Bottom,
                index: 0,
            },
        )
        .unwrap_err();
    assert_eq!(err, ModelError::NotAPanel);
    assert_eq!(state, before);
}

#[test]
fn panel_moves_between_docks_and_reveals_target() {
    let mut state = WorkbenchState::default();
    state.docks.bottom.visible = false;
    state
        .move_panel(
            &PanelId::new("inspector"),
            PanelDest::Dock(DockPosition::Bottom),
        )
        .unwrap();
    check(&state);
    assert_eq!(
        state.docks.right.panels,
        ids(&["history", "changes", "agents", "resources"])
    );
    assert_eq!(state.docks.right.active, Some(PanelId::new("history")));
    assert_eq!(state.docks.bottom.panels, ids(&["inspector"]));
    assert!(state.docks.bottom.visible);
    assert_eq!(state.focus, FocusTarget::Dock(DockPosition::Bottom));
}

#[test]
fn panel_dock_to_center_and_back() {
    let mut state = WorkbenchState::default();
    let before = sorted_items(&state);
    state
        .move_panel(&PanelId::new("sessions"), PanelDest::Center)
        .unwrap();
    check(&state);
    assert_eq!(state.docks.left.panels, ids(&["tasks"]));
    assert!(
        state
            .active_pane()
            .unwrap()
            .items
            .contains(&ItemRef::panel("sessions"))
    );
    assert_eq!(sorted_items(&state), before);
    state
        .move_panel(
            &PanelId::new("sessions"),
            PanelDest::Dock(DockPosition::Left),
        )
        .unwrap();
    check(&state);
    assert_eq!(state.docks.left.panels, ids(&["tasks", "sessions"]));
    assert_eq!(sorted_items(&state), before);
}

#[test]
fn panel_drop_into_new_split_from_dock() {
    let mut state = WorkbenchState::default();
    let first = state.first_pane_id();
    state
        .move_item(
            ItemSource::Dock {
                pos: DockPosition::Right,
                panel: PanelId::new("inspector"),
            },
            DropTarget::Split {
                pane: first,
                edge: Edge::Right,
            },
        )
        .unwrap();
    check(&state);
    assert_eq!(state.panes().len(), 2);
    assert_eq!(
        state.docks.right.panels,
        ids(&["history", "changes", "agents", "resources"])
    );
}

#[test]
fn dock_reorder() {
    let mut state = WorkbenchState::default();
    state.open_panel(&PanelId::new("notifications"), DockPosition::Left);
    assert_eq!(
        state.docks.left.panels,
        ids(&["sessions", "tasks", "notifications"])
    );
    state
        .move_item(
            ItemSource::Dock {
                pos: DockPosition::Left,
                panel: PanelId::new("notifications"),
            },
            DropTarget::Dock {
                pos: DockPosition::Left,
                index: 0,
            },
        )
        .unwrap();
    check(&state);
    assert_eq!(
        state.docks.left.panels,
        ids(&["notifications", "sessions", "tasks"])
    );
    assert_eq!(state.docks.left.active, Some(PanelId::new("notifications")));
}

#[test]
fn move_errors_leave_state_untouched() {
    let mut state = two_columns();
    let before = state.clone();
    assert!(
        state
            .move_item(
                ItemSource::Pane {
                    pane: PaneId(9),
                    index: 0
                },
                DropTarget::Pane {
                    pane: PaneId(1),
                    index: 0
                },
            )
            .is_err()
    );
    assert!(
        state
            .move_item(
                ItemSource::Pane {
                    pane: PaneId(1),
                    index: 0
                },
                DropTarget::Pane {
                    pane: PaneId(1),
                    index: 9
                },
            )
            .is_err()
    );
    assert!(
        state
            .move_item(
                ItemSource::Pane {
                    pane: PaneId(1),
                    index: 7
                },
                DropTarget::Pane {
                    pane: PaneId(2),
                    index: 0
                },
            )
            .is_err()
    );
    assert!(
        state
            .move_panel(&PanelId::new("nope"), PanelDest::Center)
            .is_err()
    );
    assert_eq!(state, before);
}

#[test]
fn open_and_close_panel() {
    let mut state = WorkbenchState::default();
    state.docks.bottom.visible = false;
    state.open_panel(&PanelId::new("notifications"), DockPosition::Bottom);
    check(&state);
    assert!(state.docks.bottom.visible);
    assert_eq!(
        state.docks.bottom.active,
        Some(PanelId::new("notifications"))
    );
    // Opening again is idempotent.
    state.open_panel(&PanelId::new("notifications"), DockPosition::Left);
    assert_eq!(state.docks.bottom.panels.len(), 1, "once");
    state.close_panel(&PanelId::new("notifications")).unwrap();
    check(&state);
    assert!(state.docks.bottom.panels.is_empty());
    assert!(state.close_panel(&PanelId::new("notifications")).is_err());
}

#[test]
fn closing_a_center_panel_tab_closes_the_panel() {
    let mut state = WorkbenchState::default();
    state
        .move_panel(&PanelId::new("inspector"), PanelDest::Center)
        .unwrap();
    state.close_panel(&PanelId::new("inspector")).unwrap();
    check(&state);
    assert!(state.find_panel(&PanelId::new("inspector")).is_none());
}

// ---- resize ----------------------------------------------------------------------------

#[test]
fn set_divider_clamps_to_min_ratio() {
    let mut state = two_columns();
    state.set_divider(&[], 0, 0.7).unwrap();
    let PaneTree::Split { ratios, .. } = &state.center else {
        panic!("expected split");
    };
    assert!((ratios[0] - 0.7).abs() < 1e-5);
    state.set_divider(&[], 0, -3.0).unwrap();
    state.set_divider(&[], 0, 0.0).unwrap();
    check(&state);
    let PaneTree::Split { ratios, .. } = &state.center else {
        panic!("expected split");
    };
    assert!((ratios[0] - MIN_RATIO).abs() < 1e-5);
    state.set_divider(&[], 0, 9.0).unwrap();
    check(&state);
    assert!(state.set_divider(&[], 5, 0.5).is_err());
    assert!(state.set_divider(&[0], 0, 0.5).is_err());
}

#[test]
fn set_divider_in_three_way_split_keeps_other_children() {
    let mut state = WorkbenchState::with_center(split(
        Axis::Horizontal,
        &[0.25, 0.25, 0.5],
        vec![leaf(1, &["a"]), leaf(2, &["b"]), leaf(3, &["c"])],
    ));
    state.set_divider(&[], 1, 0.6).unwrap();
    check(&state);
    let PaneTree::Split { ratios, .. } = &state.center else {
        panic!("expected split");
    };
    assert!((ratios[0] - 0.25).abs() < 1e-5);
    assert!((ratios[1] - 0.35).abs() < 1e-5);
    assert!((ratios[2] - 0.4).abs() < 1e-5);
}

#[test]
fn keyboard_resize_moves_nearest_divider() {
    let mut state = two_columns();
    state.focus_pane(PaneId(1)).unwrap();
    assert!(state.resize_focused(Direction::Right, 10.0, 0.1));
    let PaneTree::Split { ratios, .. } = &state.center else {
        panic!("expected split");
    };
    assert!((ratios[0] - 0.6).abs() < 1e-5);
    // Right pane resizing Left moves the same divider left.
    state.focus_pane(PaneId(2)).unwrap();
    assert!(state.resize_focused(Direction::Left, 10.0, 0.2));
    let PaneTree::Split { ratios, .. } = &state.center else {
        panic!("expected split");
    };
    assert!((ratios[0] - 0.4).abs() < 1e-5);
    // No vertical split exists.
    assert!(!state.resize_focused(Direction::Down, 10.0, 0.1));
    check(&state);
}

#[test]
fn keyboard_resize_walks_up_to_matching_axis() {
    let mut state = WorkbenchState::with_center(split(
        Axis::Horizontal,
        &[0.5, 0.5],
        vec![
            leaf(1, &["a"]),
            split(
                Axis::Vertical,
                &[0.5, 0.5],
                vec![leaf(2, &["b"]), leaf(3, &["c"])],
            ),
        ],
    ));
    state.focus_pane(PaneId(3)).unwrap();
    assert!(state.resize_focused(Direction::Left, 10.0, 0.1));
    let PaneTree::Split { ratios, .. } = &state.center else {
        panic!("expected split");
    };
    assert!((ratios[0] - 0.4).abs() < 1e-5);
    assert!(state.resize_focused(Direction::Up, 10.0, 0.1));
    check(&state);
}

#[test]
fn dock_resize_clamps_to_the_range_of_its_side_and_keyboard_resize_grows_toward_center() {
    let mut state = WorkbenchState::default();
    let (min, max) = DockPosition::Left.size_range();
    assert_eq!((min, max), (220.0, 500.0));
    assert_eq!(DockPosition::Right.size_range(), (240.0, 480.0));
    state.resize_dock(DockPosition::Left, 10.0);
    assert_eq!(state.docks.left.size, min);
    state.resize_dock(DockPosition::Left, 99999.0);
    assert_eq!(state.docks.left.size, max);
    state.resize_dock(DockPosition::Left, f32::NAN);
    assert_eq!(state.docks.left.size, max);
    state.resize_dock(DockPosition::Left, 240.0);
    state.focus_dock(DockPosition::Left);
    assert!(state.resize_focused(Direction::Right, 20.0, 0.1));
    assert_eq!(state.docks.left.size, 260.0);
    assert!(state.resize_focused(Direction::Left, 20.0, 0.1));
    assert_eq!(state.docks.left.size, 240.0);
    assert!(!state.resize_focused(Direction::Up, 20.0, 0.1));
    state.focus_dock(DockPosition::Right);
    assert!(state.resize_focused(Direction::Left, 20.0, 0.1));
    assert_eq!(state.docks.right.size, 370.0);
    state.resize_dock(DockPosition::Right, 1000.0);
    assert_eq!(state.docks.right.size, 480.0);
    state.resize_dock(DockPosition::Bottom, 1.0);
    assert_eq!(state.docks.bottom.size, MIN_DOCK_SIZE);
}

// ---- dock toggle -----------------------------------------------------------------------

#[test]
fn toggle_dock_and_focus_fallback() {
    let mut state = WorkbenchState::default();
    state.focus_dock(DockPosition::Left);
    // ⌘B: Expanded to Compact, the keyboard goes to the center.
    state.toggle_dock(DockPosition::Left);
    assert_eq!(state.docks.left.mode(), DockMode::Compact);
    assert!(matches!(state.focus, FocusTarget::Pane(_)));
    state.toggle_dock(DockPosition::Left);
    assert_eq!(state.docks.left.mode(), DockMode::Expanded);
}

#[test]
fn close_all_docks_hides_every_dock_and_returns_focus_to_the_center() {
    let mut state = WorkbenchState::default();
    state.set_dock_mode(DockPosition::Bottom, DockMode::Expanded);
    state.focus_dock(DockPosition::Bottom);
    state.close_all_docks();
    assert!(!state.docks.left.visible && !state.docks.right.visible && !state.docks.bottom.visible);
    assert!(matches!(state.focus, FocusTarget::Pane(_)));
    // Closing again changes nothing.
    let before = state.clone();
    state.close_all_docks();
    assert_eq!(state, before);
}

// ---- zoom ------------------------------------------------------------------------------

#[test]
fn zoom_toggle_and_invalidation() {
    let mut state = two_columns();
    state.focus_pane(PaneId(2)).unwrap();
    state.toggle_zoom();
    assert_eq!(state.zoomed, Some(PaneId(2)));
    // Focusing another pane while zoomed moves the zoom with it.
    state.focus_pane(PaneId(1)).unwrap();
    assert_eq!(state.zoomed, Some(PaneId(1)));
    state.toggle_zoom();
    assert_eq!(state.zoomed, None);
    state.zoom(PaneId(2)).unwrap();
    state.close_item(PaneId(2), 0).unwrap();
    assert_eq!(state.zoomed, None);
    check(&state);
}

#[test]
fn splitting_unzooms() {
    let mut state = two_columns();
    state.zoom(PaneId(1)).unwrap();
    state.split_pane(PaneId(1), Edge::Right, item("n")).unwrap();
    assert_eq!(state.zoomed, None);
}

// ---- focus neighbor --------------------------------------------------------------------

/// ```text
/// +---+-------+
/// | 1 |   2   |
/// |   +---+---+
/// |   | 3 | 4 |
/// +---+---+---+
/// ```
fn grid() -> WorkbenchState {
    WorkbenchState::with_center(split(
        Axis::Horizontal,
        &[0.4, 0.6],
        vec![
            leaf(1, &["a"]),
            split(
                Axis::Vertical,
                &[0.5, 0.5],
                vec![
                    leaf(2, &["b"]),
                    split(
                        Axis::Horizontal,
                        &[0.5, 0.5],
                        vec![leaf(3, &["c"]), leaf(4, &["d"])],
                    ),
                ],
            ),
        ],
    ))
}

#[test]
fn focus_neighbor_is_geometric() {
    let mut state = grid();
    check(&state);
    state.focus_pane(PaneId(1)).unwrap();
    assert!(state.focus_neighbor(Direction::Right));
    assert_eq!(state.focus, FocusTarget::Pane(PaneId(2)));
    assert!(state.focus_neighbor(Direction::Down));
    assert_eq!(state.focus, FocusTarget::Pane(PaneId(3)));
    assert!(state.focus_neighbor(Direction::Right));
    assert_eq!(state.focus, FocusTarget::Pane(PaneId(4)));
    assert!(state.focus_neighbor(Direction::Up));
    assert_eq!(state.focus, FocusTarget::Pane(PaneId(2)));
    // Left from the wide top pane goes to the tall left pane.
    assert!(state.focus_neighbor(Direction::Left));
    assert_eq!(state.focus, FocusTarget::Pane(PaneId(1)));
}

#[test]
fn focus_neighbor_picks_largest_overlap_and_stops_at_edges() {
    let mut state = grid();
    state.docks.left.visible = false;
    state.docks.right.visible = false;
    state.focus_pane(PaneId(1)).unwrap();
    assert!(!state.focus_neighbor(Direction::Left));
    assert!(!state.focus_neighbor(Direction::Up));
    state.focus_pane(PaneId(4)).unwrap();
    assert!(!state.focus_neighbor(Direction::Right));
    assert!(!state.focus_neighbor(Direction::Down));
    // From 3, moving left reaches pane 1.
    state.focus_pane(PaneId(3)).unwrap();
    assert!(state.focus_neighbor(Direction::Left));
    assert_eq!(state.focus, FocusTarget::Pane(PaneId(1)));
}

#[test]
fn focus_neighbor_moves_between_center_and_docks() {
    let mut state = grid();
    state.focus_pane(PaneId(1)).unwrap();
    assert!(state.focus_neighbor(Direction::Left));
    assert_eq!(state.focus, FocusTarget::Dock(DockPosition::Left));
    assert!(state.focus_neighbor(Direction::Right));
    assert_eq!(state.focus, FocusTarget::Pane(PaneId(1)));
    // Pane 4 touches the right edge; pane 3 does not.
    state.focus_pane(PaneId(3)).unwrap();
    state.focus_pane(PaneId(4)).unwrap();
    assert!(state.focus_neighbor(Direction::Right));
    assert_eq!(state.focus, FocusTarget::Dock(DockPosition::Right));
    assert!(state.focus_neighbor(Direction::Left));
    assert_eq!(state.focus, FocusTarget::Pane(PaneId(2)));
    // Hidden docks are not reachable.
    state.docks.bottom.visible = true;
    state.open_panel(&PanelId::new("notifications"), DockPosition::Bottom);
    state.focus_pane(PaneId(3)).unwrap();
    assert!(state.focus_neighbor(Direction::Down));
    assert_eq!(state.focus, FocusTarget::Dock(DockPosition::Bottom));
    assert!(state.focus_neighbor(Direction::Up));
    // Leaving the bottom dock upward lands on the bottom-left pane.
    assert_eq!(state.focus, FocusTarget::Pane(PaneId(1)));
    state.set_dock_mode(DockPosition::Bottom, DockMode::Hidden);
    state.focus_pane(PaneId(3)).unwrap();
    assert!(!state.focus_neighbor(Direction::Down));
}

#[test]
fn focus_neighbor_is_disabled_while_zoomed() {
    let mut state = two_columns();
    state.zoom(PaneId(1)).unwrap();
    assert!(!state.focus_neighbor(Direction::Right));
}

#[test]
fn add_item_appends_activates_and_rejects_duplicate_panels() {
    let mut state = WorkbenchState::with_center(leaf(1, &["a"]));
    assert_eq!(state.add_item(PaneId(1), item("b")), Ok(1));
    assert_eq!(state.pane(PaneId(1)).unwrap().active, 1);
    assert!(
        state
            .add_item(PaneId(1), ItemRef::panel("sessions"))
            .is_err()
    );
    assert!(state.add_item(PaneId(9), item("x")).is_err());
    check(&state);
}

// ---- tabs ------------------------------------------------------------------------------

#[test]
fn select_tab_bounds() {
    let mut state = WorkbenchState::with_center(leaf(1, &["a", "b"]));
    assert!(state.select_tab(1));
    assert_eq!(state.active_pane().unwrap().active, 1);
    assert!(!state.select_tab(2));
    assert_eq!(state.active_item(), Some(item("b")));
}

#[test]
fn cycling_tabs_wraps_in_the_pane_and_follows_a_focused_dock() {
    let mut state = WorkbenchState::with_center(leaf(1, &["a", "b", "c"]));
    assert!(state.select_tab(0));
    assert!(state.cycle_tab(false), "previous from the first tab wraps");
    assert_eq!(state.active_item(), Some(item("c")));
    assert!(state.cycle_tab(true));
    assert_eq!(state.active_item(), Some(item("a")));
    assert!(state.cycle_tab(true));
    assert_eq!(state.active_item(), Some(item("b")));

    // A focused dock cycles its own panels and leaves the pane alone.
    let panels = state.docks.left.panels.clone();
    assert!(panels.len() > 1, "the default left dock has several panels");
    state.docks.left.active = panels.first().cloned();
    state.focus = FocusTarget::Dock(DockPosition::Left);
    assert!(state.cycle_tab(true));
    assert_eq!(state.docks.left.active.as_ref(), Some(&panels[1]));
    assert!(state.cycle_tab(false));
    assert!(state.cycle_tab(false));
    assert_eq!(state.docks.left.active.as_ref(), panels.last());
    assert_eq!(
        state.active_pane().unwrap().active,
        1,
        "the pane kept its tab"
    );

    let mut single = WorkbenchState::with_center(leaf(1, &["only"]));
    assert!(!single.cycle_tab(true), "one tab: nothing to cycle");
}

// ---- presets ---------------------------------------------------------------------------

#[test]
fn builtin_presets_match_design_table() {
    let presets = default_presets();
    let names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["default", "focus", "overview"]);
    let focus = &presets[1];
    assert!(!focus.docks.left.visible && !focus.docks.right.visible && !focus.docks.bottom.visible);
    assert!(focus.zoom_active);
    let overview = &presets[2];
    assert_eq!(
        overview.center,
        CenterLayout::Items(vec![ItemRef::panel(AGENTS_PAGE)])
    );
    assert!(overview.docks.left.visible && overview.docks.right.visible);
}

#[test]
fn applying_presets_conserves_items() {
    let presets = default_presets();
    let mut state = grid();
    state.open_panel(&PanelId::new("notifications"), DockPosition::Bottom);
    state
        .move_panel(&PanelId::new("inspector"), PanelDest::Center)
        .unwrap();
    let original = sorted_items(&state);

    for preset in &presets {
        state.apply_preset(preset);
        check(&state);
        let now = sorted_items(&state);
        for it in &original {
            assert!(now.contains(it), "{it} lost applying {}", preset.name);
        }
        for pos in DockPosition::ALL {
            assert_eq!(
                state.docks.get(pos).panels,
                preset.docks.get(pos).panels,
                "{} dock {pos:?}",
                preset.name
            );
            assert_eq!(state.docks.get(pos).visible, preset.docks.get(pos).visible);
        }
        assert_eq!(state.preset, preset.name);
    }
}

#[test]
fn preset_cycle_never_loses_session_items() {
    let presets = default_presets();
    let mut state = WorkbenchState::with_center(split(
        Axis::Horizontal,
        &[0.5, 0.5],
        vec![
            PaneTree::Pane(Pane::new(
                PaneId(1),
                vec![
                    ItemRef::Terminal {
                        session: "s1".into(),
                    },
                    ItemRef::Chat {
                        session: "s2".into(),
                    },
                ],
            )),
            PaneTree::Pane(Pane::new(PaneId(2), vec![item("note")])),
        ],
    ));
    let sessions = |s: &WorkbenchState| {
        let mut v: Vec<String> = s
            .all_items()
            .into_iter()
            .filter(|i| i.as_panel().is_none())
            .map(|i| format!("{i:?}"))
            .collect();
        v.sort();
        v
    };
    let before = sessions(&state);
    for name in [
        "overview", "focus", "default", "overview", "default", "focus", "overview",
    ] {
        let p = presets.iter().find(|p| p.name == name).unwrap();
        state.apply_preset(p);
        check(&state);
        assert_eq!(sessions(&state), before, "after {name}");
    }
}

#[test]
fn overview_preset_shows_only_the_board_and_stashes_the_tabs() {
    let mut state = grid();
    state.apply_preset(&LayoutPreset::overview_preset());
    check(&state);
    assert_eq!(state.panes().len(), 1);
    let pane = state.active_pane().unwrap();
    assert_eq!(
        pane.items,
        vec![ItemRef::panel(AGENTS_PAGE)],
        "only the board is shown"
    );
    assert_eq!(pane.active, 0);
    assert_eq!(state.zoomed, None);
    // The tabs are not lost: they wait in the stash and count as open items.
    let all = state.all_items();
    for name in ["a", "b", "c", "d"] {
        assert!(all.contains(&item(name)), "{name} is stashed");
    }
}

#[test]
fn focus_preset_zooms_active_pane_and_hides_docks() {
    let mut state = grid();
    state.focus_pane(PaneId(3)).unwrap();
    state.apply_preset(&LayoutPreset::focus_preset());
    check(&state);
    assert_eq!(state.zoomed, Some(PaneId(3)));
    assert!(!state.docks.left.visible);
    assert_eq!(state.panes().len(), 4);
    state.apply_preset(&LayoutPreset::default_preset());
    assert_eq!(state.zoomed, None);
    assert!(state.docks.left.visible);
    assert_eq!(state.focus, FocusTarget::Pane(PaneId(3)));
}

#[test]
fn preset_places_unlisted_open_panels_in_first_pane() {
    let mut state = WorkbenchState::default();
    state.open_panel(&PanelId::new("notifications"), DockPosition::Bottom);
    state.apply_preset(&LayoutPreset::default_preset());
    check(&state);
    assert!(state.docks.bottom.panels.is_empty());
    assert!(
        state
            .panes()
            .iter()
            .any(|p| p.items.contains(&ItemRef::panel("notifications")))
    );
}

#[test]
fn toggle_overview_round_trips() {
    let presets = default_presets();
    let mut state = grid();
    state.apply_preset(&LayoutPreset::focus_preset());
    state.toggle_preset("overview", &presets).unwrap();
    assert_eq!(state.preset, "overview");
    assert_eq!(state.previous_preset.as_deref(), Some("focus"));
    check(&state);
    state.toggle_preset("overview", &presets).unwrap();
    check(&state);
    assert_eq!(state.preset, "focus");
    // The page only lives in the center while its preset is selected; no dock lists it (Agents
    // is a page of the center, not of a sidebar).
    assert_eq!(state.find_panel(&PanelId::new(AGENTS_PAGE)), None);
    assert_eq!(
        state.panes().iter().map(|p| p.items.len()).sum::<usize>(),
        4
    );
    assert!(!state.docks.left.visible);
}

/// A non-trivial pre-overview layout: grid, ratios dragged, an active tab, a resized dock.
fn customized_grid() -> WorkbenchState {
    let mut state = grid();
    state.add_item(PaneId(3), item("c2")).unwrap();
    state.set_active_item(PaneId(3), 0).unwrap();
    state.set_divider(&[], 0, 0.3).unwrap();
    state.set_divider(&[1], 0, 0.7).unwrap();
    state.resize_dock(DockPosition::Left, 333.0);
    state.open_panel(&PanelId::new("notifications"), DockPosition::Bottom);
    state.focus_pane(PaneId(4)).unwrap();
    check(&state);
    state
}

#[test]
fn overview_toggle_restores_the_exact_previous_state() {
    let presets = default_presets();
    let mut state = customized_grid();
    let before = state.clone();
    state.toggle_preset("overview", &presets).unwrap();
    assert_eq!(state.preset, "overview");
    assert_eq!(state.panes().len(), 1, "overview shows one pane");
    assert!(state.stashed.is_some());
    check(&state);
    state.toggle_preset("overview", &presets).unwrap();
    check(&state);
    assert!(state.stashed.is_none());
    assert_eq!(state, before);
}

#[test]
fn items_opened_during_overview_land_in_the_restored_first_pane() {
    let presets = default_presets();
    let mut state = customized_grid();
    let before = state.clone();
    state.toggle_preset("overview", &presets).unwrap();
    let pane = state.active_pane_id();
    state
        .add_item(
            pane,
            ItemRef::Terminal {
                session: "new".into(),
            },
        )
        .unwrap();
    // A panel opened while in overview counts as new too.
    state.open_panel(&PanelId::new("sessions"), DockPosition::Left);
    state.toggle_preset("overview", &presets).unwrap();
    check(&state);
    let first = state.first_pane_id();
    let items = &state.pane(first).unwrap().items;
    assert_eq!(
        items.last(),
        Some(&ItemRef::Terminal {
            session: "new".into()
        })
    );
    assert!(
        state
            .panes()
            .iter()
            .all(|p| !p.items.contains(&ItemRef::panel(AGENTS_PAGE))),
        "the board left the center"
    );
    // Everything else is as before: same tree shape and other panes untouched.
    let mut expected = before.clone();
    expected
        .pane_mut_for_test(first)
        .items
        .push(ItemRef::Terminal {
            session: "new".into(),
        });
    assert_eq!(state, expected);
}

#[test]
fn panels_closed_during_overview_stay_closed_and_hidden_tabs_come_back() {
    let presets = default_presets();
    let mut state = customized_grid();
    state.toggle_preset("overview", &presets).unwrap();
    assert!(state.find_panel(&PanelId::new("inspector")).is_some());
    state.close_panel(&PanelId::new("inspector")).unwrap();
    state.toggle_preset("overview", &presets).unwrap();
    check(&state);
    assert!(state.find_panel(&PanelId::new("inspector")).is_none());
    for name in ["a", "b", "c", "d", "c2"] {
        assert!(
            state.all_items().contains(&item(name)),
            "{name} was only hidden"
        );
    }
    // The panel the overview placed on the bottom dock before (notifications) is back too.
    assert!(state.find_panel(&PanelId::new("notifications")).is_some());
}

#[test]
fn quitting_while_in_overview_still_restores_on_toggle() {
    let presets = default_presets();
    let mut state = customized_grid();
    let before = state.clone();
    state.toggle_preset("overview", &presets).unwrap();
    let json = state.to_json().unwrap();
    let mut relaunched = WorkbenchState::from_json(&json).unwrap();
    assert_eq!(relaunched, state);
    relaunched.toggle_preset("overview", &presets).unwrap();
    check(&relaunched);
    assert_eq!(relaunched, before);
}

#[test]
fn other_presets_discard_the_saved_layout() {
    let presets = default_presets();
    let mut state = customized_grid();
    state.toggle_preset("overview", &presets).unwrap();
    state.apply_preset(&LayoutPreset::focus_preset());
    assert!(state.stashed.is_none());
    check(&state);
    // Re-applying overview while in overview keeps the original stash.
    let mut state = customized_grid();
    let before = state.clone();
    state.apply_preset(&LayoutPreset::overview_preset());
    state.apply_preset(&LayoutPreset::overview_preset());
    state.toggle_preset("overview", &presets).unwrap();
    assert_eq!(state, before);
}

#[test]
fn toggle_overview_without_history_returns_to_default() {
    let presets = default_presets();
    let mut state = WorkbenchState::default();
    state.toggle_preset("overview", &presets).unwrap();
    state.toggle_preset("overview", &presets).unwrap();
    assert_eq!(state.preset, "default");
    assert!(
        state
            .panes()
            .iter()
            .all(|p| !p.items.contains(&ItemRef::panel(AGENTS_PAGE))),
        "the board left the center"
    );
    assert_eq!(state.docks, LayoutPreset::default_preset().docks);
}

#[test]
fn toggle_unknown_preset_errors() {
    let mut state = WorkbenchState::default();
    assert_eq!(
        state.toggle_preset("nope", &default_presets()),
        Err(ModelError::UnknownPreset("nope".to_owned()))
    );
}

// ---- serialization ---------------------------------------------------------------------

#[test]
fn json_roundtrip_preserves_state() {
    let mut state = grid();
    state.focus_pane(PaneId(3)).unwrap();
    state.zoom(PaneId(3)).unwrap();
    state.open_panel(&PanelId::new("notifications"), DockPosition::Bottom);
    state.resize_dock(DockPosition::Left, 311.0);
    state.apply_preset(&LayoutPreset::overview_preset());
    state
        .split_pane(
            PaneId(1),
            Edge::Right,
            ItemRef::Terminal {
                session: "x".into(),
            },
        )
        .unwrap();
    let json = state.to_json().unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["version"], 6);
    let back = WorkbenchState::from_json(&json).unwrap();
    assert_eq!(back, state);
    check(&back);
}

#[test]
fn json_roundtrip_keeps_pane_id_counter() {
    let mut state = WorkbenchState::default();
    let first = state.first_pane_id();
    let a = state.split_pane(first, Edge::Right, item("a")).unwrap();
    state.close_item(a, 0).unwrap();
    let back = WorkbenchState::from_json(&state.to_json().unwrap()).unwrap();
    let mut back = back;
    let b = back.split_pane(first, Edge::Right, item("b")).unwrap();
    assert_ne!(a, b);
}

#[test]
fn unknown_version_falls_back_to_default_with_warning() {
    let json = r#"{ "version": 999, "docks": 1 }"#;
    let outcome = WorkbenchState::from_json_or_default(json);
    assert_eq!(outcome.state, WorkbenchState::default());
    assert!(outcome.warning.unwrap().contains("999"));
}

#[test]
fn corrupt_and_missing_version_fall_back() {
    for json in [
        "",
        "not json",
        "{}",
        r#"{"version":1}"#,
        r#"{"version":1,"docks":{}}"#,
    ] {
        let outcome = WorkbenchState::from_json_or_default(json);
        assert_eq!(outcome.state, WorkbenchState::default(), "{json}");
        assert!(outcome.warning.is_some(), "{json}");
    }
}

#[test]
fn loading_repairs_hand_edited_state() {
    let mut state = grid();
    state.docks.left.size = 5.0;
    let mut value: serde_json::Value = serde_json::from_str(&state.to_json().unwrap()).unwrap();
    // Break the ratios, add an empty pane and a duplicated panel, point focus at nothing.
    value["center"]["split"]["ratios"] = serde_json::json!([3.0, 1.0]);
    value["center"]["split"]["children"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"pane": {"id": 77, "items": [], "active": 4}}));
    value["center"]["split"]["children"][0]["pane"]["items"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"panel": {"panel": "sessions"}}));
    value["focus"] = serde_json::json!({"pane": 1234});
    value["zoomed"] = serde_json::json!(4321);
    let loaded = WorkbenchState::from_json(&value.to_string()).unwrap();
    check(&loaded);
    assert_eq!(loaded.docks.left.size, DockPosition::Left.size_range().0);
    assert_eq!(loaded.zoomed, None);
    assert_eq!(loaded.focus, FocusTarget::Pane(loaded.first_pane_id()));
    assert!(loaded.pane(PaneId(77)).is_none());
    assert!(
        !loaded
            .panes()
            .iter()
            .any(|p| p.items.contains(&ItemRef::panel("sessions")))
    );
}

#[test]
fn duplicate_pane_ids_are_reassigned_on_load() {
    let state = WorkbenchState::with_center(split(
        Axis::Horizontal,
        &[0.5, 0.5],
        vec![leaf(5, &["a"]), leaf(5, &["b"])],
    ));
    check(&state);
    let ids: Vec<PaneId> = state.panes().iter().map(|p| p.id).collect();
    assert_ne!(ids[0], ids[1]);
}

// ---- randomized invariant check --------------------------------------------------------

impl WorkbenchState {
    fn pane_mut_for_test(&mut self, id: PaneId) -> &mut Pane {
        self.center.pane_mut(id).unwrap()
    }
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % bound.max(1)
    }
}

#[test]
fn random_operations_preserve_invariants_and_conserve_items() {
    let presets = default_presets();
    let edges = [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom];
    let dirs = [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ];
    for seed in 0..40u64 {
        let mut rng = Lcg(seed + 1);
        let mut state = WorkbenchState::default();
        let mut counter = 0;
        for _ in 0..80 {
            let panes: Vec<PaneId> = state.panes().iter().map(|p| p.id).collect();
            let pane = panes[rng.next(panes.len())];
            let len = state.pane(pane).map_or(0, |p| p.items.len());
            let before = sorted_items(&state);
            // Closing drops items and presets may open their own panels; everything else must
            // conserve the multiset exactly.
            let mut closes = false;
            let mut opens = false;
            match rng.next(11) {
                0 => {
                    counter += 1;
                    let _ =
                        state.split_pane(pane, edges[rng.next(4)], item(&format!("n{counter}")));
                    // adds exactly one item
                    assert_eq!(state.all_items().len(), before.len() + 1);
                    continue;
                }
                1 => {
                    let _ = state.move_item(
                        ItemSource::Pane {
                            pane,
                            index: rng.next(len + 1),
                        },
                        DropTarget::Split {
                            pane: panes[rng.next(panes.len())],
                            edge: edges[rng.next(4)],
                        },
                    );
                }
                2 => {
                    let target = panes[rng.next(panes.len())];
                    let tlen = state.pane(target).map_or(0, |p| p.items.len());
                    let _ = state.move_item(
                        ItemSource::Pane {
                            pane,
                            index: rng.next(len + 1),
                        },
                        DropTarget::Pane {
                            pane: target,
                            index: rng.next(tlen + 2),
                        },
                    );
                }
                3 => {
                    closes = true;
                    let _ = state.close_item(pane, rng.next(len + 1));
                }
                4 => {
                    let names = ["sessions", "inspector", AGENTS_PAGE, "notifications"];
                    let panel = PanelId::new(names[rng.next(4)]);
                    let dest = match rng.next(4) {
                        0 => PanelDest::Center,
                        1 => PanelDest::Dock(DockPosition::Left),
                        2 => PanelDest::Dock(DockPosition::Right),
                        _ => PanelDest::Dock(DockPosition::Bottom),
                    };
                    let _ = state.move_panel(&panel, dest);
                }
                5 => {
                    opens = true;
                    let p = &presets[rng.next(3)];
                    state.apply_preset(p);
                }
                6 => {
                    let _ = state.resize_pane(pane, dirs[rng.next(4)], 0.13);
                }
                7 => {
                    let _ = state.focus_neighbor(dirs[rng.next(4)]);
                }
                8 => state.toggle_zoom(),
                9 => {
                    let _ = state.resize_focused(dirs[rng.next(4)], 40.0, 0.2);
                }
                _ => {
                    state.toggle_dock(DockPosition::ALL[rng.next(3)]);
                }
            }
            check(&state);
            if opens {
                let after = sorted_items(&state);
                for it in &before {
                    // The board only lives while its preset is current.
                    if *it == format!("{:?}", ItemRef::panel(AGENTS_PAGE)) {
                        continue;
                    }
                    assert!(after.contains(it), "seed {seed}: {it} lost");
                }
            } else if !closes {
                assert_eq!(sorted_items(&state), before, "seed {seed}");
            }
        }
        // Whatever happened, the state survives a JSON roundtrip.
        assert_eq!(
            WorkbenchState::from_json(&state.to_json().unwrap()).unwrap(),
            state
        );
    }
}

// ---- the Orca-style layout (OL-2) ---------------------------------------------------------

#[test]
fn builtin_presets_place_the_pages_and_the_strip_and_never_the_assistant() {
    let presets = default_presets();
    let by = |name: &str| presets.iter().find(|p| p.name == name).unwrap().clone();
    let strip = ids(&["history", "changes", "inspector", "agents", "resources"]);

    let default = by("default");
    assert_eq!(default.docks.left.panels, ids(&["sessions", "tasks"]));
    assert_eq!(default.docks.right.panels, strip);
    assert!(default.docks.left.visible && default.docks.right.visible);
    assert!(default.docks.bottom.panels.is_empty() && !default.docks.bottom.visible);

    let focus = by("focus");
    assert_eq!(focus.docks.left.panels, ids(&["sessions", "tasks"]));
    assert_eq!(focus.docks.right.panels, strip);
    assert!(!focus.docks.left.visible && !focus.docks.right.visible && focus.zoom_active);

    // The overview shows the board in the center; the left nav has no second copy of it.
    let overview = by("overview");
    assert_eq!(overview.docks.left.panels, ids(&["sessions", "tasks"]));
    assert_eq!(overview.docks.right.panels, strip);
    assert_eq!(
        overview.center,
        CenterLayout::Items(vec![ItemRef::panel(AGENTS_PAGE)])
    );

    for preset in &presets {
        for pos in DockPosition::ALL {
            assert!(
                preset
                    .docks
                    .get(pos)
                    .panels
                    .iter()
                    .all(|p| p.as_str() != "assistant"),
                "{} places Mawas Home in {pos:?}",
                preset.name
            );
        }
        assert_eq!(
            (preset.docks.left.size, preset.docks.right.size),
            (280.0, 350.0)
        );
    }
}

#[test]
fn mawas_home_opens_as_a_center_tab_once_and_reopens_after_it_is_closed() {
    let home = PanelId::new("assistant");
    let mut state = WorkbenchState::default();
    let pane = state.first_pane_id();
    let before = state.panes().iter().map(|p| p.items.len()).sum::<usize>();
    state.open_panel_in_center(&home);
    check(&state);
    let item = ItemRef::Panel {
        panel: home.clone(),
    };
    let (found, index) = state.find_item(&item).expect("Home is a tab");
    assert_eq!(found, pane);
    assert_eq!(
        state.pane(pane).unwrap().active,
        index,
        "and the active one"
    );
    assert_eq!(state.focus, FocusTarget::Pane(pane));
    assert!(state.docks.find(&home).is_none(), "not in a dock");
    assert_eq!(
        state.panes().iter().map(|p| p.items.len()).sum::<usize>(),
        before + 1
    );

    // Opening it again focuses the one there is, wherever it is.
    state
        .add_item(pane, ItemRef::placeholder("Elsewhere"))
        .unwrap();
    state.open_panel_in_center(&home);
    check(&state);
    assert_eq!(
        state.panes().iter().map(|p| p.items.len()).sum::<usize>(),
        before + 2,
        "one Home"
    );
    assert_eq!(
        state.active_pane().unwrap().active_item(),
        Some(&item),
        "focused again"
    );

    // Closing it removes the tab; opening it brings one back.
    state.close_panel(&home).unwrap();
    assert!(state.find_panel(&home).is_none());
    state.open_panel_in_center(&home);
    check(&state);
    assert!(state.find_item(&item).is_some());

    // It is an ordinary panel otherwise: a dock takes it and moves it back.
    state
        .move_panel(&home, PanelDest::Dock(DockPosition::Right))
        .unwrap();
    assert_eq!(state.docks.find(&home), Some(DockPosition::Right));
    state.move_panel(&home, PanelDest::Center).unwrap();
    assert!(state.find_item(&item).is_some());
    check(&state);
}

#[test]
fn a_preset_switch_keeps_the_layout_valid_with_home_open() {
    let home = PanelId::new("assistant");
    let presets = default_presets();
    for name in ["focus", "overview", "default"] {
        let mut state = WorkbenchState::default();
        state.open_panel_in_center(&home);
        state.switch_preset(name, &presets).unwrap();
        check(&state);
    }
}

/// A layout as the version before the Orca-style relayout saved it.
fn version_1() -> WorkbenchState {
    let mut state = WorkbenchState::with_center(split(
        Axis::Horizontal,
        &[0.5, 0.5],
        vec![leaf(1, &["a1", "a2"]), leaf(2, &["b1"])],
    ));
    state.docks = Docks {
        left: Dock {
            panels: ids(&["sessions", "history"]),
            active: Some(PanelId::new("history")),
            size: 240.0,
            visible: true,
            collapsed: false,
        },
        right: Dock {
            panels: ids(&["inspector", "agents", "tasks", "assistant"]),
            active: Some(PanelId::new("agents")),
            size: 300.0,
            visible: true,
            collapsed: false,
        },
        bottom: Dock::new(&["resources"], 200.0, false),
    };
    state
}

fn as_version_1_json(state: &WorkbenchState) -> String {
    let mut value: serde_json::Value = serde_json::from_str(&state.to_json().unwrap()).unwrap();
    value["version"] = 1.into();
    serde_json::to_string(&value).unwrap()
}

#[test]
fn the_state_version_is_six() {
    let value: serde_json::Value =
        serde_json::from_str(&WorkbenchState::default().to_json().unwrap()).unwrap();
    assert_eq!(value["version"], 6);
    assert_eq!(STATE_VERSION, 6);
}

fn as_version_3_json(state: &WorkbenchState) -> String {
    let mut value: serde_json::Value = serde_json::from_str(&state.to_json().unwrap()).unwrap();
    value["version"] = 3.into();
    serde_json::to_string(&value).unwrap()
}

#[test]
fn a_version_3_layout_loses_the_assistant_page_once_and_keeps_a_center_tab() {
    // Version 3 listed the Assistant as a page of the left sidebar.
    let home = PanelId::new("assistant");
    let mut old = WorkbenchState::default();
    old.docks.left.panels.push(home.clone());
    old.docks.left.active = Some(home.clone());
    old.resize_dock(DockPosition::Left, 311.0);
    let migrated = WorkbenchState::from_json(&as_version_3_json(&old)).unwrap();
    check(&migrated);
    assert!(migrated.find_panel(&home).is_none(), "the page is gone");
    assert_eq!(
        migrated.docks.left.active,
        migrated.docks.left.panels.first().cloned()
    );
    assert_eq!(migrated.docks.left.size, 311.0);
    assert_eq!(migrated.center, old.center);

    // The stash and a parked workspace are migrated too.
    let mut with_stash = old.clone();
    with_stash.adopt_workspace("a");
    with_stash.switch_workspace("b");
    with_stash.docks.left.panels.push(home.clone());
    with_stash
        .switch_preset("overview", &default_presets())
        .unwrap();
    let migrated = WorkbenchState::from_json(&as_version_3_json(&with_stash)).unwrap();
    check(&migrated);
    assert!(
        !migrated
            .stashed
            .as_ref()
            .unwrap()
            .docks
            .left
            .panels
            .contains(&home)
    );
    assert!(
        !migrated.parked["a"]
            .layout
            .docks
            .left
            .panels
            .contains(&home)
    );

    // A version 4 file is read as it is: Home as a center tab stays, and so does a dock the
    // user put it in.
    let mut kept = WorkbenchState::default();
    kept.open_panel_in_center(&home);
    let again = WorkbenchState::from_json(&kept.to_json().unwrap()).unwrap();
    assert_eq!(again, kept);
    let mut docked = WorkbenchState::default();
    docked.open_panel(&home, DockPosition::Right);
    let again = WorkbenchState::from_json(&docked.to_json().unwrap()).unwrap();
    assert_eq!(again, docked);
}

fn as_version_2_json(state: &WorkbenchState) -> String {
    let mut value: serde_json::Value = serde_json::from_str(&state.to_json().unwrap()).unwrap();
    value["version"] = 2.into();
    serde_json::to_string(&value).unwrap()
}

#[test]
fn a_version_2_layout_loses_the_agents_page_once_and_keeps_everything_else() {
    // Version 2 listed the overview board as the Agents page of the left sidebar.
    let mut old = WorkbenchState::default();
    old.docks.left.panels = ids(&["sessions", "tasks", "overview"]);
    old.docks.left.active = Some(PanelId::new("overview"));
    old.resize_dock(DockPosition::Left, 311.0);
    let migrated = WorkbenchState::from_json(&as_version_2_json(&old)).unwrap();
    check(&migrated);
    assert_eq!(migrated.docks.left.panels, ids(&["sessions", "tasks"]));
    assert_eq!(migrated.docks.left.active, Some(PanelId::new("sessions")));
    assert_eq!(migrated.docks.left.size, 311.0);
    assert_eq!(migrated.docks.right, old.docks.right);
    assert_eq!(migrated.center, old.center);

    // The overview stash and a parked workspace are migrated too.
    let mut with_stash = old.clone();
    with_stash.adopt_workspace("a");
    with_stash.switch_workspace("b");
    with_stash.docks.left.panels = ids(&["sessions", "tasks", "overview"]);
    with_stash
        .switch_preset("overview", &default_presets())
        .unwrap();
    let migrated = WorkbenchState::from_json(&as_version_2_json(&with_stash)).unwrap();
    check(&migrated);
    let stash = migrated.stashed.as_ref().unwrap();
    assert!(!stash.docks.left.panels.contains(&PanelId::new("overview")));
    assert!(
        !migrated.parked["a"]
            .layout
            .docks
            .left
            .panels
            .contains(&PanelId::new("overview"))
    );

    // A current file is read as it is: the Agents page a user put in a dock stays.
    let mut kept = migrated.clone();
    kept.open_panel(&PanelId::new(AGENTS_PAGE), DockPosition::Right);
    let again = WorkbenchState::from_json(&kept.to_json().unwrap()).unwrap();
    assert_eq!(again, kept);
}

fn as_version_5_json(state: &WorkbenchState) -> String {
    let mut value: serde_json::Value = serde_json::from_str(&state.to_json().unwrap()).unwrap();
    value["version"] = 5.into();
    // Version 5 called the page `overview`.
    serde_json::to_string(&value)
        .unwrap()
        .replace(&format!("\"{AGENTS_PAGE}\""), "\"overview\"")
}

#[test]
fn a_version_5_layout_calls_the_overview_panel_the_agents_page_everywhere() {
    // Version 5 had the board of terminal previews as the panel `overview`: a center tab, the
    // Agents overview preset with the stash it keeps, and a parked workspace.
    let presets = default_presets();
    let mut live = grid();
    live.open_panel_in_center(&PanelId::new(AGENTS_PAGE));
    live.adopt_workspace("a");
    live.switch_workspace("b");
    live.toggle_preset("overview", &presets).unwrap();
    check(&live);
    let json = as_version_5_json(&live);
    assert!(
        json.contains("\"overview\""),
        "the old file names the board"
    );

    let migrated = WorkbenchState::from_json(&json).unwrap();
    check(&migrated);
    assert_eq!(
        migrated, live,
        "every place that named the board names the page now"
    );
    assert!(
        migrated
            .all_items()
            .iter()
            .all(|i| i.as_panel().is_none_or(|p| p.as_str() != "overview"))
    );

    // A file of the current version is read as it is.
    let again = WorkbenchState::from_json(&migrated.to_json().unwrap()).unwrap();
    assert_eq!(again, migrated);
}

#[test]
fn a_version_1_layout_moves_to_the_orca_arrangement_and_keeps_the_center() {
    let old = version_1();
    let migrated = WorkbenchState::from_json(&as_version_1_json(&old)).unwrap();
    check(&migrated);

    assert_eq!(migrated.docks.left.panels, ids(&["sessions", "tasks"]));
    assert_eq!(migrated.docks.left.active, Some(PanelId::new("sessions")));
    assert_eq!(
        migrated.docks.right.panels,
        ids(&["history", "changes", "inspector", "agents", "resources"])
    );
    assert_eq!(migrated.docks.right.active, Some(PanelId::new("history")));
    assert!(migrated.docks.bottom.panels.is_empty());
    assert!(migrated.find_panel(&PanelId::new("assistant")).is_none());
    assert_eq!(
        (migrated.docks.left.size, migrated.docks.right.size),
        (280.0, 350.0)
    );
    assert_eq!(
        (
            migrated.docks.left.visible,
            migrated.docks.right.visible,
            migrated.docks.bottom.visible
        ),
        (true, true, false)
    );
    // The center, focus, zoom and preset are theirs.
    assert_eq!(migrated.center, old.center);
    assert_eq!(migrated.focus, old.focus);
    assert_eq!(migrated.zoomed, old.zoomed);
    assert_eq!(migrated.preset, old.preset);

    // It is a one-time move: the version 2 file the next save writes loads as it is, even after
    // the user rearranged things back.
    let mut rearranged = migrated.clone();
    rearranged
        .move_panel(
            &PanelId::new("history"),
            PanelDest::Dock(DockPosition::Left),
        )
        .unwrap();
    let again = WorkbenchState::from_json(&rearranged.to_json().unwrap()).unwrap();
    assert_eq!(again, rearranged);
    assert_eq!(
        again.docks.find(&PanelId::new("history")),
        Some(DockPosition::Left)
    );
}

#[test]
fn migration_keeps_sizes_the_user_changed_a_visible_bottom_dock_and_center_panels() {
    let mut old = version_1();
    old.docks.left.size = 333.0;
    old.docks.right.size = 410.0;
    old.docks.bottom.visible = true;
    // History and the Assistant as center tabs stay where the user put them (History) or go
    // (the Assistant, which cannot live there any more).
    old.docks.left.panels = ids(&["sessions"]);
    old.docks.right.panels = ids(&["inspector"]);
    let pane = old.first_pane_id();
    let mut json: serde_json::Value = serde_json::from_str(&old.to_json().unwrap()).unwrap();
    json["version"] = 1.into();
    json["center"] = serde_json::json!({"pane": {
        "id": pane.0, "active": 1,
        "items": [
            {"placeholder": {"title": "Notes"}},
            {"panel": {"panel": "history"}},
            {"panel": {"panel": "assistant"}},
        ],
    }});
    let migrated = WorkbenchState::from_json(&json.to_string()).unwrap();
    check(&migrated);
    assert_eq!(
        (migrated.docks.left.size, migrated.docks.right.size),
        (333.0, 410.0)
    );
    assert_eq!(migrated.docks.bottom.panels, ids(&["resources"]));
    assert!(migrated.docks.bottom.visible);
    let items = &migrated.active_pane().unwrap().items;
    assert_eq!(
        *items,
        vec![ItemRef::placeholder("Notes"), ItemRef::panel("history")]
    );
    assert_eq!(migrated.docks.right.panels, ids(&["changes", "inspector"]));
}

#[test]
fn migration_reaches_the_overview_stash_and_parked_workspaces() {
    let mut state = version_1();
    state.adopt_workspace("a");
    state.switch_workspace("b");
    // Workspace `b` is live (a fresh layout with the old docks), `a` is parked; the overview
    // stashes `b`'s layout.
    state.docks = version_1().docks;
    state.switch_preset("overview", &default_presets()).unwrap();
    assert!(state.stashed.is_some() && state.parked.contains_key("a"));

    let migrated = WorkbenchState::from_json(&as_version_1_json(&state)).unwrap();
    check(&migrated);
    let stash = migrated.stashed.as_ref().unwrap();
    let parked = &migrated.parked["a"];
    for (name, docks) in [("stash", &stash.docks), ("parked", &parked.layout.docks)] {
        assert_eq!(
            docks.right.panels.first(),
            Some(&PanelId::new("history")),
            "{name}"
        );
        assert!(
            DockPosition::ALL.into_iter().all(|p| docks
                .get(p)
                .panels
                .iter()
                .all(|x| x.as_str() != "assistant")),
            "{name}"
        );
        assert!(docks.left.panels.contains(&PanelId::new("tasks")), "{name}");
        assert_eq!(
            (docks.left.size, docks.right.size),
            (280.0, 350.0),
            "{name}"
        );
    }
}

// ---- dock modes (DK-1) -------------------------------------------------------------------

fn mode(state: &WorkbenchState, pos: DockPosition) -> DockMode {
    state.docks.get(pos).mode()
}

/// A state whose `pos` is in `from`, with the keyboard in that dock.
fn docks_in(pos: DockPosition, from: DockMode) -> WorkbenchState {
    let mut state = WorkbenchState::default();
    state.set_dock_mode(pos, from);
    state.focus_dock(pos);
    state
}

/// The transition table of the working toggle (⌘B, ⌘L, ⌘⌥J, the status bar buttons), for every
/// dock and every state it can be in.
#[test]
fn toggle_dock_transition_table() {
    use DockMode::{Compact, Expanded, Hidden};
    use DockPosition::{Bottom, Left, Right};
    for (pos, from, to) in [
        (Left, Expanded, Compact),
        (Left, Compact, Expanded),
        (Right, Expanded, Compact),
        (Right, Compact, Expanded),
        (Right, Hidden, Expanded),
        (Bottom, Expanded, Hidden),
        (Bottom, Hidden, Expanded),
    ] {
        let mut state = docks_in(pos, from);
        state.toggle_dock(pos);
        assert_eq!(mode(&state, pos), to, "{pos:?}: {from:?} then toggle");
        check(&state);
    }
}

#[test]
fn a_hidden_left_sidebar_comes_back_in_the_mode_it_was_hidden_from() {
    let mut state = WorkbenchState::default();
    state.set_dock_mode(DockPosition::Left, DockMode::Hidden);
    state.toggle_dock(DockPosition::Left);
    assert_eq!(mode(&state, DockPosition::Left), DockMode::Expanded);
    state.set_dock_mode(DockPosition::Left, DockMode::Compact);
    state.set_dock_mode(DockPosition::Left, DockMode::Hidden);
    assert_eq!(state.docks.left.last_shown(), DockMode::Compact);
    state.toggle_dock(DockPosition::Left);
    assert_eq!(mode(&state, DockPosition::Left), DockMode::Compact);
    // The titlebar button: hides a shown sidebar, restores a hidden one the same way.
    state.set_dock_mode(DockPosition::Left, DockMode::Expanded);
    state.toggle_dock_hidden(DockPosition::Left);
    assert_eq!(mode(&state, DockPosition::Left), DockMode::Hidden);
    state.toggle_dock_hidden(DockPosition::Left);
    assert_eq!(mode(&state, DockPosition::Left), DockMode::Expanded);
    state.set_dock_mode(DockPosition::Left, DockMode::Compact);
    state.toggle_dock_hidden(DockPosition::Left);
    state.toggle_dock_hidden(DockPosition::Left);
    assert_eq!(mode(&state, DockPosition::Left), DockMode::Compact);
}

#[test]
fn set_dock_mode_for_every_state_keeps_size_panels_and_the_center() {
    for pos in DockPosition::ALL {
        for to in [DockMode::Expanded, DockMode::Compact, DockMode::Hidden] {
            let mut state = WorkbenchState::default();
            state.resize_dock(pos, 333.0);
            let before = state.clone();
            state.set_dock_mode(pos, to);
            let want = if pos == DockPosition::Bottom && to == DockMode::Compact {
                DockMode::Hidden
            } else {
                to
            };
            assert_eq!(mode(&state, pos), want, "{pos:?} to {to:?}");
            assert_eq!(state.docks.get(pos).size, before.docks.get(pos).size);
            assert_eq!(state.docks.get(pos).panels, before.docks.get(pos).panels);
            assert_eq!(state.center, before.center);
            assert_eq!(state.all_items(), before.all_items());
        }
    }
}

#[test]
fn a_dock_that_collapses_gives_the_keyboard_back_to_the_center() {
    for (pos, to) in [
        (DockPosition::Left, DockMode::Compact),
        (DockPosition::Left, DockMode::Hidden),
        (DockPosition::Right, DockMode::Compact),
        (DockPosition::Right, DockMode::Hidden),
        (DockPosition::Bottom, DockMode::Hidden),
    ] {
        let mut state = docks_in(pos, DockMode::Expanded);
        state.set_dock_mode(pos, to);
        assert!(
            matches!(state.focus, FocusTarget::Pane(_)),
            "{pos:?} {to:?}"
        );
    }
    // A keyboard outside the dock is left alone.
    let mut state = WorkbenchState::default();
    let focus = state.focus;
    state.set_dock_mode(DockPosition::Right, DockMode::Compact);
    assert_eq!(state.focus, focus);
    // And a dock that is expanded again does not take it back by itself.
    state.set_dock_mode(DockPosition::Right, DockMode::Expanded);
    assert_eq!(state.focus, focus);
}

#[test]
fn the_active_icon_of_an_open_right_panel_collapses_it_and_any_icon_of_a_collapsed_one_opens_it() {
    let history = PanelId::new("history");
    let changes = PanelId::new("changes");
    let mut state = WorkbenchState::default();
    state.focus_dock(DockPosition::Right);
    // Open + active icon: collapse to the rail, the panel and the width are remembered.
    state.resize_dock(DockPosition::Right, 400.0);
    assert!(!state.click_rail_icon(DockPosition::Right, &history, DockMode::Expanded));
    assert_eq!(mode(&state, DockPosition::Right), DockMode::Compact);
    assert_eq!(state.docks.right.active, Some(history.clone()));
    assert_eq!(state.docks.right.size, 400.0);
    assert!(matches!(state.focus, FocusTarget::Pane(_)));
    // Collapsed + the remembered (active) icon: opens that panel and focuses the dock.
    assert!(state.click_rail_icon(DockPosition::Right, &history, DockMode::Compact));
    assert_eq!(mode(&state, DockPosition::Right), DockMode::Expanded);
    assert_eq!(state.focus, FocusTarget::Dock(DockPosition::Right));
    // Open + another icon: switches, stays open.
    assert!(state.click_rail_icon(DockPosition::Right, &changes, DockMode::Expanded));
    assert_eq!(state.docks.right.active, Some(changes.clone()));
    assert_eq!(mode(&state, DockPosition::Right), DockMode::Expanded);
    // Collapsed + another icon: opens that one.
    state.set_dock_mode(DockPosition::Right, DockMode::Compact);
    assert!(state.click_rail_icon(DockPosition::Right, &history, DockMode::Compact));
    assert_eq!(state.docks.right.active, Some(history.clone()));
    // Hidden + an icon cannot happen on screen, but still opens.
    state.set_dock_mode(DockPosition::Right, DockMode::Hidden);
    assert!(state.click_rail_icon(DockPosition::Right, &changes, DockMode::Hidden));
    // The window shows an auto-collapsed dock as Compact although the user's mode is Expanded:
    // its active icon opens it instead of collapsing it for real.
    assert!(state.click_rail_icon(DockPosition::Right, &changes, DockMode::Compact));
    assert_eq!(mode(&state, DockPosition::Right), DockMode::Expanded);
    // A panel the dock does not hold changes nothing.
    state.set_dock_mode(DockPosition::Right, DockMode::Compact);
    assert!(!state.click_rail_icon(
        DockPosition::Right,
        &PanelId::new("nope"),
        DockMode::Compact
    ));
    assert_eq!(mode(&state, DockPosition::Right), DockMode::Compact);
}

#[test]
fn collapsing_by_drag_keeps_the_width_for_reopening() {
    let mut state = WorkbenchState::default();
    state.resize_dock(DockPosition::Left, 333.0);
    state.collapse_dock(DockPosition::Left);
    assert_eq!(mode(&state, DockPosition::Left), DockMode::Compact);
    assert_eq!(state.docks.left.size, 333.0);
    state.toggle_dock(DockPosition::Left);
    assert_eq!(state.docks.left.size, 333.0);
    // The bottom dock has no rail to collapse to.
    state.collapse_dock(DockPosition::Bottom);
    assert_eq!(mode(&state, DockPosition::Bottom), DockMode::Hidden);
}

#[test]
fn opening_a_panel_expands_a_collapsed_or_hidden_dock() {
    let mut state = WorkbenchState::default();
    for from in [DockMode::Compact, DockMode::Hidden] {
        state.set_dock_mode(DockPosition::Right, from);
        state.open_panel(&PanelId::new("changes"), DockPosition::Right);
        assert_eq!(
            mode(&state, DockPosition::Right),
            DockMode::Expanded,
            "{from:?}"
        );
        assert_eq!(state.docks.right.active, Some(PanelId::new("changes")));
        assert_eq!(state.focus, FocusTarget::Dock(DockPosition::Right));
    }
}

#[test]
fn a_collapsed_dock_is_not_a_focus_neighbor() {
    let mut state = grid();
    state.set_dock_mode(DockPosition::Left, DockMode::Compact);
    state.focus_pane(PaneId(1)).unwrap();
    assert!(!state.focus_neighbor(Direction::Left));
}

#[test]
fn a_preset_comes_back_with_the_dock_modes_and_widths_the_user_left_in_it() {
    let mut state = grid();
    state.set_dock_mode(DockPosition::Left, DockMode::Compact);
    state.resize_dock(DockPosition::Right, 420.0);
    state.apply_preset(&LayoutPreset::focus_preset());
    assert!(!state.docks.left.visible && !state.docks.right.visible);
    // The default preset's own arrangement is what came back: compact left, 420 right.
    state.apply_preset(&LayoutPreset::default_preset());
    assert_eq!(mode(&state, DockPosition::Left), DockMode::Compact);
    assert_eq!(mode(&state, DockPosition::Right), DockMode::Expanded);
    assert_eq!(state.docks.right.size, 420.0);
    // Re-applying the current preset starts over from its own docks.
    state.apply_preset(&LayoutPreset::default_preset());
    assert_eq!(mode(&state, DockPosition::Left), DockMode::Expanded);
    assert_eq!(state.docks.right.size, LayoutPreset::RIGHT_SIZE);
    check(&state);
}

#[test]
fn dock_modes_survive_a_save_and_a_workspace_switch() {
    let mut state = WorkbenchState::default();
    state.adopt_workspace("a");
    state.set_dock_mode(DockPosition::Left, DockMode::Compact);
    state.set_dock_mode(DockPosition::Right, DockMode::Hidden);
    state.resize_dock(DockPosition::Left, 300.0);
    let back = WorkbenchState::from_json(&state.to_json().unwrap()).unwrap();
    assert_eq!(back, state);
    assert_eq!(mode(&back, DockPosition::Left), DockMode::Compact);
    assert_eq!(back.docks.left.size, 300.0);
    // Another workspace brings its own layout; coming back restores this one's modes.
    let mut state = back;
    state.switch_workspace("b");
    state.set_dock_mode(DockPosition::Left, DockMode::Expanded);
    state.switch_workspace("a");
    assert_eq!(mode(&state, DockPosition::Left), DockMode::Compact);
    assert_eq!(mode(&state, DockPosition::Right), DockMode::Hidden);
}

#[test]
fn a_version_4_layout_keeps_its_panels_widths_and_visible_or_hidden_choice() {
    // Version 4 had only `visible`: true is Expanded, false is Hidden, whatever the size.
    let mut old = WorkbenchState::default();
    old.docks.left.visible = false;
    old.docks.left.size = 333.0;
    old.docks.right.size = 410.0;
    old.docks.right.panels.push(PanelId::new("custom"));
    let mut value: serde_json::Value = serde_json::from_str(&old.to_json().unwrap()).unwrap();
    value["version"] = 4.into();
    for dock in ["left", "right", "bottom"] {
        value["docks"][dock]
            .as_object_mut()
            .unwrap()
            .remove("collapsed");
    }
    value.as_object_mut().unwrap().remove("dock_memory");
    let migrated = WorkbenchState::from_json(&value.to_string()).unwrap();
    assert_eq!(mode(&migrated, DockPosition::Left), DockMode::Hidden);
    assert_eq!(migrated.docks.left.size, 333.0);
    assert_eq!(mode(&migrated, DockPosition::Right), DockMode::Expanded);
    assert_eq!(migrated.docks.right.size, 410.0);
    assert!(migrated.docks.right.contains(&PanelId::new("custom")));
    assert_eq!(migrated.docks.left.last_shown(), DockMode::Expanded);
    check(&migrated);
    // Sizes outside what the sides allow now are clamped, like every loaded layout.
    let mut wide = old.clone();
    wide.docks.right.size = 700.0;
    let mut value: serde_json::Value = serde_json::from_str(&wide.to_json().unwrap()).unwrap();
    value["version"] = 4.into();
    value["docks"]["left"]["size"] = 100.0.into();
    let migrated = WorkbenchState::from_json(&value.to_string()).unwrap();
    assert_eq!(migrated.docks.right.size, 480.0);
    assert_eq!(migrated.docks.left.size, 220.0);
}

#[test]
fn a_file_tab_is_stored_so_that_older_binaries_read_a_placeholder() {
    let file = ItemRef::file("/work/src/lib.rs");
    let json = serde_json::to_value(&file).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"placeholder": {"title": "lib.rs", "file": "/work/src/lib.rs"}})
    );
    // This binary reads it back as the file tab.
    assert_eq!(
        serde_json::from_value::<ItemRef>(json.clone()).unwrap(),
        file
    );
    // A binary from before file tabs knows only `placeholder { title }`: unknown fields are
    // ignored by serde, so it restores a placeholder titled with the file's name.
    #[derive(serde::Deserialize, Debug, PartialEq)]
    #[serde(rename_all = "snake_case")]
    enum OldItem {
        Placeholder { title: String },
    }
    assert_eq!(
        serde_json::from_value::<OldItem>(json).unwrap(),
        OldItem::Placeholder {
            title: "lib.rs".into()
        }
    );
    // A plain placeholder keeps its old shape.
    assert_eq!(
        serde_json::to_value(item("Notes")).unwrap(),
        serde_json::json!({"placeholder": {"title": "Notes"}})
    );
}

#[test]
fn a_tab_kind_of_a_newer_binary_becomes_a_placeholder() {
    let unknown: ItemRef =
        serde_json::from_value(serde_json::json!({"notebook": {"path": "/x.ipynb"}})).unwrap();
    assert!(matches!(unknown, ItemRef::Placeholder { .. }));
}
