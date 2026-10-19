# 0018. User-extensible color themes and icon themes (data-only packs)

- Status: accepted (2026-10-20, the user: "icons and themes must be externalized so people can build their own"; recommendations of the spike taken as the defaults)
- Milestone: TH (with IC/DK/DB chrome work)
- Evidence: `docs/spikes/2026-10-20-theme-packs.md` (inventory, peer formats, contract, example manifests, slices TH-1…TH-4), `docs/spikes/2026-10-20-chrome-v3.md` §5 (semantic icon ids, Phosphor mapping).

## Context
Colors live in Rust (`crates/desktop/src/theme.rs`, `crates/settings/src/themes.rs`, `crates/terminal_view/src/theme.rs`), only two built-ins exist, and icons are 19 Lucide SVGs embedded per enum variant (`crates/desktop/src/icons.rs`). People cannot make their own look without rebuilding Hutan, and the icon choice (Agents shown as a git branch) is hard-coded.

## Decision
1. **Two independent pack kinds, data only**: a **color pack** (`theme.toml`: chrome, terminal palette, state/diff roles, fonts, density, metrics) and an **icon pack** (`icon-theme.toml`: semantic icon id → monochrome SVG tinted by the rendering location). No code, no layout rules, no embedded fonts.
2. **Native format TOML, schema v1**, one pack per directory under the Hutan data dir (`themes/`, `icon-themes/`); flat quoted dotted semantic keys (`"ui.sidebar.background"`, `"panel.agents"`), grammar `^[a-z][a-z0-9]*(?:[.-][a-z0-9]+)*$`. A pack `extends` an immutable built-in and may override a single token/icon; omitted entries fall back per key; invalid packs are listed disabled with diagnostics — never a crash.
3. **Built-ins are packs** in the same format, embedded in the binary and parsed by the same loader: `hutan-dark`, `hutan-light`, icon pack `phosphor` (Phosphor Regular, MIT, pinned revision; Bold optional later).
4. **Desktop-only**: the desktop and CLI (`hutan theme …`, local, no daemon) read packs; the daemon is untouched. The macOS app icon, menu-bar template image and notification identity stay fixed Hutan branding.
5. **User directory only** for now; no automatic repository/project packs; import of project-authored files is explicit.
6. **Safety and bounds**: size limits per pack/file, SVG sanitizing (no scripts, external references, foreign objects), path confinement (no symlinks/escapes), debounced bounded hot reload with last-good snapshots, AA contrast as warnings.
7. **Interoperability**: local importers (Ghostty, iTerm2, Alacritty, VS Code, Zed, WezTerm, Warp) with truthful loss reports, after native authoring; import never invents a license. No installer/marketplace in TH-1…TH-4.

## Consequences
- Slices: TH-1 color loader + built-ins + visual cutover; TH-2 icon registry + safe SVG + Phosphor semantic cutover (replaces the planned IC-1); TH-3 Settings pickers/preview/diagnostics + CLI; TH-4 importers.
- `terminal.theme` and `appearance.appearance` keep their meaning; new selectors pick light/dark color packs and the icon pack.
- Changing a token id or icon id after release is a schema change (version bump with migration), reviewed like a protocol change.
