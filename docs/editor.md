# The editor

Hutan's own code editor (ADR 0012 and its amendment of 2026-10-21; plan and budgets: `docs/spikes/2026-10-21-editor.md`). It is built in slices; this page says what exists today.

| Slice | What | Where |
|---|---|---|
| E-1 | text model: rope, transactions, undo, selections, anchors, search | `crates/buffer` |
| E-2 | syntax: tree-sitter on a worker thread, highlights as `syntax.*` tokens | `crates/syntax` |
| E-3 | file service in the daemon: `fs.read`, `fs.write`, `fs.stat`, … | `crates/daemon/src/files`, `docs/protocol.md` § Files |
| E-4 | `editor.*` and `syntax.*` colors in the color packs | `docs/themes.md` |
| E-5 | read-only viewer in file tabs | `crates/code_view`, `crates/desktop/src/workbench/{files,file_view}.rs` |
| **E-6** | **editing: carets, multi-cursor, input methods, undo, auto-indent and pairs, clipboard, save through the daemon, conflict bar, replace** | `crates/code_view/src/{editing,motion,input,rules}.rs`, `crates/desktop/src/workbench/file_sync.rs` |

Vim in files (E-8), LSP, and the file tree (E-13a) are not here yet. `editor.vim_mode` changes nothing in a file tab yet: typing works the same with it on.

## File tabs (E-5)

How the tab looks and how to open one: `docs/design.md` § File tabs. Under the hood:

- **One buffer per file.** The workbench's `BufferStore` keys buffers by canonical path (what `fs.stat` returns), so the same file opened twice, or shown in two panes (a file tab's **Open in Split Right / Down**), is one `FileBuffer` with two `CodeView`s. Each view has its own scroll, selection and find. The buffer is dropped when the last tab that shows it closes; the Agents overview's stash counts as a tab.
- **Reads go through the daemon.** A file is looked up with `fs.stat`, which gives its canonical path, kind and size. It is then read with `fs.read` in chunks of up to 1 MiB (`Connection::fs_read_all`). The daemon refuses what the window may not read, along with binary, non-UTF-8 and files over 64 MiB, and the tab shows why. When the user chose the file (the Finder dialog, a ⌘-click in a terminal, a restored tab), it is read with `picked`, so from anywhere. When an agent may be asking (`ui.run workbench::OpenFile`, `hutan open`), it is read without `picked`, so only below the workspace roots.
- **Line endings and BOM.** The text is normalized for drawing: `\r\n` becomes `\n` and the BOM is dropped (`hutan_buffer::Buffer::from_content`). What the file had is kept on the buffer and written back on save (`Buffer::to_bytes`), so a CRLF file stays CRLF; pasted `\r\n` is normalized.
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

## Editing (E-6)

How it looks and which keys do what: `docs/design.md` § File tabs. Under the hood:

