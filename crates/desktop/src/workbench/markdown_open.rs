//! Showing a Markdown file as a page (MD-PREVIEW; the page itself is `markdown_preview.rs`).
//!
//! - A Markdown file tab switches between its text and its page (⇧⌘V, the *Source | Preview*
//!   switch above the file): the tab's [`FileView`] holds a [`MarkdownPreview`] while it shows
//!   the page. `editor.markdown_open = "preview"` makes a file tab start that way.
//! - *Markdown: Open Preview to the Side* adds a tab of its own (`ItemRef::Preview`) in a split
//!   to the right; it follows the file's edits (they are one buffer) and the scroll of the tab it
//!   was opened from. With `editor.markdown_preview = "side"` ⇧⌘V does that too.
//! - A click on a link of a page opens the web in the browser, `#heading` scrolls the page, and a
//!   file in a tab (a Markdown file opens as a page again).

use std::path::Path;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement as _, Styled, Window, div, px,
};
use hutan_syntax::Language;

use super::file_view::FileView;
use super::files::FileOpen;
use super::markdown_preview::{LinkTarget, MarkdownPreview, MarkdownPreviewEvent, classify_link};
use super::model::{Edge, ItemRef, PaneId};
use super::ui::{self, Button};
use super::view::Workbench;
use crate::settings::{EditorSettings, MarkdownOpen, MarkdownPreviewMode};
use crate::theme::{DensityExt as _, Theme, Tokens};

/// Whether `path` is a Markdown file (by its name).
pub fn is_markdown(path: &str) -> bool {
    Language::from_path(Path::new(path)) == Some(Language::Markdown)
}

/// What a button of the bar does.
pub(crate) type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// What the bar above a Markdown file or its page shows.
pub(crate) struct Bar {
    pub mode: MarkdownPreviewMode,
    /// The page is what the tab shows now.
    pub page: bool,
    /// A tab of the page alone: its words and no button to open another one.
    pub caption: Option<SharedString>,
    /// The *Source | Preview* switch.
    pub on_toggle: Handler,
    /// *Preview to the Side*.
    pub on_side: Option<Handler>,
}

impl Bar {
    pub fn element(self, theme: &Theme, tokens: &Tokens) -> AnyElement {
        let Self {
            mode,
            page,
            caption,
            on_toggle,
            on_side,
        } = self;
        let switch = (mode == MarkdownPreviewMode::Toggle || caption.is_some()).then(|| {
            let segment = |id: &'static str, label: &'static str, selected: bool| {
                let on_toggle = on_toggle.clone();
                ui::segment(id, label, selected, theme, tokens).when(!selected, move |s| {
                    s.on_click(move |event, window, cx| on_toggle(event, window, cx))
                })
            };
            ui::segmented(theme, tokens)
                .child(segment("md-source", "Source", !page))
                .child(segment("md-page", "Preview", page))
        });
        let side = on_side.map(|on_side| {
            Button::new(
                "md-side",
                if mode == MarkdownPreviewMode::Side {
                    "Open Preview to the Side"
                } else {
                    "To the Side"
                },
                theme,
                tokens,
            )
            .ghost()
            .compact()
            .tooltip("Markdown: Open Preview to the Side")
            .on_click(move |event, window, cx| on_side(event, window, cx))
        });
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap(px(tokens.spacing * 2.0))
            .px(px(tokens.spacing * 2.0))
            .h(px(tokens.control_height + 6.0))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.muted)
            .child(div().min_w_0().truncate().children(caption))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .children(switch)
                    .children(side),
            )
            .into_any_element()
    }
}

impl Workbench {
    /// `editor.markdown_preview`.
    pub(crate) fn markdown_mode(&self) -> MarkdownPreviewMode {
        self.settings.get::<EditorSettings>().markdown_preview
    }

    /// A page of the buffer of `path`, made when the file has none yet.
    pub(crate) fn new_markdown_preview(
        &mut self,
        path: &str,
        cx: &mut Context<Self>,
    ) -> Entity<MarkdownPreview> {
        let buffer = self.ensure_buffer(path, true, None, cx);
        let style = self.code_style(cx);
        // Images are files of this computer; a daemon on another one has other files.
        let local = !matches!(
            self.sessions.read(cx).connection().endpoint(),
            Some(hutan_client::Endpoint::Ssh(_))
        );
        cx.new(|cx| MarkdownPreview::new(buffer, style, local, cx))
    }

