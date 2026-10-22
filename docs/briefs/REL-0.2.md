# REL-0.2 — Hutan 0.2.0 for Homebrew

Agent: `sol` (packaging and release engineer; prepares and verifies, never publishes)   Depends on: stabilization — E-12b, FX-E2E2-A, FX-E2E2-B (and FX-E2E2-C if ready) merged, `just check` green on master with no FLAKY   Branch: `slice/REL-0.2`

The user asked (2026-10-23): "make a Homebrew release once we are stable, and give me the brew install command". Publishing (tag, GitHub release, tap push) is approved for this release; the orchestrator runs the publish commands after reviewing this slice's output.

## Context
`docs/packaging.md` (§ Homebrew, § Prepare a release locally, § Publish after review), `xtask release`, templates `xtask/src/hutan.*.rb.in`, the published 0.1.0 (`v0.1.0`, tap https://github.com/robertt3kuk/homebrew-hutan), `CHANGELOG.md` (newest on top; the release renames **Unreleased** to `0.2.0 — <date>` and opens an empty **Unreleased** above it).

## Scope
1. Version `0.1.0` → `0.2.0` in the workspace package version (and anywhere the release reads it); `CHANGELOG.md` section renamed as above, content reviewed against `git log v0.1.0..HEAD` (user-facing, short).
2. Clone the tap to `~/proj/homebrew-hutan` (`gh repo clone robertt3kuk/homebrew-hutan`), run `cargo run -p xtask -- release --tap ~/proj/homebrew-hutan` from the release commit; record both SHA-256s.
3. Verify without installing into `/Applications` or touching the user's data: `codesign --verify --deep --strict` on the bundle; launch the bundled app from `target/release-artifacts/Hutan.app` with a scratch `HUTAN_HOME` and check the window opens, a terminal works, the daemon is the bundled one; `brew style` / `brew audit --strict` on the tap definitions locally (`brew audit --cask --new` style checks as far as possible without publishing); the formula's build in a scratch prefix if feasible within resource limits (`HOMEBREW_NO_INSTALL_FROM_API=1 brew install --build-from-source --formula ./Formula/hutan.rb` only into a temporary `HOMEBREW_PREFIX` is not supported — instead `cargo build --release --locked -p hutan-cli -p hutan-daemon` from the source archive in a temp dir, as the formula does).
4. Release notes draft `target/release-artifacts/NOTES.md`: the `0.2.0` changelog section + install/upgrade commands + Gatekeeper note (ad-hoc signed, Open Anyway) + "upgrade from 0.1.0: `brew upgrade --cask hutan`".
5. Report: commit to tag, artifact paths and checksums, tap diff, verification output, exact publish commands for the orchestrator (tag, `gh release create v0.2.0 … --verify-tag`, tap commit + push).

## Acceptance
All of the above with evidence; no tag, upload or push performed by this slice; `just check` green on the release commit; commit on `slice/REL-0.2` with the version bump and changelog (`chore(release): 0.2.0`).

## Out of scope
Notarization/Developer ID; `homebrew/cask` submission; `docs/status.md`, `docs/roadmap.md`.
