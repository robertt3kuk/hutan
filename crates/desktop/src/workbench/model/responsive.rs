//! Responsive auto-collapse of the side docks (`layout.auto_collapse`).
//!
//! The saved [`Docks`] hold what the *user* chose. A narrow window shows less than that without
//! ever rewriting it: [`Responsive`] is the transient part (never serialized) that says which
//! expanded docks the window currently shows down to their rail. Hiding a dock is the user's own
//! decision and is never undone by a resize; only a dock the window collapsed comes back.
//!
//! Rules (`docs/spikes/2026-10-20-chrome-v3.md` §4.3), checked right dock first, then left:
//! - the right body collapses below [`AUTO_COLLAPSE_RIGHT_BELOW`] pt of window width, the left
//!   sidebar goes compact below [`AUTO_COLLAPSE_LEFT_BELOW`] pt;
//! - either also collapses when the center would be narrower than [`MIN_CENTER_WIDTH`];
//! - a collapsed dock comes back only [`AUTO_COLLAPSE_HYSTERESIS`] pt above its threshold *and*
//!   when the center is wide enough with it open;
//! - a dock the user expanded on purpose while the window is narrow stays open
//!   ([`AutoState::Overridden`]) until the window has grown past the same restore width;
//! - a dock the user opens from its rail in a window too narrow for the center to keep
//!   [`MIN_CENTER_WIDTH`] beside its body shows the body as a *peek* over the center instead
//!   ([`Responsive::open_as`], `layout.rail_peek`, EX-2): nothing is pushed, nothing is saved.

use super::{DockMode, DockPosition, Docks};

/// The right body collapses to its rail below this window width.
pub const AUTO_COLLAPSE_RIGHT_BELOW: f32 = 1180.0;
/// The left sidebar goes compact below this window width.
pub const AUTO_COLLAPSE_LEFT_BELOW: f32 = 900.0;
/// The center keeps at least this much width.
pub const MIN_CENTER_WIDTH: f32 = 480.0;
/// A collapsed dock is restored this much above its collapse width.
pub const AUTO_COLLAPSE_HYSTERESIS: f32 = 80.0;
/// A peek is at most this wide in a window narrower than [`AUTO_COLLAPSE_LEFT_BELOW`].
pub const PEEK_NARROW_MAX: f32 = 300.0;
/// The least a peek is wide: the smallest a side dock's body may be.
pub const PEEK_MIN_WIDTH: f32 = 240.0;
/// A peek leaves at least this much of the center uncovered, so there is something outside it
/// to click.
pub const PEEK_CENTER_VISIBLE: f32 = 120.0;

/// How the body of a dock the user opens is shown (`docs/design.md` § Docks, *Rail peek*).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenAs {
    /// Beside the center, which gives up the room (and the dock stays open until the window
    /// has grown, [`AutoState::Overridden`]).
    Push,
    /// Floating over the center's edge at the dock's width, closing again on Esc, a click
    /// outside, opening a file or the same icon.
    Peek,
}

/// What the window did to one side dock whose user mode is expanded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AutoState {
    /// Shown as the user chose.
    #[default]
    Off,
    /// Shown down to its rail because the window is narrow.
    Collapsed,
    /// The user opened it although the window is narrow: it stays open until the window grows.
    Overridden,
}

/// The automatic part of the dock layout of one window. Transient: not saved, not part of any
/// layout preset.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Responsive {
    left: AutoState,
    right: AutoState,
    /// The window width of the last [`Self::evaluate`].
    width: f32,
}

impl Responsive {
    /// What the window currently does to `pos` (the bottom dock is never touched).
    pub fn auto_state(&self, pos: DockPosition) -> AutoState {
        match pos {
            DockPosition::Left => self.left,
            DockPosition::Right => self.right,
            DockPosition::Bottom => AutoState::Off,
        }
    }

    fn state_mut(&mut self, pos: DockPosition) -> Option<&mut AutoState> {
        match pos {
            DockPosition::Left => Some(&mut self.left),
            DockPosition::Right => Some(&mut self.right),
            DockPosition::Bottom => None,
        }
    }

    /// What is on screen of `pos`: the user's mode, except an expanded dock the window
    /// collapsed shows as [`DockMode::Compact`].
    pub fn effective(&self, docks: &Docks, pos: DockPosition) -> DockMode {
        match (docks.get(pos).mode(), self.auto_state(pos)) {
            (DockMode::Expanded, AutoState::Collapsed) => DockMode::Compact,
            (mode, _) => mode,
        }
    }

