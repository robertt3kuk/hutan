# TH-3 — Theme and icon pickers with reversible preview, diagnostics, local `hutan theme` CLI

Agent: implementer (Claude Sonnet 5.5)   Depends on: TH-1 (`853ea93`), TH-2 (icon packs)   Branch: `slice/TH-3`

Crates: `desktop` (settings sheet/pages/rows, preview plumbing), `cli` (`theme` command family), `settings` (shared validator/export/scaffold helpers). No protocol change; daemon untouched. Decision: ADR `docs/adr/0018-theme-and-icon-packs.md`. Contract: `docs/spikes/2026-10-20-theme-packs.md` §5.1 (pickers and preview), §5.2 (CLI surface, exit codes, root/sparse clarification), §3.1–§3.3 (pack layout, key grammar, selection/overrides), §3.8 (diagnostics), §6 TH-3 (scope, contract, tests, acceptance — follow it). User guide to extend: `docs/themes.md`.

## Context
Read `AGENTS.md`, ADR 0018, the spike sections above, `docs/themes.md`, `docs/design.md` § settings. Reuse what TH-1/TH-2 built: the theme/icon registries, catalog snapshots, watcher, diagnostics and `appearance.*` settings in `crates/settings/src/themes/`; the generated settings sheet in `crates/desktop/src/workbench/settings_{sheet,schema,pages,rows,layers,labels}.rs`. Do not add a second watcher, diagnostics path or picker framework. Settings v2 (`docs/spikes/2026-10-20-settings-v2.md`) will later reorganize pages; put the pickers on the existing Appearance page so ST-5 can move them as a unit.

## Scope
1. **Pickers** (§5.1): Appearance page gets light-theme, dark-theme, icon-theme selectors (mode and density stay), each showing name, author, license, origin (Embedded / User / Imported), and disabled invalid packs with their errors; search by name/id/author; actions Open pack folder, Validate / Reload, Duplicate as new pack, Reset to embedded default. The terminal-theme picker lists Auto plus every valid color pack from the same registry.
2. **Reversible preview**: moving through a picker previews live without writing; Enter/Apply writes the selection once; Escape, click-outside, closing Settings and quitting cancel and restore the **current** persisted/effective state (including system-mode or external settings changes made during preview). Preview is scoped to the window that opened it (no process-global style mutation; a second window is unaffected). Icon preview shows a representative nav/action/state/dock grid with a visible fallback example; color preview shows sample chrome, state cards, diff and a terminal ANSI row. A deliberately unreadable theme never removes the Reset path.
3. **Problems**: theme/icon diagnostics appear in the existing Settings problems footer, one actionable line per error (file, key, reason).
4. **CLI** (§5.2, local, never starts or dials the daemon): `hutan theme ls|validate|new|export` with `--kind color|icon` (default color) and `--json` (versioned output). Exit 0 ok incl. warnings, 1 errors/I/O, 2 invalid invocation; `--strict` turns warnings into exit 1. `new` writes a complete sparse pack (explicit `extends`, real metadata; icon packs get one safe example SVG) and refuses to overwrite. `export` writes editable native declarations plus every needed asset and license notice; `--resolved` writes a self-contained `root = true` pack. `import` is TH-4: do not add it.
5. Writes are atomic and touch only the user's pack directories and settings file; reset removes keys instead of writing redundant defaults.
6. Docs: `docs/themes.md` (pickers, preview semantics, CLI with examples and exit codes); `docs/design.md` if the Appearance page layout changes.

## Tests
As §6 TH-3 "Tests": picker catalog follows add/delete/invalid files; Enter persists once, Escape/click-outside/settings-close cancel without a write; system-mode and external settings changes during preview restore the latest effective state; icon and color picks are independent; a diagnostics-only generation updates Problems; invalid selection falls back; reset removes keys; two-window preview isolation; export → re-read gives an equal resolved theme and keeps notices/provenance; CLI `--json`, exit statuses, kind disambiguation, no-clobber, temp `HUTAN_HOME`, `new` → `validate` → select flow; no daemon start or socket access (assert with a socket path that must stay absent).

## Acceptance
A user creates a theme with `hutan theme new`, validates it, edits it, previews/cancels/applies it in Settings and exports it, with no rebuild; the same flow works for the two-icon example pack (§4.2). An erroneous file or key shows one actionable diagnostic in Settings and CLI, never a crash or silent fallback. Existing settings/keymap/layer diagnostics unchanged. Gate green within the repo limits (`.cargo/config.toml` 4 jobs, `.config/nextest.toml` 4 threads; never more): `just check` (fmt, clippy `-D warnings`, nextest, cargo-deny). One commit on `slice/TH-3` ending `(TH-3)`.

## Out of scope
Importers (TH-4), foreign-format export, installing packs from URLs, project-scoped packs, Settings v2 page reorganization (ST-5), any `core`/`protocol`/daemon change. Do not edit `docs/status.md` or `docs/roadmap.md`.

## Report
Summary, files changed, commands run with results (test counts), deviations from the spike contract, open questions. Every `hutan`/`hutand`/`hutan-desktop` run needs `HUTAN_HOME=<temp dir>` inline; never touch `/Applications/Hutan.app`, the user's data, `~/.omp`, `~/.claude`, `~/.codex` or Keychain; after test runs no test `hutand` may be left (`pgrep -x hutand`).