- **One transaction per command.** Every command runs on every caret and becomes one `hutan_buffer::Transaction` with the selections before and after it (`code_view/src/editing.rs`), so a multi-caret edit, a line move or a Replace All is one undo step and undo puts the selections back. Typing (and an input method's composition) joins the typing of the same view that came less than a second before (`Coalesce::Typing`, an open undo group in the buffer); a caret move, a command, a save or a pause ends the step. Undo is per buffer, shared by its views.
- **Other views follow.** The buffer announces every change (`BufferEdited`, with the transaction); the other views of the file map their carets, marked text and find matches through it.
- **The parser follows incrementally.** Each edit is fed to the syntax worker as tree-sitter edits (`edits_of_transaction`) and a reparse of the new rope snapshot, never a parse from scratch (`SyntaxStats::incremental` counts them); highlights are carried across edits until the new tree lands.
- **What a keystroke draws.** Shaped lines are cached twice: by line and text version (a frame without changes shapes nothing) and by what a line shows (its text and colors), so an edit reshapes the lines it changed, not every line in view.
- **Input methods.** `CodeView` implements GPUI's `EntityInputHandler` over UTF-16 offsets of the whole file (the rope's UTF-16 metric, `O(log n)`). Plain typing gets the auto-pair rules; a composition is applied to the text as marked ranges (one per caret), its candidate window goes under `bounds_for_range`, and the commit is one step with the typing around it. A replacement range (press-and-hold accents) replaces the letter before the caret. Return and Escape during a composition only end it.
- **Auto-indent** uses `Snapshot::indent_level_after` when the tree is of the current text, else the line's own indentation plus one level after an opening bracket (Python and YAML: also `:`); the unit (tab or 2/3/4/8 spaces) is read from the file when it loads.
- **Saving** (`desktop/src/workbench/file_sync.rs`): `fs.write` with `expect` = the version the file was read or last saved as. A `conflict` answer shows the conflict bar with the file's text as it is now; **Keep Mine** writes with `expect` = that newer version, so a third change is still refused; a deleted file is created again (`create`). While a save is on its way, typing goes on in a new undo step; the buffer is clean afterwards only if it was not edited meanwhile.
- **Watching.** Every open file is watched (`fs.watch`; the connection watches again after a reconnect and then checks every file). On `fs.changed`: origin `self` (our own save) is ignored; a change of anybody else is read with `fs.read`, and a clean buffer takes it with `apply_external` (one undo step, carets in unchanged text stay, clean afterwards), a changed one gets the bar. A removed file gets the *deleted* bar.


| Key | Default | |
|---|---|---|
| `editor.font_size` | `13.0` | points; the family is the pack's `[fonts] mono` |
| `editor.line_height` | `1.45` | multiple of the font size |
| `editor.large_file_bytes` | `16777216` | above it: ask first, then read-only without syntax |
| `editor.autosave` | `off` | `on_focus_change` or `after_delay`; never while the conflict bar shows |
| `editor.autosave_delay_ms` | `1000` | for `after_delay` |
| `terminal.open_files_with` | `system` | `editor` sends ⌘-clicked paths to a file tab at the line |

## Tests

- `crates/code_view/tests/it/view.rs`:
  - opening a 50 000-line file shapes at most the lines in view; an idle frame shapes none; scrolling one line shapes at most one;
  - a theme change restyles without a parse;
  - the tree is freed when hidden and parsed again when shown, and never exists in large-file mode;
  - find steps and wraps;
  - mouse drag and copy.
- `crates/code_view/tests/it/editing.rs` (E-6): auto-pairs, step-over, pair deletion, soft-tab backspace, surround; outdent of a closer; Return indented from the tree; ⌘D and a multi-caret edit as one undo step (selections restored); select all occurrences; ⌥-click and ⌥⇧-drag; caret motions with a kept column; Replace and Replace All (one step); a keystroke reshapes one line; edits reparse incrementally; another view keeps its caret; multi-caret and line-wise clipboard; comment, duplicate, move, delete, indent lines; large files are read-only; CRLF and BOM survive an edit.
- `crates/code_view/tests/it/ime.rs` (E-6, TI-1 for the editor): Japanese composition marked in place with its candidate bounds and one undo step; Return and Escape end a composition; a composition at two carets; a Cyrillic layout; press-and-hold accent.
- `crates/desktop/tests/it/editor_gpui.rs`, over a real daemon:
  - two panes share one buffer, and closing the last tab frees the buffer and its tree;
  - the layout round-trips with a file tab;
  - a theme change restyles the open files without a reparse;
  - large files ask first, and binary files are refused.
- `links_gpui.rs`: a ⌘-click with `editor` opens the file at the line.
- `control_gpui.rs`: `workbench::OpenFile` dispatches.
- `model/tests.rs`: the stored form that older binaries read.
- Timings: `just bench --only B11,B13` (`docs/perf.md` § Editor: the viewer).
