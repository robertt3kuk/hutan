# E-7a — extract the Vim layer into a `vim` crate

Agent: implementer (Claude Sonnet 5.5)   Depends on: UX-CHAT-1 merged (same chat files)   Branch: `slice/E-7a`

## Context
Read `AGENTS.md`, ADR 0012, `docs/spikes/2026-10-21-editor.md` §4.6 (Vim plan, item 1) and §4.7 row **E-7a**, `docs/design.md` § Vim mode. Today's layer: `crates/desktop/src/workbench/chat/vim.rs` (V-1; ~40 tests) used by the chat composer and other text fields.

## Scope
New crate `crates/vim` (package `hutan-vim`, GPUI-free): `Vim<H: VimHost>` where `VimHost` exposes length, line↔offset, slice, char_at, apply edits (as transactions), selections, undo grouping, registers/clipboard, marks, search, ex `command`. Move the state machine (modes, motions, operators, text objects, registers, dot-repeat) into it unchanged in behaviour. In `desktop`, implement `VimHost` for the existing text input (adapter); the adapter may still build a `Vec<char>` for one-line fields. All fields that use Vim today keep working.

## Acceptance
- The ~40 V-1 tests pass unchanged against the adapter (moved into the crate or kept as adapter tests).
- GPUI suites touching Vim (composer, palette, settings) green; no behaviour change in `docs/design.md` § Vim mode.

## Acceptance (common)
- Unit/property tests in the crate; integration tests in `tests/it/` (BLD-1 layout). Iterate with `cargo nextest run -p <crate>`; `just check` once at the end (machine-wide lock). One commit on the slice branch ending `(E-7a)`.
- New dependencies in root `[workspace.dependencies]`, licenses accepted by `deny.toml`; record build-time and binary-size deltas in `docs/perf.md` (Editor rows).
- `docs/architecture.md` crate list updated.

## Parity
None expected (new crate; the desktop keeps every Vim affordance).

## Out of scope
GPUI view (`code_view`), daemon/protocol, LSP, file I/O. Do not edit `docs/status.md`, `docs/roadmap.md`.

## Report
Summary, API outline, numbers, files changed, commands with results, open questions.
