//! The status items that follow the caret of the focused file tab, `editor.position`
//! (`Ln 12, Col 5`) and `editor.mode` (Vim's mode and the keys of a command in progress), as
//! views of their own (B12a fix, slice E-8).
//!
//! Each is drawn as a cached view of a fixed width that reads the focused code view when it
//! renders. A cached view is drawn again only when it is notified itself (GPUI replays its last
//! frame otherwise, even when an entity it read changed), so each item observes the code view
//! it read and the workbench (another tab focused): a caret move, a typed character or a Vim
//! key re-renders these two items, and the workbench is not notified for them (FX-VIM-1).
//! (GPUI still renders every ancestor view of a view that changed, the workbench's own `render`
//! included, but the panels, the other status items and the tabs it holds come from cache.)

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AnyView, App, AppContext as _, Context, Entity, EntityId, IntoElement,
    ParentElement, Render, SharedString, StyleRefinement, Styled, Subscription, WeakEntity, Window,
    div, px,
};
use hutan_code_view::{CodeView, VimMode};

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
    /// The code view the item read last, observed: it draws again when that view changes.
    watched: Option<(EntityId, Subscription)>,
    /// What the item drew last (tests).
    shown: Option<String>,
    _workbench: Subscription,
}

/// The two item views of a window, made at its first status bar.
#[derive(Debug, Clone)]
pub struct EditorItems {
    position: Entity<EditorItem>,
    mode: Entity<EditorItem>,
}

impl EditorItems {
    pub fn new(workbench: &Entity<Workbench>, cx: &mut App) -> Self {
        let item = |kind, cx: &mut App| {
            cx.new(|cx| EditorItem {
                workbench: workbench.downgrade(),
                kind,
                watched: None,
                shown: None,
                _workbench: cx.observe(workbench, |_, _, cx| cx.notify()),
            })
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

    /// What `editor.position` showed in the last frame (tests).
    pub fn position_text(&self, cx: &App) -> Option<String> {
        self.position.read(cx).shown.clone()
    }

    /// What `editor.mode` showed in the last frame: the mode's word and the pending keys
    /// (tests).
    pub fn mode_text(&self, cx: &App) -> Option<String> {
        self.mode.read(cx).shown.clone()
    }
}

fn cached(view: &Entity<EditorItem>, width: f32, height: f32) -> AnyElement {
    AnyView::from(view.clone())
        .cached(StyleRefinement::default().w(px(width)).h(px(height)))
        .into_any_element()
}

impl EditorItem {
    /// The focused file tab's code view, once its file is in.
    fn code_view(&self, cx: &App) -> Option<Entity<CodeView>> {
        let workbench = self.workbench.upgrade()?;
        let view = workbench.read(cx).focused_file_view()?;
        let view = view.read(cx);
        view.buffer()
            .read(cx)
            .is_ready()
            .then(|| view.code().clone())
    }

    /// The line and column of `code`'s caret, or its Vim mode and pending keys.
    fn read(&self, code: &CodeView, cx: &App) -> Option<(Option<VimMode>, String)> {
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

    /// Observes `code` (the view this frame reads) instead of the one before.
    fn watch(&mut self, code: Option<&Entity<CodeView>>, cx: &mut Context<Self>) {
        let id = code.map(Entity::entity_id);
        if self.watched.as_ref().map(|(id, _)| *id) == id {
            return;
        }
        self.watched =
            code.map(|code| (code.entity_id(), cx.observe(code, |_, _, cx| cx.notify())));
    }
}

fn text_of(mode: Option<VimMode>, text: &str) -> String {
    match mode {
        Some(mode) if text.is_empty() => mode.label().to_owned(),
        Some(mode) => format!("{} {text}", mode.label()),
        None => text.to_owned(),
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
        let code = self.code_view(cx);
        self.watch(code.as_ref(), cx);
        let read = code.and_then(|code| self.read(code.read(cx), cx));
        self.shown = read.as_ref().map(|(mode, text)| text_of(*mode, text));
        match (self.kind, read) {
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
