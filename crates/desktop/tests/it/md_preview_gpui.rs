#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The Markdown preview (MD-PREVIEW) over a real daemon: a Markdown file tab switches between its
//! text and its page (⇧⌘V), *Open Preview to the Side* opens a page that follows the file's edits
//! and scrolling and redraws only what changed, links open files and scroll to headings, images of
//! the repository are found, and a Markdown file asks no language server for trust unless the user
//! enabled one. The renderer's pieces (tables, task boxes, anchors) are unit tests of
//! `hutan-chat-view` and `markdown_preview.rs`.
//!
//! The page parses 120 ms after the last edit on the test clock: after typing the tests advance it
//! past the debounce.

use crate::common;

use std::path::Path;
use std::time::Duration;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_chat_view::markdown::{Block, Inline};
use hutan_code_view::{CodeView, FileBuffer, LoadState};
use hutan_desktop::workbench::file_view::FileView;
use hutan_desktop::workbench::markdown_preview::{DEBOUNCE, MarkdownPreview};
use hutan_desktop::workbench::{FileOpen, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};

const DOC: &str = "# Guide\n\nIntro with a [link](#usage) and **bold**.\n\n## Usage\n\n\
- [x] done\n- [ ] todo\n\n| name | value |\n|:-----|------:|\n| a    | 1     |\n\n\
```rust\nfn main() {}\n```\n\n![logo](img/logo.png)\n";

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    settings_toml: &str,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        format!("[app]\nsetup_done = true\n{settings_toml}"),
    )
    .unwrap();
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let (view, cx) = open_with(cx, settings, daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    (view, cx)
}

struct Tab {
    path: String,
    view: Entity<FileView>,
    code: Entity<CodeView>,
    buffer: Entity<FileBuffer>,
}

/// Opens `text` as `name` below `dir` in a tab and waits until its text is in.
fn open_text(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    dir: &Path,
    name: &str,
    text: &str,
) -> Tab {
    let file = dir.join(name);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&file, text).unwrap();
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(&file), window, cx);
    });
    let path = file.canonicalize().unwrap().display().to_string();
    wait_until(cx, "the file's tab", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.focused_file_view().is_some_and(|v| {
                let buffer = v.read(cx).buffer().read(cx);
                buffer.path().display().to_string() == path && *buffer.state() == LoadState::Ready
            })
        })
    });
    let file_view = view.read_with(cx, |wb, _| wb.focused_file_view().unwrap());
    let (code, buffer) = file_view.read_with(cx, |v, _| (v.code().clone(), v.buffer().clone()));
    draw(cx);
    Tab {
        path,
        view: file_view,
        code,
        buffer,
    }
}

/// A folder (canonical) with a small PNG in `img/`.
fn repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("img")).unwrap();
    image::RgbaImage::from_pixel(4, 3, image::Rgba([200, 30, 30, 255]))
        .save(root.join("img/logo.png"))
        .unwrap();
    (dir, root)
}

/// Lets the debounce of the page run out and waits until it shows the buffer as it is.
fn settle_page(page: &Entity<MarkdownPreview>, cx: &mut VisualTestContext) {
    cx.executor()
        .advance_clock(DEBOUNCE + Duration::from_millis(10));
    cx.run_until_parked();
    wait_until(cx, "the page to show the text", |cx| {
        page.read_with(cx, |p, cx| p.is_current(cx))
    });
}

/// The page of the file tab, once the tab shows one and has parsed.
fn tab_page(tab: &Tab, cx: &mut VisualTestContext) -> Entity<MarkdownPreview> {
    wait_until(cx, "the tab's page", |cx| {
        tab.view.read_with(cx, |v, cx| {
            v.page()
                .is_some_and(|p| !p.read(cx).parsed().blocks.is_empty())
        })
    });
    tab.view.read_with(cx, |v, _| v.page().unwrap().clone())
}

