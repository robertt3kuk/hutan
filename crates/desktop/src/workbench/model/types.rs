//! Plain data types of the layout model. No GPUI, no I/O.

use serde::{Deserialize, Serialize};

pub use super::term_split::TermSplit;

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

    /// The smallest and largest size a dock of this side is resized to: the left sidebar's
    /// 220–500 pt, the right panel body's 240–480 pt, the bottom dock's height. Dragging below
    /// the minimum never persists an illegal size (it collapses the dock instead, see
    /// `WorkbenchState::collapse_dock`).
    pub const fn size_range(self) -> (f32, f32) {
        match self {
            DockPosition::Left => (220.0, 500.0),
            DockPosition::Right => (240.0, 480.0),
            DockPosition::Bottom => (super::ops::MIN_DOCK_SIZE, super::ops::MAX_DOCK_SIZE),
        }
    }

    /// Width of the dock's rail while it is collapsed to it: the left sidebar's 44 pt icon rail,
    /// the right side's 40 pt activity rail (which also stands next to an open body). The bottom
    /// dock has none.
    pub const fn rail_width(self) -> f32 {
        match self {
            DockPosition::Left => 44.0,
            DockPosition::Right => 40.0,
            DockPosition::Bottom => 0.0,
        }
    }
}

/// How much of a dock is on screen. Left: the sidebar, its 44 pt icon rail, nothing. Right: the
/// activity rail with the panel body beside it, the rail alone, nothing. Bottom: open or hidden
/// (it has no rail).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DockMode {
    /// The body is shown (the left sidebar with its pages, the right panel next to its rail).
    Expanded,
    /// Only the rail is shown. Never the mode of the bottom dock.
    Compact,
    /// Nothing is shown and nothing can take focus; a titlebar button or a command brings it back.
    Hidden,
}

/// A panel stack at a window edge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dock {
    pub panels: Vec<PanelId>,
    pub active: Option<PanelId>,
    /// Width (left/right) or height (bottom) in logical pixels. For a side dock this is the
    /// expanded width and is kept while the dock is collapsed or hidden.
    pub size: f32,
    /// The dock is on screen at all (as its body or, with `collapsed`, as its rail). `false` is
    /// [`DockMode::Hidden`].
    pub visible: bool,
    /// The dock is down to its rail ([`DockMode::Compact`]) while `visible`. Kept while the dock
    /// is hidden: it is the mode the dock comes back in. Never set on the bottom dock.
    #[serde(default)]
    pub collapsed: bool,
    /// Panels of this dock its rail lists in the `⋯` menu instead of as icons (the clean
    /// default's Requests and Resources). Such a panel still shows as an icon while it is the
    /// dock's active panel or carries a badge (a request waiting). Always a subset of `panels`;
    /// absent in layouts saved before CL-CHROME, which keep every icon on the rail.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub more: Vec<PanelId>,
}

impl Dock {
    pub fn new(panels: &[&str], size: f32, visible: bool) -> Self {
        let panels: Vec<PanelId> = panels.iter().map(|p| PanelId::new(*p)).collect();
        Self {
            active: panels.first().cloned(),
            panels,
            size,
            visible,
            collapsed: false,
            more: Vec::new(),
        }
    }

    /// The same dock with `panels` listed in the rail's `⋯` menu (see [`Dock::more`]).
    pub fn with_more(mut self, panels: &[&str]) -> Self {
        self.more = panels.iter().map(|p| PanelId::new(*p)).collect();
        self
    }

    /// Whether the rail draws `panel` as an icon: it is not in the `⋯` menu, or it is the active
    /// panel, or it has a badge to show (`badged`).
    pub fn on_rail(&self, panel: &PanelId, badged: bool) -> bool {
        !self.more.contains(panel) || self.active.as_ref() == Some(panel) || badged
    }

    pub fn contains(&self, panel: &PanelId) -> bool {
        self.panels.contains(panel)
    }

    /// What is on screen of this dock (the user's choice; the window may show less, see
    /// `Responsive`).
    pub fn mode(&self) -> DockMode {
        match (self.visible, self.collapsed) {
            (false, _) => DockMode::Hidden,
            (true, true) => DockMode::Compact,
            (true, false) => DockMode::Expanded,
        }
    }

    /// The body is shown: the left sidebar, the right panel, the bottom dock.
    pub fn is_expanded(&self) -> bool {
        self.mode() == DockMode::Expanded
    }

    /// The mode a hidden dock comes back in: the one it had when it was hidden.
    pub fn last_shown(&self) -> DockMode {
        if self.collapsed {
            DockMode::Compact
        } else {
            DockMode::Expanded
        }
    }

    pub(super) fn set_mode(&mut self, mode: DockMode) {
        match mode {
            DockMode::Expanded => {
                self.visible = true;
                self.collapsed = false;
            }
            DockMode::Compact => {
                self.visible = true;
                self.collapsed = true;
            }
            DockMode::Hidden => self.visible = false,
        }
    }
}