    /// Width a side dock takes in `mode`: nothing, its rail, or its body (the right body stands
    /// beside its rail). The bottom dock takes none.
    pub fn extent(docks: &Docks, pos: DockPosition, mode: DockMode) -> f32 {
        match (pos, mode) {
            (DockPosition::Bottom, _) | (_, DockMode::Hidden) => 0.0,
            (_, DockMode::Compact) => pos.rail_width(),
            (DockPosition::Left, DockMode::Expanded) => docks.left.size,
            (DockPosition::Right, DockMode::Expanded) => pos.rail_width() + docks.right.size,
        }
    }

    /// The width the center has between the docks as they are shown now.
    pub fn center_width(&self, docks: &Docks, width: f32) -> f32 {
        self.center_with(docks, width, |pos| self.effective(docks, pos))
    }

    fn center_with(
        &self,
        docks: &Docks,
        width: f32,
        shown: impl Fn(DockPosition) -> DockMode,
    ) -> f32 {
        width
            - Self::extent(docks, DockPosition::Left, shown(DockPosition::Left))
            - Self::extent(docks, DockPosition::Right, shown(DockPosition::Right))
    }

    fn threshold(pos: DockPosition) -> f32 {
        match pos {
            DockPosition::Right => AUTO_COLLAPSE_RIGHT_BELOW,
            DockPosition::Left => AUTO_COLLAPSE_LEFT_BELOW,
            DockPosition::Bottom => 0.0,
        }
    }

    /// Would `pos`, expanded and with the other dock as it is shown now, be collapsed at
    /// `width`: the window is below its threshold or the center would be too narrow.
    fn would_collapse(&self, docks: &Docks, width: f32, pos: DockPosition) -> bool {
        let center = self.center_with(docks, width, |p| {
            if p == pos {
                DockMode::Expanded
            } else {
                self.effective(docks, p)
            }
        });
        width < Self::threshold(pos) || center < MIN_CENTER_WIDTH
    }

    /// Re-evaluates both side docks for a window `width` pt wide. With `enabled` off nothing is
    /// ever collapsed. Returns whether what the window shows changed.
    pub fn evaluate(&mut self, docks: &Docks, width: f32, enabled: bool) -> bool {
        let before = (self.left, self.right);
        self.width = width;
        if enabled {
            // The left sidebar going compact frees room the right body may need, and the other
            // way round: two rounds settle it.
            for _ in 0..2 {
                for pos in [DockPosition::Right, DockPosition::Left] {
                    self.step(docks, width, pos);
                }
            }
        } else {
            self.left = AutoState::Off;
            self.right = AutoState::Off;
        }
        before != (self.left, self.right)
    }

    fn step(&mut self, docks: &Docks, width: f32, pos: DockPosition) {
        let expanded = docks.get(pos).mode() == DockMode::Expanded;
        let below = self.would_collapse(docks, width, pos);
        let grown = width >= Self::threshold(pos) + AUTO_COLLAPSE_HYSTERESIS;
        let Some(state) = self.state_mut(pos) else {
            return;
        };
        *state = match (expanded, *state) {
            // Hidden or compact by choice: the window has nothing to add or to undo.
            (false, _) => AutoState::Off,
            (true, AutoState::Off) if below => AutoState::Collapsed,
            (true, AutoState::Collapsed) if grown && !below => AutoState::Off,
            (true, AutoState::Overridden) if grown => AutoState::Off,
            (true, other) => other,
        };
    }

    /// How the body of `pos` is shown when the user opens it from its rail (an icon, ⌘L, a
    /// command that reveals a panel in it): [`OpenAs::Peek`] when `rail_peek` is on and the
    /// center would be narrower than [`MIN_CENTER_WIDTH`] beside that body (the other dock as
    /// it is shown now), else [`OpenAs::Push`] — in a window narrower than the dock's own
    /// threshold but with room, the dock opens beside the center and stays open
    /// ([`AutoState::Overridden`]). The bottom dock never peeks. Before the first
    /// [`Self::evaluate`] there is no window width, so nothing peeks.
    pub fn open_as(&self, docks: &Docks, pos: DockPosition, rail_peek: bool) -> OpenAs {
        if !rail_peek || pos == DockPosition::Bottom || self.width <= 0.0 {
            return OpenAs::Push;
        }
        let center = self.center_with(docks, self.width, |p| {
            if p == pos {
                DockMode::Expanded
            } else {
                self.effective(docks, p)
            }
        });
        if center < MIN_CENTER_WIDTH {
            OpenAs::Peek
        } else {
            OpenAs::Push
        }
    }