/// The page of the preview tab of `path`, once the tab was drawn.
fn side_page(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    path: &str,
) -> Entity<MarkdownPreview> {
    wait_until(cx, "the preview tab", |cx| {
        view.read_with(cx, |wb, _| {
            wb.preview_tabs()
                .iter()
                .any(|(pane, file)| file == path && wb.markdown_preview_of(*pane, path).is_some())
        })
    });
    let page = view.read_with(cx, |wb, _| {
        let (pane, _) = wb
            .preview_tabs()
            .into_iter()
            .find(|(_, file)| file == path)
            .unwrap();
        wb.markdown_preview_of(pane, path).unwrap()
    });
    wait_until(cx, "the page to be parsed", |cx| {
        page.read_with(cx, |p, cx| {
            p.is_current(cx) && !p.parsed().blocks.is_empty()
        })
    });
    page
}

fn kinds(page: &Entity<MarkdownPreview>, cx: &VisualTestContext) -> Vec<&'static str> {
    page.read_with(cx, |p, _| {
        p.parsed()
            .blocks
            .iter()
            .map(|located| match &located.block {
                Block::Paragraph(_) => "paragraph",
                Block::Heading { .. } => "heading",
                Block::Code { .. } => "code",
                Block::Table { .. } => "table",
                Block::List { .. } => "list",
                Block::Quote(_) => "quote",
                Block::Rule => "rule",
            })
            .collect()
    })
}

fn paragraph_texts(page: &Entity<MarkdownPreview>, cx: &VisualTestContext) -> Vec<String> {
    page.read_with(cx, |p, _| {
        p.parsed()
            .blocks
            .iter()
            .filter_map(|located| match &located.block {
                Block::Paragraph(inlines) => Some(hutan_chat_view::markdown::plain_text(inlines)),
                _ => None,
            })
            .collect()
    })
}

#[gpui::test]
fn a_markdown_tab_switches_between_its_text_and_its_page(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "");
    let (_keep, root) = repo();
    let tab = open_text(&view, cx, &root, "guide.md", DOC);
    assert!(!tab.view.read_with(cx, |v, _| v.is_rendered()));

    view.update_in(cx, |wb, window, cx| wb.toggle_markdown_preview(window, cx));
    let page = tab_page(&tab, cx);
    assert!(tab.view.read_with(cx, |v, _| v.is_rendered()));
    assert_eq!(
        kinds(&page, cx),
        [
            "heading",
            "paragraph",
            "heading",
            "list",
            "table",
            "code",
            "paragraph"
        ],
        "headings, lists, tables, code and the image's paragraph"
    );
    // Task boxes, the table's alignment and the image are in the page's blocks.
    page.read_with(cx, |p, _| {
        let blocks = &p.parsed().blocks;
        let Block::List { items, .. } = &blocks[3].block else {
            panic!("a list")
        };
        assert!(matches!(
            &items[0][0],
            Block::Paragraph(content) if content[0] == Inline::Task(true)
        ));
        assert!(matches!(
            &items[1][0],
            Block::Paragraph(content) if content[0] == Inline::Task(false)
        ));
        assert!(matches!(
            &blocks[4].block,
            Block::Table { rows, .. } if rows.len() == 1
        ));
        assert!(matches!(
            &blocks[6].block,
            Block::Paragraph(content) if matches!(content[0], Inline::Image { .. })
        ));
    });
    draw(cx);
    assert!(
        page.read_with(cx, |p, _| p.blocks_built()) > 0,
        "the page was drawn"
    );

    // Back to the text.
    view.update_in(cx, |wb, window, cx| wb.toggle_markdown_preview(window, cx));
    assert!(!tab.view.read_with(cx, |v, _| v.is_rendered()));
    draw(cx);
    assert_eq!(
        tab.buffer.read_with(cx, |b, _| b.text().to_string()),
        DOC,
        "the text was not touched"
    );
}

#[gpui::test]
fn shift_cmd_v_is_the_key_of_the_toggle(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "");
    let (_keep, root) = repo();
    let tab = open_text(&view, cx, &root, "guide.md", DOC);
    cx.simulate_keystrokes("shift-cmd-v");
    wait_until(cx, "the page", |cx| {
        tab.view.read_with(cx, |v, _| v.is_rendered())
    });
    // The keyboard is with the page: the same key goes back to the text.
    cx.simulate_keystrokes("shift-cmd-v");
    wait_until(cx, "the text", |cx| {
        tab.view.read_with(cx, |v, _| !v.is_rendered())
    });
}

