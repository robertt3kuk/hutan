# REL-0.3.5 — prepare the 0.3.5 test release (Homebrew)

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/REL-0.3.5`

**Contract ownership:** none (version bump, release notes, local packaging checks). **Never publish**: no tag, no push, no `gh release`, no tap commit/push — the orchestrator does those.

## Context
User (2026-10-31): "yes" to 0.3.5 — installed by the user with *Update now* from 0.3.4. Contents: the 2026-10-30/31 wave (CHANGELOG § Unreleased: tabs/files, Markdown preview, font pickers, Mawas global supervisor, board/Recent/Flow/review/suggestions/digests, security fixes). Protocol moved **0.60 → 0.69**, DB schema **21 → 24** (migrations 22 grants target, 23 `agent_state_log`, 24 `digest_runs`; forward-only — say an older Hutan can't open the data after the update). The upgrade text must say 0.3.4 users can update in the app (Hutan menu → Check for Updates…, then Update now → Quit and Install), and list the **behaviour changes on update**: closing a tab now closes its session (`tabs.close_session`), preview tabs off, Mawas Home opens the Global chat (`assistant.home_scope = "follow_workspace"` for the old behaviour), Mawas's web access off by default (`[assistant] web`), marksman opt-in. The orchestrator tags the tap.

## Items (in commit order)
1. **Version**: workspace version `0.3.4` → `0.3.5` (root `Cargo.toml`, `Cargo.lock` via `cargo update -w --offline` or a build), and any test/fixture/doc that pins the current version string (grep `0.3.4`; leave history mentions alone). `CHANGELOG.md`: rename `## Unreleased` to `## 0.3.5 — 2026-10-31` and open an empty `## Unreleased` above it (rule in AGENTS.md: newest on top; released sections never edited). In the 0.3.5 section, make sure the experimental remote-hosts entry keeps its "Experimental — at your own risk" wording. `just check` green; commit `chore(release): 0.3.5`.
2. **Build the release** from that clean commit: `cargo run -p xtask -- release --tap /Users/homie/proj/homebrew-hutan` (it builds, signs ad hoc, verifies, zips, archives HEAD, writes `Casks/hutan.rb` + `Formula/hutan.rb` into the tap clone — leave those tap changes uncommitted). Record both SHA-256s.
3. **Release notes** `target/release-artifacts/NOTES.md`: the 0.3.5 CHANGELOG section, a short "Test release" line at the top (pre-release; may have rough edges), an **Experimental** paragraph repeating the remote-hosts disclaimer and how to turn it on/off, install/upgrade commands (`brew install --cask robertt3kuk/hutan/hutan`, `brew update && brew upgrade --cask hutan`, formula alternative), and the Gatekeeper note exactly as 0.3.4's notes (see `docs/packaging.md` § Security and uninstall; tap README § Gatekeeper).
4. **Local tap verification** exactly as the tap README says: a temporary copy of the tap with `file://` URLs to the generated archives (keep SHA-256s), bundle ids replaced with `dev.hutan.homebrew-test`, committed in the temp copy, tapped as `robertt3kuk/hutan-local`; `brew style`, `brew audit` (record expected online/`--new` failures separately), cask install into a **temporary `--appdir`** (never `/Applications`: the user's own Hutan is there and running), `HUTAN_HOME=<temp> hutan --version` reports 0.3.5, uninstall; formula build-from-source + `hutan --version`/`hutand --version` + `brew test`, uninstall; then untrust/untap and delete the temp copy. Never `brew autoremove`; remove only test-installed build deps.
5. Tap `README.md`: add "Local results for 0.3.5 (2026-10-31)" like the 0.3.4 section (leave uncommitted in `~/proj/homebrew-hutan`).

## Rules
- Work only in `/Users/homie/proj/hutan-wt/REL-0.3.5` (worktree exists) plus `~/proj/homebrew-hutan` (files only, no commits) and temp dirs. Shell: `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"`.
- The user's own Hutan runs on this machine (`/Applications/Hutan.app`, `~/Library/Application Support/Hutan`): never touch, quit, signal, or replace it; never `pkill` by name; never install anything into `/Applications`.
- Do not edit `docs/status.md`, `docs/roadmap.md`.
- Report: commit hash of `chore(release): 0.3.5`, both checksums and artifact paths, NOTES.md path, every check with its result, anything that failed.