    /// The width of the peek of `pos`: the dock's own width, at most [`PEEK_NARROW_MAX`] in a
    /// window narrower than [`AUTO_COLLAPSE_LEFT_BELOW`], and never so wide that less than
    /// [`PEEK_CENTER_VISIBLE`] of the center is left (nor narrower than the dock's minimum,
    /// unless the window itself is).
    pub fn peek_width(&self, docks: &Docks, pos: DockPosition) -> f32 {
        let beside: f32 = [DockPosition::Left, DockPosition::Right]
            .into_iter()
            .map(|p| Self::extent(docks, p, self.effective(docks, p)))
            .sum();
        let room = (self.width - beside).max(0.0);
        let mut width = docks.get(pos).size;
        if self.width < AUTO_COLLAPSE_LEFT_BELOW {
            width = width.min(PEEK_NARROW_MAX);
        }
        width
            .min((room - PEEK_CENTER_VISIBLE).max(PEEK_MIN_WIDTH))
            .min(room)
    }

    /// The user opened `pos` (expanded it, revealed a panel in it): a narrow window does not
    /// collapse it again until the window has grown. Call after the model changed. Nothing
    /// happens before the first [`Self::evaluate`].
    pub fn reveal(&mut self, docks: &Docks, pos: DockPosition) {
        if self.width <= 0.0
            || docks.get(pos).mode() != DockMode::Expanded
            || !self.would_collapse(docks, self.width, pos)
        {
            return;
        }
        if let Some(state) = self.state_mut(pos) {
            *state = AutoState::Overridden;
        }
    }

    /// Takes back [`Self::reveal`] for `pos`: the dock the user just brought back is left to the
    /// window, which shows it on its rail while the window is too narrow for its body. For a
    /// dock whose body would only have been a peek.
    pub fn leave_to_window(&mut self, pos: DockPosition) {
        if let Some(state) = self.state_mut(pos)
            && *state == AutoState::Overridden
        {
            *state = AutoState::Off;
        }
    }

