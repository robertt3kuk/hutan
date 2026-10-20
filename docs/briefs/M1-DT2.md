# M1-DT2: M1 debt — CLI test infrastructure and attach to ended sessions

Agent: implementer   Depends on: master `b6e6867`   Branch: slice/M1-DT2

## Context
Read `AGENTS.md`, `docs/status.md` § Tech debt, `docs/protocol.md` daemon semantics (terminals of ended sessions stay attachable and show the final screen). Code: `crates/cli` (`tests/it/cli.rs`, `tests/it/common/mod.rs` both define `hutand_dir()` with a nested `cargo build`; `src/commands/attach.rs`), `justfile`.

## Scope
`crates/cli`, `justfile`, `docs/demos/M1.md` (only if behavior it describes changes). Do not edit other crates or `docs/status.md`/`docs/roadmap.md`.

## Changes

1. **No nested `cargo build` in tests** (debt 1). Delete both `hutand_dir()` copies. One helper in `tests/it/common/mod.rs` locates `hutand` next to the test's own `hutan` binary (`CARGO_BIN_EXE_hutan`'s directory, same profile); if missing, panic with exactly: `hutand not built: run \`cargo build -p hutan-daemon --bin hutand\` (or \`just test\`)`. `tests/it/cli.rs` uses the shared helper (move shared code into `common`, no duplication). `justfile`: `test` builds bins first — `cargo build --workspace --bins --locked` then `cargo nextest run …`; `update-golden` unchanged.

2. **Attach to an ended session shows the final screen** (behavior gap). `hutan attach <ended>`: render the final screen inside the usual enter/leave, then leave immediately, print `[session exited with code N]` (or `stopped` / `killed by signal N` / `failed: reason`) with the `restart`/`history` hint, and exit 0 instead of 1. Update the existing test that expects exit 1. The host-signal helper `signalled(...)` in `attach.rs` is unrelated; leave it.

## Acceptance
- `cargo clean -p hutan-daemon && cargo nextest run -p hutan-cli` fails fast with the exact message above (no build happens); `just test` from a clean target builds bins then passes.
- Test: attach to an exited session over a PTY shows the final output on the host screen and exits 0 with the status line.
- `just check` green; committed on `slice/M1-DT2`.

## Report
Summary, files changed, commands run with results, deviations. Do not edit `docs/status.md` or `docs/roadmap.md`.