#[gpui::test]
fn a_page_beside_the_text_follows_its_edits_and_redraws_only_what_changed(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "");
    let (_keep, root) = repo();
    let long: String = (0..60)
        .map(|n| format!("paragraph number {n}\n\n"))
        .collect();
    let tab = open_text(&view, cx, &root, "long.md", &long);

    view.update_in(cx, |wb, window, cx| {
        wb.open_markdown_preview_side(window, cx);
    });
    let page = side_page(&view, cx, &tab.path);
    assert_eq!(page.read_with(cx, |p, _| p.parsed().blocks.len()), 60);
    // The text keeps the keyboard, and the pane count says a split was made.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.preview_tabs().len()),
        1,
        "one preview tab"
    );
    assert!(
        view.read_with(cx, |wb, _| wb.focused_file_view().is_some()),
        "the text is still the focused tab"
    );
    let (parses, _) = page.read_with(cx, |p, _| p.work());

    // Type into paragraph 30, three keystrokes in one burst.
    let at = long.find("paragraph number 30").unwrap() + "paragraph number 30".len();
    tab.code.update(cx, |code, cx| code.select(at..at, cx));
    cx.simulate_input("abc");
    settle_page(&page, cx);
    assert!(
        paragraph_texts(&page, cx).contains(&"paragraph number 30abc".to_owned()),
        "the page has the new text"
    );
    let (after, replaced) = page.read_with(cx, |p, _| p.work());
    assert_eq!(after, parses + 1, "the burst was parsed once");
    assert_eq!(replaced, 1, "only the block that changed was replaced");
    assert_eq!(page.read_with(cx, |p, _| p.parsed().blocks.len()), 60);

    // A new heading in the middle adds one block and replaces nothing else.
    let at = long.find("paragraph number 10").unwrap();
    tab.code.update(cx, |code, cx| code.select(at..at, cx));
    cx.simulate_input("## New");
    cx.dispatch_action(hutan_code_view::actions::Newline);
    cx.dispatch_action(hutan_code_view::actions::Newline);
    settle_page(&page, cx);
    assert_eq!(page.read_with(cx, |p, _| p.parsed().blocks.len()), 61);
    assert_eq!(page.read_with(cx, |p, _| p.work().1), 1);
    assert!(page.read_with(cx, |p, cx| p.shown_version()
        == tab.buffer.read(cx).version()));

    // Again with the same shortcut: the open page is focused, not opened twice.
    view.update_in(cx, |wb, window, cx| {
        wb.open_markdown_preview_side(window, cx);
    });
    assert_eq!(view.read_with(cx, |wb, _| wb.preview_tabs().len()), 1);
}

#[gpui::test]
fn a_page_beside_the_text_follows_its_scroll_by_block(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "");
    let (_keep, root) = repo();
    let long: String = (0..200)
        .map(|n| format!("## Part {n}\n\nparagraph number {n}\n\n"))
        .collect();
    let tab = open_text(&view, cx, &root, "long.md", &long);
    view.update_in(cx, |wb, window, cx| {
        wb.open_markdown_preview_side(window, cx);
    });
    let page = side_page(&view, cx, &tab.path);
    assert_eq!(page.read_with(cx, |p, _| p.top_block()), 0);

    tab.code.update(cx, |code, cx| code.scroll_to(4000.0, cx));
    draw(cx);
    draw(cx);
    let line = tab.code.read_with(cx, |code, _| code.top_line());
    assert!(line > 100, "the text scrolled to line {line}");
    let block = page.read_with(cx, |p, _| p.parsed().block_at_line(line));
    assert!(block > 50, "block {block}");
    wait_until(cx, "the page to follow", |cx| {
        page.read_with(cx, |p, _| p.top_block()) == block
    });
    assert_eq!(
        page.read_with(cx, |p, _| p.top_line()),
        page.read_with(cx, |p, _| p.parsed().line_of_block(block)),
        "the block at the top starts where the text's first line is, or above it"
    );

    // A frame of a page of 400 blocks builds the ones in view, not the 400.
    let before = page.read_with(cx, |p, _| p.blocks_built());
    page.update(cx, |_, cx| cx.notify());
    draw(cx);
    let frame = page.read_with(cx, |p, _| p.blocks_built()) - before;
    assert!(
        frame > 0 && frame < 120,
        "one frame built {frame} of 400 blocks"
    );

    // Back to the top.
    tab.code.update(cx, |code, cx| code.scroll_to(0.0, cx));
    draw(cx);
    wait_until(cx, "the page at the top", |cx| {
        page.read_with(cx, |p, _| p.top_block()) == 0
    });
}