/// The mode and expanded size of one dock, as remembered for a layout preset.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DockModes {
    pub visible: bool,
    #[serde(default)]
    pub collapsed: bool,
    pub size: f32,
}

/// What the user left each dock at in a layout preset (left, right, bottom), restored when the
/// preset is applied again.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RememberedDocks {
    pub left: DockModes,
    pub right: DockModes,
    pub bottom: DockModes,
}

impl Docks {
    /// The modes and sizes of the three docks now.
    pub fn modes(&self) -> RememberedDocks {
        let of = |dock: &Dock| DockModes {
            visible: dock.visible,
            collapsed: dock.collapsed,
            size: dock.size,
        };
        RememberedDocks {
            left: of(&self.left),
            right: of(&self.right),
            bottom: of(&self.bottom),
        }
    }

    /// Sets the modes and sizes of the three docks (their panels stay).
    pub fn set_modes(&mut self, modes: &RememberedDocks) {
        for (dock, saved) in [
            (&mut self.left, modes.left),
            (&mut self.right, modes.right),
            (&mut self.bottom, modes.bottom),
        ] {
            dock.visible = saved.visible;
            dock.collapsed = saved.collapsed;
            dock.size = saved.size;
        }
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
///
/// Written to `desktop-state.json` through [`ItemRepr`]: a file tab is stored as a placeholder
/// that also names the file, so a binary from before file tabs (which knows no `file` kind and
/// would otherwise refuse the whole layout) restores it as a placeholder titled with the file's
/// name; and a kind this binary does not know (a newer binary's) becomes a placeholder instead
/// of costing the user their layout.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "ItemRepr", from = "ItemOnDisk")]
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
    /// A file in the code view, by its canonical absolute path.
    File {
        path: String,
    },
    /// The history of a file (GIT-HISTORY): its commits, and the diff of the one selected.
    History {
        path: String,
    },
    /// A file as it was at a commit (GIT-HISTORY): read-only text, by the file's absolute path
    /// as it was named then and the commit's full hash.
    Revision {
        path: String,
        rev: String,
    },
    /// Two files side by side as one diff (EX-4, *Compare with Selected*): read-only, by the
    /// absolute path of each.
    Compare {
        left: String,
        right: String,
    },
    /// A file against its version in HEAD (EX-4, *Compare with HEAD*): read-only, by the file's
    /// absolute path.
    CompareHead {
        path: String,
    },
    /// The rendered page of a Markdown file (MD-PREVIEW, *Open Preview to the Side*): it
    /// follows the file's buffer and the scroll of the tab it was opened from. By the file's
    /// absolute path. A binary from before it reads the stored tab as a placeholder.
    Preview {
        path: String,
    },
    /// A terminal tab that holds a tree of terminal splits (Ghostty / iTerm style): every leaf is
    /// its own daemon session, the tab has one title (the focused split's) and closes with its
    /// last split. A tab with one terminal is [`ItemRef::Terminal`]; this form exists while it
    /// has two or more.
    TerminalSplit {
        split: Box<TermSplit>,
    },
}

/// How an [`ItemRef`] is stored.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ItemRepr {
    Terminal {
        session: String,
    },
    Chat {
        session: String,
    },
    Panel {
        panel: PanelId,
    },
    /// With `file`, a file tab (see [`ItemRef`]).
    Placeholder {
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
    },
    Changes {
        path: String,
    },
    History {
        path: String,
    },
    Revision {
        path: String,
        rev: String,
    },
    Compare {
        left: String,
        right: String,
    },
    CompareHead {
        path: String,
    },
    Preview {
        path: String,
    },
    /// A tab of terminal splits: the tree (each leaf a pane of one terminal item), the focused
    /// leaf, and whether it is zoomed. A binary from before splits reads this as a placeholder
    /// (the sessions stay in the daemon and in the sessions panel).
    TerminalSplit {
        tree: PaneTree,
        focus: PaneId,
        #[serde(default, skip_serializing_if = "is_false")]
        zoomed: bool,
    },
}

/// A stored item, or one of a kind this binary does not know.
#[derive(Deserialize)]
#[serde(untagged)]
enum ItemOnDisk {
    Known(ItemRepr),
    Unknown(serde::de::IgnoredAny),
}

impl From<ItemRef> for ItemRepr {
    fn from(item: ItemRef) -> Self {
        match item {
            ItemRef::Terminal { session } => ItemRepr::Terminal { session },
            ItemRef::Chat { session } => ItemRepr::Chat { session },
            ItemRef::Panel { panel } => ItemRepr::Panel { panel },
            ItemRef::Placeholder { title } => ItemRepr::Placeholder { title, file: None },
            ItemRef::Changes { path } => ItemRepr::Changes { path },
            ItemRef::History { path } => ItemRepr::History { path },
            ItemRef::Revision { path, rev } => ItemRepr::Revision { path, rev },
            ItemRef::Compare { left, right } => ItemRepr::Compare { left, right },
            ItemRef::CompareHead { path } => ItemRepr::CompareHead { path },
            ItemRef::Preview { path } => ItemRepr::Preview { path },
            ItemRef::File { path } => ItemRepr::Placeholder {
                title: std::path::Path::new(&path)
                    .file_name()
                    .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned()),
                file: Some(path),
            },
            ItemRef::TerminalSplit { split } => {
                let TermSplit {
                    tree,
                    focus,
                    zoomed,
                } = *split;
                ItemRepr::TerminalSplit {
                    tree,
                    focus,
                    zoomed,
                }
            }
        }
    }
}

