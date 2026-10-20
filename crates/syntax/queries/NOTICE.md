# Query data: attribution and licenses

`hutan-syntax` compiles tree-sitter grammars and ships the queries that drive highlighting,
folding and indentation. Two kinds of files live here:

- `vendored/` — **unmodified copies** of the `highlights` queries that the pinned grammar crates
  publish. They stay MIT-licensed under their authors' copyright; the license texts are in
  `licenses/`. They are copies, not edits: `tests/it/vendored.rs` compares each with the constant
  the crate exports, so a grammar bump that changes upstream shows up as a failing test and has
  to be reviewed here.
- `hutan/` — Hutan's own overlays (`highlights.scm`, `folds.scm`, `indents.scm` per language),
  licensed like the rest of the repository, MIT OR Apache-2.0. A highlight overlay is appended to
  the vendored query of its language; where both capture the same node the later pattern wins, so
  overlays correct upstream (for example Go's and JSON's queries expect "first pattern wins", the
  vendored Rust all-caps rule has a stray quote in its regex) without editing the vendored file.

## Vendored files

| File | Crate (exact pin) | Upstream repository | License | Copyright |
|---|---|---|---|---|
| `vendored/rust/highlights.scm` | `tree-sitter-rust` 0.24.2 | https://github.com/tree-sitter/tree-sitter-rust | MIT, `licenses/tree-sitter-rust.LICENSE` | 2017 Maxim Sokolov |
| `vendored/javascript/highlights.scm`, `vendored/javascript/highlights-jsx.scm`, `vendored/javascript/highlights-params.scm` | `tree-sitter-javascript` 0.25.0 | https://github.com/tree-sitter/tree-sitter-javascript | MIT, `licenses/tree-sitter-javascript.LICENSE` | 2014 Max Brunsfeld |
| `vendored/typescript/highlights.scm` | `tree-sitter-typescript` 0.23.2 | https://github.com/tree-sitter/tree-sitter-typescript | MIT, `licenses/tree-sitter-typescript.LICENSE` | 2017 Max Brunsfeld |
| `vendored/python/highlights.scm` | `tree-sitter-python` 0.25.0 | https://github.com/tree-sitter/tree-sitter-python | MIT, `licenses/tree-sitter-python.LICENSE` | 2016 Max Brunsfeld |
| `vendored/go/highlights.scm` | `tree-sitter-go` 0.25.0 | https://github.com/tree-sitter/tree-sitter-go | MIT, `licenses/tree-sitter-go.LICENSE` | 2014 Max Brunsfeld |
| `vendored/json/highlights.scm` | `tree-sitter-json` 0.24.8 | https://github.com/tree-sitter/tree-sitter-json | MIT, `licenses/tree-sitter-json.LICENSE` | 2014 Max Brunsfeld |
| `vendored/toml/highlights.scm` | `tree-sitter-toml-ng` 0.7.0 | https://github.com/tree-sitter-grammars/tree-sitter-toml | MIT, `licenses/tree-sitter-toml-ng.LICENSE` | Ika |
| `vendored/yaml/highlights.scm` | `tree-sitter-yaml` 0.7.2 | https://github.com/tree-sitter-grammars/tree-sitter-yaml | MIT, `licenses/tree-sitter-yaml.LICENSE` | 2019-2021 Ika, 2024 tree-sitter-grammars contributors |
| `vendored/markdown/block-highlights.scm`, `vendored/markdown/inline-highlights.scm` | `tree-sitter-md` 0.5.3 | https://github.com/tree-sitter-grammars/tree-sitter-markdown | MIT, `licenses/tree-sitter-md.LICENSE` | 2021 Matthias Deiml |
| `vendored/bash/highlights.scm` | `tree-sitter-bash` 0.25.1 | https://github.com/tree-sitter/tree-sitter-bash | MIT, `licenses/tree-sitter-bash.LICENSE` | 2017 Max Brunsfeld |
| `vendored/html/highlights.scm` | `tree-sitter-html` 0.23.2 | https://github.com/tree-sitter/tree-sitter-html | MIT, `licenses/tree-sitter-html.LICENSE` | 2014 Max Brunsfeld |
| `vendored/css/highlights.scm` | `tree-sitter-css` 0.25.0 | https://github.com/tree-sitter/tree-sitter-css | MIT, `licenses/tree-sitter-css.LICENSE` | 2018 Max Brunsfeld |
| `vendored/c/highlights.scm` | `tree-sitter-c` 0.24.2 | https://github.com/tree-sitter/tree-sitter-c | MIT, `licenses/tree-sitter-c.LICENSE` | 2014 Max Brunsfeld |
| `vendored/cpp/highlights.scm` | `tree-sitter-cpp` 0.23.4 | https://github.com/tree-sitter/tree-sitter-cpp | MIT, `licenses/tree-sitter-cpp.LICENSE` | 2014 Max Brunsfeld |

The license files were copied from the crate packages, or from the upstream repository's default
branch when the published crate does not include one (`cpp`, `html`, `json`, `md`, `toml-ng`,
`typescript`).

The grammars themselves are not stored here: they are the compiled C sources of the same
crates, pulled in by Cargo and checked by `cargo deny` (all MIT).

Some upstream query files say they were taken from `nvim-treesitter` (Apache-2.0), notably the
markdown ones (`;From nvim-treesitter/nvim-treesitter`). The crates that distribute them
publish them under MIT, and Apache-2.0 is among the project's own licenses, so no
obligation is added; the origin is recorded here for the record.

## Grammars not included

The slice brief asked to drop any grammar whose crate is unmaintained or license-incompatible.
`tree-sitter-toml` 0.20 (last release 2022) was replaced by the maintained fork
`tree-sitter-toml-ng` 0.7. Every crate above is MIT and has a release between November 2024 and
April 2026 (crates.io, checked 2026-10-21).

## Updating

1. Bump the pins in the workspace `Cargo.toml`.
2. `cargo nextest run -p hutan-syntax`: `tests/it/vendored.rs` fails for every upstream query that
   changed. Copy the new file from the crate's `queries/` (or the crate's constant) into
   `vendored/`, and update the version in the table.
3. If a pattern now fights an overlay, fix the overlay, not the vendored file.
4. `UPDATE_GOLDEN=1 cargo nextest run -p hutan-syntax` and review the golden diff like code.