    /// The user asked for a dock the window collapsed to open: it stays open until the window has
    /// grown. No effect on a dock the window did not collapse.
    pub fn open(&mut self, pos: DockPosition) {
        if let Some(state) = self.state_mut(pos)
            && *state == AutoState::Collapsed
        {
            *state = AutoState::Overridden;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::model::LayoutPreset;

    fn docks() -> Docks {
        LayoutPreset::default_preset().docks
    }

    /// What the window shows of (left, right) after `widths`, one after the other.
    fn run(r: &mut Responsive, d: &Docks, widths: &[f32]) -> Vec<(DockMode, DockMode)> {
        widths
            .iter()
            .map(|w| {
                r.evaluate(d, *w, true);
                (
                    r.effective(d, DockPosition::Left),
                    r.effective(d, DockPosition::Right),
                )
            })
            .collect()
    }

    use DockMode::{Compact, Expanded, Hidden};

    #[test]
    fn extents_are_the_rail_the_body_or_nothing() {
        let d = docks();
        let e = |pos, mode| Responsive::extent(&d, pos, mode);
        assert_eq!(e(DockPosition::Left, Expanded), 280.0);
        assert_eq!(e(DockPosition::Left, Compact), 44.0);
        assert_eq!(e(DockPosition::Left, Hidden), 0.0);
        // The right body stands beside its 40 pt rail.
        assert_eq!(e(DockPosition::Right, Expanded), 390.0);
        assert_eq!(e(DockPosition::Right, Compact), 40.0);
        assert_eq!(e(DockPosition::Right, Hidden), 0.0);
        assert_eq!(e(DockPosition::Bottom, Expanded), 0.0);
    }

    /// §4.3 at the four widths the design names, from a window that was wide.
    #[test]
    fn the_four_design_widths() {
        let d = docks();
        for (width, want) in [
            (1280.0, (Expanded, Expanded)),
            (1180.0, (Expanded, Expanded)),
            (1179.0, (Expanded, Compact)),
            (900.0, (Expanded, Compact)),
            (899.0, (Compact, Compact)),
            (640.0, (Compact, Compact)),
        ] {
            let mut r = Responsive::default();
            assert_eq!(run(&mut r, &d, &[2000.0, width])[1], want, "{width} wide");
        }
    }

    #[test]
    fn a_collapsed_dock_needs_80_pt_more_than_its_threshold_to_come_back() {
        let d = docks();
        let mut r = Responsive::default();
        let seen = run(
            &mut r,
            &d,
            &[1000.0, 1179.0, 1180.0, 1259.0, 1260.0, 1179.0],
        );
        let right: Vec<DockMode> = seen.iter().map(|s| s.1).collect();
        assert_eq!(
            right,
            [Compact, Compact, Compact, Compact, Expanded, Compact]
        );
        // The left sidebar: 900 + 80.
        let mut r = Responsive::default();
        let seen = run(&mut r, &d, &[800.0, 899.0, 900.0, 979.0, 980.0, 899.0]);
        let left: Vec<DockMode> = seen.iter().map(|s| s.0).collect();
        assert_eq!(
            left,
            [Compact, Compact, Compact, Compact, Expanded, Compact]
        );
    }

    #[test]
    fn the_center_keeps_480_pt() {
        let mut d = docks();
        d.left.size = 500.0;
        let mut r = Responsive::default();
        // 1300 pt is wide enough for both thresholds but not for 500 + 390 + 480.
        let seen = run(&mut r, &d, &[1300.0]);
        assert_eq!(seen[0], (Expanded, Compact));
        assert!(r.center_width(&d, 1300.0) >= MIN_CENTER_WIDTH);
        // Wider windows bring the right body back once there is room and the 80 pt margin.
        let seen = run(&mut r, &d, &[1500.0]);
        assert_eq!(seen[0], (Expanded, Expanded));
    }

    #[test]
    fn automatic_collapse_never_touches_what_the_user_chose() {
        let d = docks();
        let mut r = Responsive::default();
        run(&mut r, &d, &[500.0]);
        assert_eq!(r.auto_state(DockPosition::Right), AutoState::Collapsed);
        // The saved docks are the same value they were: the collapse lives in `Responsive`.
        assert_eq!(d, docks());
        assert_eq!(d.right.mode(), Expanded);
    }

    #[test]
    fn an_explicit_hide_or_collapse_is_never_auto_restored() {
        let mut d = docks();
        d.right.set_mode(Hidden);
        d.left.set_mode(Compact);
        let mut r = Responsive::default();
        for seen in run(&mut r, &d, &[500.0, 1000.0, 2000.0, 3000.0, 700.0]) {
            assert_eq!(seen, (Compact, Hidden));
        }
        assert_eq!(r.auto_state(DockPosition::Left), AutoState::Off);
        assert_eq!(r.auto_state(DockPosition::Right), AutoState::Off);
        // A dock the window collapsed, then the user hid: wide again, it stays hidden.
        let mut d = docks();
        let mut r = Responsive::default();
        run(&mut r, &d, &[500.0]);
        d.right.set_mode(Hidden);
        d.left.set_mode(Hidden);
        assert_eq!(run(&mut r, &d, &[500.0, 2000.0]), [(Hidden, Hidden); 2]);
    }

    #[test]
    fn a_dock_the_user_opens_in_a_narrow_window_stays_open_until_it_grows() {
        let mut d = docks();
        d.right.set_mode(Compact);
        let mut r = Responsive::default();
        assert_eq!(run(&mut r, &d, &[1000.0])[0].1, Compact);
        // The user opens it.
        d.right.set_mode(Expanded);
        r.reveal(&d, DockPosition::Right);
        assert_eq!(run(&mut r, &d, &[1000.0, 1100.0, 1259.0])[2].1, Expanded);
        assert_eq!(r.auto_state(DockPosition::Right), AutoState::Overridden);
        // Grown past the threshold and its margin: automatic again, so shrinking collapses it.
        assert_eq!(run(&mut r, &d, &[1260.0])[0].1, Expanded);
        assert_eq!(r.auto_state(DockPosition::Right), AutoState::Off);
        assert_eq!(run(&mut r, &d, &[1000.0])[0].1, Compact);
        // Opening one the window collapsed keeps it open the same way.
        r.open(DockPosition::Right);
        assert_eq!(run(&mut r, &d, &[1000.0])[0].1, Expanded);
        // Before the first evaluation there is nothing to override.
        let mut fresh = Responsive::default();
        fresh.reveal(&d, DockPosition::Right);
        assert_eq!(fresh.auto_state(DockPosition::Right), AutoState::Off);
    }

    /// §3.7 of the explorer spike, row by row: what the right side shows at rest and what a
    /// rail click opens as. Left 280 (compact 44 below 900), right body 350 + rail 40.
    #[test]
    fn the_window_width_decides_push_peek_or_rail() {
        let d = docks();
        // (window, right at rest, right opened from the rail, center beside the rails)
        for (width, rest, open, center) in [
            (1440.0, Expanded, OpenAs::Push, 770.0),
            (1280.0, Expanded, OpenAs::Push, 610.0),
            (1180.0, Expanded, OpenAs::Push, 510.0),
            (1100.0, Compact, OpenAs::Peek, 780.0),
            (900.0, Compact, OpenAs::Peek, 580.0),
            (640.0, Compact, OpenAs::Peek, 556.0),
        ] {
            let mut r = Responsive::default();
            r.evaluate(&d, 2000.0, true);
            r.evaluate(&d, width, true);
            assert_eq!(
                r.effective(&d, DockPosition::Right),
                rest,
                "{width} at rest"
            );
            assert_eq!(
                r.open_as(&d, DockPosition::Right, true),
                open,
                "{width} opened"
            );
            assert_eq!(r.center_width(&d, width), center, "{width} center");
            // With the setting off it is always pushed (the behaviour before EX-2).
            assert_eq!(
                r.open_as(&d, DockPosition::Right, false),
                OpenAs::Push,
                "{width} setting off"
            );
        }
    }

    #[test]
    fn a_narrow_window_with_room_for_the_center_pushes() {
        // The sidebar hidden: 1100 − 390 = 710 leaves the center its room, so the body is
        // pushed (and, below 1180, stays open as an override) rather than peeking.
        let mut d = docks();
        d.left.set_mode(Hidden);
        let mut r = Responsive::default();
        r.evaluate(&d, 1100.0, true);
        assert_eq!(r.effective(&d, DockPosition::Right), Compact);
        assert_eq!(r.open_as(&d, DockPosition::Right, true), OpenAs::Push);
        // The left sidebar compact by choice: 44 + 390 + 480 = 914 is the edge.
        d.left.set_mode(Compact);
        r.evaluate(&d, 700.0, true);
        assert_eq!(r.open_as(&d, DockPosition::Right, true), OpenAs::Peek);
        r.evaluate(&d, 914.0, true);
        assert_eq!(r.open_as(&d, DockPosition::Right, true), OpenAs::Push);
        r.evaluate(&d, 913.0, true);
        assert_eq!(r.open_as(&d, DockPosition::Right, true), OpenAs::Peek);
    }

    #[test]
    fn the_left_sidebar_peeks_when_its_body_would_squeeze_the_center() {
        let d = docks();
        let mut r = Responsive::default();
        // 800: the left rail (44) + the right rail (40) leave 716; 280 more leaves 480: push.
        r.evaluate(&d, 800.0, true);
        assert_eq!(r.open_as(&d, DockPosition::Left, true), OpenAs::Push);
        r.evaluate(&d, 799.0, true);
        assert_eq!(r.open_as(&d, DockPosition::Left, true), OpenAs::Peek);
        // The bottom dock never peeks, and before any window width nothing does.
        assert_eq!(r.open_as(&d, DockPosition::Bottom, true), OpenAs::Push);
        assert_eq!(
            Responsive::default().open_as(&d, DockPosition::Right, true),
            OpenAs::Push
        );
    }

    #[test]
    fn a_peek_is_the_docks_width_capped_by_the_window() {
        let d = docks();
        let mut r = Responsive::default();
        r.evaluate(&d, 1100.0, true);
        assert_eq!(r.peek_width(&d, DockPosition::Right), 350.0);
        // Below 900 at most 300.
        r.evaluate(&d, 640.0, true);
        assert_eq!(r.peek_width(&d, DockPosition::Right), 300.0);
        // Never so wide that under 120 of the center is left (nor under the dock's minimum).
        r.evaluate(&d, 420.0, true);
        let width = r.peek_width(&d, DockPosition::Right);
        assert!(
            r.center_width(&d, 420.0) - width >= PEEK_CENTER_VISIBLE || width <= PEEK_MIN_WIDTH,
            "{width}"
        );
        // The left peek is the sidebar's own width.
        r.evaluate(&d, 1100.0, true);
        assert_eq!(r.peek_width(&d, DockPosition::Left), 280.0);
    }

    #[test]
    fn with_the_setting_off_nothing_is_collapsed_and_what_was_comes_back() {
        let d = docks();
        let mut r = Responsive::default();
        assert!(r.evaluate(&d, 500.0, true));
        assert!(r.evaluate(&d, 500.0, false));
        assert_eq!(r.effective(&d, DockPosition::Right), Expanded);
        assert_eq!(r.effective(&d, DockPosition::Left), Expanded);
        // No change reported when nothing changes.
        assert!(!r.evaluate(&d, 400.0, false));
    }
}
