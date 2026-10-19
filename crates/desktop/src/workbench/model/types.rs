//! Plain data types of the layout model. No GPUI, no I/O.

use serde::{Deserialize, Serialize};

/// Identifier of a registered panel (`sessions`, `inspector`, …).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PanelId(pub String);

impl PanelId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for PanelId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl std::fmt::Display for PanelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifier of a pane in the center tree. Allocated monotonically per state, never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaneId(pub u64);

impl std::fmt::Display for PaneId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pane-{}", self.0)
    }
}

/// Split orientation. `Horizontal` lays children out left to right; `Vertical` top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// A side of a pane (drop zones, split placement).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    /// Axis of the split created when something is placed at this edge.
    pub fn axis(self) -> Axis {
        match self {
            Edge::Left | Edge::Right => Axis::Horizontal,
            Edge::Top | Edge::Bottom => Axis::Vertical,
        }
    }

    /// Whether the new sibling goes after (right of / below) the existing pane.
    pub fn is_after(self) -> bool {
        matches!(self, Edge::Right | Edge::Bottom)
    }
}

/// Navigation direction (focus, keyboard resize).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub fn axis(self) -> Axis {
        match self {
            Direction::Left | Direction::Right => Axis::Horizontal,
            Direction::Up | Direction::Down => Axis::Vertical,
        }
    }

    pub fn is_positive(self) -> bool {
        matches!(self, Direction::Right | Direction::Down)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockPosition {
    Left,
    Right,
    Bottom,
}

impl DockPosition {
    pub const ALL: [DockPosition; 3] = [
        DockPosition::Left,
        DockPosition::Right,
        DockPosition::Bottom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DockPosition::Left => "Left",
            DockPosition::Right => "Right",
            DockPosition::Bottom => "Bottom",
        }
    }
}

/// A panel stack at a window edge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dock {
    pub panels: Vec<PanelId>,
    pub active: Option<PanelId>,
    /// Width (left/right) or height (bottom) in logical pixels.
    pub size: f32,
    pub visible: bool,
}

impl Dock {
    pub fn new(panels: &[&str], size: f32, visible: bool) -> Self {
        let panels: Vec<PanelId> = panels.iter().map(|p| PanelId::new(*p)).collect();
        Self {
            active: panels.first().cloned(),
            panels,
            size,
            visible,
        }
    }

    pub fn contains(&self, panel: &PanelId) -> bool {
        self.panels.contains(panel)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Docks {
    pub left: Dock,
    pub right: Dock,
    pub bottom: Dock,
}

impl Docks {
    pub fn get(&self, pos: DockPosition) -> &Dock {
        match pos {
            DockPosition::Left => &self.left,
            DockPosition::Right => &self.right,
            DockPosition::Bottom => &self.bottom,
        }
    }

    pub fn get_mut(&mut self, pos: DockPosition) -> &mut Dock {
        match pos {
            DockPosition::Left => &mut self.left,
            DockPosition::Right => &mut self.right,
            DockPosition::Bottom => &mut self.bottom,
        }
    }

    pub fn find(&self, panel: &PanelId) -> Option<DockPosition> {
        DockPosition::ALL
            .into_iter()
            .find(|pos| self.get(*pos).contains(panel))
    }
}

/// Something that can be a tab in a pane.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemRef {
    Terminal {
        session: String,
    },
    Chat {
        session: String,
    },
    Panel {
        panel: PanelId,
    },
    Placeholder {
        title: String,
    },
    /// The Changes view of a checkout (a worktree or a main checkout), by its directory.
    Changes {
        path: String,
    },
}

impl ItemRef {
    pub fn placeholder(title: impl Into<String>) -> Self {
        ItemRef::Placeholder {
            title: title.into(),
        }
    }

    pub fn changes(path: impl Into<String>) -> Self {
        ItemRef::Changes { path: path.into() }
    }

    pub fn panel(id: &str) -> Self {
        ItemRef::Panel {
            panel: PanelId::new(id),
        }
    }

    pub fn as_panel(&self) -> Option<&PanelId> {
        match self {
            ItemRef::Panel { panel } => Some(panel),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pane {
    pub id: PaneId,
    pub items: Vec<ItemRef>,
    pub active: usize,
}

impl Pane {
    pub fn new(id: PaneId, items: Vec<ItemRef>) -> Self {
        Self {
            id,
            items,
            active: 0,
        }
    }

    pub fn active_item(&self) -> Option<&ItemRef> {
        self.items.get(self.active)
    }
}

/// Binary-or-n-ary split tree. Invariants (restored by `normalize`): a split has at least two
/// children, `ratios.len() == children.len()` and sums to 1, a split never directly contains a
/// split of the same axis, and an empty pane only exists as the sole pane of the tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneTree {
    Split {
        axis: Axis,
        ratios: Vec<f32>,
        children: Vec<PaneTree>,
    },
    Pane(Pane),
}

/// What currently has keyboard focus in the workbench chrome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusTarget {
    Pane(PaneId),
    Dock(DockPosition),
}

/// Axis-aligned rectangle in an arbitrary unit (the unit square for tree layout).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const UNIT: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ModelError {
    #[error("no such pane: {0}")]
    NoSuchPane(PaneId),
    #[error("item index {index} out of range (len {len})")]
    IndexOutOfRange { index: usize, len: usize },
    #[error("only panels can be placed in a dock")]
    NotAPanel,
    #[error("panel `{0}` is not open")]
    NoSuchPanel(PanelId),
    #[error("unknown preset `{0}`")]
    UnknownPreset(String),
    #[error("no split at path {0:?}")]
    NoSuchSplit(Vec<usize>),
    #[error("invalid layout state: {0}")]
    Invalid(String),
}