#[gpui::test]
fn links_scroll_to_headings_open_files_and_markdown_opens_as_a_page(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "");
    let (_keep, root) = repo();
    let filler: String = (0..40).map(|n| format!("filler {n}\n\n")).collect();
    let after: String = (0..80).map(|n| format!("after {n}\n\n")).collect();
    let text = format!(
        "# Top\n\n{filler}## Usage\n\n{after}see [other](other.md), [code](src/main.rs#L3)\n"
    );
    let tab = open_text(&view, cx, &root, "docs/guide.md", &text);
    std::fs::write(root.join("docs/other.md"), "# Other\n\ntext\n").unwrap();
    std::fs::create_dir_all(root.join("docs/src")).unwrap();
    std::fs::write(
        root.join("docs/src/main.rs"),
        "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n",
    )
    .unwrap();

    view.update_in(cx, |wb, window, cx| wb.toggle_markdown_preview(window, cx));
    let page = tab_page(&tab, cx);
    draw(cx);

    // `#usage`: the page scrolls to the heading (no tab opens).
    page.update(cx, |page, cx| page.activate_link("#Usage", cx));
    let usage = page.read_with(cx, |p, _| p.parsed().anchors["usage"]);
    assert!(usage > 10);
    assert_eq!(page.read_with(cx, |p, _| p.top_block()), usage);
    assert_eq!(
        view.read_with(cx, |wb, cx| wb
            .focused_file_view()
            .unwrap()
            .read(cx)
            .buffer()
            .read(cx)
            .path()
            .to_owned()),
        Path::new(&tab.path)
    );

    // A Markdown file opens in a tab, as a page (the reader stays in the rendered view).
    page.update(cx, |page, cx| page.activate_link("other.md", cx));
    let other = root.join("docs/other.md").display().to_string();
    wait_until(cx, "the other page", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.focused_file_view().is_some_and(|v| {
                let v = v.read(cx);
                v.buffer().read(cx).path().display().to_string() == other && v.is_rendered()
            })
        })
    });

    // Back on the first file, a source file opens as text at the line `#L3` names.
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(&tab.path), window, cx);
    });
    wait_until(cx, "the guide again", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.focused_file_view()
                .is_some_and(|v| v.read(cx).buffer().read(cx).path() == Path::new(&tab.path))
        })
    });
    let page = view.read_with(cx, |wb, cx| {
        wb.focused_file_view().unwrap().read(cx).page().cloned()
    });
    let page = page.expect("the guide is still a page");
    page.update(cx, |page, cx| page.activate_link("src/main.rs#L3", cx));
    let main = root.join("docs/src/main.rs").display().to_string();
    wait_until(cx, "the source file", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.focused_file_view().is_some_and(|v| {
                let v = v.read(cx);
                v.buffer().read(cx).path().display().to_string() == main
                    && !v.is_rendered()
                    && v.code().read(cx).position(cx).0 == 3
            })
        })
    });
}

#[gpui::test]
fn images_of_the_repository_are_found_and_the_web_is_not_fetched(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "");
    let (_keep, root) = repo();
    let text = "![logo](img/logo.png)\n\n![remote](https://example.com/a.png)\n\n\
                ![gone](img/none.png)\n\n[![badge](img/logo.png)](https://example.com)\n\ntext\n";
    let tab = open_text(&view, cx, &root, "readme.md", text);
    view.update_in(cx, |wb, window, cx| wb.toggle_markdown_preview(window, cx));
    let page = tab_page(&tab, cx);
    draw(cx);
    draw(cx);
    let resolved = page.read_with(cx, |p, _| p.resolved_images());
    let logo = root.join("img/logo.png");
    assert_eq!(
        resolved,
        vec![
            ("https://example.com/a.png".to_owned(), None),
            ("img/logo.png".to_owned(), Some(logo)),
            ("img/none.png".to_owned(), None),
        ],
        "the local file is found; the web and a missing file are their alt text"
    );
}