impl From<ItemOnDisk> for ItemRef {
    fn from(stored: ItemOnDisk) -> Self {
        match stored {
            ItemOnDisk::Known(ItemRepr::Terminal { session }) => ItemRef::Terminal { session },
            ItemOnDisk::Known(ItemRepr::Chat { session }) => ItemRef::Chat { session },
            ItemOnDisk::Known(ItemRepr::Panel { panel }) => ItemRef::Panel { panel },
            ItemOnDisk::Known(ItemRepr::Placeholder {
                file: Some(path), ..
            }) => ItemRef::File { path },
            ItemOnDisk::Known(ItemRepr::Placeholder { title, file: None }) => {
                ItemRef::Placeholder { title }
            }
            ItemOnDisk::Known(ItemRepr::Changes { path }) => ItemRef::Changes { path },
            ItemOnDisk::Known(ItemRepr::History { path }) => ItemRef::History { path },
            ItemOnDisk::Known(ItemRepr::Revision { path, rev }) => ItemRef::Revision { path, rev },
            ItemOnDisk::Known(ItemRepr::Compare { left, right }) => {
                ItemRef::Compare { left, right }
            }
            ItemOnDisk::Known(ItemRepr::CompareHead { path }) => ItemRef::CompareHead { path },
            ItemOnDisk::Known(ItemRepr::Preview { path }) => ItemRef::Preview { path },
            ItemOnDisk::Known(ItemRepr::TerminalSplit {
                tree,
                focus,
                zoomed,
            }) => ItemRef::TerminalSplit {
                split: Box::new(TermSplit {
                    tree,
                    focus,
                    zoomed,
                }),
            },
            ItemOnDisk::Unknown(_) => ItemRef::placeholder("A tab of a newer Hutan"),
        }
    }
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

    pub fn file(path: impl Into<String>) -> Self {
        ItemRef::File { path: path.into() }
    }

    pub fn history(path: impl Into<String>) -> Self {
        ItemRef::History { path: path.into() }
    }

    pub fn revision(path: impl Into<String>, rev: impl Into<String>) -> Self {
        ItemRef::Revision {
            path: path.into(),
            rev: rev.into(),
        }
    }

    pub fn compare(left: impl Into<String>, right: impl Into<String>) -> Self {
        ItemRef::Compare {
            left: left.into(),
            right: right.into(),
        }
    }

    pub fn compare_head(path: impl Into<String>) -> Self {
        ItemRef::CompareHead { path: path.into() }
    }

    pub fn preview(path: impl Into<String>) -> Self {
        ItemRef::Preview { path: path.into() }
    }

    /// The path of a Markdown preview tab.
    pub fn as_preview(&self) -> Option<&str> {
        match self {
            ItemRef::Preview { path } => Some(path),
            _ => None,
        }
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

    /// The path of a file tab.
    pub fn as_file(&self) -> Option<&str> {
        match self {
            ItemRef::File { path } => Some(path),
            _ => None,
        }
    }

    /// The terminal splits of a tab that has them.
    pub fn as_term_split(&self) -> Option<&TermSplit> {
        match self {
            ItemRef::TerminalSplit { split } => Some(split),
            _ => None,
        }
    }

    /// The session a terminal or chat tab shows; for a tab of splits the focused split's.
    pub fn session(&self) -> Option<&str> {
        match self {
            ItemRef::Terminal { session } | ItemRef::Chat { session } => Some(session),
            ItemRef::TerminalSplit { split } => split.focused_session(),
            _ => None,
        }
    }

    /// Every session the tab shows: one for a terminal or chat, one per split, else none.
    pub fn sessions(&self) -> Vec<&str> {
        match self {
            ItemRef::Terminal { session } | ItemRef::Chat { session } => vec![session],
            ItemRef::TerminalSplit { split } => split.sessions(),
            _ => Vec::new(),
        }
    }

    /// The sessions the tab shows on screen right now: all of them, but a zoomed tab of
    /// splits shows only the focused split.
    pub fn visible_sessions(&self) -> Vec<&str> {
        match self {
            ItemRef::TerminalSplit { split } if split.zoomed => {
                split.focused_session().into_iter().collect()
            }
            _ => self.sessions(),
        }
    }

    /// Whether the tab shows `session` (as its terminal, chat or one of its splits).
    pub fn shows_session(&self, session: &str) -> bool {
        match self {
            ItemRef::Terminal { session: s } | ItemRef::Chat { session: s } => s == session,
            ItemRef::TerminalSplit { split } => split.leaf_of(session).is_some(),
            _ => false,
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
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
