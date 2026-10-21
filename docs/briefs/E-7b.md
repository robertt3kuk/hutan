# E-7b — Vim growth with differential fixtures from a real Vim

Agent: implementer (Claude Sonnet 5.5)   Depends on: E-7a (`crates/vim`)   Branch: `slice/E-7b`

## Context
Read `AGENTS.md`, ADR 0012 **Amendment 2026-10-21** item 6 (Vim first-class before the M11 demo; spec from Vim/Neovim docs; Zed's GPL vim is not read; MIT/Apache sources may be borrowed with attribution), `docs/spikes/2026-10-21-editor.md` §4.6 (Vim plan items 2–5) and §4.7 row **E-7b**, `crates/vim` (E-7a: `Vim<H: VimHost>`, byte offsets, `Transaction`), `docs/design.md` § Vim mode.

## Scope
`crates/vim` (+ its tests/fixtures), a dev-only recorder `xtask vim-record` (or `crates/vim/examples/record.rs`) that drives `/usr/bin/vim -Nu NONE -es` (pin and record its version in each fixture header), `docs/editor.md` (new: supported-commands matrix), `docs/design.md` § Vim mode where behaviour visible in text fields changes. Extend `VimHost` only as needed (marks, search, ex command hooks, registers incl. clipboard as decided — say which in the report); keep the desktop `TextInput` adapter compiling and behaving (one-line fields may ignore multi-line features).
Features (spike §4.6 item 2): linewise and blockwise Visual (incl. block `I`/`A`/`c`), registers (`"a`–`"z`, `"A` append, `"0`–`"9`, `"-`, `"_`, `"+`, `".`, `":`, `"/`), marks and jumplist, macros (`q`/`@`/`@@`), `/ ? n N * #`, `%`, `{ } ( ) [[ ]]`, sentence/paragraph objects, quote/bracket objects, `gu gU g~ gJ J >> << =`, `:` line via a small ex parser (`:s`, `:g`, `:normal`, ranges, `:noh`, `:set` subset) with unsupported commands reporting "Not supported: X" instead of being swallowed.

## Acceptance
- ≥ 300 recorded fixtures replay equal to the pinned Vim (text, cursor, registers where applicable) in the gate; recording is not part of the gate.
- Unsupported commands report; macro record/replay; block-visual `I`/`A`/`c`.
- B15: any non-search Normal command on a 1 MB text ≤ 2 ms with a rope-like host (bench in the crate; record in `docs/perf.md`).
- Existing V-1/E-7a tests and desktop Vim GPUI suites green; `just check` green (machine-wide lock). Commits per feature group ending `(E-7b)`.

## Parity
None removed; new Vim behaviour in text fields listed in the report.

## Out of scope
The editor view (E-8), tree-sitter text objects (E-9). Do not edit `docs/status.md`, `docs/roadmap.md`.

## Report
Summary, supported matrix highlights, fixture count and Vim version, numbers, files, commands with results, open questions.