#[gpui::test]
fn a_markdown_file_asks_no_language_server_for_trust_on_a_fresh_home(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "");
    let (_keep, root) = repo();
    let tab = open_text(&view, cx, &root, "notes.md", "# notes\n");
    wait_until(cx, "the document to reach the daemon", |cx| {
        view.read_with(cx, |wb, cx| wb.lsp().read(cx).opens_sent() >= 1)
    });
    for _ in 0..8 {
        draw(cx);
    }
    assert_eq!(
        tab.view.read_with(cx, |v, cx| v.lsp_bar_text(cx)),
        None,
        "no trust bar, no install bar"
    );
}

#[gpui::test]
fn marksman_is_there_for_a_user_who_enabled_it(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start_with_config("[lsp.servers.marksman]\n");
    let (view, cx) = open(cx, &daemon, "");
    let (_keep, root) = repo();
    let tab = open_text(&view, cx, &root, "notes.md", "# notes\n");
    wait_until(cx, "the bar of the server the user enabled", |cx| {
        draw(cx);
        tab.view
            .read_with(cx, |v, cx| v.lsp_bar_text(cx))
            .is_some_and(|text| text.contains("marksman"))
    });
}

#[gpui::test]
fn with_the_preview_off_there_is_no_page_and_the_key_says_why(cx: &mut TestAppContext) {
    let (_keep, root) = repo();
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "[editor]\nmarkdown_preview = \"off\"\n");
    let tab = open_text(&view, cx, &root, "a.md", "# a\n");
    view.update_in(cx, |wb, window, cx| wb.toggle_markdown_preview(window, cx));
    assert!(!tab.view.read_with(cx, |v, _| v.is_rendered()));
    assert!(
        view.read_with(cx, |wb, _| wb.notice().cloned())
            .is_some_and(|n| n.contains("editor.markdown_preview"))
    );
    view.update_in(cx, |wb, window, cx| {
        wb.open_markdown_preview_side(window, cx);
    });
    assert!(view.read_with(cx, |wb, _| wb.preview_tabs().is_empty()));
    // Even a file that asks to open as a page stays text.
    draw(cx);
    assert!(!tab.view.read_with(cx, |v, _| v.is_rendered()));
}

#[gpui::test]
fn with_the_preview_beside_the_key_opens_the_page_and_the_page_goes_back(cx: &mut TestAppContext) {
    let (_keep, root) = repo();
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "[editor]\nmarkdown_preview = \"side\"\n");
    let tab = open_text(&view, cx, &root, "b.md", "# b\n");
    view.update_in(cx, |wb, window, cx| wb.toggle_markdown_preview(window, cx));
    let page = side_page(&view, cx, &tab.path);
    assert!(!tab.view.read_with(cx, |v, _| v.is_rendered()));
    assert_eq!(kinds(&page, cx), ["heading"]);
    // Pressed again from the text, the key focuses the page that is open (one page, not two);
    // pressed in the page it shows the text again in the page's own tab.
    view.update_in(cx, |wb, window, cx| wb.toggle_markdown_preview(window, cx));
    assert_eq!(view.read_with(cx, |wb, _| wb.preview_tabs().len()), 1);
    view.update_in(cx, |wb, window, cx| wb.toggle_markdown_preview(window, cx));
    assert!(
        view.read_with(cx, |wb, _| wb.preview_tabs().is_empty()),
        "the page's tab shows the text now"
    );
}

#[gpui::test]
fn markdown_open_preview_starts_markdown_files_as_pages_and_nothing_else(cx: &mut TestAppContext) {
    let (_keep, root) = repo();
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "[editor]\nmarkdown_open = \"preview\"\n");
    let tab = open_text(&view, cx, &root, "c.md", "# c\n");
    assert!(tab.view.read_with(cx, |v, _| v.is_rendered()));
    let rs = open_text(&view, cx, &root, "c.rs", "fn c() {}\n");
    assert!(
        !rs.view.read_with(cx, |v, _| v.is_rendered()),
        "only Markdown files"
    );
}
