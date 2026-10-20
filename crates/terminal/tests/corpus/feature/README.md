# Feature corpus

Hand-written VT streams (`NAME.vt`) with their expected outcomes (`NAME.expect.json`).
Replayed by `tests/it/corpus.rs` through a fresh `Emulator` whole, one byte at a time and in
7-byte chunks; all three must give the identical outcome.

## `.vt` format
Raw bytes written as text: `\e` ESC, `\a` BEL, `\r`, `\n`, `\t`, `\\`, `\xHH`. Literal
newlines and lines starting with `//` are layout only and are not part of the stream. Any
other byte (including UTF-8 text) is taken literally.

## `.expect.json`
Every key is optional; absent keys are not checked.

| key | meaning |
|---|---|
| `size` | `[cols, rows]` or `[cols, rows, pixel_width, pixel_height]`, default `[80, 24]` without pixel size |
| `grid` | plain text of the first rows (trailing spaces trimmed); rows not listed must be blank |
| `runs` | `{"<row>": [partial Run, …]}`: exact run list of the row; each run object is a subset match against the serialized `Run` |
| `wrapped` | per-row soft-wrap flags |
| `cursor`, `modes`, `palette` | subset match against the serialized DTO |
| `links` | hyperlink table URIs in id order |
| `replies` | exact bytes the engine wrote back to the PTY |
| `meta` | `Debug` strings of the metadata events, in order |
| `sync_pending` | whether a DEC 2026 batch is open at the end |
| `history_text` | scrollback rows (newest first) as plain text |
| `history_len`, `title` | exact |
