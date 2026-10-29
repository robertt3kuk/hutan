# UPDATES-2 — one-click update of a hand-copied app

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/UPDATES-2`

**Contract ownership:** **delegated**: extends `update.install` (method `bundle`) within the same capability; additive fields only.

## Context
ADR 0024 §3 (hand-copied bundle). UPDATES-1 merged (checker, status, brew paths, sheet).

## Items (in commit order)
1. Daemon download of `Hutan-<v>-arm64.zip` to a temp dir in the data folder (bounded size, resumable not needed), SHA-256 verified against **both** the GitHub asset digest and the tap's `Casks/hutan.rb` at the release tag (raw.githubusercontent URL, configurable for tests); extract with `ditto -x -k`; `codesign --verify --deep --strict`; refuse on any mismatch with the reason.
2. Swap: after the desktop quits (it asks the daemon to swap and quits), the daemon moves the old bundle to the Trash (NSFileManager, like `fs.trash`), moves the new one into place, and relaunches it with `open -n <bundle>` (the user's own app, by design here); if the target folder is not writable, fall back to showing the manual command; the old bundle stays intact on any failure.
3. Desktop: *Update now* for this method shows progress (download, verify, ready), then *Quit and Install*.
4. Docs `docs/packaging.md`.

## Acceptance
Daemon tests with a mock server and a fake bundle layout in a temp dir (digest mismatch from either source refuses, codesign failure refuses, swap moves old to an injected trash and puts the new one in place, unwritable target falls back), GPUI tests for the progress states; never touch `/Applications` or the real Trash in tests; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(UPDATES-2)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/updates2/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
