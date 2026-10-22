//! A reinstall offers the setup again, an update says what is new (FX-REINSTALL-SETUP,
//! `docs/design.md` § First-run setup → Reinstall and update).
//!
//! The decision is [`crate::install::Install::change`]; this is what the workbench does with it:
//!
//! - **Reinstalled**: a quiet card at the top of the centre — *Hutan was reinstalled. Run setup
//!   again? Your projects, sessions and settings stay.* — with **Run setup** (the one setup flow,
//!   every answer pre-filled with today's value) and **Not now**. Either answer is remembered for
//!   this install (`desktop-state.json` → `install`); the card stays until one is given.
//! - **Updated**: no offer; *Updated to 0.2.0 · What's new* in the status bar for the session,
//!   opening the version's section of the bundled `CHANGELOG.md` in a file tab.
//!
//! A development build has no install and nothing here shows.

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString, Styled,
    Window, div, px,
};

use super::files::FileOpen;
use super::render::probe;
use super::ui::{Button, ButtonKind};
use super::view::{Region, Workbench};
use crate::icons::{Icon, glyph};
use crate::install::{Change, Install, InstallRecord, whats_new_line};
use crate::theme::DensityExt as _;

/// The install of this window and what it found out about it.
#[derive(Default)]
pub(crate) struct InstallControl {
    install: Option<Install>,
    /// What `desktop-state.json` keeps; the previous record while this build has no install.
    pub(crate) record: Option<InstallRecord>,
    /// The reinstall card is on screen.
    card: bool,
    /// The version this launch updated to; the notice is on screen until it is opened.
    updated: Option<String>,
}

impl InstallControl {
    /// Compares `install` with the `previous` record. Returns the control and whether the
    /// record to save differs from `previous`.
    pub(crate) fn start(
        install: Option<Install>,
        previous: Option<InstallRecord>,
        setup_done: bool,
    ) -> (Self, bool) {
        let Some(install) = install else {
            return (
                Self {
                    install: None,
                    record: previous,
                    ..Self::default()
                },
                false,
            );
        };
        let change = install.change(previous.as_ref(), setup_done);
        let mut record = install.record();
        let (mut card, mut updated) = (false, None);
        match change {
            Change::None => {}
            Change::Updated { .. } => updated = Some(install.version.clone()),
            Change::Reinstalled => {
                card = true;
                // Settled only when the card is answered.
                if let Some(previous) = &previous {
                    record.install_id.clone_from(&previous.install_id);
                }
            }
        }
        let changed = previous.as_ref() != Some(&record);
        (
            Self {
                install: Some(install),
                record: Some(record),
                card,
                updated,
            },
            changed,
        )
    }

    /// The card was answered (or the setup it offers was started): this install is settled.
    fn settle(&mut self) -> bool {
        let Some(install) = &self.install else {
            return false;
        };
        let was = std::mem::replace(&mut self.card, false);
        if let Some(record) = &mut self.record {
            record.install_id.clone_from(&install.id);
        }
        was
    }

    /// `path` is a file of the running app bundle: it is shown read-only and never offered to a
    /// language server (the changelog of *What's new*).
    pub(crate) fn holds(&self, path: &std::path::Path) -> bool {
        self.install.as_ref().is_some_and(|install| {
            path.starts_with(&install.bundle)
                || std::fs::canonicalize(&install.bundle)
                    .is_ok_and(|bundle| path.starts_with(bundle))
        })
    }
}

impl Workbench {
    /// The reinstall card is on screen.
    pub fn reinstall_card_shown(&self) -> bool {
        self.install.card
    }

    /// The version of the *Updated to … · What's new* notice, while it is shown.
    pub fn whats_new_shown(&self) -> Option<&str> {
        self.install.updated.as_deref()
    }

    /// What the window will keep in `desktop-state.json` about its install.
    pub fn install_record(&self) -> Option<&InstallRecord> {
        self.install.record.as_ref()
    }

    /// **Run setup** of the card: the card is answered for good and the setup opens.
    pub fn reinstall_run_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.install.settle() {
            self.schedule_save(cx);
        }
        self.open_setup(window, cx);
    }

    /// **Not now**: the card is dismissed for this install.
    pub fn reinstall_not_now(&mut self, cx: &mut Context<Self>) {
        if self.install.settle() {
            self.schedule_save(cx);
        }
        cx.notify();
    }

    /// *What's new*: the bundled changelog in a file tab, at the section of this version (else
    /// *Unreleased*, which is what a build of a version that is not released yet has). The notice
    /// goes away.
    pub fn open_whats_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(version) = self.install.updated.take() else {
            return;
        };
        cx.notify();
        let Some(path) = self.install.install.as_ref().map(Install::changelog_path) else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            self.set_notice("The changelog is not part of this install", cx);
            return;
        };
        let line = whats_new_line(&text, &version);
        self.open_file(
            FileOpen {
                path,
                line,
                column: None,
                picked: true,
            },
            window,
            cx,
        );
    }

    /// The card, at the top of the centre while it is open.
    pub(crate) fn render_reinstall_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.install.card {
            return None;
        }
        let theme = self.theme;
        let tokens = self.density.ui();
        let button = |id: &'static str, label: &'static str, kind: ButtonKind| {
            Button::new(id, label, &theme, &tokens)
                .kind(kind)
                .child(probe(&self.regions, Region::Offer(id)))
        };
        Some(
            div()
                .id("reinstall-card")
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .px(px(tokens.spacing * 4.0))
                .py(px(tokens.spacing))
                .bg(theme.surface)
                .border_b_1()
                .border_color(theme.border)
                .text_size(px(tokens.secondary_font_size))
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.muted)
                        .child(glyph(Icon::StateInfo)),
                )
                .child(div().flex_1().min_w_0().text_color(theme.fg).child(
                    "Hutan was reinstalled. Run setup again? Your projects, sessions and \
                     settings stay.",
                ))
                .child(
                    button("reinstall-run-setup", "Run setup", ButtonKind::Secondary).on_click(
                        cx.listener(|this, _, window, cx| this.reinstall_run_setup(window, cx)),
                    ),
                )
                .child(
                    button("reinstall-not-now", "Not now", ButtonKind::Ghost)
                        .on_click(cx.listener(|this, _, _, cx| this.reinstall_not_now(cx))),
                )
                .child(probe(&self.regions, Region::Offer("reinstall")))
                .into_any_element(),
        )
    }

    /// *Updated to 0.2.0 · What's new*, one line in the status bar.
    pub(crate) fn render_whats_new(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let version = self.install.updated.as_deref()?;
        let theme = self.theme;
        let tokens = self.density.ui();
        Some(
            div()
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .text_color(theme.fg)
                .child(SharedString::from(format!("Updated to {version} · ")))
                .child(
                    Button::new("whats-new", "What's new", &theme, &tokens)
                        .link()
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_whats_new(window, cx)),
                        )
                        .child(probe(&self.regions, Region::Offer("whats-new"))),
                )
                .into_any_element(),
        )
    }
}