    /// The page of a preview tab, made on first use.
    pub(crate) fn md_preview(
        &mut self,
        pane: PaneId,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<MarkdownPreview> {
        let key = (pane, path.to_owned());
        if let Some(view) = self.md_previews.get(&key) {
            return view.clone();
        }
        let view = self.new_markdown_preview(path, cx);
        let file = path.to_owned();
        let subscription = cx.subscribe_in(
            &view,
            window,
            move |this, _, event: &MarkdownPreviewEvent, window, cx| match event {
                MarkdownPreviewEvent::OpenLink(url) => {
                    this.open_markdown_link(&file, url, window, cx);
                }
            },
        );
        self.md_preview_subscriptions
            .insert(key.clone(), subscription);
        self.md_previews.insert(key, view.clone());
        view
    }

    /// The page of the preview tab of `path` in `pane`, if it was drawn.
    pub fn markdown_preview_of(&self, pane: PaneId, path: &str) -> Option<Entity<MarkdownPreview>> {
        self.md_previews.get(&(pane, path.to_owned())).cloned()
    }

    /// Every preview tab as `(pane, path)`, in tree order.
    pub fn preview_tabs(&self) -> Vec<(PaneId, String)> {
        self.state
            .center
            .panes()
            .into_iter()
            .flat_map(|pane| {
                pane.items
                    .iter()
                    .filter_map(move |item| item.as_preview().map(|p| (pane.id, p.to_owned())))
            })
            .collect()
    }

    /// Body of a preview tab: the bar, and the page under it.
    pub(crate) fn render_preview_item(
        &mut self,
        pane: PaneId,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = self.md_preview(pane, path, window, cx);
        self.rendered_previews.insert((pane, path.to_owned()));
        // Follow the text this page was opened beside (the first tab of the file that shows the
        // text; none: the page scrolls on its own).
        let source = self
            .file_views
            .iter()
            .filter(|((_, file), _)| file == path)
            .map(|(_, file_view)| file_view)
            .find(|file_view| !file_view.read(cx).is_rendered())
            .map(|file_view| file_view.read(cx).code().clone());
        view.update(cx, |view, cx| view.set_source(source, true, cx));
        let (theme, tokens) = (self.theme, self.density.ui());
        let key = path.to_owned();
        let to_source: Handler = {
            let workbench = cx.weak_entity();
            let key = key.clone();
            Rc::new(move |_, window, cx| {
                let key = key.clone();
                workbench
                    .update(cx, |this, cx| {
                        this.preview_to_source(pane, &key, window, cx)
                    })
                    .ok();
            })
        };
        let caption = SharedString::from(format!(
            "Preview of {}",
            hutan_code_view::file_name(Path::new(path))
        ));
        let bar = Bar {
            mode: self.markdown_mode(),
            page: true,
            caption: Some(caption),
            on_toggle: to_source,
            on_side: None,
        }
        .element(&theme, &tokens);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(bar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(super::render::cached_view(&self.regions, view)),
            )
            .into_any_element()
    }

    /// Drops the pages of preview tabs that are gone.
    pub(crate) fn sync_md_previews(&mut self) {
        let keep: std::collections::HashSet<(PaneId, String)> =
            self.preview_tabs().into_iter().collect();
        let gone: Vec<(PaneId, String)> = self
            .md_previews
            .keys()
            .filter(|key| !keep.contains(*key))
            .cloned()
            .collect();
        for key in gone {
            self.md_preview_subscriptions.remove(&key);
            if let Some(view) = self.md_previews.remove(&key) {
                // Focus may still sit on it; `rescue_orphaned_focus` checks before it is dropped.
                self.pruned_previews.push(view);
            }
        }
    }

    /// ⇧⌘V and *Markdown: Toggle Preview*: what `editor.markdown_preview` says for the focused
    /// tab: its text and its page in turn, or the page beside it; in a preview tab, the text.
    pub fn toggle_markdown_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.state.active_pane_id();
        match self.state.active_center_item() {
            Some(ItemRef::File { path }) if is_markdown(&path) => match self.markdown_mode() {
                MarkdownPreviewMode::Off => self.markdown_preview_is_off(cx),
                MarkdownPreviewMode::Toggle => self.toggle_markdown_in_tab(pane, &path, window, cx),
                MarkdownPreviewMode::Side => self.open_markdown_preview_side(window, cx),
            },
            Some(ItemRef::Preview { path }) => self.preview_to_source(pane, &path, window, cx),
            _ => self.set_notice("Open a Markdown file to preview it", cx),
        }
    }

    fn markdown_preview_is_off(&mut self, cx: &mut Context<Self>) {
        self.set_notice(
            "Markdown preview is off: set editor.markdown_preview to toggle or side",
            cx,
        );
    }

