# BLD-1 — One integration-test binary per crate

Agent: implementer (Claude Sonnet 5.5)   Depends on: master `7c29634` (TH-2)   Branch: `slice/BLD-1`

## Context
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents → Resources, § Quality gates), `.config/nextest.toml`, `justfile`.

Why: every `crates/<crate>/tests/*.rs` file is its own test binary, and each one links the crate's whole dependency graph. Today there are 127 of them (desktop 42 — each links GPUI —, daemon 30, cli 27, agents 4, client 5, index 4, terminal 6, settings 3, terminal_view 3, gateway 2, protocol 1). Linking them dominates `just check` (12 min wall on master `b49c54a`, 2801 tests, `target/` 24 GB, `target/debug/deps` 17 GB). Cargo's recommended layout is one integration-test crate per package with the files as modules. nextest still runs every test in its own process, so tests do not start sharing process state.

## Scope
For every crate with integration tests (`agents`, `cli`, `client`, `daemon`, `desktop`, `gateway`, `index`, `protocol`, `settings`, `terminal`, `terminal_view`):
1. Move test files into `tests/it/` (`git mv`, keeping history), add `tests/it/main.rs` with one `mod <file>;` per former binary plus `mod common;` where a `tests/common/` exists (move it to `tests/it/common/`). Former `mod common;` lines in test files become `use crate::common…` / `use super::common…` as needed. Fixture directories (`tests/fixtures`, `tests/corpus`, `tests/golden`) stay where they are; fix any path computed relative to the test file (`file!()`, `include_str!`, `include_bytes!`, `#[path]`).
2. In each `Cargo.toml`: `autotests = false` under `[package]` and one `[[test]] name = "it" path = "tests/it/main.rs"`. Keep `daemon`'s `main_thread_loop` (`harness = false`) as its own binary, unchanged.
3. Inner attributes (`#![allow(...)]`) at the top of former test files stay valid as module inner attributes; keep them per module (do not widen them crate-wide). `dead_code` allowances that existed only because each binary used a subset of `common` should go away where the single binary now uses everything; keep the rest.
4. Anything that depends on the test binary's name or re-executes the current test binary (`std::env::current_exe`, a test name used as a filter, `CARGO_BIN_EXE_*` is fine) must keep working; check each of the ~27 uses.
5. Update docs and briefs that name test binaries for commands (`cargo nextest run -p X --test Y`, `-E 'binary(...)'`, `docs/agents.md` test lists, etc.) to the new form (`--test it` plus a module filter such as `-E 'test(/^harness_gpui::/)'`). Do not rewrite historical log entries in `docs/status.md` (you may not edit it anyway).
6. Nothing else: no test logic changes, no renames of test functions, no new dependencies.

## Contract
- The set of tests is unchanged: the same test count passes as on master (2816 run, 2 skipped on `7c29634`); a test's full name becomes `<module>::<old name>` inside binary `it`.
- `just check` stays the single gate entry point; `.cargo/config.toml` (4 jobs) and `.config/nextest.toml` (4 threads, retries) stay as they are.

## Acceptance
- `just check` green in the worktree (fmt, clippy `-D warnings`, nextest, cargo-deny).
- Measurements in the report, on this machine, before (master, run once in your worktree before the change) and after, from a `cargo clean` start each time: wall time of `just check`, `du -sh target`, number of test binaries (`cargo nextest list --workspace --message-format json-pretty | jq '."rust-suites" | length'`), test count. Run both with nothing else heavy running.
- No `hutand` left after test runs (`pgrep -x hutand`); every run uses a fresh `HUTAN_HOME`.

## Out of scope
Changing test behaviour, timeouts or retries; build-profile/linker/sccache changes; source code outside `tests/`, `Cargo.toml` and docs.

## Report
Summary, files changed (counts per crate), the before/after numbers, any test that needed more than a mechanical move and why, open questions. One commit on `slice/BLD-1`: `test: one integration-test binary per crate (BLD-1)`. Do not edit `docs/status.md` or `docs/roadmap.md`.
