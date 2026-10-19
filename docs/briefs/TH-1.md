# TH-1 — Color packs: TOML loader, built-ins as packs, full visual cutover

Owner: implementer. Crates: `settings`, `desktop`, `terminal_view`, docs. No protocol change; daemon untouched. Decision: ADR `docs/adr/0018-theme-and-icon-packs.md`. Contract: `docs/spikes/2026-10-20-theme-packs.md` §1 (inventory of every color/font/density/effect role), §3.1–3.5 and §3.8 (pack layout, token grammar, resolution, selection, fonts/density, discovery/reload, validation/security), §4.1 (complete example `theme.toml`), §6 TH-1 (scope, contract, tests, acceptance — follow it).

## Change (summary of §6 TH-1)

1. `crates/settings/src/themes.rs` becomes a `themes/` module: manifest parser (schema v1), token registry (one exhaustive table id → typed field + docs + fallback), resolver (`extends` a built-in, sparse overrides, complete root mode), catalog (built-ins + user packs from `<data dir>/themes/*/theme.toml`), validation diagnostics through the existing settings error/diagnostic path. GPUI-free.
2. Built-ins `hutan-dark` and `hutan-light` move into embedded TOML (`assets/themes/<id>/theme.toml`) parsed by the same loader; every palette literal/table in `desktop/src/theme.rs`, `terminal_view/src/theme.rs` and other consumers listed in §1 is replaced by resolved tokens — no competing hard-coded palettes remain. Preserve today's visual defaults exactly (document any alpha quantization; reconcile the 22/26 vs 24 status-bar height note in §1.4 to the rendered 24/24).
3. Selection: keep `appearance.appearance` (system|light|dark) as the polarity selector; add `appearance.light_theme` / `appearance.dark_theme` (default the built-ins); `terminal.theme` keeps working (`auto` + any valid color-pack id). User font/density settings win over pack defaults.
4. Discovery + hot reload: one registry, bounded debounced watcher, last-good snapshot on invalid edits, generations; immutable `Arc` snapshots — no I/O or string lookups per frame; terminal rows re-style once per committed generation; an inactive pack's change invalidates nothing.
5. Safety: size/depth caps, path confinement (no symlink escape), unknown keys warn, unsupported schema refused with a diagnostic.
6. Docs: `docs/design.md` (visual system → tokens), a new user-facing section "Themes" (where packs live, example, token list reference generated or tabulated) in `docs/design.md` or a new `docs/themes.md` added to the AGENTS.md document map.

## Tests
As listed in §6 TH-1 "Tests" (parse root/sparse; token coverage/uniqueness; dark/light golden snapshots of resolved themes; fallbacks; malformed/unknown/unsupported; caps; terminal pinning; font precedence; reload/last-good/removal/reappearance; terminal OSC/indexed/truecolor semantics unchanged; settings crate has no GPUI dependency). GPUI: editing a user theme file changes live chrome and terminal without restart; invalid edit keeps last-good and shows a diagnostic.

## Acceptance
Gate green with `CARGO_BUILD_JOBS=4` and `cargo nextest run --workspace --locked -j 4` (laptop overheats; never more): fmt, clippy `-D warnings`, nextest, cargo-deny. One commit on `slice/TH-1`. Icons are NOT in this slice (TH-2). Do not edit `core`, `protocol`, `docs/status.md`, `docs/roadmap.md`. A concurrent slice (DK-1) edits `workbench/sidebars.rs`, `render.rs` and the layout model — touch those files only for color-token substitution, keep diffs minimal. Any `hutan`/`hutand`/`hutan-desktop` run needs `HUTAN_HOME=<temp dir>` inline; never touch `/Applications/Hutan.app`, `~/Library/Application Support/Hutan`, `~/.omp`, `~/.claude`, `~/.codex` or the Keychain.