    /// Switches the file tab of `path` in `pane` between its text and its page.
    pub(crate) fn toggle_markdown_in_tab(
        &mut self,
        pane: PaneId,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.markdown_mode() == MarkdownPreviewMode::Off {
            self.markdown_preview_is_off(cx);
            return;
        }
        let view = self.file_view(pane, path, window, cx);
        let rendered = view.read(cx).is_rendered();
        let page = (!rendered).then(|| self.new_markdown_preview(path, cx));
        view.update(cx, |view, cx| view.set_rendered(page, Some(window), cx));
    }

    /// The page of the file tab `view` shows, when `editor.markdown_open` says so (or a link of
    /// a page opened the file).
    pub(crate) fn open_markdown_as_page(
        &mut self,
        view: &Entity<FileView>,
        path: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let asked = self.md_rendered_on_open.remove(path);
        let settings = self.settings.get::<EditorSettings>();
        if !is_markdown(path)
            || settings.markdown_preview == MarkdownPreviewMode::Off
            || !(asked || settings.markdown_open == MarkdownOpen::Preview)
        {
            return;
        }
        let page = self.new_markdown_preview(path, cx);
        view.update(cx, |view, cx| view.set_rendered(Some(page), None, cx));
    }

    /// *Markdown: Open Preview to the Side*: the focused Markdown file's page in a split to the
    /// right (the one already open is focused instead). The keyboard stays with the text.
    pub fn open_markdown_preview_side(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.state.active_pane_id();
        match self.state.active_center_item() {
            Some(ItemRef::File { path } | ItemRef::Preview { path }) if is_markdown(&path) => {
                self.show_markdown_preview_side(pane, &path, window, cx);
            }
            _ => self.set_notice("Open a Markdown file to preview it", cx),
        }
    }

    /// The page of `path`, the text of which is in `pane`, in a split to the right of it.
    pub(crate) fn show_markdown_preview_side(
        &mut self,
        pane: PaneId,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.markdown_mode() == MarkdownPreviewMode::Off {
            self.markdown_preview_is_off(cx);
            return;
        }
        let item = ItemRef::preview(path);
        if let Some((found, index)) = self.state.find_item(&item) {
            self.try_mutate(cx, |s| {
                s.set_active_item(found, index)?;
                s.focus_pane(found)
            });
        } else {
            self.ensure_buffer(path, true, None, cx);
            self.try_mutate(cx, |s| {
                s.split_pane(pane, Edge::Right, item.clone())?;
                s.focus_pane(pane)
            });
        }
        self.focus_root(window, cx);
    }

    /// A preview tab goes back to the text: the same tab, the file's.
    pub(crate) fn preview_to_source(
        &mut self,
        pane: PaneId,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let item = ItemRef::preview(path);
        // The tab of this pane, else the first one that shows this page.
        let found = self
            .state
            .pane(pane)
            .and_then(|p| p.items.iter().position(|i| *i == item))
            .map(|index| (pane, index))
            .or_else(|| self.state.find_item(&item));
        let Some((found, index)) = found else {
            return;
        };
        let text = ItemRef::file(path);
        self.try_mutate(cx, |s| {
            let slot = s
                .center
                .pane_mut(found)
                .and_then(|p| p.items.get_mut(index))
                .ok_or("the preview tab is gone")?;
            *slot = text;
            s.set_active_item(found, index).map_err(|_| "no such tab")?;
            s.focus_pane(found).map_err(|_| "no such pane")
        });
        self.focus_root(window, cx);
    }

    /// A click on a link of the page of `file`: the web goes to the browser, a file opens in a
    /// tab (a Markdown file as a page, from a page).
    pub(crate) fn open_markdown_link(
        &mut self,
        file: &str,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dir = Path::new(file).parent().unwrap_or(Path::new("/"));
        match classify_link(url, dir) {
            Some(LinkTarget::Web(url)) => cx.open_url(&url),
            Some(LinkTarget::File { path, line }) => {
                let shown = path.display().to_string();
                if is_markdown(&shown) {
                    // The daemon names the tab by the canonical path.
                    let canonical = std::fs::canonicalize(&path).unwrap_or(path.clone());
                    self.md_rendered_on_open
                        .insert(canonical.display().to_string());
                }
                let mut target = FileOpen::picked(path);
                target.line = line;
                self.open_file(target, window, cx);
            }
            Some(LinkTarget::Anchor(_)) | None => {}
        }
    }
}
