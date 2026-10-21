//! The status items that follow the caret of the focused file tab, `editor.position`
//! (`Ln 12, Col 5`) and `editor.mode` (Vim's mode and the keys of a command in progress), as
//! views of their own (B12a fix, slice E-8).
//!
//! Each is drawn as a cached view of a fixed width that reads the focused code view when it
//! renders, so GPUI redraws it when that code view changes. A caret move, a typed character or
//! a Vim key therefore re-renders these two items; the workbench is not notified for them.
//! (GPUI still renders every ancestor view of a view that changed, the workbench's own `render`
//! included, but the panels, the other status items and the tabs it holds come from cache.)

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AnyView, App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, StyleRefinement, Styled, WeakEntity, Window, div, px,
};
use hutan_code_view::VimMode;

use super::style::UiStyle;
use super::view::Workbench;
use crate::theme::DensityExt as _;

/// Width of `editor.position` (fits `Ln 99999, Col 9999`).
const POSITION_WIDTH: f32 = 124.0;
/// Width of `editor.mode` (the mode pill and a few pending keys).
const MODE_WIDTH: f32 = 112.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Position,
    Mode,
}

/// One of the two items.
#[derive(Debug)]
pub struct EditorItem {
    workbench: WeakEntity<Workbench>,
    kind: Kind,
}

/// The two item views of a window, made at its first status bar.
#[derive(Debug, Clone)]
pub struct EditorItems {
    position: Entity<EditorItem>,
    mode: Entity<EditorItem>,
}

impl EditorItems {
    pub fn new(workbench: WeakEntity<Workbench>, cx: &mut App) -> Self {
        let item = |kind, cx: &mut App| {
            let workbench = workbench.clone();
            cx.new(|_| EditorItem { workbench, kind })
        };
        Self {
            position: item(Kind::Position, cx),
            mode: item(Kind::Mode, cx),
        }
    }

    /// `editor.position`, from cache unless the focused code view changed.
    pub fn position(&self, height: f32) -> AnyElement {
        cached(&self.position, POSITION_WIDTH, height)
    }

    /// `editor.mode`.
    pub fn mode(&self, height: f32) -> AnyElement {
        cached(&self.mode, MODE_WIDTH, height)
    }

    /// What `editor.position` shows now (tests).
    pub fn position_text(&self, cx: &App) -> Option<String> {
        self.position.read(cx).text(cx)
    }

    /// What `editor.mode` shows now: the mode's word and the pending keys (tests).
    pub fn mode_text(&self, cx: &App) -> Option<String> {
        self.mode.read(cx).text(cx)
    }
}

fn cached(view: &Entity<EditorItem>, width: f32, height: f32) -> AnyElement {
    AnyView::from(view.clone())
        .cached(StyleRefinement::default().w(px(width)).h(px(height)))
        .into_any_element()
}

impl EditorItem {
    /// The focused file tab's line and column, or its Vim mode and pending keys.
    fn read(&self, cx: &App) -> Option<(Option<VimMode>, String)> {
        let workbench = self.workbench.upgrade()?;
        let view = workbench.read(cx).focused_file_view()?;
        let view = view.read(cx);
        if !view.buffer().read(cx).is_ready() {
            return None;
        }
        let code = view.code().read(cx);
        match self.kind {
            Kind::Position => {
                let (line, column) = code.position(cx);
                Some((None, format!("Ln {line}, Col {column}")))
            }
            Kind::Mode => {
                let mode = code.vim_mode()?;
                Some((Some(mode), code.vim_pending()))
            }
        }
    }

    fn text(&self, cx: &App) -> Option<String> {
        let (mode, text) = self.read(cx)?;
        Some(match mode {
            Some(mode) if text.is_empty() => mode.label().to_owned(),
            Some(mode) => format!("{} {text}", mode.label()),
            None => text,
        })
    }
}

impl Render for EditorItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let root = div()
            .size_full()
            .flex()
            .items_center()
            .overflow_hidden()
            .text_color(theme.muted);
        match (self.kind, self.read(cx)) {
            (_, None) => root,
            (Kind::Position, Some((_, text))) => root.justify_end().child(SharedString::from(text)),
            (Kind::Mode, Some((mode, pending))) => root
                .gap(px(6.0))
                .when_some(mode, |s, mode| {
                    s.child(super::vim_ui::mode_pill(mode, &theme, &tokens))
                })
                .when(!pending.is_empty(), |s| {
                    s.child(
                        div()
                            .text_color(theme.fg)
                            .child(SharedString::from(pending)),
                    )
                }),
        }
    }
}
