# The editor

Hutan's own code editor (ADR 0012 and its amendment of 2026-10-21; plan and budgets: `docs/spikes/2026-10-21-editor.md`). It is built in slices; this page says what exists today.

| Slice | What | Where |
|---|---|---|
| E-1 | text model: rope, transactions, undo, selections, anchors, search | `crates/buffer` |
| E-2 | syntax: tree-sitter on a worker thread, highlights as `syntax.*` tokens | `crates/syntax` |
| E-3 | file service in the daemon: `fs.read`, `fs.write`, `fs.stat`, … | `crates/daemon/src/files`, `docs/protocol.md` § Files |
| E-4 | `editor.*` and `syntax.*` colors in the color packs | `docs/themes.md` |
| **E-5** | **read-only viewer in file tabs** | `crates/code_view`, `crates/desktop/src/workbench/{files,file_view}.rs` |

Editing (E-6), Vim in files (E-8), LSP, and the file tree (E-13a) are not here yet.

## File tabs (E-5)

How the tab looks and how to open one: `docs/design.md` § File tabs. Under the hood:

- **One buffer per file.** The workbench's `BufferStore` keys buffers by canonical path (what `fs.stat` returns), so the same file opened twice, or shown in two panes (a file tab's **Open in Split Right / Down**), is one `FileBuffer` with two `CodeView`s. Each view has its own scroll, selection and find. The buffer is dropped when the last tab that shows it closes; the Agents overview's stash counts as a tab.
- **Reads go through the daemon.** A file is looked up with `fs.stat`, which gives its canonical path, kind and size. It is then read with `fs.read` in chunks of up to 1 MiB (`Connection::fs_read_all`). The daemon refuses what the window may not read, along with binary, non-UTF-8 and files over 64 MiB, and the tab shows why. When the user chose the file (the Finder dialog, a ⌘-click in a terminal, a restored tab), it is read with `picked`, so from anywhere. When an agent may be asking (`ui.run workbench::OpenFile`, `hutan open`), it is read without `picked`, so only below the workspace roots.
- **Line endings and BOM.** The text is normalized for drawing: `\r\n` becomes `\n` and the BOM is dropped (`hutan_buffer::Buffer::from_content`). What the file had is kept on the buffer (`line_ending`, `has_bom`) for E-6's saves.
- **Syntax only while shown.** A tree-sitter tree of token-dense code takes about 37 bytes per byte of source (`docs/perf.md` § Editor: syntax). So a buffer holds a `Syntax` only while some pane shows it as its active tab. The rope stays; the tree is parsed again, in the background, when the tab is shown again. The parser's worker wakes the views through a one-slot channel when a tree lands. Until then the text is drawn uncolored, never blocked.
- **Large-file mode.** Above `editor.large_file_bytes` (default 16 MiB) nothing is read until the user confirms. The file then opens read-only without syntax.
- **Drawing.** `CodeElement` lays out only the lines that intersect the view (`OVERSCAN_LINES` = 0), and only the columns in view plus a margin of 8, so a 5 MB line costs a window, not the line. Code is drawn on a cell grid, one glyph per cell (GPUI's `force_width`, ligatures off):
  - a tab runs to the next stop of 4;
  - a wide character takes two cells;
  - a control character is one replacement glyph;
  - a combining mark rides on its base.

  Hit testing and the selection and match fills are therefore arithmetic (`layout.rs`). Shaped lines are cached by line, text version, column window, style generation and tree version. A frame that changes nothing shapes nothing, and scrolling a line shapes one line.
- **Theme.** `CodeStyle` is the pack's `editor()` colors, its monospace family, `editor.font_size` and `editor.line_height`. A theme or appearance change restyles the open tabs: the lines in view are shaped again in the new colors, and nothing is parsed again.
- **Find.** The search runs on a background thread over an `O(1)` snapshot of the rope (`hutan_buffer::Text::search_all`). It is literal or regex, case-insensitive unless **Aa** is on, and keeps at most 10 000 matches. A newer query drops the answer of an older one.

## Settings

| Key | Default | |
|---|---|---|
| `editor.font_size` | `13.0` | points; the family is the pack's `[fonts] mono` |
| `editor.line_height` | `1.45` | multiple of the font size |
| `editor.large_file_bytes` | `16777216` | above it: ask first, then read-only without syntax |
| `terminal.open_files_with` | `system` | `editor` sends ⌘-clicked paths to a file tab at the line |

## Tests

- `crates/code_view/tests/it/view.rs`:
  - opening a 50 000-line file shapes at most the lines in view; an idle frame shapes none; scrolling one line shapes at most one;
  - a theme change restyles without a parse;
  - the tree is freed when hidden and parsed again when shown, and never exists in large-file mode;
  - find steps and wraps;
  - mouse drag and copy.
- `crates/desktop/tests/it/editor_gpui.rs`, over a real daemon:
  - two panes share one buffer, and closing the last tab frees the buffer and its tree;
  - the layout round-trips with a file tab;
  - a theme change restyles the open files without a reparse;
  - large files ask first, and binary files are refused.
- `links_gpui.rs`: a ⌘-click with `editor` opens the file at the line.
- `control_gpui.rs`: `workbench::OpenFile` dispatches.
- `model/tests.rs`: the stored form that older binaries read.
- Timings: `just bench --only B11,B13` (`docs/perf.md` § Editor: the viewer).
