//! The overlays that leave: what the workbench keeps of an overlay for the exit after it closed
//! (`docs/design.md` § Motion, `ui::motion::Leaving`).
//!
//! An overlay's state is dropped when it closes (`self.palette = None`), so its exit has nothing
//! to draw. A pointer-initiated close calls `leave_*`, which moves the state here instead of
//! dropping it; the overlay's render lends it back to its slot while it leaves and builds it
//! with its own code at the value its `Transition` has reached. The keyboard goes back to its
//! owner at the close, before any of this; a leaving overlay is inert ([`Transition::inert`])
//! and its focus handle is not tracked. A keyboard-initiated close (Escape, a shortcut) leaves
//! nothing: it is instant.

use gpui::Context;

use super::agents::AgentPicker;
use super::launcher::Launcher;
use super::modal::Modal;
use super::palette::PaletteState;
use super::rewind::{CheckpointsList, RewindSheet};
use super::settings_sheet::SettingsSheet;
use super::ui::motion::{Kind, Leaving};
use super::view::Workbench;
use super::workspaces::PickerState;

/// One [`Leaving`] per overlay whose state is dropped at its close.
#[derive(Debug, Default)]
pub(crate) struct OverlayExits {
    pub palette: Leaving<PaletteState>,
    pub picker: Leaving<PickerState>,
    pub agent_picker: Leaving<AgentPicker>,
    pub launcher: Leaving<Launcher>,
    pub modal: Leaving<Modal>,
    pub sheet: Leaving<SettingsSheet>,
    pub rewind: Leaving<RewindSheet>,
    pub checkpoints: Leaving<CheckpointsList>,
    /// The tag of the Settings dropdown that just closed, while its panel fades out.
    pub dropdown: Leaving<String>,
    /// The dropdown that was open at the last frame.
    dropdown_seen: Option<String>,
}

impl Workbench {
    pub(crate) fn leave_palette(&mut self, state: PaletteState, cx: &mut Context<Self>) {
        self.exits.palette.start(state, Kind::Popover, cx);
    }

    pub(crate) fn leave_picker(&mut self, state: PickerState, cx: &mut Context<Self>) {
        self.exits.picker.start(state, Kind::Popover, cx);
    }

    pub(crate) fn leave_agent_picker(&mut self, state: AgentPicker, cx: &mut Context<Self>) {
        self.exits.agent_picker.start(state, Kind::Popover, cx);
    }

    pub(crate) fn leave_launcher(&mut self, state: Launcher, cx: &mut Context<Self>) {
        if !state.inline {
            self.exits.launcher.start(state, Kind::Sheet, cx);
        }
    }

    /// The prompt's text (a secret, perhaps) is not kept for the exit: the field fades empty.
    pub(crate) fn leave_modal(&mut self, mut state: Modal, cx: &mut Context<Self>) {
        state.input = state.input.map(|_| String::new());
        self.exits.modal.start(state, Kind::Sheet, cx);
    }

    pub(crate) fn leave_sheet(&mut self, state: SettingsSheet, cx: &mut Context<Self>) {
        self.exits.sheet.start(state, Kind::Sheet, cx);
    }

    pub(crate) fn leave_rewind(&mut self, state: RewindSheet, cx: &mut Context<Self>) {
        self.exits.rewind.start(state, Kind::Sheet, cx);
    }

    pub(crate) fn leave_checkpoints(&mut self, state: CheckpointsList, cx: &mut Context<Self>) {
        self.exits.checkpoints.start(state, Kind::Sheet, cx);
    }

    /// Notes, once a frame, that a Settings dropdown closed: its panel is drawn again, from the
    /// same builders, for the exit (`dropdown_drawn`).
    pub(crate) fn sync_dropdown_exit(&mut self, cx: &mut Context<Self>) {
        let current = self.sheet.as_ref().and_then(|s| s.dropdown.clone());
        if current == self.exits.dropdown_seen {
            return;
        }
        let closed = self.exits.dropdown_seen.take();
        match (closed, &current) {
            (Some(tag), None) => self.exits.dropdown.start(tag, Kind::Popover, cx),
            _ => self.exits.dropdown.cancel(),
        }
        self.exits.dropdown_seen = current;
    }

    /// Whether the panel of the dropdown `tag` is drawn: it is open, or it is leaving.
    pub(crate) fn dropdown_drawn(&self, tag: &str, cx: &gpui::App) -> bool {
        self.sheet
            .as_ref()
            .is_some_and(|s| s.dropdown.as_deref() == Some(tag))
            || self
                .exits
                .dropdown
                .held(cx.background_executor().now())
                .is_some_and(|held| held == tag)
    }

    /// The overlays that are still on their way out at `now`, by name (for tests).
    pub fn overlays_leaving(&self, now: std::time::Instant) -> Vec<&'static str> {
        let e = &self.exits;
        [
            ("palette", e.palette.leaving(now)),
            ("picker", e.picker.leaving(now)),
            ("agent-picker", e.agent_picker.leaving(now)),
            ("launcher", e.launcher.leaving(now)),
            ("modal", e.modal.leaving(now)),
            ("settings", e.sheet.leaving(now)),
            ("rewind", e.rewind.leaving(now)),
            ("checkpoints", e.checkpoints.leaving(now)),
        ]
        .into_iter()
        .filter_map(|(name, leaving)| leaving.then_some(name))
        .collect()
    }
}
